use std::io::{Cursor, Read};
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
    #[error("archive extracted but expected entry `{entry}` not found under {dir}")]
    MissingEntry { entry: String, dir: PathBuf },
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

/// Install `spec` into `paths.bin()`, updating the manifest at
/// `paths.bin()/manifest.json`.
///
/// Single-file formats (`Raw`, `Gzip`) write one binary to
/// `~/.aide/bin/<executable>`. Multi-file formats (`TarGz`, `Zip`)
/// extract into `~/.aide/bin/<name>-<version>/` and drop a symlink at
/// `~/.aide/bin/<executable>` pointing at the archive's entry path.
///
/// Re-installing the same version when the binary is on disk is a
/// no-op returning [`InstallOutcome::AlreadyInstalled`].
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

    match asset.archive {
        ArchiveFormat::Raw | ArchiveFormat::Gzip => {
            let binary = decode_single(&bytes, &asset.archive)?;
            write_executable(&install_path, &binary)?;
        }
        ArchiveFormat::TarGz { entry_path } => {
            let extract_dir = bin_dir.join(format!("{}-{}", spec.name, spec.version));
            extract_tar_gz(&bytes, &extract_dir)?;
            link_entry(&extract_dir, entry_path, &install_path)?;
        }
        ArchiveFormat::Zip { entry_path } => {
            let extract_dir = bin_dir.join(format!("{}-{}", spec.name, spec.version));
            extract_zip(&bytes, &extract_dir)?;
            link_entry(&extract_dir, entry_path, &install_path)?;
        }
    }

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

fn decode_single(bytes: &[u8], format: &ArchiveFormat) -> Result<Vec<u8>, InstallError> {
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
        ArchiveFormat::TarGz { .. } | ArchiveFormat::Zip { .. } => Err(InstallError::Decode(
            "decode_single() called with multi-file archive".into(),
        )),
    }
}

fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<(), InstallError> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let gz = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(true);
    archive
        .unpack(dest)
        .map_err(|e| InstallError::Decode(format!("tar.gz: {e}")))?;
    Ok(())
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), InstallError> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| InstallError::Decode(format!("zip open: {e}")))?;
    archive
        .extract(dest)
        .map_err(|e| InstallError::Decode(format!("zip extract: {e}")))?;
    Ok(())
}

fn link_entry(
    extract_dir: &Path,
    entry_path: &str,
    install_path: &Path,
) -> Result<(), InstallError> {
    let target = extract_dir.join(entry_path);
    if !target.exists() {
        return Err(InstallError::MissingEntry {
            entry: entry_path.to_string(),
            dir: extract_dir.to_path_buf(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&target, perms)?;
    }
    // Replace an existing symlink / file so the new version takes over.
    if install_path.exists() || install_path.is_symlink() {
        std::fs::remove_file(install_path)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, install_path)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(&target, install_path)?;
    }
    Ok(())
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
    use tempfile::TempDir;

    #[test]
    fn decode_raw_is_identity() {
        let bytes = b"hello";
        let out = decode_single(bytes, &ArchiveFormat::Raw).unwrap();
        assert_eq!(out, bytes);
    }

    #[test]
    fn decode_gzip_roundtrip() {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(b"rust-analyzer-fake-binary").unwrap();
        let gz = enc.finish().unwrap();
        let out = decode_single(&gz, &ArchiveFormat::Gzip).unwrap();
        assert_eq!(out, b"rust-analyzer-fake-binary");
    }

    #[test]
    fn extract_tar_gz_unpacks_files() {
        // Build a tar.gz with one file in memory.
        let mut tar_buf: Vec<u8> = Vec::new();
        {
            let gz = GzEncoder::new(&mut tar_buf, Compression::default());
            let mut builder = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_path("bin/scip-java").unwrap();
            let payload = b"fake-scip-java-binary";
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, &payload[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dir = TempDir::new().unwrap();
        extract_tar_gz(&tar_buf, dir.path()).unwrap();
        let extracted = std::fs::read(dir.path().join("bin/scip-java")).unwrap();
        assert_eq!(extracted, b"fake-scip-java-binary");
    }

    #[test]
    fn link_entry_creates_symlink_and_errors_on_missing() {
        let dir = TempDir::new().unwrap();
        let extract = dir.path().join("pkg-1.0");
        std::fs::create_dir_all(extract.join("bin")).unwrap();
        std::fs::write(extract.join("bin/tool"), b"#!/bin/sh\nexit 0\n").unwrap();
        let install = dir.path().join("tool");
        link_entry(&extract, "bin/tool", &install).unwrap();
        #[cfg(unix)]
        assert!(install.is_symlink());
        assert!(install.exists());

        let err = link_entry(&extract, "bin/missing", &install).unwrap_err();
        assert!(matches!(err, InstallError::MissingEntry { .. }));
    }
}
