# Phase 1 checklist — engine only, no TUI

Definition of done: every item below is a passing `cargo test` in
`git-tui-core`, run against scripted fixture repos (no real repo required).

## 0. Test fixtures (first thing to build)

`src/testutil.rs` (behind `#[cfg(test)]` or a `fixtures` feature):

- [ ] `fn init_repo() -> (TempDir, Repository)` — `git2::Repository::init` in a
      temp dir, default branch `main`, local user config set
- [ ] `fn commit_file(repo, path, contents, msg) -> Oid` — write file, stage,
      commit
- [ ] `fn dirty_file(repo, path, append_contents)` — modify without staging
- [ ] `fn new_branch(repo, name)`

## 1. `repo.rs`

- [ ] `Repo::discover(path)` walks up to find `.git`
- [ ] current branch name; detached HEAD case
- [ ] test: `discovers_repo_from_nested_subdir`
- [ ] test: `reports_detached_head`

## 2. `status.rs` — `RepoStatus` per architecture.md

- [ ] test: `lists_untracked_files`
- [ ] test: `lists_unstaged_modifications`
- [ ] test: `lists_staged_modifications`
- [ ] test: `file_modified_in_index_and_workdir_is_both_staged_and_unstaged`
- [ ] test: `unmerged_paths_reported_as_conflicted`
- [ ] test: `empty_repo_reports_zero_files_and_no_panic`

## 3. `diff.rs` — into owned `FileDiff`/`Hunk`/`DiffLine`

- [ ] unstaged diff for a path (workdir vs index)
- [ ] staged diff for a path (index vs HEAD)
- [ ] test: `diff_splits_multiple_hunks_correctly`
- [ ] test: `context_lines_before_and_after_change_are_preserved`
- [ ] test: `new_file_diff_is_all_adds`
- [ ] test: `no_word_level_highlighting_yet` (explicitly out of scope)

## 4. `stage.rs`

- [ ] stage whole file; unstage whole file
- [ ] stage a single hunk by index (via partial-index application — budget
      time here, this is the known-hard one)
- [ ] test: `stage_single_hunk_leaves_other_hunks_unstaged`
- [ ] test: `unstage_file_restores_index_to_head`
- [ ] test: `stage_out_of_range_hunk_index_errors`

## 5. `commit.rs`

- [ ] create commit from index; author/committer from repo config
- [ ] test: `commit_advances_head_and_empties_status`
- [ ] test: `commit_with_no_staged_changes_errors`

## 6. Phase 1 gate — the checkpoint test

- [ ] `end_to_end_init_stage_hunk_and_commit`: init repo, two commits of real
      content, dirty a file with two separate changed regions, stage exactly
      one hunk, commit, assert log shows the commit and `status` still shows
      the file as unstaged with only the remaining hunk in its diff.

**Only after that test is green: start Phase 2 (job queue).**
