//! User-wide configuration at `~/.aide/config.toml`.
//!
//! The file is optional — missing means "use defaults", and individual
//! fields can be omitted too (they fall back to the same defaults as if
//! the whole file were missing). Values tune subsystems without
//! requiring a rebuild:
//!
//! ```toml
//! [scip]
//! retention_ready = 3       # keep the last 3 Ready commits per repo
//!
//! [exec]
//! default_timeout_secs = 900    # 15 min default for run_*
//!
//! [dap]
//! stop_timeout_secs = 120
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("TOML parse error in {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub scip: ScipConfig,
    #[serde(default)]
    pub exec: ExecConfig,
    #[serde(default)]
    pub dap: DapConfig,
}

impl Config {
    /// Load `path`. Missing file → `Self::default()`. Partial file →
    /// each missing field falls back to its default.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.display().to_string(),
            source: e,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScipConfig {
    /// How many Ready SCIP indexes to keep per repo. When a new commit
    /// reaches `Ready`, older Ready indexes beyond this count are
    /// evicted from state and their `.scip` files are deleted.
    #[serde(default = "default_retention")]
    pub retention_ready: usize,
}

impl Default for ScipConfig {
    fn default() -> Self {
        Self {
            retention_ready: default_retention(),
        }
    }
}

fn default_retention() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecConfig {
    /// Default wall-clock budget for `run_project` / `run_tests` /
    /// `install_package` when the caller does not pass `timeout_secs`.
    #[serde(default = "default_exec_timeout")]
    pub default_timeout_secs: u64,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            default_timeout_secs: default_exec_timeout(),
        }
    }
}

fn default_exec_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapConfig {
    /// How long `dap_continue` / `dap_step_*` / `dap_pause` wait for
    /// the next `stopped` event before giving up.
    #[serde(default = "default_dap_stop_timeout")]
    pub stop_timeout_secs: u64,
}

impl Default for DapConfig {
    fn default() -> Self {
        Self {
            stop_timeout_secs: default_dap_stop_timeout(),
        }
    }
}

fn default_dap_stop_timeout() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_file_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let cfg = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(cfg.scip.retention_ready, 1);
        assert_eq!(cfg.exec.default_timeout_secs, 300);
        assert_eq!(cfg.dap.stop_timeout_secs, 60);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[scip]\nretention_ready = 5\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.scip.retention_ready, 5);
        assert_eq!(cfg.exec.default_timeout_secs, 300);
    }

    #[test]
    fn full_override() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[scip]\nretention_ready = 7\n\
             [exec]\ndefault_timeout_secs = 900\n\
             [dap]\nstop_timeout_secs = 120\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.scip.retention_ready, 7);
        assert_eq!(cfg.exec.default_timeout_secs, 900);
        assert_eq!(cfg.dap.stop_timeout_secs, 120);
    }

    #[test]
    fn malformed_file_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Parse { .. })
        ));
    }
}
