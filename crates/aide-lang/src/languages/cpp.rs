//! C / C++ plugin.
//!
//! Claims directories containing one of `CMakeLists.txt`,
//! `compile_commands.json`, `meson.build`, or `.clangd`. Bare
//! `Makefile` is deliberately excluded — too many non-C/C++ projects
//! ship Makefiles; requiring one of the C/C++-specific markers keeps
//! the plugin from claiming unrelated repos.
//!
//! - **LSP:** `clangd` (LLVM project, auto-installed from the
//!   clangd/clangd GitHub release zip). Multi-file archive: the zip
//!   extracts to `~/.aide/bin/clangd-<VERSION>/clangd_<VERSION>/`;
//!   a symlink at `~/.aide/bin/clangd` points at the binary inside
//!   that tree so clangd can still find its sibling `lib/clang/`
//!   builtin-header directory at runtime.
//! - **SCIP:** `scip-clang` (Sourcegraph, bare-binary GitHub
//!   release). Available only for Linux `x86_64` and Apple Silicon
//!   macOS — Intel-Mac and Linux arm64 users fall back to
//!   `scip-clang` from their package manager.
//! - **DAP:** `codelldb`, shared with the Rust plugin. No second
//!   download; `project_setup` is idempotent on the same pinned
//!   version, so a workspace with both Rust and C++ pays the
//!   codelldb install cost exactly once.
//! - **Runner / tests / packages:** `cmake --build build`,
//!   `ctest --test-dir build`, `vcpkg install`. These defaults cover
//!   the most common CMake-centric setup; callers with meson / bazel
//!   / bare-make pass their own command lines through `extra_args`.
//!
//! clangd requires a compilation database (`compile_commands.json`)
//! to resolve include paths. `CMake` emits one when invoked with
//! `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`; other build systems have
//! analogous flags or bear-style wrappers. aide-mcp itself does not
//! generate the database — the user is expected to configure their
//! build tree before asking aide for semantic navigation.

use std::ffi::OsString;
use std::path::Path;

use aide_install::{ArchiveFormat, InstallError, Source, TargetAsset, ToolSpec};

use crate::languages::rust::codelldb_spec;
use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct CppPlugin;

/// Pin for clangd from the clangd/clangd release lane. Bump together
/// with a smoke test; the version also appears as a subdirectory
/// inside the distributed zip, which the installer discovers via glob.
const CLANGD_TAG: &str = "22.1.0";

/// Pin for scip-clang. Bump together with a smoke test against a
/// configured `CMake` project.
const SCIP_CLANG_TAG: &str = "v0.4.0";

impl LanguagePlugin for CppPlugin {
    fn id(&self) -> LanguageId {
        LanguageId::new("cpp")
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("CMakeLists.txt").is_file()
            || root.join("compile_commands.json").is_file()
            || root.join("meson.build").is_file()
            || root.join(".clangd").is_file()
    }

    fn lsp(&self) -> LspSpec {
        LspSpec {
            name: "clangd",
            executable: "clangd",
        }
    }

    fn scip(&self) -> Option<ScipSpec> {
        Some(ScipSpec {
            name: "scip-clang",
            executable: "scip-clang",
        })
    }

    fn scip_args(&self, workdir: &Path, output: &Path) -> Vec<OsString> {
        // scip-clang reads the project's compile_commands.json from
        // `--compdb-path` and writes the index to `--index-output`.
        // The default compile_commands.json location is the project
        // root; build-tree locations (`build/compile_commands.json`)
        // are common and can be passed via extra agent configuration
        // once the SCIP args API supports it.
        vec![
            "--compdb-path".into(),
            workdir.join("compile_commands.json").into_os_string(),
            "--index-output".into(),
            output.as_os_str().to_os_string(),
        ]
    }

    fn dap(&self) -> Option<DapSpec> {
        // codelldb speaks DAP for any target with LLDB backends —
        // including C and C++. The binary itself is pinned by the
        // Rust plugin.
        Some(DapSpec {
            name: "codelldb",
            executable: "codelldb",
        })
    }

    fn package_manager(&self) -> PackageManager {
        // vcpkg is the closest thing C/C++ has to a universal package
        // manager. Users on conan / nix / system-package-manager
        // projects replace this with their own command via
        // install_package's failure path (error surfaces cleanly when
        // the executable is missing).
        PackageManager {
            executable: "vcpkg",
            install_args: &["install"],
        }
    }

