//! Minimal Debug Adapter Protocol client for aide-mcp.
//!
//! Speaks DAP over the adapter's stdio using the same Content-Length
//! framing as LSP (shared via `aide_proto::framing`). The surface is
//! intentionally small — enough to drive a standard
//! launch/setBreakpoints/continue/inspect loop from MCP tools. If a
//! future tool needs a DAP request we do not yet wrap, call
//! [`DapClient::request`] directly with raw `serde_json::Value`
//! arguments.

pub mod client;

pub use client::{
    DapCapabilities, DapClient, DapClientError, Scope, StackFrame, StoppedInfo, Variable,
};
