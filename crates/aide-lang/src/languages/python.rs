//! Python plugin.
//!
//! Claims directories containing `pyproject.toml`, `setup.py`, or
//! `requirements.txt` — covers PEP 621 / Poetry / PDM / Hatch / legacy
//! setuptools / bare-requirements flows.
//!
//! - **LSP:** `pyright-langserver` (Microsoft, TypeScript-bundled).
//!   Installed from the `pyright` npm tarball with a generated shell
//!   wrapper that does `exec node <extract>/package/langserver.index.js
//!   --stdio "$@"`. Node runtime is a system prerequisite (shared with
//!   the Node plugin).
//! - **SCIP:** `scip-python index --output <file> <workdir>`. Same
//!   npm-tarball install pattern.
//! - **Runner / tests / packages:** `python3`, `python3 -m pytest`,
//!   `python3 -m pip install`. Using `-m pip` instead of a bare `pip`
//!   executable makes the install hit the same interpreter the project
//!   will use to run (venv-aware), and `-m pytest` works whether pytest
//!   is a standalone bin or only importable from the current env.
//!
//! Python interpreter itself is a system prerequisite. Virtualenv /
//! conda / pyenv management is out of scope — the plugin runs
//! `python3` from `$PATH`, which agents select by activating the
//! appropriate environment before invoking aide's run tools.

use std::ffi::OsString;
use std::path::Path;

use aide_install::{ArchiveFormat, DirectAsset, InstallError, Source, ToolSpec};

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct PythonPlugin;

/// Pin for `pyright` on npm. Bump together with a smoke test against
/// a real Python project.
const PYRIGHT_VERSION: &str = "1.1.409";

/// Pin for `@sourcegraph/scip-python` on npm.
const SCIP_PYTHON_VERSION: &str = "0.6.6";

impl LanguagePlugin for PythonPlugin {
    fn id(&self) -> LanguageId {
        LanguageId::new("python")
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("pyproject.toml").is_file()
            || root.join("setup.py").is_file()
            || root.join("requirements.txt").is_file()
    }

    fn lsp(&self) -> LspSpec {
        LspSpec {
            name: "pyright-langserver",
            executable: "pyright-langserver",
        }
    }

    fn lsp_spawn_args(
        &self,
        _workspace_root: &Path,
        _paths: &aide_core::AidePaths,
    ) -> Vec<OsString> {
        // pyright-langserver requires `--stdio` to speak LSP on stdio;
        // without it the binary exits immediately.
        vec!["--stdio".into()]
    }

    fn scip(&self) -> Option<ScipSpec> {
        Some(ScipSpec {
            name: "scip-python",
            executable: "scip-python",
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
        // debugpy wiring is a follow-up milestone.
        None
    }

    fn package_manager(&self) -> PackageManager {
        // `python3 -m pip install` — routes through whichever Python
        // is on PATH, so a venv-activated shell installs into the venv
        // rather than polluting the system interpreter.
        PackageManager {
            executable: "python3",
            install_args: &["-m", "pip", "install"],
        }
    }

    fn runner(&self) -> Runner {
        // No default entry point — Python projects pick their own main
        // module. Agents pass the script / `-m module` via extra_args.
        Runner {
            executable: "python3",
            args: &[],
        }
    }

    fn test_runner(&self) -> TestRunner {
        // `-m pytest` works whether pytest is installed as a bin or
        // only importable from the current environment.
        TestRunner {
            executable: "python3",
            args: &["-m", "pytest"],
        }
    }

    fn tools(&self) -> Vec<ToolSpec> {
        vec![pyright_spec(), scip_python_spec()]
    }

    fn is_test_symbol(&self, relative_path: &str, display_name: &str) -> bool {
        is_python_test(relative_path, display_name)
    }

    fn classify_path(&self, relative_path: &str) -> &'static str {
        classify_python_path(relative_path)
    }
}

/// Broad path-based classification of a Python source file. Picks the
/// first matching bucket in this order (tests win over bins so
/// `tests/cli/main.py` still counts as test):
///
/// - `test` — under `test/`, `tests/`, or matching `test_*.py` /
///   `*_test.py`
/// - `bin` — under `bin/` or `scripts/`, or file named `__main__.py`
/// - `example` — under `examples/`
/// - `lib` — everything else
fn classify_python_path(relative_path: &str) -> &'static str {
    if is_python_test(relative_path, "") {
        return "test";
    }
    let p = relative_path.to_ascii_lowercase();
    if p.starts_with("bin/")
        || p.contains("/bin/")
        || p.starts_with("scripts/")
        || p.contains("/scripts/")
        || p.ends_with("/__main__.py")
        || p == "__main__.py"
    {
        return "bin";
    }
    if p.starts_with("examples/") || p.contains("/examples/") {
        return "example";
    }
    "lib"
}

