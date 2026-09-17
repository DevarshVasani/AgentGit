//! Commit history (read-only).

use crate::error::GitError;

/// Owned one-line commit summary — sendable across the job channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Short id (7 hex chars).
    pub id: String,
    pub summary: String,
    pub author: String,
}

/// Newest-first history from HEAD, up to `limit` entries.
/// Empty repos yield an empty list (not an error).
pub fn log(repo: &git2::Repository, limit: usize) -> Result<Vec<CommitInfo>, GitError> {
    let mut walk = repo
        .revwalk()
        .map_err(|e| GitError::Log(format!("cannot walk history: {e}")))?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL)
        .map_err(|e| GitError::Log(format!("cannot sort history: {e}")))?;
    // Unborn HEAD: no history yet.
    if walk.push_head().is_err() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for oid in walk.take(limit.max(1)) {
        let oid = oid.map_err(|e| GitError::Log(format!("cannot read commit: {e}")))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|e| GitError::Log(format!("cannot read commit: {e}")))?;
        let id = format!("{oid:.7}");
        let summary = commit
            .message()
            .unwrap_or("(empty message)")
            .lines()
            .next()
            .unwrap_or("(empty message)")
            .trim()
            .to_string();
        let author = commit.author().name().unwrap_or("?").to_string();
        out.push(CommitInfo {
            id,
            summary,
            author,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn lists_commits_newest_first() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "first");
        testutil::commit_file(&repo, "a.txt", "a\nb\n", "second");
        testutil::commit_file(&repo, "a.txt", "a\nb\nc\n", "third");
        let entries = log(&repo, 50).unwrap();
        let summaries: Vec<&str> = entries.iter().map(|e| e.summary.as_str()).collect();
        assert_eq!(summaries, ["third", "second", "first"], "got {summaries:?}");
        assert_eq!(entries[0].id.len(), 7);
        assert_eq!(entries[0].author, "Test User");
    }

    #[test]
    fn empty_repo_yields_empty_log() {
        let (_dir, repo) = testutil::init_repo();
        assert!(log(&repo, 50).unwrap().is_empty());
    }

    #[test]
    fn limit_is_respected() {
        let (_dir, repo) = testutil::init_repo();
        for i in 0..5 {
            testutil::commit_file(&repo, "a.txt", &format!("v{i}\n"), &format!("c{i}"));
        }
        assert_eq!(log(&repo, 2).unwrap().len(), 2);
    }
}
