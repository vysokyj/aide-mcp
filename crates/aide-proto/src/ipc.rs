use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Liveness probe.
    Ping,
    /// Register a commit as something the indexer should know about.
    /// In v0.3 the daemon records the SHA but does not yet build SCIP
    /// (real indexing arrives in v0.4).
    Enqueue { repo_root: String, sha: String },
    /// Ask the daemon for the state of a specific commit. If `sha` is
    /// omitted, the daemon returns the state of the most recently
    /// enqueued commit for that repo.
    IndexStatus {
        repo_root: String,
        sha: Option<String>,
    },
    /// Ask the daemon for the last commit it knows about for this repo.
    LastKnownState { repo_root: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Ok,
    IndexStatus {
        repo_root: String,
        sha: String,
        state: IndexState,
        enqueued_at_unix: i64,
        indexed_at_unix: Option<i64>,
    },
    NoCommit {
        repo_root: String,
    },
    LastKnownState {
        repo_root: String,
        commit: Option<CommitInfo>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    /// Enqueued, waiting to be indexed.
    Pending,
    /// Indexer is currently working on this commit.
    InProgress,
    /// Index is ready to query.
    Ready,
    /// Indexer tried and gave up; the string is a human-readable reason.
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub state: IndexState,
    pub enqueued_at_unix: i64,
    pub indexed_at_unix: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request::Enqueue {
            repo_root: "/tmp/repo".into(),
            sha: "deadbeef".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""kind":"enqueue""#));
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn response_roundtrip_ready() {
        let resp = Response::IndexStatus {
            repo_root: "/tmp/repo".into(),
            sha: "abc".into(),
            state: IndexState::Ready,
            enqueued_at_unix: 100,
            indexed_at_unix: Some(200),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""state":"ready""#));
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn failed_state_is_externally_tagged() {
        let state = IndexState::Failed("disk full".into());
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, r#"{"failed":"disk full"}"#);
        let back: IndexState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }
}