/// Python test heuristic. pytest / unittest conventions converge on:
/// tests live under `test/` or `tests/` directories OR file names of
/// the form `test_*.py` / `*_test.py`. Function-level naming follows
/// the same `test_*` prefix and `TestCase` class pattern, so the
/// display-name check catches `#[test]`-equivalent functions even when
/// they live in an unusual directory.
fn is_python_test(relative_path: &str, display_name: &str) -> bool {
    let path = relative_path.to_ascii_lowercase();
    let in_test_path = path.starts_with("test/")
        || path.starts_with("tests/")
        || path.contains("/test/")
        || path.contains("/tests/");
    let file_stem_looks_like_test = python_file_is_test(&path);
    let name = display_name; // preserve case for TestCase-class check
    let looks_like_test_fn = name.starts_with("test_") || name.ends_with("_test");
    let looks_like_test_class = name.starts_with("Test") && name != "Test";
    in_test_path || file_stem_looks_like_test || looks_like_test_fn || looks_like_test_class
}

fn python_file_is_test(path: &str) -> bool {
    let Some(filename) = path.rsplit('/').next() else {
        return false;
    };
    // `path` is already lowercased by callers, so an ASCII-case check
    // against the `.py` extension is safe — but clippy pedantic
    // flags the literal `ends_with(".py")`. Route through `Path`'s
    // extension parser, which treats both arms identically without
    // the lint tripwire.
    let ext_is_py = std::path::Path::new(filename)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("py"));
    if !ext_is_py {
        return false;
    }
    filename.starts_with("test_") || filename.ends_with("_test.py")
}

/// Spec for pyright (LSP). Extracts the npm tarball under
/// `~/.aide/bin/pyright-<VERSION>/` and writes a shell wrapper at
/// `~/.aide/bin/pyright-langserver` that invokes `node
/// <extract>/package/langserver.index.js --stdio "$@"`.
pub fn pyright_spec() -> ToolSpec {
    let url = format!("https://registry.npmjs.org/pyright/-/pyright-{PYRIGHT_VERSION}.tgz");
    ToolSpec {
        name: "pyright".to_string(),
        version: PYRIGHT_VERSION.to_string(),
        executable: "pyright-langserver".to_string(),
        source: Source::DirectUrl {
            label: format!("pyright-{PYRIGHT_VERSION}"),
            assets: vec![DirectAsset {
                triple: "any",
                url,
                archive: ArchiveFormat::TarGz { entry_path: "" },
            }],
        },
        custom_install: Some(install_pyright_wrapper),
    }
}

/// Spec for scip-python (SCIP indexer). Shell wrapper invokes `node
/// <extract>/package/index.js`.
pub fn scip_python_spec() -> ToolSpec {
    let url = format!(
        "https://registry.npmjs.org/@sourcegraph/scip-python/-/scip-python-{SCIP_PYTHON_VERSION}.tgz"
    );
    ToolSpec {
        name: "scip-python".to_string(),
        version: SCIP_PYTHON_VERSION.to_string(),
        executable: "scip-python".to_string(),
        source: Source::DirectUrl {
            label: format!("scip-python-{SCIP_PYTHON_VERSION}"),
            assets: vec![DirectAsset {
                triple: "any",
                url,
                archive: ArchiveFormat::TarGz { entry_path: "" },
            }],
        },
        custom_install: Some(install_scip_python_wrapper),
    }
}

fn install_pyright_wrapper(extract_dir: &Path, install_path: &Path) -> Result<(), InstallError> {
    let langserver = extract_dir.join("package").join("langserver.index.js");
    if !langserver.is_file() {
        return Err(InstallError::MissingEntry {
            entry: "package/langserver.index.js".into(),
            dir: extract_dir.to_path_buf(),
        });
    }
    let script = render_pyright_wrapper(&langserver);
    write_executable_script(install_path, &script).map_err(InstallError::Io)
}

fn install_scip_python_wrapper(
    extract_dir: &Path,
    install_path: &Path,
) -> Result<(), InstallError> {
    let main = extract_dir.join("package").join("index.js");
    if !main.is_file() {
        return Err(InstallError::MissingEntry {
            entry: "package/index.js".into(),
            dir: extract_dir.to_path_buf(),
        });
    }
    let script = render_scip_python_wrapper(&main);
    write_executable_script(install_path, &script).map_err(InstallError::Io)
}

fn render_pyright_wrapper(langserver: &Path) -> String {
    // `--stdio` is supplied by `lsp_spawn_args` when aide-lsp spawns
    // the wrapper; the wrapper itself just forwards args so invoking
    // it standalone (e.g. for smoke-testing) still works with any
    // mode the user wants.
    format!(
        "#!/bin/sh\n\
         # Generated by aide-mcp install for pyright-langserver\n\
         exec node \"{langserver}\" \"$@\"\n",
        langserver = langserver.display(),
    )
}

