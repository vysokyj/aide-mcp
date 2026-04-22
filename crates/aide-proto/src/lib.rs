//! Shared schemas used across aide-mcp crates.
//!
//! Currently just the indexer data model — commit state, timestamps, and
//! the path to the produced SCIP index. Kept as its own crate so the
//! types can be referenced from any tool surface without pulling in the
//! rest of the indexing machinery.

pub mod ipc;

pub use ipc::{CommitInfo, IndexState};
