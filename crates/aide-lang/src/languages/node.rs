//! Node.js / TypeScript plugin.
//!
//! Claims directories containing `package.json`. Covers both TypeScript
//! (`*.ts`, `*.tsx`) and plain JavaScript (`*.js`, `*.jsx`) projects —
//! the LSP is always `typescript-language-server`, which handles both
//! via the `languageId` on `textDocument/didOpen`.
//!
//! - **LSP:** `typescript-language-server` over stdio. Spawned with
//!   `--stdio` and pointed at a sibling `tsserver.js` via
//!   `--tsserver-path` so the bundled TypeScript version is the one
//!   actually used, not whatever the project happens to resolve.
//! - **SCIP:** `scip-typescript index --output <file> <workdir>`.
//! - **Runner / tests / packages:** `npm start`, `npm test`,
//!   `npm install <packages>`. Works for projects whose `package.json`
//!   has `"scripts": { "start": …, "test": … }`. Projects without
//!   `start` will need `extra_args` (e.g. `node dist/index.js`).
//!
//! Three tools auto-install via npm-registry tarballs:
//!
//! - `typescript` — the TypeScript compiler package. Extracted into
//!   `~/.aide/bin/typescript-<VERSION>/`. A symlink
//!   `~/.aide/bin/tsserver.js` points at `package/lib/tsserver.js` so
//!   the language-server wrapper can reach it through `$(dirname
//!   "$0")`.
//! - `typescript-language-server` — the LSP server. Extracted into its
//!   own versioned dir; a generated shell wrapper at
//!   `~/.aide/bin/typescript-language-server` does
//!   `exec node <extract>/package/lib/cli.mjs --tsserver-path
//!   $DIR/tsserver.js "$@"`.
//! - `scip-typescript` — the SCIP indexer (Sourcegraph). Same tarball
//!   shape; wrapper just does `exec node <extract>/package/dist/src/main.js
//!   "$@"`.
//!
//! Node itself is a system prerequisite (same posture as Java for
//! JDT-LS) — we do not bundle a Node runtime.

use std::ffi::OsString;
use std::path::Path;

use aide_install::{ArchiveFormat, DirectAsset, InstallError, Source, ToolSpec};

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct NodePlugin;

/// Pin for `typescript-language-server` on npm. Bump together with a
/// smoke test against a real TS project.
const TS_LANGUAGE_SERVER_VERSION: &str = "5.1.3";

/// Pin for the `typescript` package on npm. The language server loads
/// `package/lib/tsserver.js` from this exact version regardless of
/// what a project's own `node_modules` happen to pull in, so
/// diagnostics are reproducible.
const TYPESCRIPT_VERSION: &str = "6.0.3";

/// Pin for `@sourcegraph/scip-typescript` on npm.
const SCIP_TYPESCRIPT_VERSION: &str = "0.4.0";

impl LanguagePlugin for NodePlugin {
    fn id(&self) -> LanguageId {
        LanguageId::new("node")
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("package.json").is_file()
    }

    fn lsp(&self) -> LspSpec {
        LspSpec {
            name: "typescript-language-server",
            executable: "typescript-language-server",
        }
    }

    fn lsp_spawn_args(
        &self,
        _workspace_root: &Path,
        _paths: &aide_core::AidePaths,
    ) -> Vec<OsString> {
        // tsls auto-detects stdio on a non-TTY stdin but also accepts
        // the explicit flag; pass it to remove one flake source when
        // the env doesn't look the way the binary expects.
        vec!["--stdio".into()]
    }

    fn scip(&self) -> Option<ScipSpec> {
        Some(ScipSpec {
            name: "scip-typescript",
            executable: "scip-typescript",
        })
    }

    fn scip_args(&self, workdir: &Path, output: &Path) -> Vec<OsString> {
        vec![
            "index".into(),
            "--output".into(),
            output.as_os_str().to_os_string(),
            workdir.as_os_str().to_os_string(),
        ]
    }

    fn dap(&self) -> Option<DapSpec> {
        // js-debug (vscode-js-debug) wiring is non-trivial; deferred.
        None
    }

    fn package_manager(&self) -> PackageManager {
        PackageManager {
            executable: "npm",
            install_args: &["install"],
        }
    }

    fn runner(&self) -> Runner {
        Runner {
            executable: "npm",
            args: &["start"],
        }
    }

    fn test_runner(&self) -> TestRunner {
        TestRunner {
            executable: "npm",
            args: &["test"],
        }
    }

    fn tools(&self) -> Vec<ToolSpec> {
        // Order matters only for readability — the wrappers reference
        // sibling files in `~/.aide/bin/` that resolve at runtime, so
        // install order does not affect correctness.
        vec![
            typescript_spec(),
            typescript_language_server_spec(),
            scip_typescript_spec(),
        ]
    }

