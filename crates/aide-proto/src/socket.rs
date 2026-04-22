use std::path::PathBuf;

use aide_core::AidePaths;

/// Filename of the indexer daemon's unix socket inside `AidePaths::sock()`.
pub const INDEXER_SOCKET_NAME: &str = "indexer.sock";

/// Resolve the default socket path for the indexer daemon: `~/.aide/sock/indexer.sock`.
pub fn default_indexer_socket(paths: &AidePaths) -> PathBuf {
    paths.sock().join(INDEXER_SOCKET_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_under_sock_dir() {
        let paths = AidePaths::at("/tmp/aide-test");
        assert_eq!(
            default_indexer_socket(&paths),
            PathBuf::from("/tmp/aide-test/sock/indexer.sock")
        );
    }
}
