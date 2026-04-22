use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Records every tool aide-mcp has installed under `~/.aide/bin/`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Keyed by [`crate::ToolSpec::name`].
    #[serde(default)]
    pub tools: BTreeMap<String, InstalledRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledRecord {
    pub version: String,
    pub path: PathBuf,
    /// Unix epoch seconds when the install finished.
    pub installed_at: u64,
}

impl InstalledRecord {
    pub fn new(version: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let installed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self {
            version: version.into(),
            path: path.into(),
            installed_at,
        }
    }
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn record(&mut self, name: impl Into<String>, record: InstalledRecord) {
        self.tools.insert(name.into(), record);
    }

    pub fn get(&self, name: &str) -> Option<&InstalledRecord> {
        self.tools.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let m = Manifest::default();
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert!(loaded.tools.is_empty());
    }

    #[test]
    fn roundtrip_with_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let mut m = Manifest::default();
        m.record(
            "rust-analyzer",
            InstalledRecord::new("2026-04-20", "/tmp/rust-analyzer"),
        );
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.tools["rust-analyzer"].version, "2026-04-20");
    }

    #[test]
    fn load_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let loaded = Manifest::load(&path).unwrap();
        assert!(loaded.tools.is_empty());
    }
}
