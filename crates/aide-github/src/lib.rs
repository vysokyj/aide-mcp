//! GitHub integration for aide-mcp.
//!
//! Two responsibilities, deliberately minimal:
//!
//! 1. **Auth via piggyback** ([`auth`]) — a token-resolution waterfall:
//!    `$GITHUB_TOKEN` → `gh auth token` → `~/.aide/auth/github.token`.
//!    We do not run our own OAuth flow (see v0.19 in `STATUS.md` for the
//!    deferred variant C). Users without any of the three sources get a
//!    structured error pointing at the three remediations.
//! 2. **REST client over issues** ([`client`]) — a thin `reqwest` wrapper
//!    that hits `api.github.com/repos/:owner/:repo/issues`. Owner/repo
//!    is detected from the project's `origin` remote via [`repo`].
//!
//! The `ux_gotcha` module wraps `IssueCreate` with the policy from
//! `CLAUDE.md` § "Reporting UX gotchas": hardcoded `ux-gotcha` label,
//! title prefixed with the implicated tool, and a provenance footer so
//! filed issues are always attributable to this channel.

pub mod auth;
pub mod client;
pub mod repo;
pub mod ux_gotcha;

pub use auth::{resolve_token, AuthError, AuthSource, ResolvedToken, NO_AUTH_REMEDIATION};
pub use client::{
    Branch, CheckRun, CheckRunsResponse, CloseReason, Comment, CommentCreate, GithubClient,
    GithubError, Issue, IssueCreate, IssueListFilter, IssueState, IssueUpdate, Label, PullRequest,
    PullRequestCreate, PullRequestListFilter, Repo, User,
};
pub use repo::{detect_github_slug, parse_github_slug, RepoError, RepoSlug};
