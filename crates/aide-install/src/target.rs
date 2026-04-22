use thiserror::Error;

#[derive(Debug, Error)]
pub enum TargetTripleError {
    #[error("unsupported platform: arch={arch} os={os}")]
    Unsupported {
        arch: &'static str,
        os: &'static str,
    },
}

/// Return the Rust-style target triple for the current host.
///
/// Only the triples we actively support are emitted; anything else returns an
/// error so the caller can surface a clear message to the user.
pub fn current_triple() -> Result<&'static str, TargetTripleError> {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    let triple = match (arch, os) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        _ => return Err(TargetTripleError::Unsupported { arch, os }),
    };
    Ok(triple)
}