    fn runner(&self) -> Runner {
        // "Run" maps onto "build" for C/C++ since there is no
        // universal entry-point convention; agents launch the
        // produced binary via a subsequent `Bash(...)` call.
        Runner {
            executable: "cmake",
            args: &["--build", "build"],
        }
    }

    fn test_runner(&self) -> TestRunner {
        // `ctest --test-dir build` works out of the box for CMake
        // projects that have `enable_testing()` in their root
        // CMakeLists.txt. Meson / Bazel users pass their own runner.
        TestRunner {
            executable: "ctest",
            args: &["--test-dir", "build"],
        }
    }

    fn tools(&self) -> Vec<ToolSpec> {
        vec![clangd_spec(), scip_clang_spec(), codelldb_spec()]
    }

    fn is_test_symbol(&self, relative_path: &str, display_name: &str) -> bool {
        is_cpp_test(relative_path, display_name)
    }

    fn classify_path(&self, relative_path: &str) -> &'static str {
        classify_cpp_path(relative_path)
    }
}

/// Broad path-based classification of a C/C++ source file. Picks the
/// first matching bucket in this order:
///
/// - `test` — under `test/` or `tests/`, or matching `test_*` /
///   `*_test.<ext>` / `*_tests.<ext>`
/// - `bin` — under `bin/`, or file named `main.<ext>`
/// - `example` — under `examples/`
/// - `lib` — everything else
fn classify_cpp_path(relative_path: &str) -> &'static str {
    if is_cpp_test(relative_path, "") {
        return "test";
    }
    let p = relative_path.to_ascii_lowercase();
    if p.starts_with("bin/") || p.contains("/bin/") || is_cpp_main_file(&p) {
        return "bin";
    }
    if p.starts_with("examples/") || p.contains("/examples/") {
        return "example";
    }
    "lib"
}

fn is_cpp_main_file(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or("");
    let ext_ok = std::path::Path::new(filename).extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("cpp")
            || e.eq_ignore_ascii_case("cc")
            || e.eq_ignore_ascii_case("cxx")
            || e.eq_ignore_ascii_case("c")
    });
    ext_ok
        && std::path::Path::new(filename)
            .file_stem()
            .is_some_and(|s| s.eq_ignore_ascii_case("main"))
}

/// C/C++ test heuristic. Covers the common conventions across
/// `GoogleTest`, Catch2, `Boost.Test`, and hand-rolled main-based tests:
/// tests live under `test/` or `tests/`, or carry a `test_` prefix
/// / `_test(s)` suffix on the filename. Symbol-name detection is
/// intentionally coarse — `TEST_*` macro expansions become
/// function-like symbols that SCIP reports with the `TEST_` prefix.
fn is_cpp_test(relative_path: &str, display_name: &str) -> bool {
    let path = relative_path.to_ascii_lowercase();
    let in_test_path = path.starts_with("test/")
        || path.starts_with("tests/")
        || path.contains("/test/")
        || path.contains("/tests/");
    let file_looks_like_test = cpp_filename_looks_like_test(&path);
    let looks_like_test_fn = display_name.starts_with("test_")
        || display_name.ends_with("_test")
        || display_name.ends_with("_tests")
        || display_name.starts_with("TEST_")
        || display_name.starts_with("TEST(");
    in_test_path || file_looks_like_test || looks_like_test_fn
}

fn cpp_filename_looks_like_test(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or("");
    let stem = match std::path::Path::new(filename).file_stem() {
        Some(s) => s.to_string_lossy().into_owned(),
        None => return false,
    };
    stem.starts_with("test_") || stem.ends_with("_test") || stem.ends_with("_tests")
}