    fn is_test_symbol(&self, relative_path: &str, display_name: &str) -> bool {
        is_node_test(relative_path, display_name)
    }

    fn classify_path(&self, relative_path: &str) -> &'static str {
        classify_node_path(relative_path)
    }
}

/// Broad path-based classification of a Node/TS source file. Picks the
/// first matching bucket in this order (tests win over bins so
/// `__tests__/bin.ts` still counts as test):
///
/// - `test` — under `test/`, `tests/`, `__tests__/`, or with a
///   `*.test.{ts,tsx,js,jsx}` / `*.spec.{ts,tsx,js,jsx}` suffix
/// - `bin` — under `bin/`
/// - `example` — under `examples/`
/// - `lib` — everything else
fn classify_node_path(relative_path: &str) -> &'static str {
    if is_node_test(relative_path, "") {
        return "test";
    }
    let p = relative_path.to_ascii_lowercase();
    if p.starts_with("bin/") || p.contains("/bin/") {
        return "bin";
    }
    if p.starts_with("examples/") || p.contains("/examples/") {
        return "example";
    }
    "lib"
}

/// Node/TS test heuristic. Jest / Vitest / Mocha conventions all agree
/// that test files live under a `__tests__/`, `test/`, or `tests/`
/// directory OR carry a `*.test.*` / `*.spec.*` suffix; that's what we
/// match. Name-based detection is deliberately coarse (any function
/// whose display name starts or ends with "test") to keep pace with the
/// range of framework-specific helpers (`it`, `describe`, `expect`)
/// without hard-coding a closed list.
fn is_node_test(relative_path: &str, display_name: &str) -> bool {
    let path = relative_path.to_ascii_lowercase();
    let in_test_path = path.starts_with("test/")
        || path.starts_with("tests/")
        || path.contains("/test/")
        || path.contains("/tests/")
        || path.contains("/__tests__/")
        || path.starts_with("__tests__/");
    let has_test_suffix = has_js_test_suffix(&path);
    let name = display_name.to_ascii_lowercase();
    let looks_like_test =
        name.starts_with("test_") || name.starts_with("test ") || name.ends_with("_test");
    in_test_path || has_test_suffix || looks_like_test
}

fn has_js_test_suffix(path: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        ".test.ts",
        ".test.tsx",
        ".test.js",
        ".test.jsx",
        ".spec.ts",
        ".spec.tsx",
        ".spec.js",
        ".spec.jsx",
    ];
    SUFFIXES.iter().any(|s| path.ends_with(s))
}

/// Spec for the bundled TypeScript compiler package. Installs the
/// tarball under `~/.aide/bin/typescript-<VERSION>/` and places a
/// symlink at `~/.aide/bin/tsserver.js` pointing at
/// `package/lib/tsserver.js`. The language-server wrapper reaches this
/// symlink through `$(dirname "$0")/tsserver.js` — hence the
/// non-executable filename for `executable`.
pub fn typescript_spec() -> ToolSpec {
    let url =
        format!("https://registry.npmjs.org/typescript/-/typescript-{TYPESCRIPT_VERSION}.tgz");
    ToolSpec {
        name: "typescript".to_string(),
        version: TYPESCRIPT_VERSION.to_string(),
        executable: "tsserver.js".to_string(),
        source: Source::DirectUrl {
            label: format!("typescript-{TYPESCRIPT_VERSION}"),
            assets: vec![DirectAsset {
                triple: "any",
                url,
                archive: ArchiveFormat::TarGz { entry_path: "" },
            }],
        },
        custom_install: Some(install_typescript_symlink),
    }
}

/// Spec for the LSP server. Generates a shell wrapper that invokes
/// `node <extract>/package/lib/cli.mjs` with `--tsserver-path
/// $(dirname "$0")/tsserver.js`, which points at the symlink created
/// by [`typescript_spec`].
pub fn typescript_language_server_spec() -> ToolSpec {
    let url = format!(
        "https://registry.npmjs.org/typescript-language-server/-/typescript-language-server-{TS_LANGUAGE_SERVER_VERSION}.tgz"
    );
    ToolSpec {
        name: "typescript-language-server".to_string(),
        version: TS_LANGUAGE_SERVER_VERSION.to_string(),
        executable: "typescript-language-server".to_string(),
        source: Source::DirectUrl {
            label: format!("typescript-language-server-{TS_LANGUAGE_SERVER_VERSION}"),
            assets: vec![DirectAsset {
                triple: "any",
                url,
                archive: ArchiveFormat::TarGz { entry_path: "" },
            }],
        },
        custom_install: Some(install_ts_language_server_wrapper),
    }
}

