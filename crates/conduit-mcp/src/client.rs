//! MCP client: connection lifecycle and tool discovery.
//!
//! An [`McpClient`] wraps a transport factory. Opening the transport is
//! deferred until the first request, so constructing a client never touches
//! the network — registration stays cheap and tests can inject a fake
//! transport. Each exchange opens a fresh session that performs the MCP
//! `initialize` handshake before serving.

use std::sync::Arc;
use std::time::Duration;

use conduit_core::{Error, Result};
use conduit_provider::storage::McpTransport;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::transport::{open_transport, Transport};

/// How long a single JSON-RPC exchange may take before it is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The MCP protocol version this client announces in `initialize`.
const PROTOCOL_VERSION: &str = "2025-03-26";

/// A client for one MCP server.
///
/// The client is cheap to build and safe to share: it only holds a transport
/// factory, and a fresh session is opened for every request.
pub struct McpClient {
    /// Opens a fresh transport on demand.
    open: Arc<dyn Fn() -> Box<dyn Transport> + Send + Sync>,
}

impl McpClient {
    /// A client for the given transport configuration.
    #[must_use]
    pub fn new(config: McpTransport) -> Self {
        Self { open: Arc::new(move || open_transport(&config)) }
    }

    /// A client that opens `open` instead of a real transport (tests only).
    #[cfg(test)]
    pub(crate) fn with_open(
        open: impl Fn() -> Box<dyn Transport> + Send + Sync + 'static,
    ) -> Self {
        Self { open: Arc::new(open) }
    }

    /// Lists the tools the server exposes.
    ///
    /// # Errors
    ///
    /// Returns an error when the server cannot be reached, the handshake
    /// fails, or the response is malformed.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        let mut session = McpSession::connect(&*self.open).await?;
        let result = session.request("tools/list", json!({})).await?;
        let tools = parse_tools(&result)?;
        session.close().await;
        Ok(tools)
    }

    /// Invokes `name` with `arguments`, returning the flattened result.
    ///
    /// # Errors
    ///
    /// Returns an error when the server cannot be reached, the handshake
    /// fails, or the server reports a failure.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let mut session = McpSession::connect(&*self.open).await?;
        let result = session
            .request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await?;
        session.close().await;
        Ok(map_content(&result))
    }
}

/// A tool advertised by an MCP server.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    /// The tool's name, sent as the `tools/call` `name`.
    pub name: String,
    /// Human-readable description shown to models.
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub parameters: Value,
}

impl McpToolInfo {
    /// Parses one entry from a `tools/list` result.
    fn from_raw(raw: &Value) -> Result<Self> {
        let name = raw
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Config("MCP tool entry is missing a name".to_owned()))?;
        let description = raw.get("description").and_then(Value::as_str).unwrap_or_default();
        let parameters = raw.get("inputSchema").cloned().unwrap_or(Value::Null);
        Ok(Self { name: name.to_owned(), description: description.to_owned(), parameters })
    }
}

/// Parses the `result` of a `tools/list` call into tool descriptions.
///
/// # Errors
///
/// Returns [`Error::Config`] when `result` does not carry a `tools` array.
fn parse_tools(result: &Value) -> Result<Vec<McpToolInfo>> {
    let tools = result.get("tools").and_then(Value::as_array).ok_or_else(|| {
        Error::Config("MCP tools/list response omitted the tools array".to_owned())
    })?;
    tools.iter().map(McpToolInfo::from_raw).collect()
}

/// Flattens a `tools/call` result into a plain text value.
///
/// Text content items are joined with newlines so the model sees the tool's
/// output as prose. Results with no text content (or none at all) are returned
/// unchanged so no data is lost.
#[must_use]
fn map_content(result: &Value) -> Value {
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return result.clone();
    };
    let mut texts = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            texts.push(text.to_owned());
        }
    }
    if texts.is_empty() {
        result.clone()
    } else {
        Value::String(texts.join("\n"))
    }
}

/// One MCP exchange: an open transport plus the next request id.
///
/// A session is only produced after the `initialize` handshake succeeds, so
/// callers can assume the server has accepted the client.
pub struct McpSession {
    transport: Box<dyn Transport>,
    next_id: u64,
}

impl McpSession {
    /// Opens `open` and performs the MCP `initialize` handshake.
    ///
    /// # Errors
    ///
    /// Returns an error when the transport cannot be opened or the server
    /// rejects the handshake.
    pub async fn connect(
        open: &(dyn Fn() -> Box<dyn Transport> + Send + Sync),
    ) -> Result<Self> {
        let mut session = Self { transport: open(), next_id: 0 };
        session.transport.connect().await?;
        session.initialize().await?;
        Ok(session)
    }

