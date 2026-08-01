//! Transports for talking to an MCP server.
//!
//! MCP servers can be reached over stdio, streamable HTTP, or SSE. Each
//! variant is a [`Transport`]: a JSON-RPC channel that handles connection
//! setup, message framing, and id-matched request/response correlation. The
//! MCP `initialize` handshake is deliberately *not* part of a transport —
//! that lives in the session layer — so [`Transport::connect`] only opens the
//! underlying channel.

use std::io::ErrorKind;

use conduit_core::{Error, Result};
use conduit_provider::storage::McpTransport;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::jsonrpc::{response_result, Notification, Request, Response};
use crate::sse::{Decoder, SseEvent};

/// A transport: a JSON-RPC channel to an MCP server.
///
/// Implementations are responsible for opening the channel, framing messages,
/// and discarding responses whose id does not match the outstanding request.
#[async_trait::async_trait]
pub trait Transport: Send {
    /// Opens the underlying channel (spawns the process or establishes the
    /// stream). No MCP handshake happens here.
    ///
    /// # Errors
    ///
    /// Returns an error when the channel cannot be opened.
    async fn connect(&mut self) -> Result<()>;

    /// Sends a request and waits for the response with the matching id.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be sent, the channel closes
    /// before the response arrives, or the server reports a JSON-RPC error.
    async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value>;

    /// Sends a notification, which expects no response.
    ///
    /// # Errors
    ///
    /// Returns an error when the notification cannot be sent.
    async fn notify(&mut self, method: &str, params: Value) -> Result<()>;

    /// Closes the channel, releasing any process or stream.
    async fn close(&mut self);
}

/// Opens the transport described by a saved provider definition.
///
/// The transport is opened lazily: nothing is spawned or connected until
/// [`Transport::connect`] is called.
#[must_use]
pub fn open_transport(config: &McpTransport) -> Box<dyn Transport> {
    match config {
        McpTransport::Sse { url } => Box::new(SseTransport::new(url.clone())),
        McpTransport::StreamableHttp { url } => {
            Box::new(StreamableHttpTransport::new(url.clone()))
        }
        McpTransport::Stdio { command, args } => {
            Box::new(StdioTransport::new(command.clone(), args.clone()))
        }
    }
}

/// Builds a provider error carrying a plain message.
fn provider_msg(message: impl Into<String>) -> Error {
    Error::provider("mcp", std::io::Error::other(message.into()))
}

/// Serializes `message` as one newline-terminated JSON line on `writer`.
async fn write_line<W, M>(writer: &mut W, message: &M) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    M: serde::Serialize,
{
    let mut bytes =
        serde_json::to_vec(message).map_err(|error| Error::provider("mcp", error))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await.map_err(|error| Error::provider("mcp", error))?;
    writer.flush().await.map_err(|error| Error::provider("mcp", error))
}

/// Logs a child process's stderr line by line.
async fn forward_stderr(stderr: tokio::process::ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => tracing::warn!(target: "conduit_mcp::stdio", "{}", line.trim_end()),
        }
    }
}

/// Talks to a local MCP server over its standard input and output.
///
/// Messages are newline-delimited JSON objects. Responses are matched to
/// requests by id, so lines that are not JSON, not JSON-RPC responses, or that
/// answer a different id are skipped.
pub struct StdioTransport {
    /// The executable to spawn.
    command: String,
    /// Extra arguments for the executable.
    args: Vec<String>,
    /// The running child process, once connected.
    session: Option<StdioSession>,
}

struct StdioSession {
    /// The child process, kept alive for the duration of the session.
    child: tokio::process::Child,
    /// The child's standard input.
    stdin: tokio::process::ChildStdin,
    /// The child's standard output, read line by line.
    stdout: BufReader<tokio::process::ChildStdout>,
    /// Task forwarding the child's stderr to the logs.
    stderr_task: tokio::task::JoinHandle<()>,
}

impl StdioTransport {
    /// A transport that spawns `command` with `args`.
    #[must_use]
    pub fn new(command: String, args: Vec<String>) -> Self {
        Self { command, args, session: None }
    }
}

#[async_trait::async_trait]
impl Transport for StdioTransport {
    async fn connect(&mut self) -> Result<()> {
        let mut command = tokio::process::Command::new(&self.command);
        command
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| Error::provider("mcp", error))?;

