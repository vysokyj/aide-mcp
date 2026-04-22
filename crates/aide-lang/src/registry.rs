use std::path::Path;
use std::sync::Arc;

use crate::languages::java::JavaMavenPlugin;
use crate::languages::rust::RustPlugin;
use crate::plugin::{LanguageId, LanguagePlugin};

/// Central registry of language plugins.
#[derive(Clone)]
pub struct Registry {
    plugins: Vec<Arc<dyn LanguagePlugin>>,
}

impl Registry {
    /// Registry preloaded with every language plugin shipped with aide-mcp.
    pub fn builtin() -> Self {
        Self {
            plugins: vec![Arc::new(RustPlugin), Arc::new(JavaMavenPlugin)],
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
