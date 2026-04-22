use std::path::Path;

use aide_install::ToolSpec;
use serde::{Deserialize, Serialize};

/// Stable identifier of a supported language (lowercase, kebab-case).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageId(pub String);

impl LanguageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How to obtain and launch an LSP server for this language.
#[derive(Debug, Clone)]
pub struct LspSpec {
    pub name: &'static str,
    pub executable: &'static str,
}

/// How to obtain and invoke a SCIP indexer (post-commit).
#[derive(Debug, Clone)]
pub struct ScipSpec {
    pub name: &'static str,
    pub executable: &'static str,
}

/// How to obtain and launch a DAP debug adapter.
#[derive(Debug, Clone)]
pub struct DapSpec {
    pub name: &'static str,
    pub executable: &'static str,
}

/// Package manager entry point for install-type operations.
#[derive(Debug, Clone)]
pub struct PackageManager {
    pub executable: &'static str,
    pub install_args: &'static [&'static str],
}

/// How to run the project (e.g. `cargo run`).
#[derive(Debug, Clone)]
pub struct Runner {
    pub executable: &'static str,
    pub args: &'static [&'static str],
}

/// How to run the project's tests (e.g. `cargo test`).
#[derive(Debug, Clone)]
pub struct TestRunner {
    pub executable: &'static str,
    pub args: &'static [&'static str],
}

/// Capabilities a language plugin exposes to aide-mcp.
pub trait LanguagePlugin: Send + Sync {
    fn id(&self) -> LanguageId;

    /// Return `true` if `root` appears to be a project of this language.
    fn detect(&self, root: &Path) -> bool;

    fn lsp(&self) -> LspSpec;
    fn scip(&self) -> Option<ScipSpec>;
    fn dap(&self) -> Option<DapSpec>;

    fn package_manager(&self) -> PackageManager;
    fn runner(&self) -> Runner;
    fn test_runner(&self) -> TestRunner;

    /// Tools that `project.setup` should install for this language.
    ///
    /// Typically the LSP server, SCIP indexer, and debug adapter — whichever
    /// are shipped as third-party binaries. Return an empty vec if the language
    /// relies entirely on tools found on the user's `$PATH`.
    fn tools(&self) -> Vec<ToolSpec> {
        Vec::new()
    }
}