    /// Sends a request, waiting up to [`REQUEST_TIMEOUT`] for the response.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] when the deadline passes, and the transport's
    /// own errors otherwise.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        timeout(REQUEST_TIMEOUT, self.transport.request(id, method, params)).await.map_err(
            |_| Error::Timeout { operation: format!("mcp {method}"), elapsed: REQUEST_TIMEOUT },
        )?
    }

    /// Sends a notification, waiting up to [`REQUEST_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] when the deadline passes, and the transport's
    /// own errors otherwise.
    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        timeout(REQUEST_TIMEOUT, self.transport.notify(method, params)).await.map_err(|_| {
            Error::Timeout { operation: format!("mcp {method}"), elapsed: REQUEST_TIMEOUT }
        })?
    }

    /// Performs the `initialize` handshake, then announces the client with
    /// the `notifications/initialized` notification.
    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "conduit", "version": "0.1.0" },
                }),
            )
            .await?;
        if result.get("protocolVersion").is_none() {
            return Err(Error::Config(
                "MCP initialize response omitted protocolVersion".to_owned(),
            ));
        }
        self.notify("notifications/initialized", json!({})).await
    }

    /// Closes the underlying transport.
    pub async fn close(&mut self) {
        self.transport.close().await;
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared fake transport for the client and tool tests.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use conduit_core::Result;
    use serde_json::Value;

    use super::McpClient;
    use crate::transport::Transport;

    /// A transport that answers every request from canned per-method results
    /// and records the calls it saw.
    #[derive(Clone, Default)]
    pub(crate) struct TestTransport {
        log: Arc<Mutex<Vec<(u64, String)>>>,
        results: Arc<Mutex<HashMap<String, Value>>>,
    }

    impl TestTransport {
        /// A transport answering each `(method, result)` pair.
        pub(crate) fn answering(pairs: &[(&str, Value)]) -> Self {
            let transport = Self::default();
            for (method, result) in pairs {
                transport
                    .results
                    .lock()
                    .expect("results")
                    .insert((*method).to_owned(), result.clone());
            }
            transport
        }

        /// The (id, method) pairs sent to this transport, in order.
        pub(crate) fn log(&self) -> Vec<(u64, String)> {
            self.log.lock().expect("log").clone()
        }
    }

    #[async_trait::async_trait]
    impl Transport for TestTransport {
        async fn connect(&mut self) -> Result<()> {
            Ok(())
        }

        async fn request(&mut self, id: u64, method: &str, _params: Value) -> Result<Value> {
            self.log.lock().expect("log").push((id, method.to_owned()));
            let results = self.results.lock().expect("results");
            Ok(results.get(method).cloned().unwrap_or(Value::Null))
        }

        async fn notify(&mut self, method: &str, _params: Value) -> Result<()> {
            self.log.lock().expect("log").push((0, method.to_owned()));
            Ok(())
        }

        async fn close(&mut self) {}
    }

    /// A client wired to a fresh clone of `transport` per session.
    pub(crate) fn fake_client(transport: TestTransport) -> McpClient {
        McpClient::with_open(move || Box::new(transport.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{fake_client, TestTransport};
    use serde_json::json;

    fn initialized() -> TestTransport {
        TestTransport::answering(&[
            ("initialize", json!({ "protocolVersion": "2025-03-26" })),
            ("tools/list", json!({ "tools": [] })),
        ])
    }

    #[tokio::test]
    async fn initialize_handshake_runs_before_tools_list() {
        let transport = initialized();
        let client = fake_client(transport.clone());
        let tools = client.list_tools().await.expect("list tools");
        assert!(tools.is_empty());
        let log = transport.log();
        assert_eq!(log[0].1, "initialize");
        assert_eq!(log[1].1, "notifications/initialized");
        assert_eq!(log[2].0, 1, "tools/list should use the second id");
        assert_eq!(log[2].1, "tools/list");
    }

    #[tokio::test]
    async fn list_tools_parses_the_tools_array() {
        let transport = TestTransport::answering(&[
            ("initialize", json!({ "protocolVersion": "2025-03-26" })),
            (
                "tools/list",
                json!({
                    "tools": [
                        { "name": "a", "description": "Tool A", "inputSchema": { "type": "object" } },
                        { "name": "b" },
                    ]
                }),
            ),
        ]);
        let client = fake_client(transport);
        let tools = client.list_tools().await.expect("list tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "a");
        assert_eq!(tools[0].description, "Tool A");
        assert_eq!(tools[0].parameters, json!({ "type": "object" }));
        assert_eq!(tools[1].name, "b");
        assert_eq!(tools[1].description, "");
        assert_eq!(tools[1].parameters, Value::Null);
    }

    #[tokio::test]
    async fn call_tool_returns_concatenated_text_content() {
        let transport = TestTransport::answering(&[
            ("initialize", json!({ "protocolVersion": "2025-03-26" })),
            (
                "tools/call",
                json!({
                    "content": [
                        { "type": "text", "text": "one" },
                        { "type": "text", "text": "two" },
                    ]
                }),
            ),
        ]);
        let client = fake_client(transport);
        let result = client.call_tool("a", json!({ "q": 1 })).await.expect("call");
        assert_eq!(result, json!("one\ntwo"));
    }

    #[tokio::test]
    async fn request_ids_increment_across_sessions() {
        let transport = TestTransport::answering(&[
            ("initialize", json!({ "protocolVersion": "2025-03-26" })),
            ("tools/list", json!({ "tools": [] })),
            ("tools/call", json!({ "content": [] })),
        ]);
        let client = fake_client(transport.clone());
        client.list_tools().await.expect("list tools");
        client.call_tool("a", json!({})).await.expect("call");
        let requests: Vec<(u64, String)> = transport
            .log()
            .iter()
            .filter(|(_, method)| *method != "notifications/initialized")
            .cloned()
            .collect();
        assert_eq!(
            requests,
            vec![
                (0, "initialize".to_owned()),
                (1, "tools/list".to_owned()),
                (0, "initialize".to_owned()),
                (1, "tools/call".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_tools_rejects_a_missing_tools_array() {
        let error = parse_tools(&json!({ "other": true })).expect_err("no tools array");
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn map_content_keeps_the_raw_result_without_text() {
        let result = json!({ "structured": { "x": 1 } });
        assert_eq!(map_content(&result), result);
    }
}
