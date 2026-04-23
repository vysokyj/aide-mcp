use std::ffi::OsString;
use std::path::Path;

use aide_core::AidePaths;
use aide_install::ToolSpec;
use aide_proto::Diagnostic;
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

    /// Arguments to pass to the LSP executable when spawning it for
    /// `workspace_root`. `paths` lets the plugin reserve a cache dir
    /// under `~/.aide/` (e.g. JDT-LS requires a per-workspace
    /// `-data <dir>`). Default: no extra arguments.
    fn lsp_spawn_args(&self, workspace_root: &Path, paths: &AidePaths) -> Vec<OsString> {
        let _ = (workspace_root, paths);
        Vec::new()
    }

    fn package_manager(&self) -> PackageManager;
    fn runner(&self) -> Runner;
    fn test_runner(&self) -> TestRunner;

    /// Command arguments (without the executable itself) for running the
    /// SCIP indexer over `workdir` and writing the index to `output`.
    /// Only meaningful when [`Self::scip`] returns `Some`.
    fn scip_args(&self, workdir: &Path, output: &Path) -> Vec<OsString> {
        let _ = (workdir, output);
        Vec::new()
    }

    /// Tools that `project.setup` should install for this language.
    ///
    /// Typically the LSP server, SCIP indexer, and debug adapter — whichever
    /// are shipped as third-party binaries. Return an empty vec if the language
    /// relies entirely on tools found on the user's `$PATH`.
    fn tools(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    /// Extra arguments to inject into the [`Self::runner`] / [`Self::test_runner`]
    /// command line that switch the underlying build tool into a
    /// machine-readable output mode. The MCP layer inserts these
    /// *between* the runner's default args and any user-supplied
    /// `extra_args`. Empty by default: the tool's output is left
    /// human-formatted and [`Self::parse_diagnostics`] will return no
    /// structured data.
    fn structured_output_args(&self) -> &'static [&'static str] {
        &[]
    }

    /// Parse the `stdout` produced with [`Self::structured_output_args`] in
    /// effect into a flat list of [`Diagnostic`]s. Default returns empty.
    /// Plugins override this when their build tool emits a machine
    /// format (cargo JSON, Maven/Gradle XML, …) that can be mapped into
    /// aide's common diagnostic shape.
    fn parse_diagnostics(&self, stdout: &str) -> Vec<Diagnostic> {
        let _ = stdout;
        Vec::new()
    }

    /// Language-aware heuristic: does a symbol with the given name,
    /// defined in the given repo-relative path, look like a test
    /// function? Used by test-discovery tools (`tests_for_symbol`,
    /// `tests_for_changed_files`) to filter candidate callers.
    /// Default rejects everything — plugins without a test convention
    /// will simply never light up these tools.
    fn is_test_symbol(&self, relative_path: &str, display_name: &str) -> bool {
        let _ = (relative_path, display_name);
        false
    }

    /// Classify a repo-relative path into a broad category. Used by
    /// impact-analysis tools so callers can be grouped into "test",
    /// "bin", "lib", "example", "bench" without each caller
    /// re-inventing the language's layout conventions. Default
    /// returns `"code"` — a neutral, non-informative label.
    fn classify_path(&self, relative_path: &str) -> &'static str {
        let _ = relative_path;
        "code"
    }
}
