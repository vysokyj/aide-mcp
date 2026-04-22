//! Shared protocol primitives and schemas used across aide-mcp crates.
//!
//! - [`framing`] — `Content-Length`-framed JSON over stdio, shared by
//!   `aide-lsp` and `aide-dap`.
//! - [`ipc`] — indexer data model (commit state, timestamps, index
//!   path) surfaced through the MCP tool layer.

pub mod framing;
pub mod ipc;

pub use ipc::{CommitInfo, IndexState};
