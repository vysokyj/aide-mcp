//! Java plugin — Maven flavour.
//!
//! Claims directories containing `pom.xml`. Exec, SCIP, and LSP bindings
//! are wired as follows:
//!
//! - **LSP:** `jdtls` (Eclipse JDT Language Server). Requires a
//!   per-workspace data directory, so [`LanguagePlugin::lsp_spawn_args`]
//!   produces `-data <cache>`; the cache lives under
//!   `~/.aide/lsp-cache/jdtls/<slug>/`.
//! - **SCIP:** `scip-java index --output <file> <workdir>`.
//! - **Runner / tests / packages:** `mvn exec:java`, `mvn test`,
//!   `mvn dependency:get`. Note that Maven has no direct
//!   equivalent of `cargo add`; agents should pass
//!   `-Dartifact=groupId:artifactId:version` strings as `packages` to
//!   `install_package`.
//!
//! Tool auto-install (`tools()`) is empty. `jdtls` and `scip-java` are
//! expected on the user's `$PATH` (arch: `pacman -S jdtls scip-java`,
//! mac: `brew install jdtls`, or upstream release archives). Gradle
//! projects are not yet supported — add a sibling `JavaGradlePlugin`
//! when needed.

use std::ffi::OsString;
use std::path::Path;

use aide_core::AidePaths;
use aide_install::ToolSpec;

use crate::plugin::{
    DapSpec, LanguageId, LanguagePlugin, LspSpec, PackageManager, Runner, ScipSpec, TestRunner,
};

pub struct JavaMavenPlugin;

impl LanguagePlugin for JavaMavenPlugin {
    fn id(&self) -> LanguageId {
        LanguageId::new("java-maven")
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("pom.xml").is_file()
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
        // Maven has no `cargo add` equivalent. Agents are expected to
        // pass full `-Dartifact=groupId:artifactId:version` entries.
        PackageManager {
            executable: "mvn",
            install_args: &["dependency:get"],
        }
    }

    fn runner(&self) -> Runner {
        Runner {
            executable: "mvn",
            args: &["exec:java"],
        }
    }

    fn test_runner(&self) -> TestRunner {
        TestRunner {
            executable: "mvn",
            args: &["test"],
        }
    }

    fn tools(&self) -> Vec<ToolSpec> {
        // jdtls + scip-java are best installed via the user's system
        // package manager; we don't ship our own download recipe yet.
        Vec::new()
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
    fn detects_pom_xml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        assert!(JavaMavenPlugin.detect(dir.path()));
    }

    #[test]
    fn rejects_when_no_pom() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert!(!JavaMavenPlugin.detect(dir.path()));
    }

    #[test]
    fn lsp_spawn_args_contains_workspace_data_dir() {
        let paths = AidePaths::at("/tmp/aide-test");
        let args = JavaMavenPlugin.lsp_spawn_args(Path::new("/home/u/proj"), &paths);
        assert_eq!(args[0], "-data");
        let data = args[1].to_string_lossy();
        assert!(data.contains("/tmp/aide-test/lsp-cache/jdtls/"));
        assert!(data.ends_with("home_u_proj"));
    }

    #[test]
    fn scip_args_matches_scip_java_cli_shape() {
        let args = JavaMavenPlugin
            .scip_args(Path::new("/p"), Path::new("/out/index.scip"))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, vec!["index", "--output", "/out/index.scip", "/p"]);
    }
}
