//! Commit creation from the index.

use crate::error::GitError;

/// Create a commit from the current index. Returns the new commit id.
///
/// Errors with [`GitError::EmptyCommit`] when there is nothing staged,
/// and with [`GitError::HunkStaging`] when the message is empty.
pub fn commit(repo: &git2::Repository, message: &str) -> Result<git2::Oid, GitError> {
    if message.trim().is_empty() {
        return Err(GitError::HunkStaging("commit message is empty".into()));
    }
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    // Nothing staged? Compare index tree against HEAD tree.
    if let Ok(head) = repo.head() {
        if let Ok(head_commit) = head.peel_to_commit() {
            if let Ok(head_tree) = head_commit.tree() {
                if head_tree.id() == tree_id {
                    return Err(GitError::EmptyCommit);
                }
            }
            let sig = repo.signature()?;
            let parents = [head_commit];
            let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
            return Ok(repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)?);
        }
    } else {
        // No HEAD at all: index must be non-empty.
        if index.is_empty() {
            return Err(GitError::EmptyCommit);
        }
    }

    // Unborn HEAD with staged content: initial commit, no parents.
    if index.is_empty() {
        return Err(GitError::EmptyCommit);
    }
    let sig = repo.signature()?;
    Ok(repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn commit_advances_head_and_empties_status() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        testutil::dirty_file(&repo, "a.txt", "more\n");
        crate::stage::stage_file(&repo, "a.txt").unwrap();
        let oid = commit(&repo, "second").unwrap();
        assert!(!oid.is_zero());
        let st = crate::status::repo_status(&repo).unwrap();
        assert!(st.files.is_empty(), "status should be clean: {st:?}");
        assert_eq!(st.head_summary, "second");
    }

    #[test]
    fn commit_with_no_staged_changes_errors() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        let err = commit(&repo, "nothing").unwrap_err();
        assert!(matches!(err, GitError::EmptyCommit), "got {err:?}");
        // Unstaged changes alone must also error.
        testutil::dirty_file(&repo, "a.txt", "unstaged\n");
        let err = commit(&repo, "nothing").unwrap_err();
        assert!(matches!(err, GitError::EmptyCommit), "got {err:?}");
    }
}
