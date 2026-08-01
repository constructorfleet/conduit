//! JSON-RPC 2.0 message types used by MCP.

use conduit_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request: a method call with an id to match the response.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    /// The protocol version marker, always `"2.0"`.
    jsonrpc: &'static str,
    /// Client-generated id echoed by the matching response.
    id: u64,
    /// The method to invoke.
    method: String,
    /// Named (or positional) arguments.
    params: Value,
}

impl Request {
    /// A request with `params`.
    #[must_use]
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self { jsonrpc: "2.0", id, method: method.into(), params }
    }
}

/// A JSON-RPC notification: a request that expects no response.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    /// The protocol version marker.
    jsonrpc: &'static str,
    /// The method to invoke.
    method: String,
    /// Named (or positional) arguments.
    params: Value,
}

impl Notification {
    /// A notification with `params`.
    #[must_use]
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self { jsonrpc: "2.0", method: method.into(), params }
    }
}

/// A JSON-RPC response.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    /// The protocol version marker.
    pub jsonrpc: String,
    /// Echoes the request id.
    pub id: u64,
    /// The successful result, when the call succeeded.
    #[serde(default)]
    pub result: Option<Value>,
    /// The error, when the call failed.
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    /// Machine-readable error code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Optional structured details.
    #[serde(default)]
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// Turns a response into its result, checking that it answers `id`.
///
/// # Errors
///
/// Returns [`Error::Config`] for a mismatched id or a response with neither
/// result nor error, and [`Error::Provider`] when the server reported a
/// JSON-RPC error.
pub fn response_result(response: Response, id: u64) -> Result<Value> {
    if response.id != id {
        return Err(Error::Config(format!(
            "JSON-RPC response id {} does not match request id {id}",
            response.id
        )));
    }
    match response.error {
        Some(error) => Err(Error::provider("mcp", error)),
        None => response.result.ok_or_else(|| {
            Error::Config("JSON-RPC response carries neither result nor error".to_owned())
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_serializes_to_the_json_rpc_shape() {
        let request = Request::new(1, "tools/list", json!({}));
        let json = serde_json::to_string(&request).expect("serialize");
        assert_eq!(json, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#);
    }

    #[test]
    fn a_response_deserializes() {
        let response: Response =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#)
                .expect("parse");
        assert_eq!(response.id, 1);
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn an_error_response_becomes_a_provider_error() {
        let response: Response = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"method not found"}}"#,
        )
        .expect("parse");
        let error = response_result(response, 2).expect_err("error response");
        assert!(error.to_string().contains("method not found"));
    }

    #[test]
    fn a_mismatched_id_is_a_config_error() {
        let response: Response =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":5,"result":{"ok":true}}"#)
                .expect("parse");
        let error = response_result(response, 2).expect_err("mismatch");
        assert!(matches!(error, Error::Config(_)));
    }
}