/// Spec for clangd (LSP). The LLVM release lane ships per-platform
/// zips whose internal layout is `clangd_<VERSION>/bin/clangd` plus a
/// `lib/clang/<VERSION>/` sibling holding builtin headers that clangd
/// must be able to find at runtime. A [`custom_install`] hook
/// glob-finds the binary inside the extracted tree (so a future
/// version bump doesn't have to touch `entry_path`) and symlinks it.
/// The binary resolves its own install root via argv[0], so a
/// symlink from `~/.aide/bin/clangd` keeps the `lib/clang/` sibling
/// reachable.
pub fn clangd_spec() -> ToolSpec {
    let assets = vec![
        TargetAsset {
            triple: "aarch64-apple-darwin",
            filename: format!("clangd-mac-{CLANGD_TAG}.zip"),
            archive: ArchiveFormat::Zip { entry_path: "" },
        },
        TargetAsset {
            triple: "x86_64-apple-darwin",
            filename: format!("clangd-mac-{CLANGD_TAG}.zip"),
            archive: ArchiveFormat::Zip { entry_path: "" },
        },
        TargetAsset {
            triple: "aarch64-unknown-linux-gnu",
            filename: format!("clangd-linux-{CLANGD_TAG}.zip"),
            archive: ArchiveFormat::Zip { entry_path: "" },
        },
        TargetAsset {
            triple: "x86_64-unknown-linux-gnu",
            filename: format!("clangd-linux-{CLANGD_TAG}.zip"),
            archive: ArchiveFormat::Zip { entry_path: "" },
        },
    ];
    ToolSpec {
        name: "clangd".to_string(),
        version: CLANGD_TAG.to_string(),
        executable: "clangd".to_string(),
        source: Source::GithubRelease {
            repo: "clangd/clangd".to_string(),
            tag: CLANGD_TAG.to_string(),
            assets,
        },
        custom_install: Some(install_clangd_symlink),
    }
}

/// Spec for scip-clang. Bare-binary release — only two of aide's four
/// supported triples get a prebuilt; the missing triples surface a
/// `NoAssetForTriple` error at install time so the user knows to
/// fall back to a system install.
pub fn scip_clang_spec() -> ToolSpec {
    let assets = vec![
        TargetAsset {
            triple: "aarch64-apple-darwin",
            filename: "scip-clang-arm64-darwin".to_string(),
            archive: ArchiveFormat::Raw,
        },
        TargetAsset {
            triple: "x86_64-unknown-linux-gnu",
            filename: "scip-clang-x86_64-linux".to_string(),
            archive: ArchiveFormat::Raw,
        },
    ];
    ToolSpec {
        name: "scip-clang".to_string(),
        version: SCIP_CLANG_TAG.to_string(),
        executable: "scip-clang".to_string(),
        source: Source::GithubRelease {
            repo: "sourcegraph/scip-clang".to_string(),
            tag: SCIP_CLANG_TAG.to_string(),
            assets,
        },
        custom_install: None,
    }
}

/// Post-extract hook for clangd. The zip extracts to
/// `<extract_dir>/clangd_<VERSION>/bin/clangd` (plus siblings). We
/// glob the first-level directory for a `clangd_*/bin/clangd` entry
/// rather than hard-coding the version string into `entry_path` so
/// future version bumps only need to touch `CLANGD_TAG`.
fn install_clangd_symlink(extract_dir: &Path, install_path: &Path) -> Result<(), InstallError> {
    let inner = find_clangd_inner_dir(extract_dir)?;
    let bin = inner.join("bin").join("clangd");
    if !bin.is_file() {
        return Err(InstallError::MissingEntry {
            entry: "bin/clangd".into(),
            dir: inner,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin)
            .map_err(InstallError::Io)?
            .permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&bin, perms).map_err(InstallError::Io)?;
    }
    if install_path.exists() || install_path.is_symlink() {
        std::fs::remove_file(install_path).map_err(InstallError::Io)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&bin, install_path).map_err(InstallError::Io)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(&bin, install_path).map_err(InstallError::Io)?;
    }
    Ok(())
}

