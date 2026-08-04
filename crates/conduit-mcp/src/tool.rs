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
use conduit_provider::{Capability, Descriptor, Health, Provider, SettingsSchema};
use serde_json::Value;

use crate::client::{McpClient, McpToolInfo};

/// A tool backed by a remote MCP server.
///
/// The Conduit registration name and the tool's real MCP name are kept
/// separate because a server definition may register a tool under an alias
/// while the model must still see (and call) the real name.
pub struct McpTool {
    /// Identity, version, and the argument schema this tool declares.
    descriptor: Descriptor,
    /// The tool's real name on the MCP server.
    tool_name: String,
    /// Human-readable description shown to models.
    description: String,
    /// The argument schema exactly as the server stated it, which is what the
    /// model is shown.
    ///
    /// The descriptor's declared settings are this same document when it
    /// describes an object, which is the only shape Conduit can check a value
    /// against. A server that states something else is carried through here
    /// untouched rather than corrected.
    parameters: Value,
    /// The client used to reach the server.
    client: Arc<McpClient>,
}

impl McpTool {
    /// Wraps `tool` behind `client`, registered in Conduit as `registry_name`.
    ///
    /// The tool's argument schema becomes the descriptor's declared settings.
    /// A tool is the one capability where the settings a caller supplies and
    /// the schema the model fills in are the same named values, so it declares
    /// them once.
    #[must_use]
    pub fn new(
        registry_name: impl Into<String>,
        tool: McpToolInfo,
        client: Arc<McpClient>,
    ) -> Self {
        // A server is free to say anything, and Conduit does not correct it:
        // an argument schema that does not describe an object cannot be
        // checked against, so the descriptor declares no settings while the
        // model still sees the original through `spec`.
        let settings = SettingsSchema::new(tool.parameters.clone()).unwrap_or_else(|_| {
            tracing::debug!(
                tool = %tool.name,
                "MCP tool declares a non-object argument schema; it declares no settings"
            );
            SettingsSchema::none()
        });
        let descriptor = Descriptor::new(registry_name, Capability::Tool)
            .with_label(tool.description.clone())
            .with_settings(settings);
        Self {
            descriptor,
            tool_name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
            client,
        }
    }
}

#[async_trait::async_trait]
impl Provider for McpTool {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
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
        assert_eq!(tool.descriptor().label, "Does a thing", "the label is for people");
        assert_eq!(tool.descriptor().capability, Capability::Tool);
    }

    #[test]
    fn the_declared_settings_are_the_schema_the_model_is_shown() {
        // One document, not two: an operator screen rendering the arguments and
        // the model filling them in read the same declaration.
        let tool = McpTool::new(
            "alias",
            McpToolInfo {
                parameters: json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"],
                }),
                ..sample_tool()
            },
            Arc::new(fake_client(TestTransport::default())),
        );

        assert_eq!(tool.spec().parameters, *tool.descriptor().settings.as_json());
        assert!(tool.descriptor().validate_settings(&json!({ "city": "Denver" })).is_ok());
        assert!(
            tool.descriptor().validate_settings(&json!({ "city": 7 })).is_err(),
            "an argument of the wrong type is refused against the tool's own schema"
        );
    }

    #[test]
    fn a_tool_whose_schema_is_not_an_object_still_works() {
        // A server is free to say anything; Conduit carries it to the model
        // unchanged and simply declares no settings of its own.
        let tool = McpTool::new(
            "alias",
            McpToolInfo { parameters: json!("anything"), ..sample_tool() },
            Arc::new(fake_client(TestTransport::default())),
        );
        assert!(tool.descriptor().settings.is_empty());
        assert_eq!(tool.spec().parameters, json!("anything"), "the model sees it unchanged");
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
