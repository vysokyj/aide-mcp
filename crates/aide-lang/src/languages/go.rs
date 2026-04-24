//! Go plugin.
//!
//! Claims directories containing `go.mod`. Policy: any `go.mod` is a
//! Go module root — nested `go.mod` files for workspace members are
//! also valid claims, so a monorepo that runs aide against a
//! sub-module picks up the correct root.
//!
//! - **LSP:** `gopls` (official Go language server, part of
//!   `golang.org/x/tools`). **System install expected** — same
//!   posture as scip-java. Users obtain it via `go install
//!   golang.org/x/tools/gopls@<pinned-version>`; pre-built binaries
//!   aren't published to GitHub releases and reimplementing `go
//!   install` inside aide-install isn't worth the complexity for one
//!   language.
//! - **SCIP:** `scip-go` (Sourcegraph). **Auto-installed** from the
//!   sourcegraph/scip-go GitHub releases — pre-built tarballs exist
//!   for Linux (`x86_64`, arm64) and macOS (arm64); Intel-Mac users
//!   fall back to `go install github.com/sourcegraph/scip-go@<ver>`.
//!   scip-go requires the Go toolchain at index time (it invokes
//!   `go list` internally), so Go itself remains a system prereq.
//! - **Runner / tests / packages:** `go run ./...`, `go test ./...`,
//!   `go get <packages>`. `./...` matches every package in the
//!   module — callers who want a specific target pass it via
//!   `extra_args` (e.g. `./cmd/server`).
//!
//! DAP (`delve` via `dlv dap`) deferred — delve's DAP mode is stable
//! but the launch handshake (attach vs launch, test-binary mode,
//! remote attach) is non-trivial and wants its own milestone.

use std::ffi::OsString;
use std::path::Path;

use aide_install::{ArchiveFormat, Source, TargetAsset, ToolSpec};

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct GoPlugin;

/// Pin for scip-go. Bump together with a smoke test against a real
/// Go module.
const SCIP_GO_TAG: &str = "v0.2.3";

impl LanguagePlugin for GoPlugin {
    fn id(&self) -> LanguageId {
        LanguageId::new("go")
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("go.mod").is_file()
    }

    fn lsp(&self) -> LspSpec {
        LspSpec {
            name: "gopls",
            executable: "gopls",
        }
    }

    fn scip(&self) -> Option<ScipSpec> {
        Some(ScipSpec {
            name: "scip-go",
            executable: "scip-go",
        })
    }

    fn scip_args(&self, workdir: &Path, output: &Path) -> Vec<OsString> {
        // `--module-root` pins the go.mod location; `--output`
        // redirects the scip file away from scip-go's default
        // `index.scip` in cwd. Belt-and-suspenders with `current_dir`
        // set by the indexer worker — either flag alone has been
        // reported to misbehave on nested workspace modules.
        vec![
            "--module-root".into(),
            workdir.as_os_str().to_os_string(),
            "--output".into(),
            output.as_os_str().to_os_string(),
        ]
    }

    fn dap(&self) -> Option<DapSpec> {
        // delve-backed DAP deferred; see module-level docs.
        None
    }

    fn package_manager(&self) -> PackageManager {
        // `go get <pkg>` adds (or updates) a dependency in go.mod.
        // Callers pass full module paths like
        // `github.com/foo/bar@v1.2.3`.
        PackageManager {
            executable: "go",
            install_args: &["get"],
        }
    }

    fn runner(&self) -> Runner {
        // `./...` runs every main package in the module. Callers
        // narrow to a single binary via extra_args (e.g.
        // `./cmd/server`).
        Runner {
            executable: "go",
            args: &["run", "./..."],
        }
    }

    fn test_runner(&self) -> TestRunner {
        TestRunner {
            executable: "go",
            args: &["test", "./..."],
        }
    }

    fn tools(&self) -> Vec<ToolSpec> {
        vec![scip_go_spec()]
    }

    fn is_test_symbol(&self, relative_path: &str, display_name: &str) -> bool {
        is_go_test(relative_path, display_name)
    }

    fn classify_path(&self, relative_path: &str) -> &'static str {
        classify_go_path(relative_path)
    }
}

/// Broad path-based classification of a Go source file. Picks the
/// first matching bucket in this order:
///
/// - `test` — `*_test.go` filename (Go's compiler-enforced test
///   convention)
/// - `bin` — under `cmd/` (Go convention for binary entry points)
///   or files named `main.go` at any level with a `package main`
///   heuristic approximated by path alone
/// - `example` — under `examples/`
/// - `lib` — everything else
fn classify_go_path(relative_path: &str) -> &'static str {
    if is_go_test(relative_path, "") {
        return "test";
    }
    let p = relative_path.to_ascii_lowercase();
    if p.starts_with("cmd/") || p.contains("/cmd/") || p.ends_with("/main.go") || p == "main.go" {
        return "bin";
    }
    if p.starts_with("examples/") || p.contains("/examples/") {
        return "example";
    }
    "lib"
}

