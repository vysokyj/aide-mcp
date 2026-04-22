//! IPC protocol between `aide-mcp` and the `aide-indexer` daemon.
//!
//! Wire format is newline-delimited JSON (`serde_json` values followed by
//! `\n`) over a unix-domain stream. One request, one response, one line each.
//! Keep the schema strictly additive — the daemon may outlive many mcp
//! versions.

pub mod framing;
pub mod ipc;
pub mod socket;

pub use ipc::{CommitInfo, IndexState, Request, Response};
pub use socket::{default_indexer_socket, INDEXER_SOCKET_NAME};
