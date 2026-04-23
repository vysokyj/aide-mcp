//! Shared diagnostic shape for structured compiler / tester output.
//!
//! Language plugins parse their build tool's machine-readable output
//! (e.g. `cargo --message-format=json-render-diagnostics`) into this
//! common form, and the MCP layer enriches each entry with the
//! enclosing SCIP symbol before returning it to the agent. Every
//! location field is optional because not every diagnostic has a
//! primary span (linker errors, internal compiler messages, …).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity as reported by the producing tool: typically `error`,
    /// `warning`, `note`, or `help`.
    pub level: String,

    /// Tool-specific code, e.g. `E0382` for rustc or `clippy::needless_borrow`
    /// for clippy. `None` when the tool has no code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    pub message: String,

    /// Path of the primary span — typically relative to the workspace /
    /// package root as the producing tool sees it. `None` when the
    /// diagnostic has no source span (linker errors, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// 1-indexed line numbers of the primary span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_end: Option<u32>,

    /// Filled in by the MCP layer from the last Ready SCIP index: the
    /// display name of the definition that encloses `line_start`. `None`
    /// when no SCIP is available, the path is not covered, or the
    /// lookup misses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,

    /// Human-readable rendering preserved from the producing tool when
    /// available (e.g. rustc's ANSI-coloured block). Useful when an
    /// agent wants to show the original formatting verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
}
