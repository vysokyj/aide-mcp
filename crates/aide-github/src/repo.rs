//! Resolve the (owner, repo) slug GitHub's REST API wants from the
//! project's `origin` remote URL. We accept both SSH
//! (`git@github.com:owner/repo.git`) and HTTPS
//! (`https://github.com/owner/repo[.git]`) forms; anything else returns
//! `None` and the caller surfaces a "not a GitHub repo" error instead
//! of guessing.

use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("not a git repository at {0}")]
    NotARepo(String),
    #[error("git2: {0}")]
    Git2(#[from] git2::Error),
    #[error("origin remote is not a GitHub URL: {0}")]
    NotGithub(String),
    #[error("repo has no `origin` remote configured")]
    NoOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSlug {
    pub owner: String,
    pub repo: String,
}

impl RepoSlug {
    pub fn path(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Look at `path`'s repo, read `origin`, parse it.
pub fn detect_github_slug(path: &Path) -> Result<RepoSlug, RepoError> {
    let repo = git2::Repository::discover(path).map_err(|e| {
        if e.code() == git2::ErrorCode::NotFound {
            RepoError::NotARepo(path.display().to_string())
        } else {
            RepoError::Git2(e)
        }
    })?;
    let remote = match repo.find_remote("origin") {
        Ok(r) => r,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Err(RepoError::NoOrigin),
        Err(e) => return Err(RepoError::Git2(e)),
    };
    let url = remote
        .url()
        .ok_or_else(|| RepoError::NotGithub("<non-utf8>".to_string()))?
        .to_string();
    parse_github_slug(&url).ok_or(RepoError::NotGithub(url))
}

/// Parse `git@github.com:owner/repo.git` and
/// `https://github.com/owner/repo[.git]`. Returns `None` on anything
/// else — the caller decides whether that's fatal.
pub fn parse_github_slug(url: &str) -> Option<RepoSlug> {
    let trimmed = url.trim();
    let rest = if let Some(r) = trimmed.strip_prefix("git@github.com:") {
        r
    } else if let Some(r) = trimmed.strip_prefix("https://github.com/") {
        r
    } else if let Some(r) = trimmed.strip_prefix("http://github.com/") {
        r
    } else if let Some(r) = trimmed.strip_prefix("ssh://git@github.com/") {
        r
    } else {
        return None;
    };
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(RepoSlug {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_form() {
        let got = parse_github_slug("git@github.com:vysokyj/aide-mcp.git").unwrap();
        assert_eq!(got.owner, "vysokyj");
        assert_eq!(got.repo, "aide-mcp");
    }

    #[test]
    fn parses_https_form_with_git() {
        let got = parse_github_slug("https://github.com/vysokyj/aide-mcp.git").unwrap();
        assert_eq!(got.owner, "vysokyj");
        assert_eq!(got.repo, "aide-mcp");
    }

    #[test]
    fn parses_https_form_without_git() {
        let got = parse_github_slug("https://github.com/vysokyj/aide-mcp").unwrap();
        assert_eq!(got.owner, "vysokyj");
        assert_eq!(got.repo, "aide-mcp");
    }

    #[test]
    fn parses_https_form_with_trailing_slash() {
        let got = parse_github_slug("https://github.com/vysokyj/aide-mcp/").unwrap();
        assert_eq!(got.owner, "vysokyj");
        assert_eq!(got.repo, "aide-mcp");
    }

    #[test]
    fn parses_ssh_scheme_form() {
        let got = parse_github_slug("ssh://git@github.com/vysokyj/aide-mcp.git").unwrap();
        assert_eq!(got.owner, "vysokyj");
        assert_eq!(got.repo, "aide-mcp");
    }

    #[test]
    fn rejects_non_github_hosts() {
        assert!(parse_github_slug("git@gitlab.com:user/repo.git").is_none());
        assert!(parse_github_slug("https://bitbucket.org/user/repo").is_none());
    }

    #[test]
    fn rejects_missing_repo() {
        assert!(parse_github_slug("https://github.com/vysokyj/").is_none());
        assert!(parse_github_slug("git@github.com:vysokyj:").is_none());
    }

    #[test]
    fn slug_renders_owner_slash_repo() {
        let slug = RepoSlug {
            owner: "a".into(),
            repo: "b".into(),
        };
        assert_eq!(slug.path(), "a/b");
    }
}
