//! `Repo`: owning wrapper around `git2::Repository`.
//!
//! The TUI holds `Repo`, never a `git2::*` type directly.

use crate::diff::{self, FileDiff};
use crate::error::GitError;
use crate::status::{self, RepoStatus};
use std::path::Path;

/// Owning handle to a git repository.
pub struct Repo {
    inner: git2::Repository,
}

impl Repo {
    /// Open a repository at exactly `path` (must contain `.git` or be one).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GitError> {
        let inner = git2::Repository::open(path.as_ref())
            .map_err(|_| GitError::NotARepo(path.as_ref().display().to_string()))?;
        Ok(Self { inner })
    }

    /// Walk upward from `path` to find `.git`.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self, GitError> {
        let start = path.as_ref().to_path_buf();
        let inner = git2::Repository::discover(&start)
            .map_err(|_| GitError::NotARepo(start.display().to_string()))?;
        Ok(Self { inner })
    }

    /// Borrow the underlying `git2::Repository` (crate-internal).
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &git2::Repository {
        &self.inner
    }

    /// Construct from an already-open `git2::Repository` (tests / job thread).
    pub fn from_inner(inner: git2::Repository) -> Self {
        Self { inner }
    }

    /// Workdir root, if this is not a bare repo. Lets the TUI discover the
    /// root and hand it to the job thread without touching `git2` types.
    pub fn workdir(&self) -> Option<std::path::PathBuf> {
        self.inner.workdir().map(|p| p.to_path_buf())
    }

    /// Current branch short name, or `"(detached HEAD)"`.
    pub fn branch(&self) -> Result<String, GitError> {
        branch_name(&self.inner)
    }

    /// One-line HEAD commit summary, or a placeholder when unborn.
    pub fn head_summary(&self) -> Result<String, GitError> {
        head_summary(&self.inner)
    }

    pub fn status(&self) -> Result<RepoStatus, GitError> {
        status::repo_status(&self.inner)
    }

    pub fn unstaged_diff(&self, path: &str) -> Result<FileDiff, GitError> {
        diff::unstaged_diff(&self.inner, path)
    }

    pub fn staged_diff(&self, path: &str) -> Result<FileDiff, GitError> {
        diff::staged_diff(&self.inner, path)
    }

    /// Whole file content for viewing files with no changes.
    pub fn whole_file(&self, path: &str) -> Result<FileDiff, GitError> {
        diff::whole_file_diff(&self.inner, path)
    }

    pub fn stage_file(&self, path: &str) -> Result<(), GitError> {
        crate::stage::stage_file(&self.inner, path)
    }

    pub fn unstage_file(&self, path: &str) -> Result<(), GitError> {
        crate::stage::unstage_file(&self.inner, path)
    }

    pub fn stage_hunk(&self, path: &str, hunk_index: usize) -> Result<(), GitError> {
        crate::stage::stage_hunk(&self.inner, path, hunk_index)
    }

    pub fn commit(&self, message: &str) -> Result<git2::Oid, GitError> {
        crate::commit::commit(&self.inner, message)
    }

    pub fn list_branches(&self) -> Result<Vec<crate::branch::BranchInfo>, GitError> {
        crate::branch::list_branches(&self.inner)
    }

    pub fn create_branch(&self, name: &str) -> Result<(), GitError> {
        crate::branch::create_branch(&self.inner, name)
    }

    pub fn checkout_branch(&self, name: &str) -> Result<(), GitError> {
        crate::branch::checkout_branch(&self.inner, name)
    }

    pub fn delete_branch(&self, name: &str) -> Result<(), GitError> {
        crate::branch::delete_branch(&self.inner, name)
    }

    pub fn log(&self, limit: usize) -> Result<Vec<crate::log::CommitInfo>, GitError> {
        crate::log::log(&self.inner, limit)
    }

    pub fn list_stash(&mut self) -> Result<Vec<crate::stash::StashEntry>, GitError> {
        crate::stash::list_stash(&mut self.inner)
    }

    pub fn stash_push(&mut self, message: &str) -> Result<(), GitError> {
        crate::stash::stash_push(&mut self.inner, message)
    }

    pub fn stash_pop(&mut self, index: usize) -> Result<(), GitError> {
        crate::stash::stash_pop(&mut self.inner, index)
    }

    pub fn stash_drop(&mut self, index: usize) -> Result<(), GitError> {
        crate::stash::stash_drop(&mut self.inner, index)
    }
}

/// Free function so tests using raw `git2::Repository` don't need the wrapper.
pub fn branch_name(repo: &git2::Repository) -> Result<String, GitError> {
    if repo.head_detached().unwrap_or(false) {
        return Ok("(detached HEAD)".to_string());
    }
    match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                Ok(head.shorthand().unwrap_or("HEAD").to_string())
            } else if repo.head_detached().unwrap_or(false) {
                Ok("(detached HEAD)".to_string())
            } else {
                // Unborn or otherwise non-branch HEAD: read symbolic target.
                unborn_branch_name(repo)
            }
        }
        Err(_) => unborn_branch_name(repo),
    }
}

fn unborn_branch_name(repo: &git2::Repository) -> Result<String, GitError> {
    // HEAD exists as a symbolic ref like refs/heads/main even with no commits.
    if let Ok(head_ref) = repo.find_reference("HEAD") {
        if let Some(target) = head_ref.symbolic_target() {
            if let Some(name) = target.strip_prefix("refs/heads/") {
                return Ok(name.to_string());
            }
        }
    }
    Ok("(detached HEAD)".to_string())
}

/// Free function version of [`Repo::head_summary`].
pub fn head_summary(repo: &git2::Repository) -> Result<String, GitError> {
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return Ok("(no commits yet)".to_string()),
    };
    let commit = match head.peel_to_commit() {
        Ok(c) => c,
        Err(_) => return Ok("(no commits yet)".to_string()),
    };
    let msg = commit
        .message()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    if msg.is_empty() {
        Ok("(empty message)".to_string())
    } else {
        Ok(msg.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn discovers_repo_from_nested_subdir() {
        let (dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "hello\n", "init");
        let nested = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let found = Repo::discover(&nested).expect("discover from nested subdir");
        // Same repo: workdir should match.
        assert_eq!(found.inner.workdir().unwrap(), repo.workdir().unwrap());
    }

    #[test]
    fn reports_detached_head() {
        let (_dir, repo) = testutil::init_repo();
        let oid = testutil::commit_file(&repo, "a.txt", "hello\n", "init");
        repo.set_head_detached(oid).unwrap();
        {
            let obj = repo.find_object(oid, None).unwrap();
            repo.checkout_tree(&obj, None).unwrap();
        }
        let r = Repo::from_inner(repo);
        assert_eq!(r.branch().unwrap(), "(detached HEAD)");
    }

    #[test]
    fn reports_branch_name_on_main() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "x\n", "init");
        let r = Repo::from_inner(repo);
        assert_eq!(r.branch().unwrap(), "main");
    }

    #[test]
    fn unborn_repo_does_not_panic() {
        let (_dir, repo) = testutil::init_repo();
        let r = Repo::from_inner(repo);
        assert_eq!(r.branch().unwrap(), "main");
        assert_eq!(r.head_summary().unwrap(), "(no commits yet)");
    }

    #[test]
    fn workdir_returns_repo_root_for_tui_bootstrap() {
        let (dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "x\n", "init");
        let r = Repo::from_inner(repo);
        assert_eq!(r.workdir().unwrap(), dir.path());
    }
}
