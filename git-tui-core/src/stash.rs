//! Stash operations (tracked changes only).

use crate::error::GitError;

/// Owned stash summary — sendable across the job channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    pub index: usize,
    pub message: String,
}

/// Newest-last list (index 0 is the most recent stash).
pub fn list_stash(repo: &mut git2::Repository) -> Result<Vec<StashEntry>, GitError> {
    let mut out = Vec::new();
    repo.stash_foreach(|index, message, _oid| {
        out.push(StashEntry {
            index,
            message: message.to_string(),
        });
        true
    })
    .map_err(|e| GitError::Stash(format!("cannot list stash: {e}")))?;
    Ok(out)
}

/// Stash tracked modifications with `message`.
pub fn stash_push(repo: &mut git2::Repository, message: &str) -> Result<(), GitError> {
    if message.trim().is_empty() {
        return Err(GitError::Stash("stash message is empty".into()));
    }
    let sig = repo
        .signature()
        .map_err(|e| GitError::Stash(format!("cannot read signature: {e}")))?;
    repo.stash_save(&sig, message, None)
        .map(|_| ())
        .map_err(|e| GitError::Stash(format!("nothing to stash?: {e}")))?;
    Ok(())
}

/// Apply stash `index` and drop it on success (like `git stash pop`).
pub fn stash_pop(repo: &mut git2::Repository, index: usize) -> Result<(), GitError> {
    repo.stash_pop(index, None)
        .map_err(|e| GitError::Stash(format!("cannot pop stash@{index}: {e}")))?;
    Ok(())
}

/// Drop stash `index` without applying.
pub fn stash_drop(repo: &mut git2::Repository, index: usize) -> Result<(), GitError> {
    repo.stash_drop(index)
        .map_err(|e| GitError::Stash(format!("cannot drop stash@{index}: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn dirty_repo() -> (tempfile::TempDir, git2::Repository) {
        let (dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        testutil::dirty_file(&repo, "a.txt", "work\n");
        (dir, repo)
    }

    #[test]
    fn push_and_list_roundtrip() {
        let (_dir, mut repo) = dirty_repo();
        stash_push(&mut repo, "wip").unwrap();
        let entries = list_stash(&mut repo).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].index, 0);
        assert!(
            entries[0].message.contains("wip"),
            "got {}",
            entries[0].message
        );
        // Workdir is clean after push.
        let st = crate::status::repo_status(&repo).unwrap();
        assert!(st.files.is_empty(), "got {st:?}");
    }

    #[test]
    fn push_with_nothing_to_stash_errors() {
        let (_dir, mut repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        assert!(stash_push(&mut repo, "empty").is_err());
    }

    #[test]
    fn pop_restores_changes_and_drops_entry() {
        let (_dir, mut repo) = dirty_repo();
        stash_push(&mut repo, "wip").unwrap();
        stash_pop(&mut repo, 0).unwrap();
        assert!(list_stash(&mut repo).unwrap().is_empty());
        let content = std::fs::read_to_string(repo.workdir().unwrap().join("a.txt")).unwrap();
        assert!(content.contains("work"), "got {content:?}");
    }

    #[test]
    fn drop_removes_entry_without_restoring() {
        let (_dir, mut repo) = dirty_repo();
        stash_push(&mut repo, "wip").unwrap();
        stash_drop(&mut repo, 0).unwrap();
        assert!(list_stash(&mut repo).unwrap().is_empty());
        let content = std::fs::read_to_string(repo.workdir().unwrap().join("a.txt")).unwrap();
        assert!(!content.contains("work"), "got {content:?}");
    }

    #[test]
    fn pop_out_of_range_errors() {
        let (_dir, mut repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        assert!(stash_pop(&mut repo, 0).is_err());
        assert!(stash_drop(&mut repo, 3).is_err());
    }
}