        // The server's stderr is diagnostics, not protocol data; forward it to
        // the logs so setup problems stay visible.
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Config("failed to capture MCP server stderr".to_owned()))?;
        let stderr_task = tokio::spawn(forward_stderr(stderr));

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Config("failed to capture MCP server stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Config("failed to capture MCP server stdout".to_owned()))?;

        self.session =
            Some(StdioSession { child, stdin, stdout: BufReader::new(stdout), stderr_task });
        Ok(())
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Config("stdio transport is not connected".to_owned()))?;
        let request = Request::new(id, method, params);
        write_line(&mut session.stdin, &request).await?;

        loop {
            let mut raw = String::new();
            let read = session
                .stdout
                .read_line(&mut raw)
                .await
                .map_err(|error| Error::provider("mcp", error))?;
            if read == 0 {
                return Err(Error::provider(
                    "mcp",
                    std::io::Error::new(ErrorKind::UnexpectedEof, "MCP server closed stdout"),
                ));
            }
            let value: Value = match serde_json::from_str(&raw) {
                Ok(value) => value,
                // A stray non-JSON line from the server; keep reading.
                Err(_) => continue,
            };
            let response: Response = match serde_json::from_value(value) {
                Ok(response) => response,
                // Not a JSON-RPC response (for example, a server notification).
                Err(_) => continue,
            };
            if response.id != id {
                // A stale or concurrent response; keep reading.
                continue;
            }
            return response_result(response, id);
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Config("stdio transport is not connected".to_owned()))?;
        let notification = Notification::new(method, params);
        write_line(&mut session.stdin, &notification).await
    }

    async fn close(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.stderr_task.abort();
            let _ = session.child.kill().await;
            let _ = session.child.wait().await;
        }
    }
}

