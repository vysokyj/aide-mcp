//! Java plugin — Gradle flavour.
//!
//! Claims directories containing `build.gradle` or `build.gradle.kts`.
//! Uses `jdtls` + `scip-java` (same as Maven) but drives builds /
//! tests / packages through `gradle`. When a `gradlew` wrapper is
//! present in the root, agents should invoke it directly via
//! `run_project` with `extra_args`; the plugin itself points at the
//! system `gradle` binary so the tool works without a wrapper.

use std::ffi::OsString;
use std::path::Path;

use aide_core::AidePaths;
use aide_install::ToolSpec;

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct JavaGradlePlugin;

impl LanguagePlugin for JavaGradlePlugin {
    fn id(&self) -> LanguageId {
        LanguageId::new("java-gradle")
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("build.gradle").is_file() || root.join("build.gradle.kts").is_file()
    }

    fn lsp(&self) -> LspSpec {
        LspSpec {
            name: "jdtls",
            executable: "jdtls",
        }
    }

    fn lsp_spawn_args(&self, workspace_root: &Path, paths: &AidePaths) -> Vec<OsString> {
        let data_dir = paths
            .root()
            .join("lsp-cache")
            .join("jdtls")
            .join(slug(workspace_root));
        vec!["-data".into(), data_dir.into_os_string()]
    }

    fn scip(&self) -> Option<ScipSpec> {
        Some(ScipSpec {
            name: "scip-java",
            executable: "scip-java",
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
        None
    }

    fn package_manager(&self) -> PackageManager {
        // Gradle does not have a "cargo add" equivalent either —
        // dependencies live in build.gradle. We expose
        // `gradle dependencies` which at least surfaces what's wired in.
        PackageManager {
            executable: "gradle",
            install_args: &["dependencies"],
        }
    }

    fn runner(&self) -> Runner {
        Runner {
            executable: "gradle",
            args: &["run"],
        }
    }

    fn test_runner(&self) -> TestRunner {
        TestRunner {
            executable: "gradle",
            args: &["test"],
        }
    }

    fn tools(&self) -> Vec<ToolSpec> {
        // Share the Eclipse JDT-LS tarball and Lombok jar with the
        // Maven flavour so project_setup on a mixed workspace doesn't
        // download twice (install_tool is idempotent on the same
        // version pin).
        vec![super::java::jdtls_spec(), super::java::lombok_spec()]
    }

    fn is_test_symbol(&self, relative_path: &str, display_name: &str) -> bool {
        // Same JVM-ecosystem conventions as Maven.
        super::java::is_java_test(relative_path, display_name)
    }

    fn classify_path(&self, relative_path: &str) -> &'static str {
        super::java::classify_java_path(relative_path)
    }
}

fn slug(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches('/')
        .chars()
        .map(|c| match c {
            '/' | ':' | '\\' | ' ' => '_',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn detects_build_gradle() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("build.gradle"), "").unwrap();
        assert!(JavaGradlePlugin.detect(dir.path()));
    }

    #[test]
    fn detects_build_gradle_kts() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("build.gradle.kts"), "").unwrap();
        assert!(JavaGradlePlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_plain_pom() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        assert!(!JavaGradlePlugin.detect(dir.path()));
    }

    #[test]
    fn scip_args_matches_scip_java_cli_shape() {
        let args = JavaGradlePlugin
            .scip_args(Path::new("/p"), Path::new("/out/index.scip"))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, vec!["index", "--output", "/out/index.scip", "/p"]);
    }

    #[test]
    fn lsp_spawn_args_contains_workspace_data_dir() {
        let root = std::env::temp_dir().join("aide-test-gradle");
        let paths = aide_core::AidePaths::at(&root);
        let args = JavaGradlePlugin.lsp_spawn_args(Path::new("/home/u/proj"), &paths);
        assert_eq!(args[0], "-data");
        let data = PathBuf::from(&args[1]);
        let expected_parent = root.join("lsp-cache").join("jdtls");
        assert!(data.starts_with(&expected_parent));
        assert_eq!(data.file_name().unwrap().to_string_lossy(), "home_u_proj");
    }

    #[test]
    fn tools_share_jdtls_and_lombok_specs_with_maven() {
        let names: Vec<_> = JavaGradlePlugin
            .tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(names.contains(&"jdtls".to_string()));
        assert!(names.contains(&"lombok".to_string()));
    }

    #[test]
    fn shares_java_test_heuristic_with_maven() {
        // Quick sanity that the Gradle plugin delegates to the shared
        // Maven heuristic — one positive, one negative.
        assert!(JavaGradlePlugin.is_test_symbol("src/test/java/FooTest.java", "anything"));
        assert!(!JavaGradlePlugin.is_test_symbol("src/main/java/Transit.java", "bar"));
    }

    #[test]
    fn shares_classify_path_with_maven() {
        assert_eq!(
            JavaGradlePlugin.classify_path("src/test/java/FooTest.java"),
            "test",
        );
        assert_eq!(
            JavaGradlePlugin.classify_path("src/main/java/App.java"),
            "lib",
        );
    }
}
