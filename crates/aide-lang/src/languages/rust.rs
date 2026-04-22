use std::path::Path;

use aide_install::{ArchiveFormat, Source, TargetAsset, ToolSpec};

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct RustPlugin;

/// Pinned rust-analyzer release. Bump when we validate a newer tag.
const RUST_ANALYZER_TAG: &str = "2026-04-20";

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

    fn tools(&self) -> Vec<ToolSpec> {
        vec![rust_analyzer_spec()]
    }
}

fn rust_analyzer_spec() -> ToolSpec {
    let triples = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
    ];
    let assets = triples
        .iter()
        .map(|triple| TargetAsset {
            triple,
            filename: format!("rust-analyzer-{triple}.gz"),
            archive: ArchiveFormat::Gzip,
        })
        .collect();
    ToolSpec {
        name: "rust-analyzer".to_string(),
        version: RUST_ANALYZER_TAG.to_string(),
        executable: "rust-analyzer".to_string(),
        source: Source::GithubRelease {
            repo: "rust-lang/rust-analyzer".to_string(),
            tag: RUST_ANALYZER_TAG.to_string(),
            assets,
        },
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
