//! Owned diff model per `docs/architecture.md`.

use crate::error::GitError;
use std::cell::RefCell;
use std::path::Path;

/// All diff types are owned — sendable across the job channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<Hunk>,
    /// Binary content: `hunks` is empty and the UI shows a placeholder
    /// instead of the raw bytes (which would corrupt the terminal).
    pub binary: bool,
}

/// Git's heuristic: a NUL byte in the first 8000 bytes means binary.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// Placeholder diff for a binary file: no hunks, `binary` set.
fn binary_diff(path: &str) -> FileDiff {
    FileDiff {
        path: path.to_string(),
        hunks: Vec::new(),
        binary: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// e.g. `"@@ -10,4 +10,6 @@ fn foo"`.
    pub header: String,
    /// 1-based number of the first old-file line in this hunk (0 when the
    /// hunk adds to an empty old side, e.g. a new file).
    pub old_start: u32,
    /// 1-based number of the first new-file line in this hunk.
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    HunkHeader,
}

/// Whole file content as a single all-context hunk, for viewing files
/// with no changes (clean files have empty staged/unstaged diffs).
/// Reads the workdir file; a clean file is identical to HEAD by definition.
/// Binary files come back as [`binary_diff`]; other non-UTF-8 bytes are
/// shown lossily.
pub fn whole_file_diff(repo: &git2::Repository, path: &str) -> Result<FileDiff, GitError> {
    let full = repo
        .workdir()
        .ok_or_else(|| GitError::HunkStaging("bare repo".into()))?
        .join(Path::new(path));
    let bytes = std::fs::read(&full)
        .map_err(|e| GitError::HunkStaging(format!("cannot read {path}: {e}")))?;
    if is_binary(&bytes) {
        return Ok(binary_diff(path));
    }
    let contents = String::from_utf8_lossy(&bytes);
    let lines = contents
        .lines()
        .map(|l| DiffLine {
            kind: LineKind::Context,
            text: l.to_string(),
        })
        .collect::<Vec<_>>();
    let count = lines.len();
    Ok(FileDiff {
        path: path.to_string(),
        hunks: vec![Hunk {
            header: format!("@@ -1,{count} +1,{count} @@"),
            old_start: 1,
            new_start: 1,
            lines,
        }],
        binary: false,
    })
}

/// Full new-version text for Markdown preview: workdir file when
/// `staged` is false, index blob when true (staged view). Lossy UTF-8
/// so binary renders instead of erroring.
pub fn new_content(repo: &git2::Repository, path: &str, staged: bool) -> Result<String, GitError> {
    if !staged {
        let full = repo
            .workdir()
            .ok_or_else(|| GitError::HunkStaging("bare repo".into()))?
            .join(Path::new(path));
        let bytes = std::fs::read(&full)
            .map_err(|e| GitError::HunkStaging(format!("cannot read {path}: {e}")))?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let index = repo.index()?;
    if let Some(entry) = index.get_path(Path::new(path), 0) {
        let blob = repo.find_blob(entry.id)?;
        return Ok(String::from_utf8_lossy(blob.content()).into_owned());
    }
    // Staged deletion or missing index entry: no new content.
    Ok(String::new())
}

/// Workdir vs index for `path`.
pub fn unstaged_diff(repo: &git2::Repository, path: &str) -> Result<FileDiff, GitError> {
    // Untracked files are not in any diff by default: synthesize all-adds.
    if is_untracked(repo, path)? {
        return untracked_file_diff(repo, path);
    }
    let mut opts = git2::DiffOptions::new();
    opts.pathspec(path).context_lines(3);
    let index = repo.index()?;
    let diff = repo.diff_index_to_workdir(Some(&index), Some(&mut opts))?;
    from_diff(&diff, path)
}

/// Index vs HEAD for `path`.
pub fn staged_diff(repo: &git2::Repository, path: &str) -> Result<FileDiff, GitError> {
    let mut opts = git2::DiffOptions::new();
    opts.pathspec(path).context_lines(3);
    let index = repo.index()?;
    let diff = match repo.head().ok().and_then(|h| h.peel_to_tree().ok()) {
        Some(tree) => repo.diff_tree_to_index(Some(&tree), Some(&index), Some(&mut opts))?,
        None => repo.diff_tree_to_index(None, Some(&index), Some(&mut opts))?,
    };
    from_diff(&diff, path)
}

fn is_untracked(repo: &git2::Repository, path: &str) -> Result<bool, GitError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .include_ignored(false);
    opts.pathspec(path);
    let statuses = repo.statuses(Some(&mut opts))?;
    for entry in statuses.iter() {
        if entry.path() == Some(path) && entry.status().contains(git2::Status::WT_NEW) {
            // WT_NEW alone (no index bits) means untracked.
            let index_bits = git2::Status::INDEX_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED;
            if !entry.status().intersects(index_bits) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn untracked_file_diff(repo: &git2::Repository, path: &str) -> Result<FileDiff, GitError> {
    let full = repo
        .workdir()
        .ok_or_else(|| GitError::HunkStaging("bare repo".into()))?
        .join(Path::new(path));
    let bytes = std::fs::read(&full)
        .map_err(|e| GitError::HunkStaging(format!("cannot read untracked file {path}: {e}")))?;
    if is_binary(&bytes) {
        return Ok(binary_diff(path));
    }
    let contents = String::from_utf8_lossy(&bytes);
    let lines = contents
        .lines()
        .map(|l| DiffLine {
            kind: LineKind::Add,
            text: l.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(FileDiff {
        path: path.to_string(),
        hunks: vec![Hunk {
            header: format!("@@ -0,0 +1,{} @@", lines.len()),
            old_start: 0,
            new_start: 1,
            lines,
        }],
        binary: false,
    })
}

fn from_diff(diff: &git2::Diff, path: &str) -> Result<FileDiff, GitError> {
    let hunks: RefCell<Vec<Hunk>> = RefCell::new(Vec::new());
    diff.foreach(
        &mut |_delta, _progress| true,
        None,
        Some(&mut |_delta, _hunk| {
            // hunk header bytes like "@@ -1,3 +1,4 @@ ..."
            true
        }),
        None,
    )?;
    // Real collection pass: hunk_cb gives us the header bytes.
    let err: RefCell<Option<git2::Error>> = RefCell::new(None);
    let binary = std::cell::Cell::new(false);
    let res = diff.foreach(
        &mut |delta, _| {
            if delta.flags().is_binary() {
                binary.set(true);
            }
            true
        },
        Some(&mut |_, _| {
            binary.set(true);
            true
        }),
        Some(&mut |_, hunk| {
            let header = String::from_utf8_lossy(hunk.header()).into_owned();
            hunks.borrow_mut().push(Hunk {
                header,
                old_start: hunk.old_start(),
                new_start: hunk.new_start(),
                lines: Vec::new(),
            });
            true
        }),
        Some(&mut |_, _hunk, line| {
            let origin = line.origin();
            let kind = match origin {
                ' ' => Some(LineKind::Context),
                '+' => Some(LineKind::Add),
                '-' => Some(LineKind::Del),
                // Skip file headers ('F'), hunk headers ('H'), binary ('B'),
                // additions-eof etc. Header text is already in Hunk::header.
                _ => None,
            };
            if let Some(kind) = kind {
                let mut text = String::from_utf8_lossy(line.content()).into_owned();
                // libgit2 content may include trailing newline; strip it for TUI.
                while text.ends_with('\n') || text.ends_with('\r') {
                    text.pop();
                }
                if let Some(last) = hunks.borrow_mut().last_mut() {
                    last.lines.push(DiffLine { kind, text });
                } else {
                    // Line outside any hunk — should not happen; ignore.
                }
            }
            let _ = &err;
            true
        }),
    );
    res?;
    if binary.get() {
        return Ok(binary_diff(path));
    }
    Ok(FileDiff {
        path: path.to_string(),
        hunks: hunks.into_inner(),
        binary: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use std::fs;

    fn two_region_contents() -> (String, String) {
        let base = (1..=40).map(|i| format!("line {i}\n")).collect::<String>();
        let mut dirty = base.clone();
        // Two changes far apart so they land in separate hunks.
        dirty = dirty.replacen("line 5\n", "line 5 CHANGED\n", 1);
        dirty = dirty.replacen("line 35\n", "line 35 CHANGED\n", 1);
        (base, dirty)
    }

    #[test]
    fn diff_splits_multiple_hunks_correctly() {
        let (_dir, repo) = testutil::init_repo();
        let (base, dirty) = two_region_contents();
        testutil::commit_file(&repo, "a.txt", &base, "init");
        fs::write(repo.workdir().unwrap().join("a.txt"), &dirty).unwrap();
        let d = unstaged_diff(&repo, "a.txt").unwrap();
        assert!(
            d.hunks.len() >= 2,
            "expected >=2 hunks, got {}: {:?}",
            d.hunks.len(),
            d.hunks.iter().map(|h| &h.header).collect::<Vec<_>>()
        );
    }

    #[test]
    fn context_lines_before_and_after_change_are_preserved() {
        let (_dir, repo) = testutil::init_repo();
        let (base, _) = two_region_contents();
        testutil::commit_file(&repo, "a.txt", &base, "init");
        let mut dirty = base.clone();
        dirty = dirty.replacen("line 20\n", "line 20 CHANGED\n", 1);
        fs::write(repo.workdir().unwrap().join("a.txt"), &dirty).unwrap();
        let d = unstaged_diff(&repo, "a.txt").unwrap();
        assert_eq!(d.hunks.len(), 1);
        let hunk = &d.hunks[0];
        // Context lines around the change must be present.
        let texts: Vec<&str> = hunk.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(
            texts.contains(&"line 19"),
            "missing context before: {texts:?}"
        );
        assert!(
            texts.contains(&"line 21"),
            "missing context after: {texts:?}"
        );
        // Kinds: context vs del/add.
        let changed_del = hunk
            .lines
            .iter()
            .find(|l| l.text == "line 20" && l.kind == LineKind::Del);
        let changed_add = hunk
            .lines
            .iter()
            .find(|l| l.text == "line 20 CHANGED" && l.kind == LineKind::Add);
        assert!(changed_del.is_some(), "missing Del line: {hunk:?}");
        assert!(changed_add.is_some(), "missing Add line: {hunk:?}");
    }

    #[test]
    fn new_file_diff_is_all_adds() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        fs::write(repo.workdir().unwrap().join("new.txt"), "one\ntwo\n").unwrap();
        let d = unstaged_diff(&repo, "new.txt").unwrap();
        assert!(!d.hunks.is_empty());
        for h in &d.hunks {
            for l in &h.lines {
                assert_eq!(l.kind, LineKind::Add, "expected all adds: {d:?}");
            }
        }
    }

    #[test]
    fn no_word_level_highlighting_yet() {
        // Explicitly out of scope: lines are whole, no intra-line spans.
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "hello world\n", "init");
        fs::write(repo.workdir().unwrap().join("a.txt"), "hello WORLD\n").unwrap();
        let d = unstaged_diff(&repo, "a.txt").unwrap();
        // Only line-level kinds exist; there is no field for word spans.
        for h in &d.hunks {
            for l in &h.lines {
                match l.kind {
                    LineKind::Context | LineKind::Add | LineKind::Del | LineKind::HunkHeader => {}
                }
                // Whole-line text, not split into spans.
                assert!(!l.text.is_empty() || l.kind == LineKind::Context);
            }
        }
        // The model has no word-span type at all: assert by construction —
        // DiffLine only carries kind + text.
    }

    #[test]
    fn staged_diff_shows_index_vs_head() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        testutil::dirty_file(&repo, "a.txt", "staged\n");
        // Before staging: staged diff empty, unstaged non-empty.
        assert!(staged_diff(&repo, "a.txt").unwrap().hunks.is_empty());
        assert!(!unstaged_diff(&repo, "a.txt").unwrap().hunks.is_empty());
        // After staging: staged non-empty, unstaged empty.
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        assert!(!staged_diff(&repo, "a.txt").unwrap().hunks.is_empty());
        assert!(unstaged_diff(&repo, "a.txt").unwrap().hunks.is_empty());
    }

    #[test]
    fn hunks_carry_old_and_new_start_lines() {
        let (_dir, repo) = testutil::init_repo();
        let (base, _) = two_region_contents();
        testutil::commit_file(&repo, "a.txt", &base, "init");
        let mut dirty = base.clone();
        dirty = dirty.replacen("line 20\n", "line 20 CHANGED\n", 1);
        fs::write(repo.workdir().unwrap().join("a.txt"), &dirty).unwrap();
        let d = unstaged_diff(&repo, "a.txt").unwrap();
        assert_eq!(d.hunks.len(), 1);
        // 3 context lines before the change at line 20.
        assert_eq!(d.hunks[0].old_start, 17);
        assert_eq!(d.hunks[0].new_start, 17);
    }

    #[test]
    fn new_file_hunk_starts_at_zero_old_one_new() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        fs::write(repo.workdir().unwrap().join("new.txt"), "one\ntwo\n").unwrap();
        let d = unstaged_diff(&repo, "new.txt").unwrap();
        assert_eq!(d.hunks[0].old_start, 0);
        assert_eq!(d.hunks[0].new_start, 1);
    }

    #[test]
    fn whole_file_diff_lists_every_line_as_context() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "one\ntwo\nthree\n", "init");
        // No changes: both real diffs are empty…
        assert!(unstaged_diff(&repo, "a.txt").unwrap().hunks.is_empty());
        // …but the whole-file view shows every line as context.
        let d = whole_file_diff(&repo, "a.txt").unwrap();
        assert_eq!(d.path, "a.txt");
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].old_start, 1);
        assert_eq!(d.hunks[0].new_start, 1);
        let texts: Vec<&str> = d.hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
        assert!(d.hunks[0].lines.iter().all(|l| l.kind == LineKind::Context));
    }

    #[test]
    fn whole_file_diff_of_missing_path_errors() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        assert!(whole_file_diff(&repo, "gone.txt").is_err());
    }

    #[test]
    fn whole_file_diff_is_lossy_for_non_utf8_bytes() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        fs::write(
            repo.workdir().unwrap().join("bin.dat"),
            [0xff, 0xfe, b'a', b'\n'],
        )
        .unwrap();
        let d = whole_file_diff(&repo, "bin.dat").unwrap();
        assert_eq!(d.hunks.len(), 1);
        assert!(d.hunks[0].lines.iter().all(|l| l.kind == LineKind::Context));
        assert_eq!(d.hunks[0].lines.len(), 1);
        assert!(d.hunks[0].lines[0].text.contains('a'));
    }

    /// PNG-like bytes: header, a NUL, and an ESC that would drive the
    /// terminal if it were ever printed.
    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\x1b[2J";

    #[test]
    fn untracked_binary_file_is_flagged_not_rendered() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        fs::write(repo.workdir().unwrap().join("img.png"), PNG_BYTES).unwrap();
        let d = unstaged_diff(&repo, "img.png").unwrap();
        assert!(d.binary);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn staged_and_modified_binary_files_are_flagged() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        let path = repo.workdir().unwrap().join("img.png");
        fs::write(&path, PNG_BYTES).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("img.png")).unwrap();
        index.write().unwrap();
        let staged = staged_diff(&repo, "img.png").unwrap();
        assert!(staged.binary, "staged new binary must be flagged");
        assert!(staged.hunks.is_empty());
        // Change it again in the workdir: index-vs-workdir is binary too.
        fs::write(&path, [PNG_BYTES, b"more\0"].concat()).unwrap();
        let unstaged = unstaged_diff(&repo, "img.png").unwrap();
        assert!(unstaged.binary, "modified binary must be flagged");
        assert!(unstaged.hunks.is_empty());
    }

    #[test]
    fn whole_file_view_of_binary_is_flagged() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        fs::write(repo.workdir().unwrap().join("img.png"), PNG_BYTES).unwrap();
        let d = whole_file_diff(&repo, "img.png").unwrap();
        assert!(d.binary);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn text_diffs_are_not_flagged_binary() {
        let (_dir, repo) = testutil::init_repo();
        testutil::commit_file(&repo, "a.txt", "a\n", "init");
        testutil::dirty_file(&repo, "a.txt", "b\n");
        assert!(!unstaged_diff(&repo, "a.txt").unwrap().binary);
        assert!(!whole_file_diff(&repo, "a.txt").unwrap().binary);
    }
}
