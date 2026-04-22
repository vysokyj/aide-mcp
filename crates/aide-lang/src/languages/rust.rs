use std::path::Path;

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct RustPlugin;

impl LanguagePlugin for RustPlugin {
    fn id(&self) -> LanguageId {
        LanguageId::new("rust")
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("Cargo.toml").is_file()
    }

    fn lsp(&self) -> LspSpec {
        LspSpec {
            name: "rust-analyzer",
            executable: "rust-analyzer",
        }
    }

    fn scip(&self) -> Option<ScipSpec> {
        Some(ScipSpec {
            name: "scip-rust",
            executable: "scip-rust",
        })
    }

    fn dap(&self) -> Option<DapSpec> {
        Some(DapSpec {
            name: "codelldb",
            executable: "codelldb",
        })
    }

    fn package_manager(&self) -> PackageManager {
        PackageManager {
            executable: "cargo",
            install_args: &["add"],
        }
    }

    fn runner(&self) -> Runner {
        Runner {
            executable: "cargo",
            args: &["run"],
        }
    }

    fn test_runner(&self) -> TestRunner {
        TestRunner {
            executable: "cargo",
            args: &["test"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert!(RustPlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!RustPlugin.detect(dir.path()));
    }
}