/// Go test heuristic. Go's compiler treats `_test.go` files as
/// tests — no other naming is needed. Function-level detection adds
/// the four Go test-harness prefixes: `Test`, `Benchmark`, `Example`,
/// `Fuzz`. Each prefix must be followed by at least one more
/// character so bare identifiers named exactly after the prefix
/// (e.g. a type `Test`) aren't classified as tests.
fn is_go_test(relative_path: &str, display_name: &str) -> bool {
    let path = relative_path.to_ascii_lowercase();
    let filename = path.rsplit('/').next().unwrap_or("");
    let is_test_file = filename.ends_with("_test.go");
    let looks_like_test_fn = ["Test", "Benchmark", "Example", "Fuzz"]
        .iter()
        .any(|prefix| display_name.starts_with(prefix) && display_name.len() > prefix.len());
    is_test_file || looks_like_test_fn
}

/// Spec for the scip-go SCIP indexer. Ships pre-built tarballs on
/// GitHub releases for Linux (`x86_64`, arm64) and macOS (arm64).
/// Intel-Mac users fall back to `go install` — no asset is provided
/// for that triple because the release lane doesn't publish one.
pub fn scip_go_spec() -> ToolSpec {
    // The tag `v0.2.3` and the filenames below are what
    // sourcegraph/scip-go actually publishes. Each tarball contains
    // a single `scip-go` binary at the archive root.
    let assets = vec![
        TargetAsset {
            triple: "aarch64-apple-darwin",
            filename: "scip-go-darwin-arm64.tar.gz".to_string(),
            archive: ArchiveFormat::TarGz {
                entry_path: "scip-go",
            },
        },
        TargetAsset {
            triple: "aarch64-unknown-linux-gnu",
            filename: "scip-go-linux-arm64.tar.gz".to_string(),
            archive: ArchiveFormat::TarGz {
                entry_path: "scip-go",
            },
        },
        TargetAsset {
            triple: "x86_64-unknown-linux-gnu",
            filename: "scip-go-linux-amd64.tar.gz".to_string(),
            archive: ArchiveFormat::TarGz {
                entry_path: "scip-go",
            },
        },
    ];
    ToolSpec {
        name: "scip-go".to_string(),
        version: SCIP_GO_TAG.to_string(),
        executable: "scip-go".to_string(),
        source: Source::GithubRelease {
            repo: "sourcegraph/scip-go".to_string(),
            tag: SCIP_GO_TAG.to_string(),
            assets,
        },
        custom_install: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module x\n\ngo 1.22\n").unwrap();
        assert!(GoPlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_when_no_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.go"), "package main\n").unwrap();
        assert!(!GoPlugin.detect(dir.path()));
    }

    #[test]
    fn scip_args_pins_module_root_and_output() {
        let args = GoPlugin
            .scip_args(Path::new("/repo"), Path::new("/out/index.scip"))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec!["--module-root", "/repo", "--output", "/out/index.scip"],
        );
    }

    #[test]
    fn tools_are_scip_go_only() {
        // gopls is a system-install; only scip-go auto-installs.
        let names: Vec<_> = GoPlugin.tools().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["scip-go".to_string()]);
    }

    #[test]
    fn pin_is_exact_version() {
        assert!(!SCIP_GO_TAG.is_empty());
        assert_ne!(SCIP_GO_TAG, "latest");
        assert!(
            SCIP_GO_TAG.starts_with('v'),
            "tag must start with 'v' prefix (GitHub convention), got {SCIP_GO_TAG}",
        );
    }

    #[test]
    fn scip_go_release_covers_three_triples() {
        let spec = scip_go_spec();
        let triples: Vec<&str> = match &spec.source {
            Source::GithubRelease { assets, .. } => assets.iter().map(|a| a.triple).collect(),
            Source::DirectUrl { .. } => panic!("scip-go must be GithubRelease"),
        };
        assert!(triples.contains(&"aarch64-apple-darwin"));
        assert!(triples.contains(&"aarch64-unknown-linux-gnu"));
        assert!(triples.contains(&"x86_64-unknown-linux-gnu"));
        // Intel-Mac is intentionally absent — no upstream asset.
        assert!(!triples.contains(&"x86_64-apple-darwin"));
    }

    #[test]
    fn classify_go_path_buckets_by_convention() {
        assert_eq!(classify_go_path("foo_test.go"), "test");
        assert_eq!(classify_go_path("internal/foo/foo_test.go"), "test");
        assert_eq!(classify_go_path("cmd/server/main.go"), "bin");
        assert_eq!(classify_go_path("cmd/cli/cli.go"), "bin");
        assert_eq!(classify_go_path("main.go"), "bin");
        assert_eq!(classify_go_path("pkg/serve/main.go"), "bin");
        assert_eq!(classify_go_path("examples/demo.go"), "example");
        assert_eq!(classify_go_path("pkg/foo/foo.go"), "lib");
        assert_eq!(classify_go_path("internal/bar/bar.go"), "lib");
    }

    #[test]
    fn is_go_test_picks_up_file_suffix_and_name_prefixes() {
        assert!(is_go_test("foo_test.go", "anything"));
        assert!(is_go_test("internal/foo_test.go", "anything"));
        assert!(is_go_test("src/foo.go", "TestBar"));
        assert!(is_go_test("src/foo.go", "BenchmarkBar"));
        assert!(is_go_test("src/foo.go", "ExampleBar"));
        assert!(is_go_test("src/foo.go", "FuzzBar"));
        // Bare prefix without a named suffix is a type/const, not a
        // test function.
        assert!(!is_go_test("src/foo.go", "Test"));
        assert!(!is_go_test("src/foo.go", "Benchmark"));
        assert!(!is_go_test("src/foo.go", "testHelper"));
        assert!(!is_go_test("src/foo.go", "RunTest"));
    }
}
