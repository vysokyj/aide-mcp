use std::fmt;
use std::path::Path;

use crate::install::InstallError;

/// Post-extract hook for tools that can't be reduced to "extract one
/// archive, symlink one file" — typically because they need a
/// generated shell wrapper (JDT-LS, scip-java, …). Receives the
/// extract dir and the target install path; returns an error string
/// on failure.
pub type CustomInstallFn = fn(&Path, &Path) -> Result<(), InstallError>;

/// A declarative description of a tool we can install. Produced by language
/// plugins, consumed by [`crate::install_tool`].
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Stable identifier used in the manifest and in error messages.
    pub name: String,
    /// Version pin (free-form — GitHub tag, semver, date, etc.).
    pub version: String,
    /// Filename the installed binary will carry inside `~/.aide/bin/`.
    pub executable: String,
    /// Where and how to fetch the binary.
    pub source: Source,
    /// Replaces the default "symlink `entry_path`" step after extract
    /// for `TarGz` / `Zip` archives. When `Some`, the fn is responsible
    /// for making `install_path` point at a runnable executable.
    pub custom_install: Option<CustomInstallFn>,
}

impl fmt::Display for ToolSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// How to obtain the binary.
#[derive(Debug, Clone)]
pub enum Source {
    /// GitHub release asset, resolved per target triple.
    GithubRelease {
        /// `"owner/repo"`.
        repo: String,
        /// Release tag.
        tag: String,
        /// One entry per supported target triple.
        assets: Vec<TargetAsset>,
    },
    /// Direct HTTPS URL — used for non-GitHub hosts (Eclipse, CDN, …).
    /// The `label` is a human-readable identifier for logs; it does
    /// not affect resolution.
    DirectUrl {
        label: String,
        assets: Vec<DirectAsset>,
    },
}

/// A single downloadable asset for one target triple (or `"any"` for
/// language-runtime artefacts that don't depend on OS/arch).
#[derive(Debug, Clone)]
pub struct TargetAsset {
    pub triple: &'static str,
    pub filename: String,
    pub archive: ArchiveFormat,
}

/// Like [`TargetAsset`] but carries a full URL instead of a filename
/// that gets appended to a GitHub release base.
#[derive(Debug, Clone)]
pub struct DirectAsset {
    pub triple: &'static str,
    pub url: String,
    pub archive: ArchiveFormat,
}

/// How the downloaded bytes are packaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// Raw executable bytes — write directly.
    Raw,
    /// Single gzip-compressed file — decompress, then write.
    Gzip,
    /// Tar archive (possibly gzip-compressed) containing many files.
    /// Extract to `~/.aide/bin/<tool-name>-<version>/` and symlink
    /// `~/.aide/bin/<executable>` to the entry below.
    TarGz {
        /// Path of the main executable inside the archive, relative
        /// to the extract root (e.g. `"bin/scip-java"`).
        entry_path: &'static str,
    },
    /// Zip archive (covers `.vsix` — it's zip under the hood).
    /// Same extraction + symlink flow as [`Self::TarGz`].
    Zip {
        /// Path of the main executable inside the archive, relative
        /// to the extract root (e.g. `"extension/adapter/codelldb"`).
        entry_path: &'static str,
    },
}
