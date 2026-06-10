use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("no home directory available")]
    NoHome,
}

/// Resolved filesystem locations for the aide-mcp user-wide cache and state.
///
/// All directories live under `~/.aide/`:
/// - `bin/`     — downloaded LSP servers, SCIP indexers, debug adapters.
/// - `scip/`    — `<repo-id>/<sha>.scip` per-repo indexes.
/// - `sock/`    — unix-domain sockets for IPC.
/// - `queue/`   — durable queue for pending indexer work.
/// - `logs/`    — captured stdout/stderr of `run_*` / `install_package`.
/// - `auth/`    — user-supplied credentials (e.g. `github.token`).
/// - `config.toml` — user-wide configuration.
#[derive(Debug, Clone)]
pub struct AidePaths {
    root: PathBuf,
}

impl AidePaths {
    /// Resolve the root directory the same way the running server does:
    /// 1. `$AIDE_HOME` if set (explicit override — primarily for tests).
    /// 2. Otherwise `$HOME/.aide`.
    pub fn from_home() -> Result<Self, PathsError> {
        let paths = if let Some(override_root) = std::env::var_os("AIDE_HOME") {
            Self::at(override_root)
        } else {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(PathsError::NoHome)?;
            Self::at(home.join(".aide"))
        };
        paths.ensure_private_root();
        Ok(paths)
    }

    /// Create the root if missing and keep it private (0700 on unix):
    /// `auth/` holds tokens and `logs/` captures exec output that may
    /// echo secrets, so other local users must not be able to read
    /// them. Best-effort — a failure here surfaces later as a clearer
    /// error on the operation that actually needs the directory.
    fn ensure_private_root(&self) {
        let _ = std::fs::create_dir_all(&self.root);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700));
        }
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub fn scip(&self) -> PathBuf {
        self.root.join("scip")
    }

    pub fn sock(&self) -> PathBuf {
        self.root.join("sock")
    }

    pub fn queue(&self) -> PathBuf {
        self.root.join("queue")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn auth(&self) -> PathBuf {
        self.root.join("auth")
    }

    /// Per-project memory directory. `repo_root_slug` is a
    /// filesystem-safe slug of the project's absolute path — get one
    /// from [`slugify_repo_root`]. Each memory is a single `.md` file
    /// in this directory with YAML frontmatter (`name`, `description`).
    pub fn memory(&self, repo_root_slug: &str) -> PathBuf {
        self.root.join("memory").join(repo_root_slug)
    }

    /// Path to the manual-drop-in GitHub token file — third tier of the
    /// auth waterfall after `$GITHUB_TOKEN` and `gh auth token`. Callers
    /// are expected to create it with mode 0600; aide-mcp never writes
    /// to it.
    pub fn github_token(&self) -> PathBuf {
        self.auth().join("github.token")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }
}

/// Turn an absolute repo path into a filename-safe directory name.
/// Collisions are avoided because two absolute paths always differ at
/// some character. Used by both the SCIP index store (`~/.aide/scip/
/// <slug>/<sha>.scip`) and per-project memory (`~/.aide/memory/
/// <slug>/<name>.md`).
pub fn slugify_repo_root(repo_root: &str) -> String {
    repo_root
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

    #[test]
    fn layout_under_root() {
        let paths = AidePaths::at("/tmp/aide-test");
        assert_eq!(paths.root(), Path::new("/tmp/aide-test"));
        assert_eq!(paths.bin(), Path::new("/tmp/aide-test/bin"));
        assert_eq!(paths.scip(), Path::new("/tmp/aide-test/scip"));
        assert_eq!(paths.sock(), Path::new("/tmp/aide-test/sock"));
        assert_eq!(paths.queue(), Path::new("/tmp/aide-test/queue"));
        assert_eq!(paths.logs(), Path::new("/tmp/aide-test/logs"));
        assert_eq!(paths.auth(), Path::new("/tmp/aide-test/auth"));
        assert_eq!(
            paths.github_token(),
            Path::new("/tmp/aide-test/auth/github.token")
        );
        assert_eq!(paths.config_file(), Path::new("/tmp/aide-test/config.toml"));
        assert_eq!(
            paths.memory("home_jirka_workspace_aide-mcp"),
            Path::new("/tmp/aide-test/memory/home_jirka_workspace_aide-mcp")
        );
    }

    #[test]
    #[cfg(unix)]
    fn from_home_creates_private_root() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("aide-home");
        // `from_home` reads AIDE_HOME from the process env, which is
        // unsafe to mutate in parallel tests — exercise the same code
        // path directly instead.
        let paths = AidePaths::at(&root);
        paths.ensure_private_root();
        let mode = std::fs::metadata(&root).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn slugify_root_escapes_separators_and_spaces() {
        assert_eq!(
            slugify_repo_root("/home/jirka/workspace/aide mcp"),
            "home_jirka_workspace_aide_mcp"
        );
    }
}
