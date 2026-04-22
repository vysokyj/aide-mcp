use std::fmt;

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
}

/// A single downloadable asset for one target triple.
#[derive(Debug, Clone)]
pub struct TargetAsset {
    pub triple: &'static str,
    pub filename: String,
    pub archive: ArchiveFormat,
}

/// How the downloaded bytes are packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// Raw executable bytes — write directly.
    Raw,
    /// Single gzip-compressed file — decompress, then write.
    Gzip,
}
