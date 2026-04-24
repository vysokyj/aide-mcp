//! Thin `reqwest` wrapper over the two GitHub REST endpoints v0.19
//! actually uses: list issues and create an issue. A third helper,
//! [`GithubClient::current_user_with_scopes`], reads `/user` plus the
//! `x-oauth-scopes` response header so `gh_auth_status` can report
//! both identity and privileges in a single round-trip.
//!
//! The API surface is intentionally tiny. Adding PRs / releases /
//! generic `gh api` passthrough is explicitly deferred (see
//! STATUS.md v0.19) — we want evidence from dogfood runs before
//! widening it.

use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const GITHUB_API: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("aide-mcp/", env!("CARGO_PKG_VERSION"));
const API_VERSION: &str = "2022-11-28";
const ACCEPT: &str = "application/vnd.github+json";

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("no GitHub token available: {0}")]
    NoAuth(String),
    #[error("HTTP transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("GitHub {status}: {body}")]
    Api { status: StatusCode, body: String },
}

/// Authenticated client. Cheap to construct but holds a reused
/// `reqwest::Client` so repeated calls reuse the TLS connection pool.
pub struct GithubClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl GithubClient {
    pub fn new(token: String) -> Result<Self, GithubError> {
        Self::with_base(token, GITHUB_API.to_string())
    }

