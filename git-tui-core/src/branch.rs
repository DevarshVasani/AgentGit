//! Local branch operations.

use crate::error::GitError;

/// Owned branch summary — sendable across the job channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    /// One-line tip commit summary.
    pub tip_summary: String,
}

/// Sorted local branches, HEAD flagged.
pub fn list_branches(repo: &git2::Repository) -> Result<Vec<BranchInfo>, GitError> {
    let mut out = Vec::new();
    let branches = repo
        .branches(Some(git2::BranchType::Local))
        .map_err(|e| GitError::Branch(format!("cannot list branches: {e}")))?;
    for item in branches {
        let (branch, _) = item.map_err(|e| GitError::Branch(format!("cannot read branch: {e}")))?;
        let name = branch
            .name()
            .map_err(|e| GitError::Branch(format!("cannot read branch name: {e}")))?
            .unwrap_or("(invalid)")
            .to_string();
        let is_head = branch.is_head();
        let tip_summary = branch
            .get()
            .peel_to_commit()
            .ok()
            .and_then(|c| {
                c.message()
                    .map(|m| m.lines().next().unwrap_or("").trim().to_string())
            })
            .unwrap_or_default();
        out.push(BranchInfo {
            name,
            is_head,
            tip_summary,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Create branch `name` at HEAD.
pub fn create_branch(repo: &git2::Repository, name: &str) -> Result<(), GitError> {
    if name.trim().is_empty() {
        return Err(GitError::Branch("branch name is empty".into()));
    }
    let head = repo
        .head()
        .map_err(|_| GitError::Branch("cannot create branch: no commits yet".into()))?
        .peel_to_commit()
        .map_err(|e| GitError::Branch(format!("cannot read HEAD: {e}")))?;
    repo.branch(name, &head, false)
        .map(|_| ())
        .map_err(|e| GitError::Branch(format!("cannot create branch {name}: {e}")))?;
    Ok(())
}

/// Check out branch `name` (safe checkout: errors on dirty overwrite).
pub fn checkout_branch(repo: &git2::Repository, name: &str) -> Result<(), GitError> {
    let branch = repo
        .find_branch(name, git2::BranchType::Local)
        .map_err(|_| GitError::Branch(format!("no such branch: {name}")))?;
    let refname = branch
        .get()
        .name()
        .ok_or_else(|| GitError::Branch(format!("invalid branch ref: {name}")))?
        .to_string();
    repo.set_head(&refname)
        .map_err(|e| GitError::Branch(format!("cannot checkout {name}: {e}")))?;
    repo.checkout_head(None)
        .map_err(|e| GitError::Branch(format!("cannot checkout {name}: {e}")))?;
    Ok(())
}

/// Delete a branch. Refuses the checked-out branch.
pub fn delete_branch(repo: &git2::Repository, name: &str) -> Result<(), GitError> {
    let mut branch = repo
        .find_branch(name, git2::BranchType::Local)
        .map_err(|_| GitError::Branch(format!("no such branch: {name}")))?;
    if branch.is_head() {
        return Err(GitError::Branch(format!(
            "cannot delete {name}: it is checked out"
        )));
    }
    branch
        .delete()
        .map_err(|e| GitError::Branch(format!("cannot delete {name}: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn two_branch_repo() -> (tempfile::TempDir, git2::Repository) {
        let (dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        testutil::new_branch(&repo, "side");
        testutil::commit_file(&repo, "a.txt", "a\nside\n", "side commit");
        (dir, repo)
    }

    #[test]
    fn lists_branches_with_head_flagged() {
        let (_dir, repo) = two_branch_repo();
        let branches = list_branches(&repo).unwrap();
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main"), "got {names:?}");
        assert!(names.contains(&"side"), "got {names:?}");
        // Currently on side (new_branch checks out).
        let head = branches.iter().find(|b| b.is_head).unwrap();
        assert_eq!(head.name, "side");
    }

    #[test]
    fn create_and_checkout_moves_head() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        create_branch(&repo, "feature").unwrap();
        assert!(list_branches(&repo)
            .unwrap()
            .iter()
            .any(|b| b.name == "feature"));
        checkout_branch(&repo, "feature").unwrap();
        let head = list_branches(&repo).unwrap();
        assert!(head.iter().find(|b| b.is_head).unwrap().name == "feature");
    }

    #[test]
    fn create_duplicate_branch_errors() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        create_branch(&repo, "dup").unwrap();
        assert!(create_branch(&repo, "dup").is_err());
    }

    #[test]
    fn delete_branch_removes_it() {
        let (_dir, repo) = two_branch_repo();
        // On side; delete main.
        delete_branch(&repo, "main").unwrap();
        let names: Vec<String> = list_branches(&repo)
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert!(!names.contains(&"main".to_string()), "got {names:?}");
    }

    #[test]
    fn delete_checked_out_branch_errors() {
        let (_dir, repo) = two_branch_repo();
        assert!(delete_branch(&repo, "side").is_err());
    }

    #[test]
    fn checkout_nonexistent_branch_errors() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        assert!(checkout_branch(&repo, "nope").is_err());
    }
}
