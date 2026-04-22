use serde::{Deserialize, Serialize};

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
    /// Filesystem path to the `.scip` index produced for this commit.
    /// Populated only once [`IndexState::Ready`] is reached.
    pub index_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_state_is_externally_tagged() {
        let state = IndexState::Failed("disk full".into());
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, r#"{"failed":"disk full"}"#);
        let back: IndexState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn commit_info_roundtrip_with_index_path() {
        let info = CommitInfo {
            sha: "abc".into(),
            state: IndexState::Ready,
            enqueued_at_unix: 1,
            indexed_at_unix: Some(2),
            index_path: Some("/home/u/.aide/scip/r/abc.scip".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: CommitInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }
}
