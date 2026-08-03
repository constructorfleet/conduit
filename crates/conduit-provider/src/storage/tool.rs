//! Tool provider variants.

use serde::{Deserialize, Serialize};

/// MCP transport variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// Server-sent events transport.
    Sse {
        /// MCP endpoint URL.
        url: String,
    },
    /// Streamable HTTP transport.
    StreamableHttp {
        /// MCP endpoint URL.
        url: String,
    },
    /// Local stdio command transport.
    Stdio {
        /// Command to run.
        command: String,
        /// Command arguments.
        #[serde(default)]
        args: Vec<String>,
    },
}

/// Tool provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolVariant {
    /// MCP tool provider.
    Mcp {
        /// Tool transport configuration.
        transport: McpTransport,
    },
}

impl ToolVariant {
    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        self.clone()
    }
}
