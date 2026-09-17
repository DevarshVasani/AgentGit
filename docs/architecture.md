# Architecture

The contract between `git-tui-core` and `git-tui`. Written before
implementation; change this document before changing the design.

## Boundary

- `core` owns all git state. The TUI never holds a `git2::*` type.
- All `core` data crossing the async boundary is **owned** (Strings, vecs) —
  no lifetimes from `git2::Diff` leak out. Slightly more copying, none of the
  lifetime fights, and it's the only thing that survives being sent across a
  channel cleanly.
- Errors: `thiserror` typed `GitError` in core; `anyhow` in the TUI.

## Core data model

```rust
/// The home-screen model. Everything below must be derivable from
/// Repository::statuses in one pass.
pub struct RepoStatus {
    pub branch: String,          // short name, or "(detached HEAD)"
    pub head_summary: String,    // one-line HEAD commit summary
    pub files: Vec<StatusEntry>,
}

pub struct StatusEntry {
    pub path: String,            // repo-relative, forward slashes
    pub state: FileState,
}

pub enum FileState {
    Staged,                      // in index, differs from HEAD
    Unstaged,                    // workdir differs from index
    Untracked,
    Conflicted,                  // never forget this one
    BothStagedAndUnstaged,       // partially staged file
}

/// All diff types are owned — sendable across the job channel.
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<Hunk>,
}

pub struct Hunk {
    pub header: String,          // "@@ -10,4 +10,6 @@ fn foo"
    pub old_start: u32,          // 1-based first old-file line (0 = empty side)
    pub new_start: u32,          // 1-based first new-file line
    pub lines: Vec<DiffLine>,
}

pub struct DiffLine {
    pub kind: LineKind,          // Context | Add | Del | HunkHeader
    pub text: String,            // without the leading + - space char
}
```

## Job protocol (Phase 2, but the contract is fixed now)

```rust
pub enum AsyncJob {
    RefreshStatus,
    LoadDiff { path: String, staged: bool },
    StageHunk { path: String, hunk_index: usize },
    StageFile { path: String },
    UnstageFile { path: String },
    Commit { message: String },
}

pub enum AsyncResult {
    Status(RepoStatus),
    Diff(FileDiff),
    MutationDone,                // staging/commit succeeded; triggers RefreshStatus
    Error(GitError),
}
```

## Rules that are cheap now and expensive later

1. **Single writer.** Only the job thread mutates the repo (index, commits).
   The TUI never calls mutating ops directly.
2. **Mutation invalidation.** Any `MutationDone` makes snapshot-dependent state
   (current diff view, file list) stale. The TUI must treat displayed diffs as
   read-only history after any completed mutation: it refreshes status and the
   focused diff rather than trying to patch them incrementally.
3. **RefreshStatus is cheap and always allowed to re-run.** Never block the
   render loop on it.
4. Performance on very large repos is a Phase 5 concern. Phase 1 targets
   correctness on small scripted fixtures only.
