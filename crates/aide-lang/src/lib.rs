//! Language plugin registry for aide-mcp.
//!
//! Each supported language implements [`LanguagePlugin`] and declares:
//! - how to detect its projects in a working tree,
//! - which LSP / SCIP indexer / debug adapter to fetch,
//! - how to run, test, and install packages.
//!
//! The [`Registry`] returns all plugins whose [`LanguagePlugin::detect`]
//! matches a given project root.

pub mod languages;
pub mod plugin;
pub mod registry;

pub use plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};
pub use registry::Registry;