    /// Used by wiremock integration tests to point the client at a
    /// mock server.
    pub fn with_base(token: String, base: String) -> Result<Self, GithubError> {
        let http = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self { http, base, token })
    }

    /// `GET /user` — identity + token scopes from the response header.
    /// The scopes header is only present on classic tokens; for
    /// fine-grained tokens GitHub returns `x-oauth-scopes` empty and
    /// we surface that as an empty vec (the agent can still tell auth
    /// works because `login` is populated).
    pub async fn current_user_with_scopes(&self) -> Result<(User, Vec<String>), GithubError> {
        let url = format!("{}/user", self.base);
        let resp = self.build(self.http.get(&url)).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GithubError::Api { status, body });
        }
        let scopes = parse_scopes_header(&resp);
        let user = resp.json::<User>().await?;
        Ok((user, scopes))
    }

    /// `POST /repos/:owner/:repo/issues`.
    pub async fn create_issue(
        &self,
        owner: &str,
        repo: &str,
        payload: &IssueCreate,
    ) -> Result<Issue, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/issues", self.base);
        let resp = self
            .build(self.http.post(&url).json(payload))
            .send()
            .await?;
        self.parse(resp).await
    }

    /// `GET /repos/:owner/:repo/issues` with state/label/limit filters.
    pub async fn list_issues(
        &self,
        owner: &str,
        repo: &str,
        filter: &IssueListFilter,
    ) -> Result<Vec<Issue>, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/issues", self.base);
        let mut req = self.http.get(&url);
        if let Some(state) = &filter.state {
            req = req.query(&[("state", state.as_str())]);
        }
        if !filter.labels.is_empty() {
            req = req.query(&[("labels", filter.labels.join(","))]);
        }
        if let Some(limit) = filter.limit {
            req = req.query(&[("per_page", limit.to_string())]);
        }
        let resp = self.build(req).send().await?;
        self.parse(resp).await
    }

    /// `GET /repos/:owner/:repo/issues/:number` — single issue with
    /// full body. GitHub's list endpoint returns body too, but this
    /// is the canonical endpoint for "view one" when you already know
    /// the number.
    pub async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Issue, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/issues/{number}", self.base);
        let resp = self.build(self.http.get(&url)).send().await?;
        self.parse(resp).await
    }

    /// `GET /repos/:owner/:repo/issues/:number/comments` — all
    /// comments in chronological order. GitHub paginates at 30 by
    /// default; we ask for 100 (the REST maximum) in one call and
    /// leave multi-page fetching for the day somebody actually hits
    /// a 100-comment issue.
    pub async fn list_comments(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Vec<Comment>, GithubError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/issues/{number}/comments",
            self.base
        );
        let req = self.http.get(&url).query(&[("per_page", "100")]);
        let resp = self.build(req).send().await?;
        self.parse(resp).await
    }

    /// `POST /repos/:owner/:repo/issues/:number/comments`. Returns
    /// the created comment so callers can show the `html_url`.
    pub async fn create_comment(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<Comment, GithubError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/issues/{number}/comments",
            self.base
        );
        let payload = CommentCreate {
            body: body.to_string(),
        };
        let resp = self
            .build(self.http.post(&url).json(&payload))
            .send()
            .await?;
        self.parse(resp).await
    }

    /// `PATCH /repos/:owner/:repo/issues/:number` with `state:
    /// "closed"` and an optional `state_reason`. Returns the updated
    /// Issue so callers can confirm the transition landed — closing
    /// an already-closed issue is a GitHub no-op, not an error.
    pub async fn close_issue(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        reason: Option<CloseReason>,
    ) -> Result<Issue, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/issues/{number}", self.base);
        let payload = IssueUpdate {
            state: Some("closed".to_string()),
            state_reason: reason.map(|r| r.as_str().to_string()),
        };
        let resp = self
            .build(self.http.patch(&url).json(&payload))
            .send()
            .await?;
        self.parse(resp).await
    }

    /// `POST /repos/:owner/:repo/pulls`. Returns the created PR
    /// (includes head/base branch + sha pairs needed by other tools).
    pub async fn create_pr(
        &self,
        owner: &str,
        repo: &str,
        payload: &PullRequestCreate,
    ) -> Result<PullRequest, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/pulls", self.base);
        let resp = self
            .build(self.http.post(&url).json(payload))
            .send()
            .await?;
        self.parse(resp).await
    }

    /// `GET /repos/:owner/:repo/pulls/:number`. Returns the full PR
    /// record — body, draft flag, merged flag, head/base branches,
    /// timestamps. For issue-style comments on a PR, call
    /// `list_comments` with the PR number (PR numbers share the
    /// issues namespace on GitHub).
    pub async fn get_pr(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<PullRequest, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/pulls/{number}", self.base);
        let resp = self.build(self.http.get(&url)).send().await?;
        self.parse(resp).await
    }

    /// `GET /repos/:owner/:repo/pulls` with state/head/base/limit
    /// filters. `head` takes GitHub's `owner:branch` format when
    /// filtering across forks; for same-repo branches the bare branch
    /// name also works.
    pub async fn list_prs(
        &self,
        owner: &str,
        repo: &str,
        filter: &PullRequestListFilter,
    ) -> Result<Vec<PullRequest>, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/pulls", self.base);
        let mut req = self.http.get(&url);
        if let Some(state) = &filter.state {
            req = req.query(&[("state", state.as_str())]);
        }
        if let Some(head) = &filter.head {
            req = req.query(&[("head", head.as_str())]);
        }
        if let Some(base) = &filter.base {
            req = req.query(&[("base", base.as_str())]);
        }
        if let Some(limit) = filter.limit {
            req = req.query(&[("per_page", limit.to_string())]);
        }
        let resp = self.build(req).send().await?;
        self.parse(resp).await
    }

    /// `GET /repos/:owner/:repo`. Returns the `Repo` record — used
    /// by `gh_pr_create` to default `base` to the repo's configured
    /// default branch when the caller doesn't specify one.
    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<Repo, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}", self.base);
        let resp = self.build(self.http.get(&url)).send().await?;
        self.parse(resp).await
    }

    /// `GET /repos/:owner/:repo/commits/:ref/check-runs` — every
    /// check-run attached to the commit at `ref` (which is typically
    /// the PR's head sha). Returns `{total_count, check_runs}`.
    /// Accepts any git ref GitHub resolves (branch, tag, sha).
    pub async fn check_runs(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
    ) -> Result<CheckRunsResponse, GithubError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/commits/{git_ref}/check-runs",
            self.base
        );
        let req = self.http.get(&url).query(&[("per_page", "100")]);
        let resp = self.build(req).send().await?;
        self.parse(resp).await
    }

    fn build(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.bearer_auth(&self.token)
            .header(header::ACCEPT, ACCEPT)
            .header("X-GitHub-Api-Version", API_VERSION)
    }

    async fn parse<T: for<'de> Deserialize<'de>>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, GithubError> {
        let status = resp.status();
        if status.is_success() {
            Ok(resp.json::<T>().await?)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(GithubError::Api { status, body })
        }
    }
}

fn parse_scopes_header(resp: &reqwest::Response) -> Vec<String> {
    resp.headers()
        .get("x-oauth-scopes")
        .and_then(|h| h.to_str().ok())
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// Populated by `get_issue` and (for GitHub-provided bodies) by
    /// `list_issues` too. Can be `null` when the issue was created
    /// without a body.
    #[serde(default)]
    pub body: Option<String>,
    /// `completed`, `not_planned`, `reopened`, or absent. Present on
    /// every issue in recent GitHub API versions; kept optional to
    /// survive API shape changes.
    #[serde(default)]
    pub state_reason: Option<String>,
}

/// One comment on an issue. `user` is who posted it, `body` is the
/// markdown text, timestamps are ISO-8601 strings straight from
/// GitHub.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Comment {
    pub id: u64,
    pub body: String,
    pub user: User,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: String,
}

