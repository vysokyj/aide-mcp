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
        if let Some(override_root) = std::env::var_os("AIDE_HOME") {
            return Ok(Self::at(override_root));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(PathsError::NoHome)?;
        Ok(Self::at(home.join(".aide")))
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

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }
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
        assert_eq!(paths.config_file(), Path::new("/tmp/aide-test/config.toml"));
    }
}
