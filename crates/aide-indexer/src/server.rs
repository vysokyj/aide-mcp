//! Unix-socket request/response loop for the indexer daemon.

use std::path::Path;

use aide_proto::framing::{read_message, write_message};
use aide_proto::{Request, Response};
use anyhow::{Context, Result};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::state::{EnqueueOutcome, Store};
use crate::worker::Job;

pub async fn run(socket: &Path, store: Store, jobs: mpsc::UnboundedSender<Job>) -> Result<()> {
    if socket.exists() {
        tracing::warn!(path = %socket.display(), "removing stale socket");
        std::fs::remove_file(socket).context("removing stale socket")?;
    }

    let listener = UnixListener::bind(socket)
        .with_context(|| format!("binding socket {}", socket.display()))?;
    tracing::info!(path = %socket.display(), "listening");

    loop {
        let (stream, _peer) = listener.accept().await?;
        let store = store.clone();
        let jobs = jobs.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_connection(stream, store, jobs).await {
                tracing::warn!(error = %e, "connection ended with error");
            }
        });
    }
}

async fn serve_connection(
    stream: UnixStream,
    store: Store,
    jobs: mpsc::UnboundedSender<Job>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let request: Option<Request> = read_message(&mut reader).await?;
        let Some(request) = request else {
            break;
        };
        let response = dispatch(request, &store, &jobs).await;
        write_message(&mut write_half, &response).await?;
    }

    write_half.shutdown().await.ok();
    Ok(())
}

async fn dispatch(request: Request, store: &Store, jobs: &mpsc::UnboundedSender<Job>) -> Response {
    match request {
        Request::Ping => Response::Pong,
        Request::Enqueue { repo_root, sha } => match store.enqueue(&repo_root, &sha).await {
            Ok(EnqueueOutcome::NeedsIndexing) => {
                if let Err(e) = jobs.send(Job {
                    repo_root: repo_root.clone(),
                    sha: sha.clone(),
                }) {
                    return Response::Error {
                        message: format!("worker channel closed: {e}"),
                    };
                }
                Response::Ok
            }
            Ok(EnqueueOutcome::AlreadyQueued | EnqueueOutcome::AlreadyReady) => Response::Ok,
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        },
        Request::IndexStatus { repo_root, sha } => {
            match store.status(&repo_root, sha.as_deref()).await {
                Some(info) => Response::IndexStatus {
                    repo_root,
                    sha: info.sha.clone(),
                    state: info.state,
                    enqueued_at_unix: info.enqueued_at_unix,
                    indexed_at_unix: info.indexed_at_unix,
                    index_path: info.index_path,
                },
                None => Response::NoCommit { repo_root },
            }
        }
        Request::LastKnownState { repo_root } => {
            let commit = store.last_known(&repo_root).await;
            Response::LastKnownState { repo_root, commit }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aide_proto::framing::{read_message, write_message};
    use aide_proto::{IndexState, Request, Response};
    use tempfile::TempDir;

    struct Daemon {
        handle: tokio::task::JoinHandle<()>,
        /// Kept alive so the channel stays open while the server runs.
        /// The worker is not actually executed; jobs just queue here.
        #[allow(dead_code, reason = "keeps the mpsc channel open")]
        rx: mpsc::UnboundedReceiver<Job>,
    }

    async fn spawn_daemon(socket: &Path, state_path: &Path) -> (Daemon, Store) {
        let socket = socket.to_path_buf();
        let state_path = state_path.to_path_buf();
        let store = Store::load(&state_path).unwrap();
        let store_for_daemon = store.clone();
        let daemon_socket = socket.clone();
        let (jobs, rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let _ = run(&daemon_socket, store_for_daemon, jobs).await;
        });
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        (Daemon { handle, rx }, store)
    }

    async fn request(socket: &Path, req: &Request) -> Response {
        let stream = UnixStream::connect(socket).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        write_message(&mut write_half, req).await.unwrap();
        read_message::<_, Response>(&mut reader)
            .await
            .unwrap()
            .expect("daemon returned no response")
    }

    #[tokio::test]
    async fn ping_pong_enqueue_and_query() {
        let dir = TempDir::new().unwrap();
        let socket = dir.path().join("indexer.sock");
        let state = dir.path().join("state.json");
        let (daemon, _store) = spawn_daemon(&socket, &state).await;

        assert_eq!(request(&socket, &Request::Ping).await, Response::Pong);

        let enq = request(
            &socket,
            &Request::Enqueue {
                repo_root: "/repo".into(),
                sha: "deadbeef".into(),
            },
        )
        .await;
        assert_eq!(enq, Response::Ok);

        // With no worker running, a freshly enqueued commit stays Pending.
        let status = request(
            &socket,
            &Request::IndexStatus {
                repo_root: "/repo".into(),
                sha: None,
            },
        )
        .await;
        match status {
            Response::IndexStatus { sha, state, .. } => {
                assert_eq!(sha, "deadbeef");
                assert_eq!(state, IndexState::Pending);
            }
            other => panic!("unexpected {other:?}"),
        }

        let last = request(
            &socket,
            &Request::LastKnownState {
                repo_root: "/repo".into(),
            },
        )
        .await;
        match last {
            Response::LastKnownState { commit, .. } => {
                let c = commit.expect("expected commit info");
                assert_eq!(c.sha, "deadbeef");
                assert_eq!(c.state, IndexState::Pending);
            }
            other => panic!("unexpected {other:?}"),
        }

        let missing = request(
            &socket,
            &Request::IndexStatus {
                repo_root: "/other".into(),
                sha: None,
            },
        )
        .await;
        assert_eq!(
            missing,
            Response::NoCommit {
                repo_root: "/other".into()
            }
        );

        daemon.handle.abort();
    }
}
