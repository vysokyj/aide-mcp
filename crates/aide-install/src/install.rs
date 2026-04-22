use std::io::Read;
use std::path::{Path, PathBuf};

use aide_core::AidePaths;
use flate2::read::GzDecoder;
use thiserror::Error;

use crate::manifest::{InstalledRecord, Manifest, ManifestError};
use crate::spec::{ArchiveFormat, Source, TargetAsset, ToolSpec};
use crate::target::{current_triple, TargetTripleError};

#[derive(Debug, Error)]
pub enum InstallError {
    #[error(transparent)]
    Target(#[from] TargetTripleError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("no release asset for target triple {triple} in tool {tool}")]
    NoAssetForTriple { tool: String, triple: &'static str },
    #[error("download failed ({status}) from {url}")]
    Download {
        url: String,
        status: reqwest::StatusCode,
    },
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive decode error: {0}")]
    Decode(String),
}

/// Result of an install attempt.
#[derive(Debug, Clone)]
pub enum InstallOutcome {
    /// The tool was already installed at the requested version.
    AlreadyInstalled { path: PathBuf, version: String },
    /// The tool was downloaded and installed now.
    Installed { path: PathBuf, version: String },
}

impl InstallOutcome {
    pub fn path(&self) -> &Path {
        match self {
            Self::AlreadyInstalled { path, .. } | Self::Installed { path, .. } => path,
        }
    }
}

/// Install `spec` into `paths.bin()`, updating the manifest at `paths.bin()/manifest.json`.
///
/// If the manifest already records the requested version and the binary is on disk,
/// this is a no-op returning [`InstallOutcome::AlreadyInstalled`]. Otherwise the
/// binary is downloaded, decoded, written to `~/.aide/bin/<executable>`, and the
/// manifest is updated atomically.
pub async fn install_tool(
    paths: &AidePaths,
    spec: &ToolSpec,
) -> Result<InstallOutcome, InstallError> {
    let bin_dir = paths.bin();
    std::fs::create_dir_all(&bin_dir)?;
    let manifest_path = bin_dir.join("manifest.json");
    let mut manifest = Manifest::load(&manifest_path)?;

    let install_path = bin_dir.join(&spec.executable);

    if let Some(existing) = manifest.get(&spec.name) {
        if existing.version == spec.version && install_path.exists() {
            tracing::debug!(tool = %spec, "already installed");
            return Ok(InstallOutcome::AlreadyInstalled {
                path: install_path,
                version: existing.version.clone(),
            });
        }
    }

    let triple = current_triple()?;
    let asset = pick_asset(spec, triple)?;
    let url = build_url(&spec.source, &asset.filename);
    tracing::info!(tool = %spec, url = %url, "downloading");
    let bytes = http_get(&url).await?;
    let binary = decode(&bytes, asset.archive)?;
    write_executable(&install_path, &binary)?;

    manifest.record(
        &spec.name,
        InstalledRecord::new(spec.version.clone(), install_path.clone()),
    );
    manifest.save(&manifest_path)?;

    Ok(InstallOutcome::Installed {
        path: install_path,
        version: spec.version.clone(),
    })
}

fn pick_asset<'a>(
    spec: &'a ToolSpec,
    triple: &'static str,
) -> Result<&'a TargetAsset, InstallError> {
    match &spec.source {
        Source::GithubRelease { assets, .. } => assets
            .iter()
            .find(|a| a.triple == triple)
            .ok_or_else(|| InstallError::NoAssetForTriple {
                tool: spec.name.clone(),
                triple,
            }),
    }
}

fn build_url(source: &Source, filename: &str) -> String {
    match source {
        Source::GithubRelease { repo, tag, .. } => {
            format!("https://github.com/{repo}/releases/download/{tag}/{filename}")
        }
    }
}

async fn http_get(url: &str) -> Result<Vec<u8>, InstallError> {
    let response = reqwest::Client::builder()
        .user_agent(concat!("aide-mcp/", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(url)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(InstallError::Download {
            url: url.to_string(),
            status,
        });
    }
    Ok(response.bytes().await?.to_vec())
}

fn decode(bytes: &[u8], format: ArchiveFormat) -> Result<Vec<u8>, InstallError> {
    match format {
        ArchiveFormat::Raw => Ok(bytes.to_vec()),
        ArchiveFormat::Gzip => {
            let mut decoder = GzDecoder::new(bytes);
            let mut out = Vec::with_capacity(bytes.len() * 4);
            decoder
                .read_to_end(&mut out)
                .map_err(|e| InstallError::Decode(e.to_string()))?;
            Ok(out)
        }
    }
}

fn write_executable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("download");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn decode_raw_is_identity() {
        let bytes = b"hello";
        let out = decode(bytes, ArchiveFormat::Raw).unwrap();
        assert_eq!(out, bytes);
    }

    #[test]
    fn decode_gzip_roundtrip() {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(b"rust-analyzer-fake-binary").unwrap();
        let gz = enc.finish().unwrap();
        let out = decode(&gz, ArchiveFormat::Gzip).unwrap();
        assert_eq!(out, b"rust-analyzer-fake-binary");
    }
}