/// Spec for the Sourcegraph SCIP indexer. Shell wrapper invokes `node
/// <extract>/package/dist/src/main.js`.
pub fn scip_typescript_spec() -> ToolSpec {
    let url = format!(
        "https://registry.npmjs.org/@sourcegraph/scip-typescript/-/scip-typescript-{SCIP_TYPESCRIPT_VERSION}.tgz"
    );
    ToolSpec {
        name: "scip-typescript".to_string(),
        version: SCIP_TYPESCRIPT_VERSION.to_string(),
        executable: "scip-typescript".to_string(),
        source: Source::DirectUrl {
            label: format!("scip-typescript-{SCIP_TYPESCRIPT_VERSION}"),
            assets: vec![DirectAsset {
                triple: "any",
                url,
                archive: ArchiveFormat::TarGz { entry_path: "" },
            }],
        },
        custom_install: Some(install_scip_typescript_wrapper),
    }
}

fn install_typescript_symlink(extract_dir: &Path, install_path: &Path) -> Result<(), InstallError> {
    let tsserver = extract_dir.join("package").join("lib").join("tsserver.js");
    if !tsserver.is_file() {
        return Err(InstallError::MissingEntry {
            entry: "package/lib/tsserver.js".into(),
            dir: extract_dir.to_path_buf(),
        });
    }
    link_to(&tsserver, install_path).map_err(InstallError::Io)
}

fn install_ts_language_server_wrapper(
    extract_dir: &Path,
    install_path: &Path,
) -> Result<(), InstallError> {
    let cli = extract_dir.join("package").join("lib").join("cli.mjs");
    if !cli.is_file() {
        return Err(InstallError::MissingEntry {
            entry: "package/lib/cli.mjs".into(),
            dir: extract_dir.to_path_buf(),
        });
    }
    let script = render_ts_language_server_wrapper(&cli);
    write_executable_script(install_path, &script).map_err(InstallError::Io)
}

fn install_scip_typescript_wrapper(
    extract_dir: &Path,
    install_path: &Path,
) -> Result<(), InstallError> {
    let main = extract_dir
        .join("package")
        .join("dist")
        .join("src")
        .join("main.js");
    if !main.is_file() {
        return Err(InstallError::MissingEntry {
            entry: "package/dist/src/main.js".into(),
            dir: extract_dir.to_path_buf(),
        });
    }
    let script = render_scip_typescript_wrapper(&main);
    write_executable_script(install_path, &script).map_err(InstallError::Io)
}

fn render_ts_language_server_wrapper(cli: &Path) -> String {
    // `--tsserver-path` resolves via `$(dirname "$0")` so the install
    // survives a relocation of `~/.aide/bin/`. The path literal for
    // `cli.mjs` is absolute because it lives in a version-suffixed
    // extract dir that is not itself a sibling of this wrapper.
    format!(
        "#!/bin/sh\n\
         # Generated by aide-mcp install for typescript-language-server\n\
         DIR=\"$(dirname \"$0\")\"\n\
         exec node \"{cli}\" --tsserver-path \"$DIR/tsserver.js\" \"$@\"\n",
        cli = cli.display(),
    )
}

fn render_scip_typescript_wrapper(main: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         # Generated by aide-mcp install for scip-typescript\n\
         exec node \"{main}\" \"$@\"\n",
        main = main.display(),
    )
}

fn link_to(target: &Path, install_path: &Path) -> std::io::Result<()> {
    if install_path.exists() || install_path.is_symlink() {
        std::fs::remove_file(install_path)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, install_path)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(target, install_path)?;
    }
    Ok(())
}

