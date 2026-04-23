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
}
