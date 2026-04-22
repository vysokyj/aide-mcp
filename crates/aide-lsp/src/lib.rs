//! LSP client + per-workspace pool for aide-mcp.
//!
//! This crate spawns language servers (rust-analyzer, …), speaks the LSP
//! JSON-RPC wire format over their stdio, and exposes a small set of
//! higher-level queries (hover, definition, diagnostics) that MCP tools
//! map onto.

pub mod client;
pub mod framing;
pub mod ops;
pub mod pool;

pub use client::{LspClient, LspClientError};
pub use ops::{HoverHit, LocationHit, PublishedDiagnostic};
pub use pool::{LspPool, LspPoolError};