fn find_clangd_inner_dir(extract_dir: &Path) -> Result<std::path::PathBuf, InstallError> {
    let entries = std::fs::read_dir(extract_dir).map_err(InstallError::Io)?;
    for entry in entries {
        let entry = entry.map_err(InstallError::Io)?;
        if !entry.file_type().map_err(InstallError::Io)?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("clangd_") {
            return Ok(entry.path());
        }
    }
    Err(InstallError::MissingEntry {
        entry: "clangd_*/".into(),
        dir: extract_dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_cmake_lists() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("CMakeLists.txt"), "project(x)\n").unwrap();
        assert!(CppPlugin.detect(dir.path()));
    }

    #[test]
    fn detects_compile_commands_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("compile_commands.json"), "[]").unwrap();
        assert!(CppPlugin.detect(dir.path()));
    }

    #[test]
    fn detects_meson_build() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("meson.build"), "project('x', 'cpp')\n").unwrap();
        assert!(CppPlugin.detect(dir.path()));
    }

    #[test]
    fn detects_dot_clangd() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".clangd"), "CompileFlags:\n").unwrap();
        assert!(CppPlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_bare_makefile() {
        // Bare Makefile is too broad a marker — non-C/C++ projects
        // frequently ship one. Only the C/C++-specific markers
        // should claim the root.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Makefile"), "all:\n\techo hi\n").unwrap();
        assert!(!CppPlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!CppPlugin.detect(dir.path()));
    }

    #[test]
    fn scip_args_points_at_compile_commands_in_workdir() {
        let args = CppPlugin
            .scip_args(Path::new("/repo"), Path::new("/out/index.scip"))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "--compdb-path",
                "/repo/compile_commands.json",
                "--index-output",
                "/out/index.scip",
            ],
        );
    }

    #[test]
    fn dap_reuses_codelldb() {
        let dap = CppPlugin.dap().expect("C++ exposes DAP");
        assert_eq!(dap.executable, "codelldb");
    }

    #[test]
    fn tools_cover_clangd_scip_clang_and_codelldb() {
        let names: Vec<_> = CppPlugin.tools().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![
                "clangd".to_string(),
                "scip-clang".to_string(),
                "codelldb".to_string(),
            ],
        );
    }

    #[test]
    fn pins_are_exact_versions() {
        assert_ne!(CLANGD_TAG, "latest");
        assert_ne!(SCIP_CLANG_TAG, "latest");
        assert!(CLANGD_TAG.chars().next().unwrap().is_ascii_digit());
        assert!(SCIP_CLANG_TAG.starts_with('v'));
    }

    #[test]
    fn clangd_release_covers_four_triples() {
        let spec = clangd_spec();
        let triples: Vec<&str> = match &spec.source {
            Source::GithubRelease { assets, .. } => assets.iter().map(|a| a.triple).collect(),
            Source::DirectUrl { .. } => panic!("clangd must be GithubRelease"),
        };
        assert!(triples.contains(&"aarch64-apple-darwin"));
        assert!(triples.contains(&"x86_64-apple-darwin"));
        assert!(triples.contains(&"aarch64-unknown-linux-gnu"));
        assert!(triples.contains(&"x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn scip_clang_release_covers_two_triples() {
        let spec = scip_clang_spec();
        let triples: Vec<&str> = match &spec.source {
            Source::GithubRelease { assets, .. } => assets.iter().map(|a| a.triple).collect(),
            Source::DirectUrl { .. } => panic!("scip-clang must be GithubRelease"),
        };
        assert_eq!(triples.len(), 2);
        assert!(triples.contains(&"aarch64-apple-darwin"));
        assert!(triples.contains(&"x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn classify_cpp_path_buckets_by_convention() {
        assert_eq!(classify_cpp_path("tests/test_foo.cpp"), "test");
        assert_eq!(classify_cpp_path("src/foo_test.cpp"), "test");
        assert_eq!(classify_cpp_path("src/foo_tests.cc"), "test");
        assert_eq!(classify_cpp_path("bin/tool.cpp"), "bin");
        assert_eq!(classify_cpp_path("src/main.cpp"), "bin");
        assert_eq!(classify_cpp_path("src/main.cc"), "bin");
        assert_eq!(classify_cpp_path("main.c"), "bin");
        assert_eq!(classify_cpp_path("examples/demo.cpp"), "example");
        assert_eq!(classify_cpp_path("src/mylib/core.cpp"), "lib");
        assert_eq!(classify_cpp_path("include/mylib/core.hpp"), "lib");
    }

    #[test]
    fn is_cpp_test_picks_up_path_and_name_conventions() {
        assert!(is_cpp_test("tests/foo.cpp", "anything"));
        assert!(is_cpp_test("test/foo.cpp", "anything"));
        assert!(is_cpp_test("src/pkg/tests/foo.cpp", "anything"));
        assert!(is_cpp_test("src/foo_test.cpp", "anything"));
        assert!(is_cpp_test("src/test_foo.cpp", "anything"));
        assert!(is_cpp_test("src/foo_tests.cc", "anything"));
        assert!(is_cpp_test("src/foo.cpp", "test_bar"));
        assert!(is_cpp_test("src/foo.cpp", "bar_test"));
        assert!(is_cpp_test("src/foo.cpp", "TEST_CaseName"));
        assert!(!is_cpp_test("src/foo.cpp", "bar"));
        assert!(!is_cpp_test("src/foo.cpp", "TestimonyCollector"));
    }
}