/// Request body for `POST /repos/:owner/:repo/issues`.
#[derive(Debug, Clone, Serialize)]
pub struct IssueCreate {
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// Request body for `POST /repos/:owner/:repo/issues/:number/comments`.
#[derive(Debug, Clone, Serialize)]
pub struct CommentCreate {
    pub body: String,
}

/// Request body for `PATCH /repos/:owner/:repo/issues/:number`. Only
/// the two fields v0.19.1 actually exercises — GitHub accepts many
/// more (title, body, labels, assignees, milestone), but widening
/// this struct without a tool that uses the field would be dead API
/// surface.
#[derive(Debug, Clone, Serialize, Default)]
pub struct IssueUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
}

/// Allowed values for `PATCH issues/:n` `state_reason` when closing.
/// Reopen uses `state_reason: "reopened"` but we don't expose reopen
/// in v0.19.1 (YAGNI).
#[derive(Debug, Clone, Copy)]
pub enum CloseReason {
    Completed,
    NotPlanned,
}

impl CloseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NotPlanned => "not_planned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "completed" => Some(Self::Completed),
            "not_planned" | "not planned" => Some(Self::NotPlanned),
            _ => None,
        }
    }
}

/// Filters for `list_issues`. All fields optional — `None` means "no
/// constraint" (GitHub's own default applies).
#[derive(Debug, Clone, Default)]
pub struct IssueListFilter {
    pub state: Option<IssueState>,
    pub labels: Vec<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub enum IssueState {
    Open,
    Closed,
    All,
}

impl IssueState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::All => "all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

// -------- v0.21 PR workflow types --------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    pub base: Branch,
    pub head: Branch,
    #[serde(default)]
    pub draft: bool,
    /// True/false once the PR has a merged status; absent on fresh
    /// or intermediate states.
    #[serde(default)]
    pub merged: Option<bool>,
    /// Present when the API returned its mergeability computation —
    /// often null on `list`, populated on `get`.
    #[serde(default)]
    pub mergeable: Option<bool>,
    pub user: User,
}

/// Branch side of a PR — `ref` is the branch name (`serde` rename
/// to avoid the Rust keyword), `sha` is the tip commit.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Branch {
    #[serde(rename = "ref")]
    pub ref_: String,
    pub sha: String,
}

/// Request body for `POST /repos/:owner/:repo/pulls`. `head` takes
/// GitHub's `owner:branch` form when the PR crosses forks; for same-
/// repo PRs the bare branch name works too.
#[derive(Debug, Clone, Serialize)]
pub struct PullRequestCreate {
    pub title: String,
    pub body: String,
    pub head: String,
    pub base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
}

/// Filters for `list_prs`. All fields optional — `None` means "no
/// constraint" (GitHub's defaults apply: `state=open`, `per_page=30`).
#[derive(Debug, Clone, Default)]
pub struct PullRequestListFilter {
    pub state: Option<IssueState>,
    pub head: Option<String>,
    pub base: Option<String>,
    pub limit: Option<u32>,
}

