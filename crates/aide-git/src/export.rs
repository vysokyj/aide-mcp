//! Export a commit's tree into a destination directory.
//!
//! This is the "give me the working tree at SHA X, without disturbing the
//! real repo" primitive. aide-indexer uses it to feed a clean snapshot to
//! a SCIP indexer — see the architectural invariant "SCIP lives on
//! commits": we must never index a dirty working tree.

use std::path::Path;

use git2::build::CheckoutBuilder;

use crate::{open_repo, GitError};

/// Materialise the tree of commit `sha` from `repo_root` into `target`.
///
/// `target` is created if it does not exist. Existing files inside
/// `target` are overwritten (the caller owns the dir, typically a
/// `TempDir`). The source repository's workdir and index are left
/// untouched.
pub fn export_commit(repo_root: &Path, sha: &str, target: &Path) -> Result<(), GitError> {
    let repo = open_repo(repo_root)?;
    let oid = git2::Oid::from_str(sha)?;
    let tree = repo.find_commit(oid)?.tree()?;

    std::fs::create_dir_all(target)?;

    let mut opts = CheckoutBuilder::new();
    opts.target_dir(target);
    opts.force();
    opts.update_index(false);
    opts.disable_filters(true);
    repo.checkout_tree(tree.as_object(), Some(&mut opts))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn init_repo_with_file(name: &str, content: &str) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        let tree_id = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        (dir, oid.to_string())
    }

    #[test]
    fn exports_tree_at_commit() {
        let (repo_dir, sha) = init_repo_with_file("README.md", "hello from v1\n");
        let out = TempDir::new().unwrap();

        export_commit(repo_dir.path(), &sha, out.path()).unwrap();

        let content = fs::read_to_string(out.path().join("README.md")).unwrap();
        assert_eq!(content, "hello from v1\n");
    }

    #[test]
    fn dirty_workdir_does_not_leak_into_export() {
        let (repo_dir, sha) = init_repo_with_file("README.md", "committed\n");
        // Dirty the working tree AFTER the commit — the export must still
        // reflect the committed version.
        fs::write(repo_dir.path().join("README.md"), "uncommitted edit\n").unwrap();

        let out = TempDir::new().unwrap();
        export_commit(repo_dir.path(), &sha, out.path()).unwrap();
        let exported = fs::read_to_string(out.path().join("README.md")).unwrap();
        assert_eq!(exported, "committed\n");

        // Source workdir is unchanged.
        let src = fs::read_to_string(repo_dir.path().join("README.md")).unwrap();
        assert_eq!(src, "uncommitted edit\n");
    }

    #[test]
    fn unknown_sha_errors() {
        let (repo_dir, _) = init_repo_with_file("x.txt", "x");
        let out = TempDir::new().unwrap();
        let err = export_commit(
            repo_dir.path(),
            "0000000000000000000000000000000000000000",
            out.path(),
        )
        .unwrap_err();
        assert!(matches!(err, GitError::Git2(_)));
    }
}
