use std::path::Path;
use std::sync::Arc;

use crate::languages::go::GoPlugin;
use crate::languages::java::JavaMavenPlugin;
use crate::languages::java_gradle::JavaGradlePlugin;
use crate::languages::node::NodePlugin;
use crate::languages::python::PythonPlugin;
use crate::languages::rust::RustPlugin;
use crate::plugin::{LanguageId, LanguagePlugin};

/// Central registry of language plugins.
#[derive(Clone)]
pub struct Registry {
    plugins: Vec<Arc<dyn LanguagePlugin>>,
}

impl Registry {
    /// Registry preloaded with every language plugin shipped with aide-mcp.
    /// Detection order matters:
    /// - Maven before Gradle — hybrid project with both files still
    ///   routes through Maven.
    /// - Rust / Java before Node / Python / Go — polyglot repos that
    ///   contain both a primary-language marker (`Cargo.toml` /
    ///   `pom.xml`) and a `package.json` / `pyproject.toml` / `go.mod`
    ///   (common for build-tooling and monorepos) keep their primary
    ///   language as the indexer root.
    /// - Node before Python and Go — `package.json` commonly appears
    ///   in Python and Go repos for frontend assets; the opposite
    ///   (Python/Go marker in a Node repo) is rarer.
    /// - Go last — Go repos are almost always pure Go; the detection
    ///   position only matters for unusual polyglot cases where
    ///   another marker happens to be present.
    pub fn builtin() -> Self {
        Self {
            plugins: vec![
                Arc::new(RustPlugin),
                Arc::new(JavaMavenPlugin),
                Arc::new(JavaGradlePlugin),
                Arc::new(NodePlugin),
                Arc::new(PythonPlugin),
                Arc::new(GoPlugin),
            ],
        }
    }

    pub fn plugins(&self) -> &[Arc<dyn LanguagePlugin>] {
        &self.plugins
    }

    /// Return all plugins whose [`LanguagePlugin::detect`] matches `root`.
    pub fn detect(&self, root: &Path) -> Vec<Arc<dyn LanguagePlugin>> {
        self.plugins
            .iter()
            .filter(|p| p.detect(root))
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &LanguageId) -> Option<Arc<dyn LanguagePlugin>> {
        self.plugins.iter().find(|p| &p.id() == id).cloned()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::builtin()
    }
}
