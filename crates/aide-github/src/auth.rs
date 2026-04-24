//! Token resolution waterfall: `$GITHUB_TOKEN` → `gh auth token` → file.
//!
//! The three sources cover the three realistic user situations:
//!
//! - **CI / power user** — sets `$GITHUB_TOKEN` explicitly in the process
//!   environment.
//! - **Developer with `gh`** — has already run `gh auth login`; we shell
//!   out to `gh auth token` which returns the token from its keyring
//!   (macOS Keychain / Secret Service / `~/.config/gh/hosts.yml`).
//! - **Developer without `gh`** — can drop a classic or fine-grained PAT
//!   into `~/.aide/auth/github.token` (mode 0600) and we'll read it.
//!
//! If all three miss, callers report [`AuthSource::None`] and the MCP
//! tools emit an actionable error. We deliberately do not fall through
//! to anonymous API calls — every tool in this crate writes or reads
//! user-scoped data, and silently unauthenticated requests would hit
//! the 60-rph anonymous rate limit after a handful of calls.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use thiserror::Error;

/// Which source produced the active token — reported unchanged by
/// `gh_auth_status` so agents can see *why* auth is or isn't working
/// without inspecting the token itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    Env,
    Gh,
    File,
    None,
}

impl AuthSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Gh => "gh",
            Self::File => "file",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("I/O error reading token file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A token plus the source it came from. The `token` field is never
/// logged by aide-mcp and must never appear in tool responses.
#[derive(Debug, Clone)]
pub struct ResolvedToken {
    pub token: String,
    pub source: AuthSource,
}

/// Walk the waterfall. `file_path` is typically
/// `AidePaths::github_token()` but is passed in so tests can point at
/// a tempdir.
pub async fn resolve_token(file_path: &Path) -> Result<Option<ResolvedToken>, AuthError> {
    if let Some(tok) = env_token() {
        return Ok(Some(ResolvedToken {
            token: tok,
            source: AuthSource::Env,
        }));
    }
    if let Some(tok) = gh_auth_token().await {
        return Ok(Some(ResolvedToken {
            token: tok,
            source: AuthSource::Gh,
        }));
    }
    if let Some(tok) = file_token(file_path).await? {
        return Ok(Some(ResolvedToken {
            token: tok,
            source: AuthSource::File,
        }));
    }
    Ok(None)
}

fn env_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn gh_auth_token() -> Option<String> {
    let out = tokio::process::Command::new("gh")
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
}

async fn file_token(path: &Path) -> Result<Option<String>, AuthError> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => {
            let tok = s.trim().to_string();
            if tok.is_empty() {
                Ok(None)
            } else {
                Ok(Some(tok))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AuthError::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

/// Actionable text emitted when the waterfall resolves to `None`. MCP
/// tools embed this in their error body so the agent/user has a clear
/// next step without having to read source.
pub const NO_AUTH_REMEDIATION: &str = concat!(
    "No GitHub token available. Pick one:\n",
    "  1. Set $GITHUB_TOKEN in the environment.\n",
    "  2. Run `gh auth login` (aide will call `gh auth token`).\n",
    "  3. Write a classic or fine-grained PAT to ~/.aide/auth/github.token (mode 0600).\n",
    "Minimum scopes for issue tools: `repo` (or `public_repo` for public-only use)."
);

#[cfg(test)]
mod tests {
    use super::*;

    // Keeping these tests hermetic: each sets up its own tempdir file
    // and we do not assert anything about the real $GITHUB_TOKEN /
    // `gh auth token` that may be present on the dev machine — that
    // would make CI behaviour depend on the runner's shell config.

    #[tokio::test]
    async fn file_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github.token");
        let got = file_token(&path).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn file_empty_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github.token");
        tokio::fs::write(&path, "   \n  \n").await.unwrap();
        let got = file_token(&path).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn file_with_token_trims_and_returns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github.token");
        tokio::fs::write(&path, "  gho_examplevalue  \n")
            .await
            .unwrap();
        let got = file_token(&path).await.unwrap().unwrap();
        assert_eq!(got, "gho_examplevalue");
    }

    #[test]
    fn auth_source_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&AuthSource::Env).unwrap(), "\"env\"");
        assert_eq!(serde_json::to_string(&AuthSource::Gh).unwrap(), "\"gh\"");
        assert_eq!(
            serde_json::to_string(&AuthSource::None).unwrap(),
            "\"none\""
        );
    }
}
