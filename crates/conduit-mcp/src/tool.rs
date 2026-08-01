//! Adapts a remote MCP tool into a Conduit provider.
//!
//! An [`McpTool`] implements the [`Tool`] trait for a tool exposed by an MCP
//! server. The Conduit registration name and the tool's real MCP name are kept
//! separate because a server definition may register tools under aliases while
//! the model must still see — and call — the real names.

use std::sync::Arc;

use conduit_core::Result;
use conduit_provider::llm::ToolSpec;
use conduit_provider::tool::{Tool, ToolContext, ToolOutput};
use conduit_provider::{Health, Provider};
use serde_json::Value;

use crate::client::{McpClient, McpToolInfo};

/// A tool backed by a remote MCP server.
///
/// The Conduit registration name and the tool's real MCP name are kept
/// separate because a server definition may register a tool under an alias
/// while the model must still see (and call) the real name.
pub struct McpTool {
    /// The name the tool is registered under in Conduit.
    name: String,
    /// The tool's real name on the MCP server.
    tool_name: String,
    /// Human-readable description shown to models.
    description: String,
    /// JSON Schema for the tool's arguments.
    parameters: Value,
    /// The client used to reach the server.
    client: Arc<McpClient>,
}

impl McpTool {
    /// Wraps `tool` behind `client`, registered in Conduit as `registry_name`.
    #[must_use]
    pub fn new(
        registry_name: impl Into<String>,
        tool: McpToolInfo,
        client: Arc<McpClient>,
    ) -> Self {
        Self {
            name: registry_name.into(),
            tool_name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
            client,
        }
    }
}

#[async_trait::async_trait]
impl Provider for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    async fn health(&self) -> Health {
        match self.client.list_tools().await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.tool_name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    async fn invoke(&self, arguments: Value, _context: ToolContext) -> Result<ToolOutput> {
        let value = self.client.call_tool(&self.tool_name, arguments).await?;
        Ok(ToolOutput::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{fake_client, TestTransport};
    use conduit_core::id::ConversationId;
    use serde_json::json;

    fn sample_tool() -> McpToolInfo {
        McpToolInfo {
            name: "remote-name".to_owned(),
            description: "Does a thing".to_owned(),
            parameters: json!({ "type": "object" }),
        }
    }

    fn alias_tool(transport: TestTransport) -> McpTool {
        McpTool::new("alias", sample_tool(), Arc::new(fake_client(transport)))
    }

    #[test]
    fn spec_uses_the_real_tool_name() {
        let tool = alias_tool(TestTransport::default());
        let spec = tool.spec();
        assert_eq!(spec.name, "remote-name");
        assert_eq!(spec.description, "Does a thing");
        assert_eq!(spec.parameters, json!({ "type": "object" }));
    }

    #[test]
    fn provider_name_is_the_registration_name() {
        let tool = alias_tool(TestTransport::default());
        assert_eq!(tool.name(), "alias");
    }

    #[tokio::test]
    async fn health_checks_the_server() {
        let transport = TestTransport::answering(&[
            ("initialize", json!({ "protocolVersion": "2025-03-26" })),
            ("tools/list", json!({ "tools": [] })),
        ]);
        let tool = alias_tool(transport);
        assert_eq!(tool.health().await, Health::Healthy);
    }

    #[tokio::test]
    async fn invoke_calls_the_remote_tool() {
        let transport = TestTransport::answering(&[
            ("initialize", json!({ "protocolVersion": "2025-03-26" })),
            ("tools/call", json!({ "content": [{ "type": "text", "text": "done" }] })),
        ]);
        let tool = alias_tool(transport);
        let context = ToolContext { conversation: ConversationId::new(), speaker: None };
        let output = tool.invoke(json!({ "q": 1 }), context).await.expect("invoke");
        assert_eq!(output, ToolOutput::new(json!("done")));
    }
}
