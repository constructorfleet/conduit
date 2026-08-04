//! The Model Context Protocol vendor: whatever tools a server advertises.

use std::sync::Arc;
use std::time::Duration;

use conduit_core::Result;
use conduit_mcp::{McpClient, McpTool};
use conduit_provider::storage::{
    McpTransport, ProviderDefinition, ProviderDefinitionVariant, ToolVariant,
};
use conduit_runtime::Providers;
use tokio::time::timeout;

use super::{unclaimed, ProviderFactory};

/// How long MCP tool discovery may take while rebuilding the runtime provider
/// registry snapshot. A provider write waits on this, so it is far shorter
/// than the client's own per-request budget.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Tools reached over MCP.
pub struct Mcp;

#[async_trait::async_trait]
impl ProviderFactory for Mcp {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Tool { variant: ToolVariant::Mcp { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Tool { variant: ToolVariant::Mcp { transport } } =
            &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };
        Ok(register_tools(providers, &definition.id, transport).await)
    }
}

/// Registers whatever tools an MCP server currently advertises.
///
/// Discovery needs the server, but saving a provider definition must not: an
/// operator can configure an endpoint before the service behind it is running.
/// So a server that cannot be reached registers no tools and is logged, rather
/// than failing the write. A later reachability test or provider write
/// rediscovers them.
///
/// Every tool is registered as `<definition id>.<tool name>`. The definition
/// id itself names the whole server rather than any one tool: a pipeline that
/// binds it is offered everything the server advertised, which is what an
/// operator adding "the weather server" to a core meant, and it keeps saying
/// so as the server grows tools. Resolution does that expansion — see
/// `conduit_runtime::plan` — so nothing is registered under the bare id here.
async fn register_tools(providers: Providers, id: &str, transport: &McpTransport) -> Providers {
    let client = Arc::new(McpClient::new(transport.clone()));
    let discovery = timeout(DISCOVERY_TIMEOUT, client.list_tools()).await;
    let tools = match discovery {
        Ok(Ok(tools)) => tools,
        Ok(Err(error)) => {
            tracing::warn!(
                provider = id,
                error = %error,
                "MCP tool discovery failed; the provider definition is saved but registers \
                 no tools until the server can be reached"
            );
            return providers;
        }
        Err(_) => {
            tracing::warn!(
                provider = id,
                timeout_secs = DISCOVERY_TIMEOUT.as_secs(),
                "MCP tool discovery timed out; the provider definition is saved but \
                 registers no tools until the server answers"
            );
            return providers;
        }
    };

    let mut providers = providers;
    for tool in tools {
        let qualified = format!("{id}.{}", tool.name);
        providers = providers.with_tool(McpTool::new(qualified, tool, Arc::clone(&client)));
    }
    providers
}
