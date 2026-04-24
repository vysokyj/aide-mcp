use std::ffi::OsString;
use std::path::Path;

use aide_install::{ArchiveFormat, Source, TargetAsset, ToolSpec};
use aide_proto::Diagnostic;

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

    fn structured_output_args(&self) -> &'static [&'static str] {
        // JSON messages on stdout, rendered human diagnostics still on
        // stderr — the stderr stream stays a drop-in replacement for
        // the default `cargo` output so agents that only read `stderr`
        // keep working.
        &["--message-format=json-render-diagnostics"]
    }

    fn parse_diagnostics(&self, stdout: &str) -> Vec<Diagnostic> {
        parse_cargo_json_messages(stdout)
    }

    fn is_test_symbol(&self, relative_path: &str, display_name: &str) -> bool {
        is_rust_test(relative_path, display_name)
    }

    fn classify_path(&self, relative_path: &str) -> &'static str {
        classify_rust_path(relative_path)
    }
}

/// Broad path-based classification of a Rust source file. Picks the
/// first matching bucket in this order (tests win over bins because
/// integration tests can live under `tests/` regardless of name):
///
/// - `test` — under `tests/` or matching `*_test(s).rs`
/// - `bin`  — `src/main.rs` or anything under `src/bin/`
/// - `example` — under `examples/`
/// - `bench` — under `benches/`
/// - `lib` — everything else
fn classify_rust_path(relative_path: &str) -> &'static str {
    if is_rust_test(relative_path, "") {
        return "test";
    }
    let p = relative_path.to_ascii_lowercase();
    if p == "src/main.rs" || p.starts_with("src/bin/") || p.contains("/src/bin/") {
        return "bin";
    }
    if p.starts_with("examples/") || p.contains("/examples/") {
        return "example";
    }
    if p.starts_with("benches/") || p.contains("/benches/") {
        return "bench";
    }
    "lib"
}

/// Rust test heuristic: a function is considered a test when it lives
/// in a conventional test location *or* its name carries a test-like
/// prefix/suffix. Both checks are independent — integration tests
/// under `tests/*.rs` don't need `test_` prefixes to count, and
/// `#[cfg(test)] mod tests` functions named `test_foo` get picked up
/// even though the module isn't in a test directory.
fn is_rust_test(relative_path: &str, display_name: &str) -> bool {
    let path = relative_path.to_ascii_lowercase();
    let in_test_path = path.starts_with("tests/")
        || path.contains("/tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs");
    let name = display_name.to_ascii_lowercase();
    let looks_like_test =
        name.starts_with("test_") || name.ends_with("_test") || name.ends_with("_tests");
    in_test_path || looks_like_test
}

/// Parse a stdout stream of cargo machine-readable output (one JSON
/// object per line, mixed with raw program output when `cargo run` or
/// `cargo test` has proceeded past compilation) into a flat list of
/// [`Diagnostic`]s.
///
/// Non-JSON lines and JSON lines whose `reason` is not
/// `compiler-message` are silently skipped — this is normal:
/// `build-script-executed`, `compiler-artifact`, and libtest output
/// all coexist on the same stdout.
fn parse_cargo_json_messages(stdout: &str) -> Vec<Diagnostic> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<CargoMessage>(line).ok())
        .filter(|m| m.reason == "compiler-message")
        .filter_map(|m| m.message.map(diagnostic_from_cargo_message))
        .collect()
}

fn diagnostic_from_cargo_message(msg: CargoMessageBody) -> Diagnostic {
    let primary = msg.spans.iter().find(|s| s.is_primary);
    let file = primary.map(|s| s.file_name.clone());
    let line_start = primary.map(|s| s.line_start);
    let line_end = primary.map(|s| s.line_end);
    let column_start = primary.map(|s| s.column_start);
    let column_end = primary.map(|s| s.column_end);
    Diagnostic {
        level: msg.level,
        code: msg
            .code
            .and_then(|c| (!c.code.is_empty()).then_some(c.code)),
        message: msg.message,
        file,
        line_start,
        line_end,
        column_start,
        column_end,
        enclosing_symbol: None,
        rendered: msg.rendered,
    }
}

#[derive(serde::Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    message: Option<CargoMessageBody>,
}

#[derive(serde::Deserialize)]
struct CargoMessageBody {
    level: String,
    message: String,
    #[serde(default)]
    code: Option<CargoDiagnosticCode>,
    #[serde(default)]
    rendered: Option<String>,
    #[serde(default)]
    spans: Vec<CargoSpan>,
}

#[derive(serde::Deserialize)]
struct CargoDiagnosticCode {
    #[serde(default)]
    code: String,
}

#[derive(serde::Deserialize)]
struct CargoSpan {
    file_name: String,
    is_primary: bool,
    line_start: u32,
    line_end: u32,
    column_start: u32,
    column_end: u32,
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
        custom_install: None,
    }
}

