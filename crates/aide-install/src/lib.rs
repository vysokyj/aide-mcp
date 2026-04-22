//! Install engine for third-party tools (LSP servers, SCIP indexers, debug adapters).
//!
//! Downloads binaries to `~/.aide/bin/` and tracks installed versions in a manifest.
//! Tools are described by [`ToolSpec`]; actual install happens via [`install_tool`].

pub mod install;
pub mod manifest;
pub mod spec;
pub mod target;

pub use install::{install_tool, InstallError, InstallOutcome};
pub use manifest::{InstalledRecord, Manifest};
pub use spec::{ArchiveFormat, CustomInstallFn, DirectAsset, Source, TargetAsset, ToolSpec};
pub use target::{current_triple, TargetTripleError};
