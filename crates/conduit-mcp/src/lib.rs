//! Model Context Protocol tool providers for Conduit.
//!
//! This crate implements an MCP client that can connect to MCP servers over
//! any of the standard transports (stdio, streamable HTTP, or SSE) and expose
//! the server's tools through Conduit's [`Tool`] trait.
//!
//! The public entry points are [`McpClient`], which manages the connection
//! lifecycle and JSON-RPC exchange, and [`McpTool`], which adapts a remote MCP
//! tool into a [`Tool`] usable by the rest of the system.

pub mod client;
pub mod jsonrpc;
pub mod sse;
pub mod tool;
pub mod transport;

pub use client::{McpClient, McpSession, McpToolInfo};
pub use tool::McpTool;