/// Spec for codelldb — the LLDB-backed DAP adapter. Exposed `pub` so
/// the C/C++ plugin can reuse the same download instead of pinning a
/// separate copy.
pub fn codelldb_spec() -> ToolSpec {
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
        custom_install: None,
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

    #[test]
    fn parses_compiler_message_with_primary_span() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","package_id":"x","target":{"name":"x","kind":["lib"]},"profile":{},"features":[],"filenames":[],"executable":null,"fresh":true}"#,
            "\n",
            r#"{"reason":"compiler-message","package_id":"x","target":{"name":"x","kind":["lib"]},"message":{"rendered":"error[E0382]: borrow of moved value: `x`\n","children":[],"code":{"code":"E0382","explanation":null},"level":"error","message":"borrow of moved value: `x`","spans":[{"byte_end":120,"byte_start":100,"column_end":20,"column_start":10,"file_name":"src/foo.rs","is_primary":false,"label":null,"line_end":40,"line_start":40,"suggested_replacement":null,"suggestion_applicability":null,"text":[],"expansion":null},{"byte_end":200,"byte_start":180,"column_end":9,"column_start":5,"file_name":"src/foo.rs","is_primary":true,"label":"value used here after move","line_end":42,"line_start":42,"suggested_replacement":null,"suggestion_applicability":null,"text":[],"expansion":null}]}}"#,
            "\n",
            "raw stdout line that is not JSON at all\n",
        );
        let diags = RustPlugin.parse_diagnostics(stdout);
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.level, "error");
        assert_eq!(d.code.as_deref(), Some("E0382"));
        assert_eq!(d.message, "borrow of moved value: `x`");
        assert_eq!(d.file.as_deref(), Some("src/foo.rs"));
        assert_eq!(d.line_start, Some(42));
        assert_eq!(d.line_end, Some(42));
        assert_eq!(d.column_start, Some(5));
        assert_eq!(d.column_end, Some(9));
        assert!(d.rendered.is_some());
        assert!(d.enclosing_symbol.is_none());
    }

    #[test]
    fn skips_non_compiler_messages_and_garbage() {
        let stdout = concat!(
            r#"{"reason":"build-script-executed","package_id":"x","linked_libs":[],"linked_paths":[],"cfgs":[],"env":[],"out_dir":"..."}"#,
            "\n",
            "not json at all\n",
            r#"{"reason":"compiler-artifact","package_id":"x","target":{"name":"x","kind":["lib"]},"profile":{},"features":[],"filenames":[],"executable":null,"fresh":true}"#,
            "\n",
        );
        assert!(RustPlugin.parse_diagnostics(stdout).is_empty());
    }

    #[test]
    fn classify_rust_path_buckets_by_convention() {
        assert_eq!(classify_rust_path("tests/it.rs"), "test");
        assert_eq!(classify_rust_path("src/foo_test.rs"), "test");
        assert_eq!(classify_rust_path("src/main.rs"), "bin");
        assert_eq!(classify_rust_path("src/bin/tool.rs"), "bin");
        assert_eq!(
            classify_rust_path("crates/aide-mcp/src/bin/aide-mcp.rs"),
            "bin"
        );
        assert_eq!(classify_rust_path("examples/demo.rs"), "example");
        assert_eq!(classify_rust_path("benches/speed.rs"), "bench");
        assert_eq!(classify_rust_path("crates/aide-scip/src/lib.rs"), "lib");
        assert_eq!(classify_rust_path("src/module/inner.rs"), "lib");
    }

    #[test]
    fn is_rust_test_picks_up_conventional_paths_and_names() {
        assert!(is_rust_test("tests/it.rs", "anything"));
        assert!(is_rust_test("crates/x/tests/sub/it.rs", "anything"));
        assert!(is_rust_test("src/foo_test.rs", "anything"));
        assert!(is_rust_test("src/foo_tests.rs", "anything"));
        assert!(is_rust_test("src/foo.rs", "test_bar"));
        assert!(is_rust_test("src/foo.rs", "bar_test"));
        assert!(!is_rust_test("src/foo.rs", "bar"));
        assert!(!is_rust_test("src/foo.rs", "tested"));
    }

    #[test]
    fn handles_message_with_no_primary_span() {
        let stdout = concat!(
            r#"{"reason":"compiler-message","package_id":"x","target":{"name":"x","kind":["lib"]},"message":{"rendered":"error: linking with `cc` failed","children":[],"code":null,"level":"error","message":"linking with `cc` failed","spans":[]}}"#,
            "\n",
        );
        let diags = RustPlugin.parse_diagnostics(stdout);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].file.is_none());
        assert!(diags[0].line_start.is_none());
        assert!(diags[0].code.is_none());
    }
}