/// POSTs a JSON-RPC message to `url`, failing on a non-success status.
async fn sse_post<M>(url: &str, message: &M) -> Result<()>
where
    M: serde::Serialize,
{
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .json(message)
        .send()
        .await
        .map_err(|error| Error::provider("mcp", error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(provider_msg(format!("MCP POST to {url} returned {status}")));
    }
    Ok(())
}

/// Talks to a remote MCP server over the legacy SSE transport.
///
/// The client opens a GET stream carrying `endpoint` and `message` events. The
/// first `endpoint` event names the POST URL for outgoing requests; incoming
/// responses arrive as `message` events on the GET stream.
pub struct SseTransport {
    /// The URL of the SSE stream.
    url: String,
    /// The open stream, once connected.
    session: Option<SseSession>,
}

struct SseSession {
    /// POST URL for outgoing requests, from the `endpoint` event.
    post_url: String,
    /// Incoming events from the GET stream.
    rx: mpsc::Receiver<Result<SseEvent>>,
    /// Task draining the GET stream; aborted on close.
    reader_task: tokio::task::JoinHandle<()>,
}

impl SseTransport {
    /// A transport that opens `url` as an SSE stream.
    #[must_use]
    pub fn new(url: String) -> Self {
        Self { url, session: None }
    }
}

#[async_trait::async_trait]
impl Transport for SseTransport {
    async fn connect(&mut self) -> Result<()> {
        let client = reqwest::Client::new();
        let response = client
            .get(&self.url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|error| Error::provider("mcp", error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(provider_msg(format!("SSE connect returned {status}")));
        }

        let mut stream = response.bytes_stream();
        let (tx, mut rx): (mpsc::Sender<Result<SseEvent>>, mpsc::Receiver<Result<SseEvent>>) =
            mpsc::channel(64);
        // Drain the GET stream in the background, forwarding each event. The
        // task ends when the receiver is dropped or the stream closes.
        let reader_task = tokio::spawn(async move {
            let mut decoder = Decoder::new();
            loop {
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        for event in decoder.push(&chunk) {
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Some(Err(error)) => {
                        let _ = tx.send(Err(Error::provider("mcp", error))).await;
                        return;
                    }
                    None => return,
                }
            }
        });

        // The POST endpoint arrives as the first `endpoint` event.
        let post_url = loop {
            match rx.recv().await {
                Some(Ok(event)) if event.name == "endpoint" => {
                    break Some(resolve_endpoint(&self.url, &event.data));
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => return Err(error),
                None => break None,
            }
        };
        let post_url = post_url.ok_or_else(|| {
            Error::Config("SSE stream ended before an endpoint event arrived".to_owned())
        })?;

        self.session = Some(SseSession { post_url, rx, reader_task });
        Ok(())
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Config("SSE transport is not connected".to_owned()))?;
        let request = Request::new(id, method, params);
        sse_post(&session.post_url, &request).await?;

        loop {
            let event = session.rx.recv().await.ok_or_else(|| {
                Error::provider(
                    "mcp",
                    std::io::Error::new(ErrorKind::UnexpectedEof, "SSE stream closed"),
                )
            })?;
            let event = event?;
            if event.name != "message" {
                continue;
            }
            let value: Value = match serde_json::from_str(&event.data) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let response: Response = match serde_json::from_value(value) {
                Ok(response) => response,
                Err(_) => continue,
            };
            if response.id != id {
                continue;
            }
            return response_result(response, id);
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Config("SSE transport is not connected".to_owned()))?;
        let notification = Notification::new(method, params);
        sse_post(&session.post_url, &notification).await
    }

    async fn close(&mut self) {
        if let Some(session) = self.session.take() {
            session.reader_task.abort();
        }
    }
}

/// Talks to a remote MCP server over the streamable HTTP transport.
///
/// Requests are POSTed to the endpoint. The server answers either with a
/// single JSON body (for immediate results) or with an SSE stream carrying
/// `message` events (for long-running or streaming results).
pub struct StreamableHttpTransport {
    /// The endpoint URL.
    url: String,
}

impl StreamableHttpTransport {
    /// A transport that POSTs to `url`.
    #[must_use]
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

/// Reads an SSE response stream, returning the response matching `id`.
async fn stream_response(response: reqwest::Response, id: u64) -> Result<Value> {
    let mut stream = response.bytes_stream();
    let mut decoder = Decoder::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| Error::provider("mcp", error))?;
        for event in decoder.push(&chunk) {
            if event.name != "message" {
                continue;
            }
            let value: Value = match serde_json::from_str(&event.data) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let response: Response = match serde_json::from_value(value) {
                Ok(response) => response,
                Err(_) => continue,
            };
            if response.id != id {
                continue;
            }
            return response_result(response, id);
        }
    }
    Err(Error::provider(
        "mcp",
        std::io::Error::new(ErrorKind::UnexpectedEof, "streamable HTTP stream closed"),
    ))
}

#[async_trait::async_trait]
impl Transport for StreamableHttpTransport {
    async fn connect(&mut self) -> Result<()> {
        // Stateless per request; there is nothing to open.
        Ok(())
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        let request = Request::new(id, method, params);
        let client = reqwest::Client::new();
        let response = client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .json(&request)
            .send()
            .await
            .map_err(|error| Error::provider("mcp", error))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(provider_msg(format!(
                "MCP POST to {} returned {status}: {body}",
                self.url
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_default();

        if content_type.contains("text/event-stream") {
            stream_response(response, id).await
        } else {
            let value: Value =
                response.json().await.map_err(|error| Error::provider("mcp", error))?;
            let response: Response = serde_json::from_value(value).map_err(|error| {
                Error::Config(format!("malformed JSON-RPC response: {error}"))
            })?;
            response_result(response, id)
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let notification = Notification::new(method, params);
        let client = reqwest::Client::new();
        let response = client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&notification)
            .send()
            .await
            .map_err(|error| Error::provider("mcp", error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(provider_msg(format!("MCP POST to {} returned {status}", self.url)));
        }
        Ok(())
    }

    async fn close(&mut self) {}
}

/// Resolves an endpoint URL against the URL of the stream it came from.
///
/// MCP servers commonly answer the initial `endpoint` event with a relative
/// path; this turns it into an absolute URL. Absolute endpoints pass through
/// unchanged, root-relative paths keep the stream's scheme and authority, and
/// other relative paths are joined against the stream URL's directory.
fn resolve_endpoint(base: &str, endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.to_owned();
    }
    if let Some(scheme_sep) = base.find("://") {
        let authority_start = scheme_sep + 3;
        if endpoint.starts_with('/') {
            // Replace the path, keeping scheme and authority.
            let authority_end =
                base[authority_start..].find('/').map_or(base.len(), |i| authority_start + i);
            return format!("{}{}", &base[..authority_end], endpoint);
        }
        // Relative: join against the stream URL's directory.
        if let Some(slash) = base[authority_start..].rfind('/') {
            let slash = authority_start + slash;
            return format!("{}{}", &base[..=slash], endpoint);
        }
        return format!("{base}/{endpoint}");
    }
    // No scheme; treat `endpoint` as already absolute.
    endpoint.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_endpoint_passes_absolute_urls_through() {
        assert_eq!(
            resolve_endpoint("http://h/mcp/sse", "http://other/messages"),
            "http://other/messages"
        );
    }

    #[test]
    fn resolve_endpoint_keeps_authority_for_root_paths() {
        assert_eq!(
            resolve_endpoint("http://h:8080/mcp/sse", "/messages"),
            "http://h:8080/messages"
        );
    }

    #[test]
    fn resolve_endpoint_joins_relative_paths_into_the_stream_directory() {
        assert_eq!(
            resolve_endpoint("http://h:8080/mcp/sse", "messages"),
            "http://h:8080/mcp/messages"
        );
    }

    #[test]
    fn resolve_endpoint_handles_a_scheme_only_base() {
        assert_eq!(resolve_endpoint("http://h:8080", "messages"), "http://h:8080/messages");
    }

    /// Spawns an axum server answering POST /mcp with `handler`, mirroring the
    /// mock-server pattern used across the API integration tests.
    async fn spawn_mock(
        handler: axum::routing::MethodRouter,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let app = axum::Router::new().route("/mcp", handler);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}/mcp"), server)
    }

    /// Answers with a single JSON body echoing the request id.
    async fn mock_json_mcp(body: String) -> impl axum::response::IntoResponse {
        let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } }).to_string(),
        )
    }

    /// Answers with an SSE stream that first carries a stale response for id
    /// 999 and then the real response, so the transport must skip the stale
    /// event.
    async fn mock_sse_mcp(body: String) -> impl axum::response::IntoResponse {
        let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let stale = json!({ "jsonrpc": "2.0", "id": 999, "result": { "stale": true } });
        let answer = json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } });
        let payload =
            format!("event: message\ndata: {stale}\n\nevent: message\ndata: {answer}\n\n");
        ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], payload)
    }

    #[tokio::test]
    async fn streamable_http_round_trips_a_json_response() {
        let (url, server) = spawn_mock(axum::routing::post(mock_json_mcp)).await;
        let mut transport = StreamableHttpTransport::new(url);
        transport.connect().await.expect("connect");
        let result = transport.request(1, "tools/list", json!({})).await.expect("request");
        assert_eq!(result, json!({ "ok": true }));
        transport.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn streamable_http_skips_stale_events_in_a_stream() {
        let (url, server) = spawn_mock(axum::routing::post(mock_sse_mcp)).await;
        let mut transport = StreamableHttpTransport::new(url);
        transport.connect().await.expect("connect");
        let result = transport.request(7, "tools/list", json!({})).await.expect("request");
        assert_eq!(result, json!({ "ok": true }));
        transport.close().await;
        server.abort();
    }

    /// A tiny Python MCP server used to exercise the stdio transport: echoes
    /// the request method back, and answers a request with id 2 by first
    /// sending a stale response for id 1.
    const ECHO_SCRIPT: &str = r#"
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if "id" in msg:
        rid = msg["id"]
        if rid == 2:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"stale": True}}) + "\n")
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {"echo": msg["method"]}}) + "\n")
        sys.stdout.flush()
"#;

    fn stdio_transport() -> StdioTransport {
        StdioTransport::new("python3".to_owned(), vec!["-c".to_owned(), ECHO_SCRIPT.to_owned()])
    }

    #[tokio::test]
    async fn stdio_round_trips_a_request() {
        let mut transport = stdio_transport();
        transport.connect().await.expect("connect");
        let result = transport.request(1, "tools/list", json!({})).await.expect("request");
        assert_eq!(result, json!({ "echo": "tools/list" }));
        transport.close().await;
    }

    #[tokio::test]
    async fn stdio_skips_responses_for_other_ids() {
        let mut transport = stdio_transport();
        transport.connect().await.expect("connect");
        let result = transport.request(2, "tools/list", json!({})).await.expect("request");
        assert_eq!(result, json!({ "echo": "tools/list" }));
        transport.close().await;
    }

    #[tokio::test]
    async fn stdio_sends_a_notification() {
        let mut transport = stdio_transport();
        transport.connect().await.expect("connect");
        transport.notify("notifications/initialized", json!({})).await.expect("notify");
        transport.close().await;
    }
}