fn write_executable_script(path: &Path, content: &str) -> std::io::Result<()> {
    if path.exists() || path.is_symlink() {
        std::fs::remove_file(path)?;
    }
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        assert!(NodePlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_when_no_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert!(!NodePlugin.detect(dir.path()));
    }

    #[test]
    fn scip_args_matches_scip_typescript_cli_shape() {
        let args = NodePlugin
            .scip_args(Path::new("/p"), Path::new("/out/index.scip"))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, vec!["index", "--output", "/out/index.scip", "/p"]);
    }

    #[test]
    fn lsp_spawn_args_include_stdio() {
        let paths = aide_core::AidePaths::at(std::env::temp_dir().join("aide-test-node"));
        let args = NodePlugin.lsp_spawn_args(Path::new("/any/root"), &paths);
        assert_eq!(args, vec![OsString::from("--stdio")]);
    }

    #[test]
    fn tools_include_lsp_runtime_and_indexer_in_stable_order() {
        let names: Vec<_> = NodePlugin.tools().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![
                "typescript".to_string(),
                "typescript-language-server".to_string(),
                "scip-typescript".to_string(),
            ],
        );
    }

    #[test]
    fn pins_are_exact_versions() {
        // None of the version constants may be "latest", "*", or empty.
        for pin in [
            TS_LANGUAGE_SERVER_VERSION,
            TYPESCRIPT_VERSION,
            SCIP_TYPESCRIPT_VERSION,
        ] {
            assert!(!pin.is_empty(), "pin must not be empty");
            assert_ne!(pin, "latest", "pin must be an exact version");
            assert!(
                pin.chars().next().unwrap().is_ascii_digit(),
                "pin must start with a version digit, got {pin}",
            );
        }
    }

    #[test]
    fn tarball_urls_point_at_npm_registry_with_pinned_version() {
        let specs = [
            (typescript_spec(), TYPESCRIPT_VERSION),
            (
                typescript_language_server_spec(),
                TS_LANGUAGE_SERVER_VERSION,
            ),
            (scip_typescript_spec(), SCIP_TYPESCRIPT_VERSION),
        ];
        for (spec, pin) in specs {
            let url = match &spec.source {
                Source::DirectUrl { assets, .. } => assets[0].url.clone(),
                Source::GithubRelease { .. } => panic!("{} must be DirectUrl", spec.name),
            };
            assert!(
                url.starts_with("https://registry.npmjs.org/"),
                "{} url must point at npm registry, got {url}",
                spec.name,
            );
            assert!(
                url.contains(pin),
                "{} url lost the version pin: {url}",
                spec.name,
            );
        }
    }

    #[test]
    fn classify_node_path_buckets_by_convention() {
        assert_eq!(classify_node_path("tests/it.test.ts"), "test");
        assert_eq!(classify_node_path("src/__tests__/foo.ts"), "test");
        assert_eq!(classify_node_path("src/foo.spec.ts"), "test");
        assert_eq!(classify_node_path("src/foo.test.js"), "test");
        assert_eq!(classify_node_path("bin/cli.ts"), "bin");
        assert_eq!(classify_node_path("pkg/bin/tool.js"), "bin");
        assert_eq!(classify_node_path("examples/demo.ts"), "example");
        assert_eq!(classify_node_path("src/index.ts"), "lib");
        assert_eq!(classify_node_path("lib/foo.js"), "lib");
    }

    #[test]
    fn is_node_test_picks_up_path_and_suffix_conventions() {
        assert!(is_node_test("src/foo.test.ts", "anything"));
        assert!(is_node_test("src/foo.spec.tsx", "anything"));
        assert!(is_node_test("src/__tests__/foo.ts", "anything"));
        assert!(is_node_test("tests/it.js", "anything"));
        assert!(is_node_test("test/it.js", "anything"));
        assert!(is_node_test("src/foo.ts", "test_bar"));
        assert!(is_node_test("src/foo.ts", "bar_test"));
        assert!(!is_node_test("src/foo.ts", "bar"));
        assert!(!is_node_test("src/foo.ts", "tested"));
    }

    #[test]
    fn ts_language_server_wrapper_renders_tsserver_path_relative_to_self() {
        let script = render_ts_language_server_wrapper(Path::new(
            "/home/u/.aide/bin/typescript-language-server-5.1.3/package/lib/cli.mjs",
        ));
        assert!(script.contains(r#"DIR="$(dirname "$0")""#));
        assert!(script.contains(r#"--tsserver-path "$DIR/tsserver.js""#));
        assert!(script
            .contains("/home/u/.aide/bin/typescript-language-server-5.1.3/package/lib/cli.mjs",));
        assert!(script.starts_with("#!/bin/sh\n"));
    }

    #[test]
    fn scip_typescript_wrapper_execs_node_against_main_js() {
        let script = render_scip_typescript_wrapper(Path::new(
            "/home/u/.aide/bin/scip-typescript-0.4.0/package/dist/src/main.js",
        ));
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("exec node"));
        assert!(
            script.contains("/home/u/.aide/bin/scip-typescript-0.4.0/package/dist/src/main.js",)
        );
    }

    #[test]
    fn typescript_spec_exposes_tsserver_js_as_executable() {
        let spec = typescript_spec();
        assert_eq!(spec.executable, "tsserver.js");
        assert_eq!(spec.version, TYPESCRIPT_VERSION);
        assert!(spec.custom_install.is_some());
    }

    #[test]
    fn detects_runs_against_mixed_cargo_and_package() {
        // Hybrid repos with both Cargo.toml and package.json must still
        // claim as Node — detection order in Registry::builtin() is
        // Rust → Java → Node, so Rust wins a tie. This test just
        // asserts that NodePlugin itself claims its marker.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"y"}"#).unwrap();
        assert!(NodePlugin.detect(dir.path()));
    }
}