/// Minimal repo metadata — just the field `gh_pr_create` needs for
/// default-branch resolution. Widened later only when a concrete tool
/// demands it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repo {
    pub name: String,
    pub default_branch: String,
    #[serde(default)]
    pub private: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CheckRun {
    pub id: u64,
    pub name: String,
    /// `queued` / `in_progress` / `completed`.
    pub status: String,
    /// Present once `status == "completed"`: `success` / `failure` /
    /// `neutral` / `cancelled` / `skipped` / `timed_out` /
    /// `action_required` / `stale`.
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// Response shape of `GET /repos/:owner/:repo/commits/:ref/check-runs`.
/// `total_count` is the full count upstream regardless of pagination;
/// `check_runs` is capped at `per_page`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CheckRunsResponse {
    pub total_count: u64,
    pub check_runs: Vec<CheckRun>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{body_partial_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn create_issue_posts_to_repo_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/widget/issues"))
            .and(header("authorization", "Bearer gho_test"))
            .and(header("x-github-api-version", API_VERSION))
            .and(body_partial_json(serde_json::json!({
                "title": "hello",
                "body": "world",
                "labels": ["ux-gotcha"]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "number": 42,
                "title": "hello",
                "state": "open",
                "html_url": "https://github.com/acme/widget/issues/42",
                "labels": [{"name": "ux-gotcha"}]
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("gho_test".into(), server.uri()).unwrap();
        let got = client
            .create_issue(
                "acme",
                "widget",
                &IssueCreate {
                    title: "hello".into(),
                    body: "world".into(),
                    labels: vec!["ux-gotcha".into()],
                },
            )
            .await
            .unwrap();

        assert_eq!(got.number, 42);
        assert_eq!(got.labels.len(), 1);
        assert_eq!(got.labels[0].name, "ux-gotcha");
    }

    #[tokio::test]
    async fn list_issues_passes_filters_as_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget/issues"))
            .and(query_param("state", "open"))
            .and(query_param("labels", "ux-gotcha,bug"))
            .and(query_param("per_page", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let filter = IssueListFilter {
            state: Some(IssueState::Open),
            labels: vec!["ux-gotcha".into(), "bug".into()],
            limit: Some(5),
        };
        let got = client.list_issues("acme", "widget", &filter).await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn api_error_surfaces_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/widget/issues"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string("{\"message\":\"Bad credentials\"}"),
            )
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let err = client
            .create_issue(
                "acme",
                "widget",
                &IssueCreate {
                    title: "x".into(),
                    body: "x".into(),
                    labels: vec![],
                },
            )
            .await
            .unwrap_err();
        match err {
            GithubError::Api { status, body } => {
                assert_eq!(status, StatusCode::UNAUTHORIZED);
                assert!(body.contains("Bad credentials"));
            }
            other => panic!("unexpected err: {other:?}"),
        }
    }

    #[tokio::test]
    async fn current_user_reads_scopes_from_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-oauth-scopes", "repo, read:org, workflow")
                    .set_body_json(serde_json::json!({"login": "octocat"})),
            )
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let (user, scopes) = client.current_user_with_scopes().await.unwrap();
        assert_eq!(user.login, "octocat");
        assert_eq!(scopes, vec!["repo", "read:org", "workflow"]);
    }

    #[test]
    fn issue_state_round_trip() {
        for s in ["open", "closed", "all"] {
            let parsed = IssueState::parse(s).unwrap();
            assert_eq!(parsed.as_str(), s);
        }
        assert!(IssueState::parse("nope").is_none());
    }

    #[test]
    fn empty_labels_skip_serialization() {
        let payload = IssueCreate {
            title: "t".into(),
            body: "b".into(),
            labels: vec![],
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v.get("labels").is_none(), "labels field should be omitted");
    }

    #[tokio::test]
    async fn get_issue_returns_body_and_state_reason() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget/issues/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "hello",
                "state": "closed",
                "html_url": "https://github.com/acme/widget/issues/42",
                "labels": [{"name": "bug"}],
                "body": "full issue body here",
                "state_reason": "completed"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let got = client.get_issue("acme", "widget", 42).await.unwrap();
        assert_eq!(got.number, 42);
        assert_eq!(got.body.as_deref(), Some("full issue body here"));
        assert_eq!(got.state_reason.as_deref(), Some("completed"));
    }

    #[tokio::test]
    async fn list_comments_asks_for_100_per_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget/issues/7/comments"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 111,
                    "body": "looks good",
                    "user": {"login": "octocat"},
                    "created_at": "2026-04-24T12:00:00Z",
                    "updated_at": "2026-04-24T12:00:00Z",
                    "html_url": "https://github.com/acme/widget/issues/7#issuecomment-111"
                }
            ])))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let got = client.list_comments("acme", "widget", 7).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, 111);
        assert_eq!(got[0].user.login, "octocat");
    }

    #[tokio::test]
    async fn create_comment_posts_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/widget/issues/7/comments"))
            .and(body_partial_json(serde_json::json!({"body": "agree"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 222,
                "body": "agree",
                "user": {"login": "octocat"},
                "created_at": "2026-04-24T12:05:00Z",
                "updated_at": "2026-04-24T12:05:00Z",
                "html_url": "https://github.com/acme/widget/issues/7#issuecomment-222"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let got = client
            .create_comment("acme", "widget", 7, "agree")
            .await
            .unwrap();
        assert_eq!(got.id, 222);
        assert_eq!(got.body, "agree");
    }

    #[tokio::test]
    async fn close_issue_patches_state_and_reason() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/repos/acme/widget/issues/7"))
            .and(body_partial_json(
                serde_json::json!({"state": "closed", "state_reason": "completed"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 7,
                "title": "bug",
                "state": "closed",
                "state_reason": "completed",
                "html_url": "https://github.com/acme/widget/issues/7",
                "labels": []
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let got = client
            .close_issue("acme", "widget", 7, Some(CloseReason::Completed))
            .await
            .unwrap();
        assert_eq!(got.state, "closed");
        assert_eq!(got.state_reason.as_deref(), Some("completed"));
    }

    #[tokio::test]
    async fn close_issue_without_reason_omits_state_reason() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/repos/acme/widget/issues/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 7,
                "title": "bug",
                "state": "closed",
                "html_url": "https://github.com/acme/widget/issues/7",
                "labels": []
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let _ = client.close_issue("acme", "widget", 7, None).await.unwrap();

        let payload = IssueUpdate {
            state: Some("closed".into()),
            state_reason: None,
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert!(
            v.get("state_reason").is_none(),
            "state_reason should be omitted when None"
        );
    }

    #[test]
    fn close_reason_parse_round_trip() {
        assert_eq!(
            CloseReason::parse("completed").unwrap().as_str(),
            "completed"
        );
        assert_eq!(
            CloseReason::parse("not_planned").unwrap().as_str(),
            "not_planned"
        );
        assert_eq!(
            CloseReason::parse("not planned").unwrap().as_str(),
            "not_planned"
        );
        assert!(CloseReason::parse("nope").is_none());
    }

    // -------- v0.21 PR workflow tests --------

    fn pr_response() -> serde_json::Value {
        serde_json::json!({
            "number": 42,
            "title": "feat: add thing",
            "state": "open",
            "html_url": "https://github.com/acme/widget/pull/42",
            "body": "the body",
            "base": {"ref": "master", "sha": "basesha"},
            "head": {"ref": "feature", "sha": "headsha"},
            "draft": false,
            "merged": false,
            "mergeable": true,
            "user": {"login": "octocat"}
        })
    }

    #[tokio::test]
    async fn create_pr_posts_to_pulls_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/widget/pulls"))
            .and(body_partial_json(serde_json::json!({
                "title": "feat: add thing",
                "body": "the body",
                "head": "feature",
                "base": "master",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(pr_response()))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let pr = client
            .create_pr(
                "acme",
                "widget",
                &PullRequestCreate {
                    title: "feat: add thing".into(),
                    body: "the body".into(),
                    head: "feature".into(),
                    base: "master".into(),
                    draft: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.head.ref_, "feature");
        assert_eq!(pr.head.sha, "headsha");
        assert_eq!(pr.base.ref_, "master");
    }

    #[tokio::test]
    async fn get_pr_parses_branch_refs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_response()))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let pr = client.get_pr("acme", "widget", 42).await.unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.body.as_deref(), Some("the body"));
        assert_eq!(pr.head.sha, "headsha");
    }

    #[tokio::test]
    async fn list_prs_passes_filters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget/pulls"))
            .and(query_param("state", "open"))
            .and(query_param("head", "octocat:feature"))
            .and(query_param("base", "master"))
            .and(query_param("per_page", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let got = client
            .list_prs(
                "acme",
                "widget",
                &PullRequestListFilter {
                    state: Some(IssueState::Open),
                    head: Some("octocat:feature".into()),
                    base: Some("master".into()),
                    limit: Some(10),
                },
            )
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn get_repo_returns_default_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "widget",
                "default_branch": "trunk",
                "private": false
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let repo = client.get_repo("acme", "widget").await.unwrap();
        assert_eq!(repo.default_branch, "trunk");
        assert_eq!(repo.name, "widget");
        assert!(!repo.private);
    }

    #[tokio::test]
    async fn check_runs_asks_for_100_per_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widget/commits/headsha/check-runs"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total_count": 2,
                "check_runs": [
                    {
                        "id": 1,
                        "name": "fmt",
                        "status": "completed",
                        "conclusion": "success",
                        "html_url": "https://github.com/acme/widget/runs/1",
                        "started_at": "2026-04-24T12:00:00Z",
                        "completed_at": "2026-04-24T12:00:10Z"
                    },
                    {
                        "id": 2,
                        "name": "clippy",
                        "status": "in_progress",
                        "conclusion": null,
                        "html_url": "https://github.com/acme/widget/runs/2",
                        "started_at": "2026-04-24T12:00:10Z",
                        "completed_at": null
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_base("t".into(), server.uri()).unwrap();
        let resp = client
            .check_runs("acme", "widget", "headsha")
            .await
            .unwrap();
        assert_eq!(resp.total_count, 2);
        assert_eq!(resp.check_runs.len(), 2);
        assert_eq!(resp.check_runs[0].conclusion.as_deref(), Some("success"));
        assert!(resp.check_runs[1].conclusion.is_none());
    }

    #[test]
    fn pr_create_draft_field_is_omitted_when_none() {
        let payload = PullRequestCreate {
            title: "t".into(),
            body: "b".into(),
            head: "f".into(),
            base: "m".into(),
            draft: None,
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v.get("draft").is_none());
    }
}