fn render_scip_python_wrapper(main: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         # Generated by aide-mcp install for scip-python\n\
         exec node \"{main}\" \"$@\"\n",
        main = main.display(),
    )
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
    fn detects_pyproject_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        assert!(PythonPlugin.detect(dir.path()));
    }

    #[test]
    fn detects_setup_py() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("setup.py"),
            "from setuptools import setup\n",
        )
        .unwrap();
        assert!(PythonPlugin.detect(dir.path()));
    }

    #[test]
    fn detects_requirements_txt() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "requests\n").unwrap();
        assert!(PythonPlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!PythonPlugin.detect(dir.path()));
    }

    #[test]
    fn scip_args_matches_scip_python_cli_shape() {
        let args = PythonPlugin
            .scip_args(Path::new("/p"), Path::new("/out/index.scip"))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, vec!["index", "--output", "/out/index.scip", "/p"]);
    }

    #[test]
    fn lsp_spawn_args_include_stdio() {
        let paths = aide_core::AidePaths::at(std::env::temp_dir().join("aide-test-python"));
        let args = PythonPlugin.lsp_spawn_args(Path::new("/any/root"), &paths);
        assert_eq!(args, vec![OsString::from("--stdio")]);
    }

    #[test]
    fn tools_are_pyright_and_scip_python() {
        let names: Vec<_> = PythonPlugin.tools().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec!["pyright".to_string(), "scip-python".to_string()],
        );
    }

    #[test]
    fn pins_are_exact_versions() {
        for pin in [PYRIGHT_VERSION, SCIP_PYTHON_VERSION] {
            assert!(!pin.is_empty());
            assert_ne!(pin, "latest");
            assert!(
                pin.chars().next().unwrap().is_ascii_digit(),
                "pin must start with a version digit, got {pin}",
            );
        }
    }

    #[test]
    fn tarball_urls_point_at_npm_registry_with_pinned_version() {
        let specs = [
            (pyright_spec(), PYRIGHT_VERSION),
            (scip_python_spec(), SCIP_PYTHON_VERSION),
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
    fn classify_python_path_buckets_by_convention() {
        assert_eq!(classify_python_path("tests/test_foo.py"), "test");
        assert_eq!(classify_python_path("src/foo_test.py"), "test");
        assert_eq!(classify_python_path("src/test_bar.py"), "test");
        assert_eq!(classify_python_path("bin/mytool"), "bin");
        assert_eq!(classify_python_path("scripts/deploy.py"), "bin");
        assert_eq!(classify_python_path("src/mypkg/__main__.py"), "bin");
        assert_eq!(classify_python_path("__main__.py"), "bin");
        assert_eq!(classify_python_path("examples/demo.py"), "example");
        assert_eq!(classify_python_path("src/mypkg/core.py"), "lib");
        assert_eq!(classify_python_path("mypkg/__init__.py"), "lib");
    }

    #[test]
    fn is_python_test_picks_up_path_and_name_conventions() {
        assert!(is_python_test("tests/test_it.py", "anything"));
        assert!(is_python_test("test/it.py", "anything"));
        assert!(is_python_test("src/pkg/tests/foo.py", "anything"));
        assert!(is_python_test("src/test_foo.py", "anything"));
        assert!(is_python_test("src/foo_test.py", "anything"));
        assert!(is_python_test("src/foo.py", "test_bar"));
        assert!(is_python_test("src/foo.py", "bar_test"));
        assert!(is_python_test("src/foo.py", "TestFoo"));
        assert!(!is_python_test("src/foo.py", "Test"));
        assert!(!is_python_test("src/foo.py", "bar"));
        // The `_test` suffix on file names is significant but not on
        // function names that happen to contain "test" as a substring.
        assert!(!is_python_test("src/foo.py", "tested"));
    }

    #[test]
    fn pyright_wrapper_execs_node_against_langserver_js() {
        let script = render_pyright_wrapper(Path::new(
            "/home/u/.aide/bin/pyright-1.1.409/package/langserver.index.js",
        ));
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("exec node"));
        assert!(script.contains("/home/u/.aide/bin/pyright-1.1.409/package/langserver.index.js",));
    }

    #[test]
    fn scip_python_wrapper_execs_node_against_index_js() {
        let script = render_scip_python_wrapper(Path::new(
            "/home/u/.aide/bin/scip-python-0.6.6/package/index.js",
        ));
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("exec node"));
        assert!(script.contains("/home/u/.aide/bin/scip-python-0.6.6/package/index.js"));
    }

    #[test]
    fn package_manager_routes_through_python_m_pip() {
        let pm = PythonPlugin.package_manager();
        assert_eq!(pm.executable, "python3");
        assert_eq!(pm.install_args, &["-m", "pip", "install"]);
    }

    #[test]
    fn test_runner_routes_through_python_m_pytest() {
        let tr = PythonPlugin.test_runner();
        assert_eq!(tr.executable, "python3");
        assert_eq!(tr.args, &["-m", "pytest"]);
    }
}
