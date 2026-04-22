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
//! JDT-LS auto-install is provided via [`jdtls_spec`]: it downloads
//! the Eclipse snapshot tarball and generates a wrapper script under
//! `~/.aide/bin/jdtls` that invokes `java -jar <launcher> -configuration
//! <config>`. scip-java still expects a system install (coursier
//! bootstrap is out of scope).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use aide_core::AidePaths;
use aide_install::{ArchiveFormat, DirectAsset, InstallError, Source, ToolSpec};

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
        // JDT-LS auto-installs via the Eclipse snapshot tarball. scip-java
        // is still system-install (coursier bootstrap out of scope).
        vec![jdtls_spec()]
    }
}

/// Spec for the Eclipse JDT Language Server. Shared by both Java
/// plugins so the download runs once per `project_setup`, regardless
/// of whether the project is Maven or Gradle.
pub fn jdtls_spec() -> ToolSpec {
    // Eclipse publishes a rolling "latest snapshot" URL — we pin via
    // our own version label and bump it when we validate a newer
    // snapshot.
    const JDTLS_LABEL: &str = "snapshot-2026-04-23";
    const JDTLS_URL: &str =
        "https://download.eclipse.org/jdtls/snapshots/jdt-language-server-latest.tar.gz";

    ToolSpec {
        name: "jdtls".to_string(),
        version: JDTLS_LABEL.to_string(),
        executable: "jdtls".to_string(),
        source: Source::DirectUrl {
            label: JDTLS_LABEL.to_string(),
            assets: vec![DirectAsset {
                // Same tarball works on every OS — platform differences
                // are handled at launch time via config_{linux,mac,win}.
                triple: "any",
                url: JDTLS_URL.to_string(),
                archive: ArchiveFormat::TarGz { entry_path: "" },
            }],
        },
        custom_install: Some(install_jdtls_wrapper),
    }
}

/// Post-extract step for JDT-LS: find the Eclipse Equinox launcher jar
/// (its filename has a version suffix we don't know statically), pick
/// the platform config dir, and write a shell wrapper that any MCP
/// tool can invoke as plain `jdtls`.
fn install_jdtls_wrapper(extract_dir: &Path, install_path: &Path) -> Result<(), InstallError> {
    let launcher = find_launcher_jar(extract_dir)?;
    let config = extract_dir.join(platform_config_dir());
    if !config.is_dir() {
        return Err(InstallError::MissingEntry {
            entry: platform_config_dir().to_string(),
            dir: extract_dir.to_path_buf(),
        });
    }
    let script = render_jdtls_wrapper(&launcher, &config);
    write_executable_script(install_path, &script).map_err(InstallError::Io)
}

fn find_launcher_jar(extract_dir: &Path) -> Result<PathBuf, InstallError> {
    let plugins = extract_dir.join("plugins");
    let entries = std::fs::read_dir(&plugins).map_err(InstallError::Io)?;
    for entry in entries {
        let entry = entry.map_err(InstallError::Io)?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("org.eclipse.equinox.launcher_") && name_str.ends_with(".jar") {
            return Ok(entry.path());
        }
    }
    Err(InstallError::MissingEntry {
        entry: "plugins/org.eclipse.equinox.launcher_*.jar".into(),
        dir: extract_dir.to_path_buf(),
    })
}

const fn platform_config_dir() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "config_linux"
    }
    #[cfg(target_os = "macos")]
    {
        "config_mac"
    }
    #[cfg(target_os = "windows")]
    {
        "config_win"
    }
}

fn render_jdtls_wrapper(launcher: &Path, config_dir: &Path) -> String {
    // Any `-data <dir>` passed via `LanguagePlugin::lsp_spawn_args`
    // flows through `"$@"` — the wrapper deliberately does not set
    // `-data` itself.
    format!(
        "#!/bin/sh\n\
         # Generated by aide-mcp install for jdtls\n\
         exec java \\\n\
         \t-Declipse.application=org.eclipse.jdt.ls.core.id1 \\\n\
         \t-Dosgi.bundles.defaultStartLevel=4 \\\n\
         \t-Declipse.product=org.eclipse.jdt.ls.core.product \\\n\
         \t-Dlog.level=ALL \\\n\
         \t-Xms1g -Xmx2G \\\n\
         \t--add-modules=ALL-SYSTEM \\\n\
         \t--add-opens java.base/java.util=ALL-UNNAMED \\\n\
         \t--add-opens java.base/java.lang=ALL-UNNAMED \\\n\
         \t-jar \"{launcher}\" \\\n\
         \t-configuration \"{config}\" \\\n\
         \t\"$@\"\n",
        launcher = launcher.display(),
        config = config_dir.display(),
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
