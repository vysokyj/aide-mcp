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
}

/// Request body for `POST /repos/:owner/:repo/issues`.
#[derive(Debug, Clone, Serialize)]
pub struct IssueCreate {
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
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
}
