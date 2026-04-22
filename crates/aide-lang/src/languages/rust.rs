use std::ffi::OsString;
use std::path::Path;

use aide_install::{ArchiveFormat, Source, TargetAsset, ToolSpec};

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct RustPlugin;

/// Pinned rust-analyzer release. Bump when we validate a newer tag.
const RUST_ANALYZER_TAG: &str = "2026-04-20";

/// Pinned codelldb release. Bump after smoke-testing `dap_launch`
/// against the new version.
const CODELLDB_TAG: &str = "v1.11.5";

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
        // rust-analyzer ships a built-in `scip` subcommand that emits a full
        // SCIP index for a workspace, so we reuse the same binary instead of
        // maintaining a separate scip-rust tool.
        Some(ScipSpec {
            name: "rust-analyzer",
            executable: "rust-analyzer",
        })
    }

    fn scip_args(&self, workdir: &Path, output: &Path) -> Vec<OsString> {
        vec![
            "scip".into(),
            workdir.as_os_str().to_os_string(),
            "--output".into(),
            output.as_os_str().to_os_string(),
        ]
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
        vec![rust_analyzer_spec(), codelldb_spec()]
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

fn codelldb_spec() -> ToolSpec {
    // codelldb ships a `.vsix` per platform (really a zip) containing
    // `extension/adapter/codelldb` plus the bundled lldb libraries.
    let assets = vec![
        TargetAsset {
            triple: "aarch64-apple-darwin",
            filename: "codelldb-darwin-arm64.vsix".to_string(),
            archive: ArchiveFormat::Zip {
                entry_path: "extension/adapter/codelldb",
            },
        },
        TargetAsset {
            triple: "x86_64-apple-darwin",
            filename: "codelldb-darwin-x64.vsix".to_string(),
            archive: ArchiveFormat::Zip {
                entry_path: "extension/adapter/codelldb",
            },
        },
        TargetAsset {
            triple: "aarch64-unknown-linux-gnu",
            filename: "codelldb-linux-arm64.vsix".to_string(),
            archive: ArchiveFormat::Zip {
                entry_path: "extension/adapter/codelldb",
            },
        },
        TargetAsset {
            triple: "x86_64-unknown-linux-gnu",
            filename: "codelldb-linux-x64.vsix".to_string(),
            archive: ArchiveFormat::Zip {
                entry_path: "extension/adapter/codelldb",
            },
        },
    ];
    ToolSpec {
        name: "codelldb".to_string(),
        version: CODELLDB_TAG.to_string(),
        executable: "codelldb".to_string(),
        source: Source::GithubRelease {
            repo: "vadimcn/codelldb".to_string(),
            tag: CODELLDB_TAG.to_string(),
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
