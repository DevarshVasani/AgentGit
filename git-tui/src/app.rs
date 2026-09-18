//! Phase 3: status panel state machine.
//!
//! Pure logic: selection, stage toggle, commit modal. Rendering lives in
//! `ui.rs`; the event loop lives in `main.rs`.

use crossterm::event::KeyCode;
use git_tui_core::branch::BranchInfo;
use git_tui_core::diff::FileDiff;
use git_tui_core::error::GitError;
use git_tui_core::jobqueue::{AsyncJob, AsyncResult, JobQueue};
use git_tui_core::llm::LlmConfig;
use git_tui_core::log::CommitInfo;
use git_tui_core::stash::StashEntry;
use git_tui_core::status::{FileState, RepoStatus, StatusEntry};
use git_tui_core::sync::SyncStatus;
use std::cell::Cell;

use crate::config::{Config, KeyBindings, Theme};
use crate::fuzzy;

/// Labels for the LLM setup form rows: provider, model, API key, base URL.
pub const LLM_FIELD_LABELS: [&str; 4] = [
    "Provider (openai|openrouter|ollama|anthropic|gemini|custom)",
    "Model",
    "API key (empty = use env var)",
    "Base URL (optional, custom only)",
];

/// Input mode: normal list navigation, a fullscreen diff overlay, or a
/// text-input modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    FullDiff,
    Committing,
    NewBranch,
    StashPush,
    FindFile,
    OpenProject,
    ConfirmInit,
    /// Push with no upstream yet: the draft names the remote to push `-u` to.
    SetUpstream,
    /// Publish a repo with no remotes: the draft is the new `origin` URL.
    SetRemote,
    /// In-TUI LLM provider setup (`A` in the file list): provider, model,
    /// API key, and optional base URL. Enter saves to the config file.
    LlmSettings,
}

/// Which panel receives navigation keys. Everything is vertical: Tab cycles
/// the rail panels plus the right-side diff preview; Enter opens the
/// selected file fullscreen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Status,
    Branches,
    Log,
    Stash,
    Diff,
}

/// UI state. Owns the [`JobQueue`]; git state itself lives on the worker
/// thread and is mirrored here as snapshots (status, diff, branches).
pub struct App {
    queue: JobQueue,
    keys: KeyBindings,
    theme: Theme,
    /// LLM provider config for Shift+A commit generation.
    llm: LlmConfig,
    /// A GenerateCommitMessage job is in flight ("generating…" in the
    /// commit modal). Set on Shift+A, cleared when the message or an
    /// error arrives via `poll`.
    generating: bool,
    /// One-line success confirmation (e.g. "LLM settings saved"), shown
    /// above the footer until the next keypress. Like `error` but green.
    notice: Option<String>,
    /// Where the config file lives (for the LLM setup form to persist).
    /// `None` in tests: saving still updates the session, minus the file.
    config_path: Option<std::path::PathBuf>,
    /// LLM setup form buffers: provider, model, api_key, base_url.
    llm_form: [String; 4],
    /// Saved cursor (chars) per form field while switching rows.
    llm_cursors: [usize; 4],
    /// Highlighted form row.
    llm_selected: usize,
    /// Short repo name for the status panel (workdir basename).
    repo_name: String,
    status: Option<RepoStatus>,
    /// Browsable tree: changed files in status order, then clean tracked
    /// files alphabetically. Selection indexes into this, not `status.files`.
    file_list: Vec<StatusEntry>,
    selected: usize,
    mode: Mode,
    focus: Focus,
    draft: String,
    /// Cursor inside `draft` as a char index (commit/branch/stash modals,
    /// the finder query, the jump-to-path line). Byte math is derived on
    /// demand so direct `draft` writes elsewhere can't break it: readers
    /// clamp to the current char count.
    draft_cursor: usize,
    /// Cursor inside the fuzzy file finder (`Mode::FindFile`).
    finder_selected: usize,
    /// Where the finder returns on Enter/Esc: the mode it was opened
    /// from (`Normal` or `FullDiff`), so `/` works fullscreen too.
    finder_return: Mode,
    /// Where the push/publish modals return on Enter/Esc (`Normal` or
    /// `FullDiff`), so `P` works fullscreen too.
    sync_return: Mode,
    error: Option<String>,
    quit: bool,
    diff: Option<FileDiff>,
    /// (path, staged) the loaded/loading diff belongs to.
    diff_for: Option<(String, bool)>,
    /// The loaded diff is a whole-file view (clean file), not a real diff.
    diff_whole_file: bool,
    /// The whole-file view came from the empty-diff fallback (a listed file
    /// whose diff has no hunks, e.g. a mode-only change). Reset whenever the
    /// selected path changes.
    fallback_whole_file: bool,
    hunk: usize,
    diff_scroll: u16,
    branches: Option<Vec<BranchInfo>>,
    branch_selected: usize,
    log: Option<Vec<CommitInfo>>,
    log_scroll: u16,
    stash: Option<Vec<StashEntry>>,
    stash_selected: usize,
    /// Upstream tracking state (upstream ref, ahead/behind, remotes).
    /// Loaded at startup and after every mutation; drives `P` behavior.
    sync: Option<SyncStatus>,
    /// A push/pull/publish job is in flight ("pushing…", …). Shown in the
    /// status panel until it succeeds or fails.
    syncing: Option<String>,
    // List scroll offsets for the left-rail panels. `Cell` so the renderer
    // (which only gets `&App`) can follow the selection without a `&mut`.
    files_scroll: Cell<usize>,
    branch_scroll: Cell<usize>,
    stash_scroll: Cell<usize>,
    /// Collapsed directory prefixes in the files tree (no trailing slash).
    collapsed: std::collections::HashSet<String>,
    /// Directory picker for opening projects (`Mode::OpenProject`).
    open_browser: Option<OpenBrowser>,
}

/// One subdirectory row in the project browser.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    /// The directory itself contains `.git` (repo root): badged `[repo]`.
    pub is_repo_root: bool,
}

/// Which row of the browser list is highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserRow {
    /// `.`: open the browsed folder itself.
    Current,
    /// `..`: go up to the parent.
    Parent,
    /// A subdirectory: open it, or descend into it.
    Dir(usize),
}

/// Directory picker behind `Mode::OpenProject`. Row 0 is always `.`
/// (open this folder), row 1 is `..` (parent), then the visible
/// subdirectories (all of them, or the type-to-filter matches).
#[derive(Debug, Clone)]
pub struct OpenBrowser {
    pub cwd: std::path::PathBuf,
    pub entries: Vec<DirEntry>,
    /// Subset of `entries` shown: everything without a filter, the
    /// case-insensitive substring matches with one.
    pub view: Vec<DirEntry>,
    pub selected: usize,
    pub error: Option<String>,
    /// Jump-to-path line (`tab`): typing a path instead of browsing.
    pub editing_path: bool,
    /// Type-to-filter query: any typed char narrows this folder's list,
    /// so there is no separate "start searching" step.
    pub filter: String,
}

impl OpenBrowser {
    /// Rows in the list: `.` + `..` + one per visible subdirectory.
    pub fn row_count(&self) -> usize {
        self.view.len() + 2
    }

    pub fn row(&self, index: usize) -> BrowserRow {
        if index == 0 {
            BrowserRow::Current
        } else if index == 1 {
            BrowserRow::Parent
        } else {
            BrowserRow::Dir(index - 2)
        }
    }

    pub fn selected_row(&self) -> BrowserRow {
        self.row(self.selected.min(self.row_count().saturating_sub(1)))
    }

    /// Absolute path the highlighted row points at (`.` and `..`
    /// included; `..` of `/` resolves to `/` itself).
    pub fn selected_path(&self) -> std::path::PathBuf {
        match self.selected_row() {
            BrowserRow::Current => self.cwd.clone(),
            BrowserRow::Parent => self
                .cwd
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| self.cwd.clone()),
            BrowserRow::Dir(i) => self
                .view
                .get(i)
                .map(|e| self.cwd.join(&e.name))
                .unwrap_or_else(|| self.cwd.clone()),
        }
    }

    /// Start substring-filtering the folder list (any typed char lands
    /// here; there is no separate search mode to enter).
    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.apply_filter();
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.apply_filter();
    }

    /// Drop the query and show the full list again.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.apply_filter();
    }

    /// Rebuild the visible rows from `entries` + `filter`. `.`/`..` always
    /// stay; a fresh filter jumps the highlight to the first match.
    fn apply_filter(&mut self) {
        if self.filter.is_empty() {
            self.view = self.entries.clone();
            return;
        }
        let q = self.filter.to_lowercase();
        self.view = self
            .entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&q))
            .cloned()
            .collect();
        if !self.view.is_empty() {
            // First match sits at row 2 (after `.` and `..`).
            self.selected = 2;
        }
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let n = self.row_count();
        if n == 0 {
            return;
        }
        let cur = self.selected.min(n - 1) as isize;
        self.selected = (cur + delta).clamp(0, n as isize - 1) as usize;
    }

    /// Re-read `cwd` from disk (subdirectories only, sorted). Keeps the
    /// old listing when the directory cannot be read. Moving folders
    /// resets the cursor and drops any active filter.
    pub fn rescan(&mut self) {
        match read_subdirs(&self.cwd) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries = Vec::new();
                self.error = Some(e);
            }
        }
        self.filter.clear();
        self.view = self.entries.clone();
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
    }

    /// Move the browser to `dir`, rescanning only when it reads cleanly.
    /// Returns false (and keeps the old folder) on unreadable targets.
    pub fn goto(&mut self, dir: std::path::PathBuf) -> bool {
        match read_subdirs(&dir) {
            Ok(entries) => {
                self.cwd = dir;
                self.entries = entries;
                self.view = self.entries.clone();
                self.selected = 0;
                self.error = None;
                self.filter.clear();
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }
}

/// Subdirectories of `dir`, sorted by name. Files are hidden: only
/// folders can become projects.
fn read_subdirs(dir: &std::path::Path) -> Result<Vec<DirEntry>, String> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        let ft = entry
            .file_type()
            .map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        // Follow symlinked dirs so linked projects stay browsable.
        let is_dir = ft.is_dir()
            || (ft.is_symlink()
                && entry.path().is_dir());
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let is_repo_root = path.join(".git").exists();
        out.push(DirEntry { name, is_repo_root });
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    Ok(out)
}

/// Split an upstream shorthand (`origin/main`, `origin/feature/x`) into
/// its remote and branch. Remote names cannot contain `/`, so the first
/// slash is the boundary; a bare name with no slash pushes to `origin`.
fn split_upstream(upstream: &str) -> (&str, &str) {
    match upstream.find('/') {
        Some(i) => (&upstream[..i], &upstream[i + 1..]),
        None => ("origin", upstream),
    }
}

/// Cumulative ancestor prefixes: "a/b/c/f" -> ["a", "a/b", "a/b/c"].
fn ancestors_of(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, b) in path.bytes().enumerate() {
        if b == b'/' {
            out.push(path[..i].to_string());
        }
    }
    out
}

impl App {
    #[cfg(test)]
    pub fn new(queue: JobQueue) -> Self {
        Self::new_with_config(queue, Config::default())
    }

    pub fn new_with_config(queue: JobQueue, config: Config) -> Self {
        let mut app = Self {
            queue,
            keys: config.keys,
            theme: config.theme,
            llm: config.llm,
            generating: false,
            notice: None,
            config_path: None,
            llm_form: Default::default(),
            llm_cursors: [0; 4],
            llm_selected: 0,
            repo_name: "repo".into(),
            status: None,
            file_list: Vec::new(),
            selected: 0,
            mode: Mode::Normal,
            focus: Focus::Status,
            draft: String::new(),
            draft_cursor: 0,
            finder_selected: 0,
            finder_return: Mode::Normal,
            sync_return: Mode::Normal,
            error: None,
            quit: false,
            diff: None,
            diff_for: None,
            diff_whole_file: false,
            fallback_whole_file: false,
            hunk: 0,
            diff_scroll: 0,
            branches: None,
            branch_selected: 0,
            log: None,
            log_scroll: 0,
            stash: None,
            stash_selected: 0,
            sync: None,
            syncing: None,
            files_scroll: Cell::new(0),
            branch_scroll: Cell::new(0),
            stash_scroll: Cell::new(0),
            collapsed: Default::default(),
            open_browser: None,
        };
        app.refresh();
        app.preload_panels();
        app
    }

    pub fn repo_name(&self) -> &str {
        &self.repo_name
    }

    pub fn set_repo_name(&mut self, name: String) {
        self.repo_name = name;
    }

    /// Open the project browser (`o`): pick a directory, Enter opens it.
    /// Starts at the current project's root so siblings are one step away.
    pub fn begin_open_project(&mut self, start: std::path::PathBuf) {
        self.mode = Mode::OpenProject;
        self.draft.clear();
        self.error = None;
        let mut browser = OpenBrowser {
            cwd: start,
            entries: Vec::new(),
            view: Vec::new(),
            // Row 0 is `.` (the folder itself); row 1 is `..`, then dirs.
            selected: 0,
            error: None,
            editing_path: false,
            filter: String::new(),
        };
        browser.rescan();
        self.open_browser = Some(browser);
    }

    pub fn open_browser(&self) -> Option<&OpenBrowser> {
        self.open_browser.as_ref()
    }

    pub fn open_browser_mut(&mut self) -> Option<&mut OpenBrowser> {
        self.open_browser.as_mut()
    }

    pub fn push_draft_char(&mut self, c: char) {
        self.insert_draft_char(c);
    }

    pub fn pop_draft_char(&mut self) {
        self.delete_draft_before();
    }

    pub fn clear_draft(&mut self) {
        self.draft.clear();
        self.draft_cursor = 0;
    }

    /// Cursor position in `draft` (chars from the start), clamped so stale
    /// values after direct `draft` writes stay valid.
    pub fn draft_cursor(&self) -> usize {
        self.draft_cursor.min(self.draft.chars().count())
    }

    fn draft_byte_index(&self) -> usize {
        self.draft
            .char_indices()
            .nth(self.draft_cursor())
            .map(|(i, _)| i)
            .unwrap_or(self.draft.len())
    }

    /// Insert one char at the cursor (commit box, finder, jump line).
    pub fn insert_draft_char(&mut self, c: char) {
        let byte = self.draft_byte_index();
        self.draft.insert(byte, c);
        self.draft_cursor = self.draft_cursor() + 1;
    }

    /// Backspace: delete the char before the cursor.
    pub fn delete_draft_before(&mut self) {
        let cursor = self.draft_cursor();
        if cursor == 0 {
            return;
        }
        let byte = self.draft_byte_index();
        let prev = self.draft[..byte].chars().next_back().map(|c| c.len_utf8()).unwrap_or(0);
        self.draft.drain(byte - prev..byte);
        self.draft_cursor = cursor - 1;
    }

    /// Delete key: delete the char under the cursor.
    pub fn delete_draft_after(&mut self) {
        let byte = self.draft_byte_index();
        if byte >= self.draft.len() {
            return;
        }
        let len = self.draft[byte..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        self.draft.drain(byte..byte + len);
        self.draft_cursor = self.draft_cursor();
    }

    pub fn move_draft_left(&mut self) {
        self.draft_cursor = self.draft_cursor().saturating_sub(1);
    }

    pub fn move_draft_right(&mut self) {
        self.draft_cursor = (self.draft_cursor() + 1).min(self.draft.chars().count());
    }

    pub fn move_draft_home(&mut self) {
        self.draft_cursor = 0;
    }

    pub fn move_draft_end(&mut self) {
        self.draft_cursor = self.draft.chars().count();
    }

    /// Cancel the open-project flow entirely (Esc in the browser).
    pub fn cancel_open_project(&mut self) {
        self.mode = Mode::Normal;
        self.draft.clear();
        self.open_browser = None;
    }

    /// Move from the browser to the `git init` confirm step, rewriting the
    /// draft to the directory that would be initialized.
    pub fn confirm_init_prompt(&mut self, dir: String) {
        self.mode = Mode::ConfirmInit;
        self.draft = dir;
    }

    /// Close the open-project flow after a successful open/switch.
    pub fn finish_open_project(&mut self) {
        self.mode = Mode::Normal;
        self.draft.clear();
        self.open_browser = None;
    }

    /// Back out of the init-confirm step to the browser (keeps the listing).
    pub fn back_to_open_project(&mut self) {
        self.mode = Mode::OpenProject;
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn set_browser_error(&mut self, msg: String) {
        if let Some(b) = self.open_browser.as_mut() {
            b.error = Some(msg);
        } else {
            self.error = Some(msg);
        }
    }

    pub(crate) fn files_scroll(&self) -> usize {
        self.files_scroll.get()
    }

    pub(crate) fn set_files_scroll(&self, off: usize) {
        self.files_scroll.set(off);
    }

    pub(crate) fn branch_scroll(&self) -> usize {
        self.branch_scroll.get()
    }

    pub(crate) fn set_branch_scroll(&self, off: usize) {
        self.branch_scroll.set(off);
    }

    pub(crate) fn stash_scroll(&self) -> usize {
        self.stash_scroll.get()
    }

    pub(crate) fn set_stash_scroll(&self, off: usize) {
        self.stash_scroll.set(off);
    }

    pub(crate) fn is_collapsed(&self, dir: &str) -> bool {
        self.collapsed.contains(dir)
    }

    pub(crate) fn set_collapsed(&mut self, dir: &str, value: bool) {
        if value {
            self.collapsed.insert(dir.to_string());
        } else {
            self.collapsed.remove(dir);
        }
    }

    pub fn status(&self) -> Option<&RepoStatus> {
        self.status.as_ref()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_file(&self) -> Option<&StatusEntry> {
        self.file_list
            .get(self.selected.min(self.file_count().saturating_sub(1)))
    }

    /// One browsable tree entry by index (for the file finder).
    pub fn file_entry(&self, index: usize) -> Option<&StatusEntry> {
        self.file_list.get(index)
    }

    /// Every browsable file: changed first, then clean tracked files.
    /// The files panel renders this same list so the highlight always
    /// tracks the cursor (see `render_files_panel`).
    pub fn file_list(&self) -> &[StatusEntry] {
        &self.file_list
    }

    pub fn has_files(&self) -> bool {
        !self.file_list.is_empty()
    }

    /// Rebuild [`Self::file_list`] from a fresh status: changed files keep
    /// status order, then clean tracked files alphabetically.
    fn rebuild_file_list(&mut self) {
        let Some(st) = self.status.as_ref() else {
            self.file_list = Vec::new();
            self.selected = 0;
            return;
        };
        let mut list = st.files.clone();
        let changed: std::collections::HashSet<&str> =
            st.files.iter().map(|e| e.path.as_str()).collect();
        // tracked_files arrives sorted, so filtering preserves that order.
        list.extend(
            st.tracked_files
                .iter()
                .filter(|p| !changed.contains(p.as_str()))
                .map(|p| StatusEntry {
                    path: p.clone(),
                    state: FileState::Clean,
                }),
        );
        self.file_list = list;
        self.selected = self.selected.min(self.file_count().saturating_sub(1));
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Quit regardless of bindings (Ctrl-C safety hatch in the event loop).
    pub fn request_quit(&mut self) {
        self.quit = true;
    }

    pub fn diff(&self) -> Option<&FileDiff> {
        self.diff.as_ref()
    }

    /// Whether the loaded diff shows staged (`Some(true)`) or unstaged
    /// (`Some(false)`) changes; `None` when no diff is loaded.
    pub fn diff_viewing_staged(&self) -> Option<bool> {
        self.diff_for.as_ref().map(|(_, staged)| *staged)
    }

    /// The loaded diff is a whole-file view (clean file has no diff).
    pub fn diff_whole_file(&self) -> bool {
        self.diff_whole_file
    }

    pub fn hunk(&self) -> usize {
        self.hunk
    }

    pub fn diff_scroll(&self) -> u16 {
        self.diff_scroll
    }

    pub fn branches(&self) -> Option<&[BranchInfo]> {
        self.branches.as_deref()
    }

    pub fn branch_selected(&self) -> usize {
        self.branch_selected
    }

    pub fn log(&self) -> Option<&[CommitInfo]> {
        self.log.as_deref()
    }

    pub fn log_scroll(&self) -> u16 {
        self.log_scroll
    }

    pub fn stash(&self) -> Option<&[StashEntry]> {
        self.stash.as_deref()
    }

    pub fn stash_selected(&self) -> usize {
        self.stash_selected
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// File-list indices matching the finder query, best first. Empty
    /// query lists every file in tree order.
    pub fn finder_matches(&self) -> Vec<usize> {
        let query = self.draft.as_str();
        if query.is_empty() {
            return (0..self.file_list.len()).collect();
        }
        let paths: Vec<&str> = self.file_list.iter().map(|e| e.path.as_str()).collect();
        fuzzy::rank(query, &paths)
            .into_iter()
            .map(|(i, _)| i)
            .collect()
    }

    /// Clamped cursor into [`Self::finder_matches`] for the renderer.
    pub fn finder_cursor(&self) -> usize {
        self.finder_selected
            .min(self.finder_matches().len().saturating_sub(1))
    }

    /// Mode the open finder returns to on Enter/Esc.
    pub fn finder_return(&self) -> Mode {
        self.finder_return
    }

    /// Upstream tracking state, if loaded yet.
    pub fn sync(&self) -> Option<&SyncStatus> {
        self.sync.as_ref()
    }

    /// In-flight push/pull/publish label ("pushing…"), if any.
    pub fn syncing(&self) -> Option<&str> {
        self.syncing.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Whether an LLM commit-message job is in flight (commit modal shows
    /// "generating…").
    pub fn is_generating(&self) -> bool {
        self.generating
    }

    #[allow(dead_code)]
    pub fn llm_config(&self) -> &LlmConfig {
        &self.llm
    }

    /// Config file location for persisting the LLM setup form.
    pub fn set_config_path(&mut self, path: Option<std::path::PathBuf>) {
        self.config_path = path;
    }

    /// Success confirmation line (cleared on the next keypress).
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Highlighted row of the LLM setup form.
    pub fn llm_selected(&self) -> usize {
        self.llm_selected.min(LLM_FIELD_LABELS.len() - 1)
    }

    /// Display value of form row `i`: the live draft for the selected row,
    /// the stashed buffer otherwise.
    pub fn llm_field_value(&self, i: usize) -> &str {
        if i == self.llm_selected() {
            &self.draft
        } else {
            self.llm_form.get(i).map(String::as_str).unwrap_or("")
        }
    }

    /// `A` in the file list: open the LLM setup form prefilled from the
    /// current `[llm]` config (or its defaults).
    pub fn begin_llm_settings(&mut self) {
        self.mode = Mode::LlmSettings;
        self.error = None;
        self.notice = None;
        self.llm_form = [
            self.llm.provider.clone(),
            self.llm.model.clone(),
            self.llm.api_key.clone(),
            self.llm.base_url.clone().unwrap_or_default(),
        ];
        self.llm_cursors = [0; 4];
        self.llm_selected = 0;
        self.draft = self.llm_form[0].clone();
        self.move_draft_end();
        self.llm_cursors[0] = self.draft_cursor();
    }

    /// Stash the live draft/cursor into the selected form row.
    fn stash_llm_field(&mut self) {
        let i = self.llm_selected();
        self.llm_form[i] = self.draft.clone();
        self.llm_cursors[i] = self.draft_cursor();
    }

    /// Move the form highlight, stashing/loading the row buffers.
    fn move_llm_selection(&mut self, delta: isize) {
        let n = LLM_FIELD_LABELS.len();
        self.stash_llm_field();
        let cur = self.llm_selected.min(n - 1) as isize;
        self.llm_selected = (cur + delta).clamp(0, n as isize - 1) as usize;
        let i = self.llm_selected;
        self.draft = self.llm_form[i].clone();
        self.draft_cursor = self.llm_cursors[i].min(self.draft.chars().count());
    }

    /// Back out without saving.
    pub fn cancel_llm_settings(&mut self) {
        self.mode = Mode::Normal;
        self.draft.clear();
        self.draft_cursor = 0;
    }

    /// Enter in the setup form: validate, apply to the session, and persist
    /// `[llm]` to the config file. Stays open with an error on bad input.
    pub fn save_llm_settings(&mut self) {
        self.stash_llm_field();
        let provider = self.llm_form[0].trim().to_lowercase();
        if !git_tui_core::llm::PROVIDERS.contains(&provider.as_str()) {
            self.error = Some(format!(
                "unknown provider {provider:?} (expected one of: {})",
                git_tui_core::llm::PROVIDERS.join(", ")
            ));
            return;
        }
        let model = self.llm_form[1].trim().to_string();
        self.llm = LlmConfig {
            provider,
            model: if model.is_empty() {
                LlmConfig::default().model
            } else {
                model
            },
            api_key: self.llm_form[2].trim().to_string(),
            base_url: {
                let u = self.llm_form[3].trim().to_string();
                if u.is_empty() {
                    None
                } else {
                    Some(u)
                }
            },
        };
        if let Some(path) = self.config_path.clone() {
            if let Err(e) = Config::save_llm_to_path(&path, &self.llm) {
                self.error = Some(e.to_string());
                return;
            }
            self.notice = Some(format!("LLM settings saved to {}", path.display()));
        } else {
            self.notice = Some("LLM settings updated for this session".into());
        }
        self.mode = Mode::Normal;
        self.draft.clear();
        self.draft_cursor = 0;
        self.error = None;
    }

    /// Keys inside the LLM setup form. Tab/Up/Down switch rows; the draft
    /// line edits the selected row; Enter saves; Esc cancels.
    fn on_key_llm_settings(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char(c) => self.insert_draft_char(c),
            KeyCode::Backspace => self.delete_draft_before(),
            KeyCode::Delete => self.delete_draft_after(),
            KeyCode::Left => self.move_draft_left(),
            KeyCode::Right => self.move_draft_right(),
            KeyCode::Home => self.move_draft_home(),
            KeyCode::End => self.move_draft_end(),
            KeyCode::Up => self.move_llm_selection(-1),
            KeyCode::Down | KeyCode::Tab => self.move_llm_selection(1),
            KeyCode::BackTab => self.move_llm_selection(-1),
            KeyCode::Enter => self.save_llm_settings(),
            KeyCode::Esc => self.cancel_llm_settings(),
            _ => {}
        }
    }

    /// Shift+A in the commit box: reference every staged file (index vs
    /// HEAD) and ask the LLM provider for a Conventional-Commits message.
    /// The result arrives via `poll` as `GeneratedMessage` and replaces
    /// the draft (cursor jumps to the end for quick editing).
    pub fn begin_generate_commit_message(&mut self) {
        if self.mode != Mode::Committing || self.generating {
            return;
        }
        self.generating = true;
        self.error = None;
        if let Err(e) = self.queue.submit(AsyncJob::GenerateCommitMessage {
            llm: self.llm.clone(),
        }) {
            self.generating = false;
            self.error = Some(e.to_string());
        }
    }

    #[cfg(test)]
    pub(crate) fn set_status_for_test(&mut self, st: git_tui_core::status::RepoStatus) {
        self.status = Some(st);
        self.selected = 0;
        self.rebuild_file_list();
    }

    #[cfg(test)]
    pub(crate) fn set_diff_for_test(&mut self, diff: git_tui_core::diff::FileDiff, staged: bool) {
        self.diff_for = Some((diff.path.clone(), staged));
        self.diff_whole_file = false;
        self.diff = Some(diff);
        self.hunk = 0;
        self.diff_scroll = 0;
    }

    #[cfg(test)]
    pub(crate) fn set_whole_file_for_test(&mut self, whole: bool) {
        self.diff_whole_file = whole;
    }

    #[cfg(test)]
    pub(crate) fn set_branches_for_test(&mut self, branches: Vec<BranchInfo>) {
        self.branches = Some(branches);
        self.branch_selected = 0;
    }

    #[cfg(test)]
    pub(crate) fn set_log_for_test(&mut self, entries: Vec<CommitInfo>) {
        self.log = Some(entries);
        self.log_scroll = 0;
    }

    #[cfg(test)]
    pub(crate) fn set_stash_for_test(&mut self, entries: Vec<StashEntry>) {
        self.stash = Some(entries);
        self.stash_selected = 0;
    }

    fn file_count(&self) -> usize {
        self.file_list.len()
    }

    fn refresh(&mut self) {
        if let Err(e) = self.queue.submit(AsyncJob::RefreshStatus) {
            self.error = Some(e.to_string());
        }
    }

    /// Eagerly load every rail panel at startup so branches, log, and
    /// stash render data immediately instead of waiting for first focus.
    /// The channel is unbounded and reads are idempotent, so submitting
    /// up front is cheap; `maybe_load_*` stays as a backstop in case a
    /// submit here ever fails.
    fn preload_panels(&mut self) {
        let jobs = [
            AsyncJob::ListBranches,
            AsyncJob::ListLog {
                limit: Self::LOG_LIMIT,
            },
            AsyncJob::ListStash,
            AsyncJob::LoadSync,
        ];
        for job in jobs {
            if let Err(e) = self.queue.submit(job) {
                self.error = Some(e.to_string());
                return;
            }
        }
    }

    /// Dispatch a keypress (event loop calls this; then [`App::poll`]).
    /// Test/legacy path with no modifiers: `A` is treated as Shift+A so
    /// tests can drive generation with a single KeyCode. The binary uses
    /// [`Self::on_key_with_modifiers`].
    #[allow(dead_code)]
    pub fn on_key(&mut self, key: KeyCode) {
        // Test/legacy path: no modifiers known. `A` is treated as Shift+A
        // so tests can drive generation with a single KeyCode.
        let shift = matches!(key, KeyCode::Char('A'));
        self.on_key_with_modifiers(key, shift);
    }

    /// Modifier-aware dispatch (the event loop calls this). Only Shift+A
    /// generates a message in the commit box, so a literal `A` (caps lock
    /// or otherwise without Shift) still types normally.
    pub fn on_key_with_modifiers(&mut self, key: KeyCode, shift_held: bool) {
        // A new action dismisses the previous error/notice; async failures
        // from this action arrive later via `poll` and replace them.
        self.error = None;
        self.notice = None;
        if self.mode == Mode::Committing
            && matches!(key, KeyCode::Char('a' | 'A'))
            && shift_held
        {
            self.begin_generate_commit_message();
            return;
        }
        // Shift+Right in the file list jumps to the right-side diff
        // preview (plain Right expands folders, so it must stay put).
        if self.mode == Mode::Normal
            && self.focus == Focus::Status
            && key == KeyCode::Right
            && shift_held
        {
            self.focus = Focus::Diff;
            return;
        }
        self.on_key_inner(key);
    }

    fn on_key_inner(&mut self, key: KeyCode) {
        if self.mode == Mode::LlmSettings {
            // Shift+A must not leak generation in here: the form owns every
            // key until Enter saves or Esc cancels.
            self.on_key_llm_settings(key);
            return;
        }
        if self.mode == Mode::FullDiff {
            self.on_key_full_diff(key);
            return;
        }
        if self.mode == Mode::FindFile {
            match key {
                KeyCode::Up => self.finder_move(-1),
                KeyCode::Down => self.finder_move(1),
                KeyCode::Left => self.move_draft_left(),
                KeyCode::Right => self.move_draft_right(),
                KeyCode::Char(c) => {
                    self.insert_draft_char(c);
                    self.finder_selected = 0;
                }
                KeyCode::Backspace => {
                    self.delete_draft_before();
                    self.finder_selected = 0;
                }
                KeyCode::Delete => {
                    self.delete_draft_after();
                    self.finder_selected = 0;
                }
                KeyCode::Home => self.move_draft_home(),
                KeyCode::End => self.move_draft_end(),
                KeyCode::Enter => {
                    self.expand_selected();
                    self.submit_finder();
                }
                KeyCode::Esc => {
                    self.mode = self.finder_return;
                    self.draft.clear();
                }
                _ => {}
            }
            return;
        }
        if self.mode != Mode::Normal {
            match key {
                KeyCode::Char(c) => self.insert_draft_char(c),
                KeyCode::Backspace => self.delete_draft_before(),
                KeyCode::Delete => self.delete_draft_after(),
                KeyCode::Left => self.move_draft_left(),
                KeyCode::Right => self.move_draft_right(),
                KeyCode::Home => self.move_draft_home(),
                KeyCode::End => self.move_draft_end(),
                KeyCode::Enter => match self.mode {
                    Mode::Committing => self.submit_commit(),
                    Mode::NewBranch => self.submit_new_branch(),
                    Mode::StashPush => self.submit_stash_push(),
                    Mode::SetUpstream => self.submit_push_upstream(),
                    Mode::SetRemote => self.submit_publish(),
                    // FullDiff and FindFile return before reaching here.
                    // OpenProject/ConfirmInit submit through `Workspace`
                    // (it owns all projects), so they are no-ops here.
                    // LlmSettings is routed to `on_key_llm_settings` above.
                    Mode::Normal
                    | Mode::FullDiff
                    | Mode::FindFile
                    | Mode::OpenProject
                    | Mode::ConfirmInit
                    | Mode::LlmSettings => {}
                },
                KeyCode::Esc => {
                    self.mode = match self.mode {
                        Mode::SetUpstream | Mode::SetRemote => self.sync_return,
                        _ => Mode::Normal,
                    };
                    self.draft.clear();
                }
                _ => {}
            }
            return;
        }
        // Binding dispatch (first match wins; modal keys above stay fixed).
        // Cloned: small vecs, and it keeps the borrow checker happy while
        // the arms below take &mut self.
        let k = self.keys.clone();
        if k.focus_next.contains(&key) {
            // Tab cycles the left rail only (Status -> Branches -> Log ->
            // Stash -> Status). The right-side Diff preview is reached via
            // `5` / Shift+Right, never via Tab; Tab from Diff drops back
            // to the file list.
            self.focus = match self.focus {
                Focus::Status => Focus::Branches,
                Focus::Branches => Focus::Log,
                Focus::Log => Focus::Stash,
                Focus::Stash => Focus::Status,
                Focus::Diff => Focus::Status,
            };
            self.maybe_load_branches();
            self.maybe_load_log();
            self.maybe_load_stash();
        } else if self.focus == Focus::Status && key == KeyCode::Left {
            self.toggle_folder();
        } else if self.focus == Focus::Status && key == KeyCode::Right {
            self.expand_at_cursor();
        } else if k.focus_status.contains(&key) {
            self.focus = Focus::Status;
        } else if k.focus_branches.contains(&key) {
            self.focus = Focus::Branches;
            self.maybe_load_branches();
        } else if k.focus_log.contains(&key) {
            self.focus = Focus::Log;
            self.maybe_load_log();
        } else if k.focus_stash.contains(&key) {
            self.focus = Focus::Stash;
            self.maybe_load_stash();
        } else if k.focus_diff.contains(&key) {
            self.focus = Focus::Diff;
        } else if k.nav_down.contains(&key) {
            self.move_down();
        } else if k.nav_up.contains(&key) {
            self.move_up();
        } else if k.checkout.contains(&key) && self.focus == Focus::Branches {
            self.checkout_selected_branch();
        } else if k.stash_pop.contains(&key) && self.focus == Focus::Stash {
            self.pop_selected_stash();
        } else if key == KeyCode::Enter && matches!(self.focus, Focus::Status | Focus::Diff) {
            // Enter on a file opens it fullscreen (checkout/pop own Enter
            // in their own panels, so no conflict). Works from the file
            // list and from the focused diff preview on the right.
            self.open_full_diff();
        } else if k.stage.contains(&key) {
            if self.focus == Focus::Status {
                self.toggle_stage();
            }
        } else if k.branch_new.contains(&key) && self.focus == Focus::Branches {
            self.mode = Mode::NewBranch;
            self.draft.clear();
        } else if k.stash_push.contains(&key) && self.focus == Focus::Stash {
            self.mode = Mode::StashPush;
            self.draft.clear();
        } else if k.branch_delete.contains(&key) && self.focus == Focus::Branches {
            self.delete_selected_branch();
        } else if k.stash_drop.contains(&key) && self.focus == Focus::Stash {
            self.drop_selected_stash();
        } else if k.scroll_up.contains(&key) {
            self.scroll_diff_by(-10);
        } else if k.scroll_down.contains(&key) {
            self.scroll_diff_by(10);
        } else if k.commit.contains(&key) {
            self.mode = Mode::Committing;
            self.draft.clear();
        } else if k.llm_settings.contains(&key) {
            self.begin_llm_settings();
        } else if k.find_files.contains(&key) {
            self.open_finder();
        } else if k.refresh.contains(&key) {
            self.refresh();
        } else if k.sync_pull.contains(&key) {
            self.start_pull();
        } else if k.sync_push.contains(&key) {
            self.start_push();
        } else if k.quit.contains(&key) {
            self.quit = true;
        }
    }

    /// Keys inside the fullscreen diff overlay. hunk navigation and staging
    /// mirror the old diff-pane keys; `/` finds another file without
    /// leaving fullscreen; Esc closes back to the file list.
    fn on_key_full_diff(&mut self, key: KeyCode) {
        let k = self.keys.clone();
        if key == KeyCode::Esc {
            self.mode = Mode::Normal;
        } else if k.find_files.contains(&key) {
            self.open_finder();
        } else if key == KeyCode::Up {
            // Arrows scroll line-by-line so long single-hunk diffs stay
            // viewable; j/k below jump by hunk.
            self.scroll_diff_by(-1);
        } else if key == KeyCode::Down {
            self.scroll_diff_by(1);
        } else if k.nav_down.contains(&key) {
            self.select_hunk(self.hunk.saturating_add(1));
        } else if k.nav_up.contains(&key) {
            self.select_hunk(self.hunk.saturating_sub(1));
        } else if k.stage.contains(&key) {
            self.stage_selected_hunk();
        } else if k.scroll_up.contains(&key) {
            self.scroll_diff_by(-10);
        } else if k.scroll_down.contains(&key) {
            self.scroll_diff_by(10);
        } else if k.sync_pull.contains(&key) {
            self.start_pull();
        } else if k.sync_push.contains(&key) {
            self.start_push();
        } else if k.quit.contains(&key) {
            self.quit = true;
        }
    }

    /// Enter on a file: open its diff fullscreen.
    fn open_full_diff(&mut self) {
        if self.selected_file().is_some() {
            self.mode = Mode::FullDiff;
        }
    }

    fn move_down(&mut self) {
        match self.focus {
            Focus::Status => {
                // Tree navigation: one step at a time so a collapsed
                // header (`▶`) is a real cursor stop. Stepping onto a
                // hidden file lands on its header (the UI highlights the
                // header, never the hidden file rows); stepping out of a
                // hidden anchor jumps to the next visible file. Hidden
                // files are therefore never shown while navigating, yet
                // every folder stays reachable for expand/stage.
                if self.is_hidden_index(self.selected) {
                    if let Some(n) = self.nearest_visible_after(self.selected) {
                        self.selected = n;
                    }
                } else {
                    self.selected = (self.selected + 1)
                        .min(self.file_count().saturating_sub(1));
                }
                self.maybe_load_diff();
            }
            Focus::Branches => {
                self.branch_selected = self
                    .branch_selected
                    .saturating_add(1)
                    .min(self.branch_count().saturating_sub(1));
            }
            // Read-only log: navigation scrolls.
            Focus::Log => {
                self.scroll_log_by(1);
            }
            Focus::Stash => {
                self.stash_selected = self
                    .stash_selected
                    .saturating_add(1)
                    .min(self.stash_count().saturating_sub(1));
            }
            // Right-side preview: navigation scrolls the file line by line
            // (PgUp/PgDn below page by 10 regardless of focus).
            Focus::Diff => {
                self.scroll_diff_by(1);
            }
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            Focus::Status => {
                // Mirror of move_down: single step onto a hidden anchor
                // (its header takes the highlight), escape jump when
                // already on one; clamps at the top.
                if self.is_hidden_index(self.selected) {
                    if let Some(n) = self.nearest_visible_before(self.selected) {
                        self.selected = n;
                    }
                } else {
                    self.selected = self.selected.saturating_sub(1);
                }
                self.maybe_load_diff();
            }
            Focus::Branches => {
                self.branch_selected = self.branch_selected.saturating_sub(1);
            }
            Focus::Log => {
                self.scroll_log_by(-1);
            }
            Focus::Stash => {
                self.stash_selected = self.stash_selected.saturating_sub(1);
            }
            Focus::Diff => {
                self.scroll_diff_by(-1);
            }
        }
    }

    /// Whether `path` sits under a collapsed dir (i.e. its tree rows are
    /// folded away; only the `▶` header renders).
    fn is_hidden_path(&self, path: &str) -> bool {
        ancestors_of(path)
            .into_iter()
            .any(|dir| self.collapsed.contains(dir.as_str()))
    }

    pub(crate) fn is_hidden_index(&self, index: usize) -> bool {
        self.file_list
            .get(index)
            .is_some_and(|f| self.is_hidden_path(&f.path))
    }

    /// First visible file index strictly after `from` (None when there is
    /// none: end of list or everything after is folded).
    fn nearest_visible_after(&self, from: usize) -> Option<usize> {
        (from + 1..self.file_count()).find(|&i| !self.is_hidden_index(i))
    }

    /// First visible file index strictly before `from` (None at the top or
    /// when everything above is folded).
    fn nearest_visible_before(&self, from: usize) -> Option<usize> {
        (0..from).rev().find(|&i| !self.is_hidden_index(i))
    }

    /// Toggle collapse on the deepest collapsed-capable ancestor of the
    /// selected file (or expand when that ancestor is collapsed). Only
    /// the deepest directory flips so sibling subtrees stay visible.
    fn toggle_folder(&mut self) {
        let Some(file) = self.selected_file() else {
            return;
        };
        let dirs: Vec<String> = ancestors_of(&file.path);
        let Some(deepest) = dirs.last().cloned() else {
            self.error = Some("nothing to collapse: file has no parent folder".into());
            return;
        };
        let collapsed = !self.collapsed.contains(&deepest);
        self.set_collapsed(&deepest, collapsed);
        // Stay put when collapsing under the cursor: the cursor becomes a
        // hidden anchor and the `▶` header takes the highlight, so Space
        // still stages the whole dir and Right still expands it. The next
        // Up/Down escapes to the nearest visible file (skipped both ways),
        // so hidden files never render while navigating.
    }

    /// Right on a file: expand every collapsed ancestor so the file is
    /// visible (no-op already visible).
    fn expand_selected(&mut self) {
        let Some(file) = self.selected_file() else {
            return;
        };
        let path = file.path.clone();
        for i in 0..path.len() {
            if path.as_bytes()[i] == b'/' {
                self.set_collapsed(&path[..i], false);
            }
        }
    }

    /// Right on the file list: open the fold at the cursor.
    ///
    /// - Hidden anchor (just collapsed, or a finder jump): expand its own
    ///   collapsed ancestors, like `expand_selected`.
    /// - Visible file: expand the folded region immediately below the
    ///   cursor, else the one immediately above. This is the way back in
    ///   after Up/Down skipped away from a `▶` header, since the highlight
    ///   never rests on hidden files while navigating.
    fn expand_at_cursor(&mut self) {
        let Some(file) = self.selected_file().cloned() else {
            return;
        };
        if self.is_hidden_path(&file.path) {
            self.expand_selected();
            return;
        }
        if self.selected + 1 < self.file_count()
            && self.is_hidden_index(self.selected + 1)
        {
            let path = self.file_list[self.selected + 1].path.clone();
            if let Some(dir) = self.collapsed_dir_for(&path) {
                self.set_collapsed(&dir, false);
                return;
            }
        }
        if self.selected > 0 && self.is_hidden_index(self.selected - 1) {
            let path = self.file_list[self.selected - 1].path.clone();
            if let Some(dir) = self.collapsed_dir_for(&path) {
                self.set_collapsed(&dir, false);
            }
        }
    }

    fn hunk_count(&self) -> usize {
        self.diff.as_ref().map(|d| d.hunks.len()).unwrap_or(0)
    }

    fn select_hunk(&mut self, index: usize) {
        let clamped = index.min(self.hunk_count().saturating_sub(1));
        self.hunk = clamped;
        // Snap the selected hunk to the top of the view.
        self.diff_scroll = self.hunk_start_row(clamped);
    }

    /// Rendered row offset where hunk `index` starts (its header row in the
    /// side-by-side layout built by [`crate::ui::hunk_start_row`]).
    fn hunk_start_row(&self, index: usize) -> u16 {
        self.diff
            .as_ref()
            .map(|d| crate::ui::hunk_start_row(d, index))
            .unwrap_or(0)
    }

    /// Last valid preview scroll offset (unified lines), so scrolling the
    /// [5] tab stops on the last line instead of running into blank space.
    fn diff_preview_max(&self) -> u16 {
        self.diff
            .as_ref()
            .map(|d| crate::ui::diff_unified_len(d).saturating_sub(1))
            .unwrap_or(0)
            .min(u16::MAX as usize) as u16
    }

    /// Last valid fullscreen offset (side-by-side rows).
    fn diff_full_max(&self) -> u16 {
        self.diff
            .as_ref()
            .map(|d| crate::ui::diff_rows(d).len().saturating_sub(1))
            .unwrap_or(0)
            .min(u16::MAX as usize) as u16
    }

    /// Scroll the diff by `delta` lines, clamped to the content of the
    /// currently shown view (unified preview in Normal, side-by-side rows
    /// in FullDiff).
    fn scroll_diff_by(&mut self, delta: isize) {
        let max = if self.mode == Mode::FullDiff {
            self.diff_full_max()
        } else {
            self.diff_preview_max()
        };
        let cur = self.diff_scroll as isize;
        self.diff_scroll = (cur + delta).clamp(0, max as isize) as u16;
    }

    /// Scroll the log by `delta` lines, clamped to the loaded entries.
    fn scroll_log_by(&mut self, delta: isize) {
        let max = self
            .log
            .as_ref()
            .map(|l| l.len().saturating_sub(1))
            .unwrap_or(0)
            .min(u16::MAX as usize) as isize;
        let cur = self.log_scroll as isize;
        self.log_scroll = (cur + delta).clamp(0, max) as u16;
    }

    /// Space: stage unless already fully staged (then unstage). When the
    /// highlight sits on a collapsed directory header (the selected file
    /// is hidden inside it), the whole directory is staged/unstaged so
    /// its files are ready to commit together.
    fn toggle_stage(&mut self) {
        let Some(file) = self.selected_file().cloned() else {
            return;
        };
        if let Some(dir) = self.collapsed_dir_for(&file.path) {
            self.toggle_stage_dir(&dir);
        } else {
            self.toggle_stage_file(&file);
        }
    }

    /// Shallowest collapsed ancestor of `path` (the header the UI
    /// highlights when this file is hidden), or `None` when visible.
    /// Mirrors the highlight fallback in `ui.rs`.
    fn collapsed_dir_for(&self, path: &str) -> Option<String> {
        ancestors_of(path)
            .into_iter()
            .find(|dir| self.collapsed.contains(dir.as_str()))
    }

    /// Space on one visible file: stage unless already fully staged.
    fn toggle_stage_file(&mut self, file: &StatusEntry) {
        if file.state == FileState::Conflicted {
            self.error = Some(format!(
                "conflicted: resolve markers in {} first",
                file.path
            ));
            return;
        }
        if file.state == FileState::Clean {
            self.error = Some(format!("{} is unchanged — nothing to stage", file.path));
            return;
        }
        let job = if file.state == FileState::Staged {
            AsyncJob::UnstageFile {
                path: file.path.clone(),
            }
        } else {
            AsyncJob::StageFile {
                path: file.path.clone(),
            }
        };
        if let Err(e) = self.queue.submit(job) {
            self.error = Some(e.to_string());
        }
    }

    /// Space on a collapsed directory header: stage every stageable file
    /// beneath it, or unstage them all when every one is already staged.
    /// Conflicted files abort the whole directory like the single-file
    /// case; clean files are skipped silently.
    fn toggle_stage_dir(&mut self, dir: &str) {
        let prefix = format!("{dir}/");
        let under: Vec<(String, FileState)> = self
            .file_list
            .iter()
            .filter(|e| e.path.starts_with(&prefix))
            .map(|e| (e.path.clone(), e.state))
            .collect();
        if let Some((path, _)) = under.iter().find(|(_, s)| *s == FileState::Conflicted) {
            self.error = Some(format!("conflicted: resolve markers in {path} first"));
            return;
        }
        let to_stage: Vec<String> = under
            .iter()
            .filter(|(_, s)| !matches!(s, FileState::Clean | FileState::Staged))
            .map(|(p, _)| p.clone())
            .collect();
        if !to_stage.is_empty() {
            for path in to_stage {
                if let Err(e) = self.queue.submit(AsyncJob::StageFile { path }) {
                    self.error = Some(e.to_string());
                    return;
                }
            }
            return;
        }
        let staged: Vec<String> = under
            .iter()
            .filter(|(_, s)| *s == FileState::Staged)
            .map(|(p, _)| p.clone())
            .collect();
        if !staged.is_empty() {
            for path in staged {
                if let Err(e) = self.queue.submit(AsyncJob::UnstageFile { path }) {
                    self.error = Some(e.to_string());
                    return;
                }
            }
            return;
        }
        self.error = Some(format!("nothing to stage under {dir}/"));
    }

    fn submit_commit(&mut self) {
        if self.draft.trim().is_empty() {
            return;
        }
        let job = AsyncJob::Commit {
            message: self.draft.clone(),
        };
        match self.queue.submit(job) {
            Ok(()) => {
                self.mode = Mode::Normal;
                self.draft.clear();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Stage the selected hunk of the loaded (unstaged) diff.
    fn stage_selected_hunk(&mut self) {
        let Some((path, staged)) = self.diff_for.clone() else {
            self.error = Some("no diff loaded".into());
            return;
        };
        if staged {
            self.error = Some(
                "hunk is already staged — switch to status (1) and space toggles the file".into(),
            );
            return;
        }
        let Some(diff) = self.diff.as_ref() else {
            self.error = Some("no diff loaded".into());
            return;
        };
        // Whole-file views (clean files) have no changes to stage.
        let has_changes = diff.hunks.get(self.hunk).is_some_and(|h| {
            h.lines.iter().any(|l| {
                l.kind == git_tui_core::diff::LineKind::Add
                    || l.kind == git_tui_core::diff::LineKind::Del
            })
        });
        if !has_changes {
            self.error = Some("nothing to stage in this hunk".into());
            return;
        }
        let job = AsyncJob::StageHunk {
            path,
            hunk_index: self.hunk,
        };
        if let Err(e) = self.queue.submit(job) {
            self.error = Some(e.to_string());
        }
    }

    /// Which diff belongs to the selected file: fully staged files show the
    /// staged diff, clean files show the whole file, everything else the
    /// unstaged one.
    fn diff_target(&self) -> Option<(String, bool)> {
        let file = self.selected_file()?;
        Some((file.path.clone(), file.state == FileState::Staged))
    }

    /// Whether the selected file needs a whole-file view (clean).
    fn selected_is_clean(&self) -> bool {
        self.selected_file()
            .is_some_and(|f| f.state == FileState::Clean)
    }
    /// Request the selected file's diff unless it is already loaded/loading.
    /// Automatic: files with no changes load the whole workdir file (their
    /// diff would be empty); changed files load the staged/unstaged diff.
    /// A listed file whose diff arrives empty (e.g. a mode-only change)
    /// falls back to the whole file instead of showing "(no changes)".
    fn maybe_load_diff(&mut self) {
        let target = self.diff_target();
        // A new path drops the empty-diff fallback; it applies per file.
        let path_changed =
            target.as_ref().map(|(p, _)| p) != self.diff_for.as_ref().map(|(p, _)| p);
        if path_changed {
            self.fallback_whole_file = false;
        }
        let whole = self.selected_is_clean() || self.fallback_whole_file;
        if target == self.diff_for && whole == self.diff_whole_file {
            return;
        }
        self.diff_for = target.clone();
        // Stale view: show loading until the fresh diff arrives.
        self.diff = None;
        self.hunk = 0;
        self.diff_scroll = 0;
        self.diff_whole_file = whole;
        if let Some((path, staged)) = target {
            let job = if whole {
                AsyncJob::LoadFile { path }
            } else {
                AsyncJob::LoadDiff { path, staged }
            };
            if let Err(e) = self.queue.submit(job) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn reload_diff(&mut self) {
        if let Some((path, staged)) = self.diff_for.clone() {
            let job = if self.diff_whole_file {
                AsyncJob::LoadFile { path }
            } else {
                AsyncJob::LoadDiff { path, staged }
            };
            if let Err(e) = self.queue.submit(job) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn branch_count(&self) -> usize {
        self.branches.as_ref().map(|b| b.len()).unwrap_or(0)
    }

    fn selected_branch(&self) -> Option<BranchInfo> {
        self.branches
            .as_ref()?
            .get(
                self.branch_selected
                    .min(self.branch_count().saturating_sub(1)),
            )
            .cloned()
    }

    /// Request the branch list unless already loaded/loading.
    fn maybe_load_branches(&mut self) {
        if self.focus == Focus::Branches && self.branches.is_none() {
            if let Err(e) = self.queue.submit(AsyncJob::ListBranches) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn reload_branches(&mut self) {
        if self.branches.is_some() {
            // Forget the snapshot so the fresh list replaces it wholesale.
            self.branches = None;
            if let Err(e) = self.queue.submit(AsyncJob::ListBranches) {
                self.error = Some(e.to_string());
            }
        }
    }

    /// History depth for the log panel.
    const LOG_LIMIT: usize = 100;

    /// Request history unless already loaded/loading.
    fn maybe_load_log(&mut self) {
        if self.focus == Focus::Log && self.log.is_none() {
            if let Err(e) = self.queue.submit(AsyncJob::ListLog {
                limit: Self::LOG_LIMIT,
            }) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn reload_log(&mut self) {
        if self.log.is_some() {
            self.log = None;
            if let Err(e) = self.queue.submit(AsyncJob::ListLog {
                limit: Self::LOG_LIMIT,
            }) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn submit_new_branch(&mut self) {
        if self.draft.trim().is_empty() {
            return;
        }
        let job = AsyncJob::CreateBranch {
            name: self.draft.clone(),
        };
        match self.queue.submit(job) {
            Ok(()) => {
                self.mode = Mode::Normal;
                self.draft.clear();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn checkout_selected_branch(&mut self) {
        let Some(branch) = self.selected_branch() else {
            return;
        };
        if let Err(e) = self
            .queue
            .submit(AsyncJob::CheckoutBranch { name: branch.name })
        {
            self.error = Some(e.to_string());
        }
    }

    fn delete_selected_branch(&mut self) {
        let Some(branch) = self.selected_branch() else {
            return;
        };
        if let Err(e) = self
            .queue
            .submit(AsyncJob::DeleteBranch { name: branch.name })
        {
            self.error = Some(e.to_string());
        }
    }

    fn stash_count(&self) -> usize {
        self.stash.as_ref().map(|s| s.len()).unwrap_or(0)
    }

    fn selected_stash(&self) -> Option<StashEntry> {
        self.stash
            .as_ref()?
            .get(
                self.stash_selected
                    .min(self.stash_count().saturating_sub(1)),
            )
            .cloned()
    }

    /// Request the stash list unless already loaded/loading.
    fn maybe_load_stash(&mut self) {
        if self.focus == Focus::Stash && self.stash.is_none() {
            if let Err(e) = self.queue.submit(AsyncJob::ListStash) {
                self.error = Some(e.to_string());
            }
        }
    }

    fn reload_stash(&mut self) {
        if self.stash.is_some() {
            self.stash = None;
            if let Err(e) = self.queue.submit(AsyncJob::ListStash) {
                self.error = Some(e.to_string());
            }
        }
    }

    /// `/`: open the fuzzy file finder on the browsable tree. Works
    /// from `Normal` and from `FullDiff`; Enter/Esc return there.
    fn open_finder(&mut self) {
        self.finder_return = self.mode;
        self.mode = Mode::FindFile;
        self.draft.clear();
        self.finder_selected = self.selected.min(self.file_count().saturating_sub(1));
    }

    fn finder_move(&mut self, delta: isize) {
        let n = self.finder_matches().len();
        if n == 0 {
            self.finder_selected = 0;
            return;
        }
        let cur = self.finder_selected.min(n - 1) as isize;
        self.finder_selected = (cur + delta).clamp(0, n as isize - 1) as usize;
    }

    /// Enter in the finder: jump the file cursor to the chosen match,
    /// expanding any collapsed ancestors so the match is actually visible,
    /// and return where the finder was opened from (staying fullscreen
    /// when opened fullscreen, with the new file's diff loading).
    fn submit_finder(&mut self) {
        let matches = self.finder_matches();
        let Some(&index) = matches.get(self.finder_cursor()) else {
            return;
        };
        self.selected = index;
        self.expand_selected();
        self.focus = Focus::Status;
        self.mode = self.finder_return;
        self.draft.clear();
        self.finder_selected = 0;
        self.maybe_load_diff();
    }

    fn submit_stash_push(&mut self) {
        if self.draft.trim().is_empty() {
            return;
        }
        let job = AsyncJob::StashPush {
            message: self.draft.clone(),
        };
        match self.queue.submit(job) {
            Ok(()) => {
                self.mode = Mode::Normal;
                self.draft.clear();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn pop_selected_stash(&mut self) {
        let Some(entry) = self.selected_stash() else {
            return;
        };
        if let Err(e) = self.queue.submit(AsyncJob::StashPop { index: entry.index }) {
            self.error = Some(e.to_string());
        }
    }

    fn drop_selected_stash(&mut self) {
        let Some(entry) = self.selected_stash() else {
            return;
        };
        if let Err(e) = self
            .queue
            .submit(AsyncJob::StashDrop { index: entry.index })
        {
            self.error = Some(e.to_string());
        }
    }

    /// `p`, lazygit-style: pull the current branch (`git pull`, honoring
    /// the user's pull.rebase/pull.ff config). Runs on the worker thread;
    /// the status panel shows "pulling…" until it lands or fails.
    fn start_pull(&mut self) {
        self.syncing = Some("pulling…".into());
        if let Err(e) = self.queue.submit(AsyncJob::Pull) {
            self.syncing = None;
            self.error = Some(e.to_string());
        }
    }

    /// `P`, lazygit-style: push the current branch. With an upstream it
    /// pushes straight away; without one it asks for the remote (`-u`);
    /// with no remotes at all it asks for the `origin` URL (publish).
    fn start_push(&mut self) {
        let Some(branch) = self.status.as_ref().map(|st| st.branch.clone()) else {
            self.error = Some("still loading — try again in a moment".into());
            return;
        };
        let Some(sync) = self.sync.clone() else {
            self.reload_sync();
            self.error = Some("loading remotes — press P again in a moment".into());
            return;
        };
        self.sync_return = self.mode;
        if sync.remotes.is_empty() {
            self.mode = Mode::SetRemote;
            self.draft.clear();
            self.draft_cursor = 0;
        } else if let Some(upstream) = sync.upstream {
            let (remote, _) = split_upstream(&upstream);
            self.syncing = Some("pushing…".into());
            if let Err(e) = self.queue.submit(AsyncJob::Push {
                remote: remote.to_string(),
                branch,
                set_upstream: false,
            }) {
                self.syncing = None;
                self.error = Some(e.to_string());
            }
        } else {
            self.mode = Mode::SetUpstream;
            self.draft = "origin".to_string();
            self.move_draft_end();
        }
    }

    /// Enter in `SetUpstream`: push `-u <remote> <branch>`.
    fn submit_push_upstream(&mut self) {
        let remote = self.draft.trim().to_string();
        if remote.is_empty() {
            return;
        }
        let Some(branch) = self.status.as_ref().map(|st| st.branch.clone()) else {
            self.error = Some("still loading — try again in a moment".into());
            return;
        };
        match self.queue.submit(AsyncJob::Push {
            remote,
            branch,
            set_upstream: true,
        }) {
            Ok(()) => {
                self.mode = self.sync_return;
                self.draft.clear();
                self.draft_cursor = 0;
                self.syncing = Some("pushing…".into());
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Enter in `SetRemote`: point `origin` at the typed URL and push `-u`.
    /// The "not yet on GitHub" flow: create the empty repo on the host,
    /// paste its URL here, and the branch is published.
    fn submit_publish(&mut self) {
        let url = self.draft.trim().to_string();
        if url.is_empty() {
            return;
        }
        let Some(branch) = self.status.as_ref().map(|st| st.branch.clone()) else {
            self.error = Some("still loading — try again in a moment".into());
            return;
        };
        match self.queue.submit(AsyncJob::Publish {
            remote: "origin".to_string(),
            url,
            branch,
        }) {
            Ok(()) => {
                self.mode = self.sync_return;
                self.draft.clear();
                self.draft_cursor = 0;
                self.syncing = Some("publishing…".into());
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Refresh upstream tracking state (cheap local reads, no network).
    fn reload_sync(&mut self) {
        if let Err(e) = self.queue.submit(AsyncJob::LoadSync) {
            self.error = Some(e.to_string());
        }
    }

    /// Drain finished jobs without blocking (event loop calls this every
    /// frame). A [`AsyncResult::MutationDone`] invalidates the snapshot, so
    /// it refreshes status and reloads the focused diff rather than patching
    /// either incrementally.
    pub fn poll(&mut self) {
        while let Some(result) = self.queue.try_recv() {
            self.apply(result);
        }
        self.maybe_load_diff();
    }

    fn apply(&mut self, result: AsyncResult) {
            match result {
                AsyncResult::Status(st) => {
                    self.status = Some(st);
                    self.rebuild_file_list();
                    self.error = None;
                }
                AsyncResult::Diff(d) => {
                    // Drop overtaken loads: only the latest target counts
                    // (jobs run FIFO, so a newer LoadDiff may follow).
                    if self.diff_for.as_ref().is_some_and(|(p, _)| *p == d.path) {
                        if d.hunks.is_empty() && !self.diff_whole_file {
                            // Listed but no content diff (e.g. mode-only
                            // change): show the whole file automatically.
                            self.fallback_whole_file = true;
                            self.diff = None;
                            self.hunk = 0;
                            self.diff_scroll = 0;
                            self.diff_whole_file = true;
                            let job = AsyncJob::LoadFile { path: d.path };
                            if let Err(e) = self.queue.submit(job) {
                                self.error = Some(e.to_string());
                            }
                        } else {
                            self.diff = Some(d);
                            self.hunk = self.hunk.min(self.hunk_count().saturating_sub(1));
                            // The fresh content may be shorter: keep the
                            // offset inside the new content.
                            self.diff_scroll =
                                self.diff_scroll.min(self.diff_preview_max());
                        }
                    }
                }
                AsyncResult::Branches(b) => {
                    self.branch_selected = self.branch_selected.min(b.len().saturating_sub(1));
                    self.branches = Some(b);
                }
                AsyncResult::Log(entries) => {
                    self.log_scroll = self.log_scroll.min(entries.len().saturating_sub(1) as u16);
                    self.log = Some(entries);
                }
                AsyncResult::Stash(entries) => {
                    self.stash_selected = self.stash_selected.min(entries.len().saturating_sub(1));
                    self.stash = Some(entries);
                }
                AsyncResult::SyncStatus(st) => {
                    self.sync = Some(st);
                }
                AsyncResult::GeneratedMessage(msg) => {
                    self.generating = false;
                    // The user may have Esc'd while the network call was in
                    // flight: only fill the open commit box.
                    if self.mode == Mode::Committing {
                        self.draft = msg;
                        self.move_draft_end();
                        self.error = None;
                    }
                }
                AsyncResult::MutationDone => {
                    self.syncing = None;
                    self.refresh();
                    self.reload_diff();
                    self.reload_branches();
                    self.reload_log();
                    self.reload_stash();
                    self.reload_sync();
                }
                AsyncResult::Error(e) => {
                    self.syncing = None;
                    self.generating = false;
                    // EmptyCommit on its own doesn't say how to fix it.
                    self.error = Some(match e {
                        GitError::EmptyCommit => {
                            "nothing to commit: press space on a file to stage it, then c to commit"
                                .into()
                        }
                        _ => e.to_string(),
                    });
                }
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct Fixture {
        _dir: tempfile::TempDir,
        app: App,
    }

    fn init_repo() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test User").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        (dir, repo)
    }

    fn commit_file(repo: &git2::Repository, path: &str, contents: &str, msg: &str) {
        let full = repo.workdir().unwrap().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(h) => vec![h.peel_to_commit().unwrap()],
            Err(_) => vec![],
        };
        let refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &refs)
            .unwrap();
    }

    /// Settle until `pred` holds on the latest status (drives `poll`).
    fn wait_for(app: &mut App, mut pred: impl FnMut(&RepoStatus) -> bool) -> RepoStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if let Some(st) = app.status() {
                if pred(st) {
                    return st.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out; last: {:?}",
                app.status()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_error(app: &mut App) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if let Some(e) = app.error() {
                return e.to_string();
            }
            assert!(Instant::now() < deadline, "timed out waiting for error");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Repo with each named file committed then dirtied (all Unstaged).
    fn harness(names: &[&str]) -> Fixture {
        let (dir, repo) = init_repo();
        for name in names {
            commit_file(&repo, name, "base\n", "init");
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(repo.workdir().unwrap().join(name))
                .unwrap();
            f.write_all(b"dirty\n").unwrap();
        }
        let path = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let mut fx = Fixture {
            _dir: dir,
            app: App::new(JobQueue::spawn(path).unwrap()),
        };
        let n = names.len();
        wait_for(&mut fx.app, |st| st.files.len() == n);
        fx
    }

    #[test]
    fn left_collapses_only_deepest_parent() {
        let mut fx = harness(&["src/a.rs", "src/nested/b.rs", "z.txt"]);
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.selected_file().unwrap().path, "src/nested/b.rs");
        fx.app.on_key(KeyCode::Left);
        assert!(
            fx.app.is_collapsed("src/nested"),
            "deepest parent should collapse"
        );
        assert!(
            !fx.app.is_collapsed("src"),
            "sibling subtree must stay expanded"
        );
    }

    #[test]
    fn folders_collapse_expand_and_skip_hidden_files() {
        let mut fx = harness(&["src/a.rs", "src/nested/b.rs", "z.txt"]);
        // Left collapses src/; the cursor stays as a hidden anchor so the
        // `▶ src/` header takes the highlight (cursor is on the folder).
        fx.app.on_key(KeyCode::Left);
        assert!(fx.app.is_collapsed("src"));
        // Down escapes the anchor to the next visible file.
        fx.app.on_key(KeyCode::Down);
        assert_eq!(
            fx.app.selected_file().unwrap().path,
            "z.txt",
            "collapsed src/ must be skipped when moving down"
        );
        // Up from below steps onto the folded header (cursor back on the
        // folder), never showing its hidden files as rows.
        fx.app.on_key(KeyCode::Up);
        assert_eq!(
            fx.app.selected_file().unwrap().path,
            "src/nested/b.rs",
            "moving up must stop on the collapsed header's anchor"
        );
        assert!(
            fx.app.is_hidden_index(fx.app.selected()),
            "anchor must be hidden so the header takes the highlight"
        );
        // Right on the header expands it back.
        fx.app.on_key(KeyCode::Right);
        assert!(
            !fx.app.is_collapsed("src"),
            "Right on the header must expand it"
        );
        assert_eq!(
            fx.app.selected_file().unwrap().path,
            "src/nested/b.rs",
            "expand restores the anchored file"
        );
        fx.app.on_key(KeyCode::Left);
        assert!(
            fx.app.is_collapsed("src/nested"),
            "Left collapses the deepest parent"
        );
        fx.app.on_key(KeyCode::Right);
        assert!(
            !fx.app.is_collapsed("src/nested"),
            "Right on the hidden anchor expands it back"
        );
        assert_eq!(
            fx.app.selected_file().unwrap().path,
            "src/nested/b.rs",
            "expand + collapse + expand restores visibility"
        );
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        fx.app.on_key(KeyCode::Esc);
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.selected_file().unwrap().path, "z.txt");
    }

    #[test]
    fn finder_reveals_a_file_inside_collapsed_ancestors() {
        let mut fx = harness(&["src/a.rs", "src/nested/b.rs"]);
        // Collapse src/ (both files fold away; the cursor stays anchored).
        fx.app.on_key(KeyCode::Left);
        assert!(fx.app.is_collapsed("src"));
        fx.app.on_key(KeyCode::Char('/'));
        for c in "b.rs".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.selected_file().unwrap().path, "src/nested/b.rs");
        assert!(
            !fx.app.is_collapsed("src"),
            "jumping to a match must expand its ancestors"
        );
        fx.app.on_key(KeyCode::Down);
        assert_eq!(
            fx.app.selected_file().unwrap().path,
            "src/nested/b.rs",
            "at the end of the list Down clamps"
        );
    }

    #[test]
    fn up_escapes_hidden_anchor_to_nearest_visible() {
        let mut fx = harness(&["src/a.rs", "src/nested/b.rs", "z.txt"]);
        // Move onto b.rs, then collapse its deepest parent: the cursor is
        // now a hidden anchor on the `▶ nested/` header.
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.selected_file().unwrap().path, "src/nested/b.rs");
        fx.app.on_key(KeyCode::Left);
        assert!(fx.app.is_collapsed("src/nested"));
        // Up escapes to the nearest visible file above, not into hiding.
        fx.app.on_key(KeyCode::Up);
        assert_eq!(fx.app.selected_file().unwrap().path, "src/a.rs");
        // Down jumps back over the folded region to z.txt.
        fx.app.on_key(KeyCode::Down);
        fx.app.on_key(KeyCode::Down);
        assert_eq!(fx.app.selected_file().unwrap().path, "z.txt");
    }

    #[test]
    fn selection_moves_with_jk_and_clamps() {
        let mut fx = harness(&["a.txt", "b.txt", "c.txt"]);
        assert_eq!(fx.app.selected(), 0);
        fx.app.on_key(KeyCode::Char('j'));
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.selected(), 2);
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.selected(), 2, "clamps at end");
        fx.app.on_key(KeyCode::Char('k'));
        assert_eq!(fx.app.selected(), 1);
        fx.app.on_key(KeyCode::Char('k'));
        fx.app.on_key(KeyCode::Char('k'));
        assert_eq!(fx.app.selected(), 0, "clamps at start");
    }

    #[test]
    fn space_on_unstaged_stages_the_file() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char(' '));
        let st = wait_for(&mut fx.app, |st| {
            st.files
                .iter()
                .any(|e| e.path == "a.txt" && e.state == FileState::Staged)
        });
        let entry = st.files.iter().find(|e| e.path == "a.txt").unwrap();
        assert_eq!(entry.state, FileState::Staged);
    }

    #[test]
    fn space_on_staged_unstages_the_file() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char(' '));
        wait_for(&mut fx.app, |st| {
            st.files
                .iter()
                .any(|e| e.path == "a.txt" && e.state == FileState::Staged)
        });
        fx.app.on_key(KeyCode::Char(' '));
        let st = wait_for(&mut fx.app, |st| {
            st.files
                .iter()
                .any(|e| e.path == "a.txt" && e.state == FileState::Unstaged)
        });
        let entry = st.files.iter().find(|e| e.path == "a.txt").unwrap();
        assert_eq!(entry.state, FileState::Unstaged);
    }

    #[test]
    fn space_on_collapsed_dir_stages_everything_under_it() {
        let mut fx = harness(&["src/a.rs", "src/nested/b.rs", "z.txt"]);
        // Collapse src/; the cursor stays on hidden src/a.rs, so the
        // header takes the highlight and Space acts on the whole dir.
        fx.app.on_key(KeyCode::Left);
        assert_eq!(fx.app.selected_file().unwrap().path, "src/a.rs");
        fx.app.on_key(KeyCode::Char(' '));
        let st = wait_for(&mut fx.app, |st| {
            let under: Vec<_> = st
                .files
                .iter()
                .filter(|e| e.path.starts_with("src/"))
                .collect();
            under.len() == 2 && under.iter().all(|e| e.state == FileState::Staged)
        });
        let outside = st.files.iter().find(|e| e.path == "z.txt").unwrap();
        assert_eq!(
            outside.state,
            FileState::Unstaged,
            "files outside the dir must be left alone"
        );
    }

    #[test]
    fn space_on_collapsed_dir_unstages_when_everything_staged() {
        let mut fx = harness(&["src/a.rs", "src/nested/b.rs"]);
        fx.app.on_key(KeyCode::Left);
        fx.app.on_key(KeyCode::Char(' '));
        wait_for(&mut fx.app, |st| {
            st.files.len() == 2 && st.files.iter().all(|e| e.state == FileState::Staged)
        });
        // Everything under src/ is staged, so Space unstages the whole dir.
        fx.app.on_key(KeyCode::Char(' '));
        let st = wait_for(&mut fx.app, |st| {
            st.files.len() == 2 && st.files.iter().all(|e| e.state == FileState::Unstaged)
        });
        assert!(
            st.files.iter().all(|e| e.state == FileState::Unstaged),
            "got: {:?}",
            st.files
        );
    }

    #[test]
    fn space_on_collapsed_dir_with_conflict_reports_error() {
        let mut fx = harness(&[]);
        fx.app.status = Some(RepoStatus {
            branch: "main".into(),
            head_summary: "x".into(),
            files: vec![
                StatusEntry {
                    path: "src/a.rs".into(),
                    state: FileState::Unstaged,
                },
                StatusEntry {
                    path: "src/b.rs".into(),
                    state: FileState::Conflicted,
                },
            ],
            tracked_files: vec!["src/a.rs".into(), "src/b.rs".into()],
        });
        fx.app.rebuild_file_list();
        fx.app.set_collapsed("src", true);
        fx.app.on_key(KeyCode::Char(' '));
        let err = fx.app.error().expect("expected conflict error");
        assert!(err.contains("conflicted"), "got: {err}");
        assert!(err.contains("src/b.rs"), "got: {err}");
    }

    #[test]
    fn space_on_collapsed_dir_with_nothing_to_stage_reports_error() {
        let mut fx = harness(&[]);
        fx.app.status = Some(RepoStatus {
            branch: "main".into(),
            head_summary: "x".into(),
            files: vec![],
            tracked_files: vec!["src/a.rs".into()],
        });
        fx.app.rebuild_file_list();
        fx.app.set_collapsed("src", true);
        fx.app.on_key(KeyCode::Char(' '));
        let err = fx.app.error().expect("expected nothing-to-stage error");
        assert!(err.contains("under src/"), "got: {err}");
    }

    #[test]
    fn space_with_no_files_does_nothing() {
        let mut fx = harness(&[]);
        fx.app.on_key(KeyCode::Char(' '));
        assert!(fx.app.error().is_none());
        assert_eq!(fx.app.selected(), 0);
    }

    #[test]
    fn enter_with_no_files_does_not_open_fullscreen() {
        let mut fx = harness(&[]);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::Normal);
    }

    #[test]
    fn space_on_conflicted_reports_error() {
        let mut fx = harness(&[]);
        fx.app.status = Some(RepoStatus {
            branch: "main".into(),
            head_summary: "x".into(),
            files: vec![StatusEntry {
                path: "a.txt".into(),
                state: FileState::Conflicted,
            }],
            tracked_files: vec!["a.txt".into()],
        });
        fx.app.rebuild_file_list();
        fx.app.on_key(KeyCode::Char(' '));
        assert!(fx.app.error().is_some(), "expected conflict error");
    }

    #[test]
    fn c_opens_modal_esc_cancels() {
        let mut fx = harness(&["a.txt"]);
        assert_eq!(fx.app.mode(), Mode::Normal);
        fx.app.on_key(KeyCode::Char('c'));
        assert_eq!(fx.app.mode(), Mode::Committing);
        fx.app.on_key(KeyCode::Char('x'));
        assert_eq!(fx.app.draft(), "x");
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::Normal);
        assert_eq!(fx.app.draft(), "");
    }

    #[test]
    fn enter_in_modal_commits_and_returns_to_normal() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char(' '));
        wait_for(&mut fx.app, |st| {
            st.files
                .iter()
                .any(|e| e.path == "a.txt" && e.state == FileState::Staged)
        });
        fx.app.on_key(KeyCode::Char('c'));
        for c in "my commit".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::Normal);
        let st = wait_for(&mut fx.app, |st| st.files.is_empty());
        assert_eq!(st.head_summary, "my commit");
    }

    #[test]
    fn enter_with_empty_message_does_not_submit() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::Committing, "stays in modal");
    }

    #[test]
    fn commit_message_edits_mid_text_with_cursor() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        for c in "hello".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        assert_eq!(fx.app.draft_cursor(), 5);
        fx.app.on_key(KeyCode::Left);
        fx.app.on_key(KeyCode::Left);
        assert_eq!(fx.app.draft_cursor(), 3);
        // Backspace deletes before the cursor: "hello" -> "helo".
        fx.app.on_key(KeyCode::Backspace);
        assert_eq!(fx.app.draft(), "helo");
        assert_eq!(fx.app.draft_cursor(), 2);
        // Typing inserts at the cursor: "helo" -> "heXlo".
        fx.app.on_key(KeyCode::Char('X'));
        assert_eq!(fx.app.draft(), "heXlo");
        assert_eq!(fx.app.draft_cursor(), 3);
        // Delete removes under the cursor: "heXlo" -> "heXo".
        fx.app.on_key(KeyCode::Delete);
        assert_eq!(fx.app.draft(), "heXo");
        // Home/End jump; typing at the front and back works.
        fx.app.on_key(KeyCode::Home);
        fx.app.on_key(KeyCode::Char('!'));
        assert_eq!(fx.app.draft(), "!heXo");
        fx.app.on_key(KeyCode::End);
        fx.app.on_key(KeyCode::Char('?'));
        assert_eq!(fx.app.draft(), "!heXo?");
        assert_eq!(fx.app.draft_cursor(), 6);
    }

    #[test]
    fn commit_cursor_clamps_at_both_ends() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        fx.app.on_key(KeyCode::Char('a'));
        fx.app.on_key(KeyCode::Left);
        fx.app.on_key(KeyCode::Left);
        assert_eq!(fx.app.draft_cursor(), 0, "clamps at start");
        fx.app.on_key(KeyCode::Backspace);
        assert_eq!(fx.app.draft(), "a", "nothing to delete at start");
        fx.app.on_key(KeyCode::Right);
        fx.app.on_key(KeyCode::Right);
        assert_eq!(fx.app.draft_cursor(), 1, "clamps at end");
        fx.app.on_key(KeyCode::Delete);
        assert_eq!(fx.app.draft(), "a", "nothing to delete at end");
    }

    #[test]
    fn failing_job_surfaces_error_display() {
        let mut fx = harness(&["a.txt"]);
        // Commit with nothing staged -> EmptyCommit error via worker.
        fx.app.on_key(KeyCode::Char('c'));
        for c in "nothing".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        let err = wait_for_error(&mut fx.app);
        assert!(err.contains("nothing to commit"), "got: {err}");
    }

    #[test]
    fn empty_commit_error_tells_user_to_stage_first_and_clears_on_next_action() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        for c in "oops".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        let err = wait_for_error(&mut fx.app);
        assert!(err.contains("nothing to commit"), "got: {err}");
        assert!(err.contains("space"), "should hint at staging, got: {err}");
        // A new action dismisses the stale error.
        fx.app.on_key(KeyCode::Char('j'));
        assert!(fx.app.error().is_none());
    }

    #[test]
    fn right_arrow_no_longer_leaves_the_file_list_enter_opens_fullscreen() {
        let mut fx = harness(&["a.txt"]);
        assert_eq!(fx.app.focus(), Focus::Status);
        // Vertical-only navigation: Right does nothing now.
        fx.app.on_key(KeyCode::Right);
        assert_eq!(fx.app.focus(), Focus::Status);
        assert_eq!(fx.app.mode(), Mode::Normal);
        // Enter opens the file fullscreen; Esc closes back.
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::Normal);
        // Left still jumps back to the file list.
        fx.app.on_key(KeyCode::Char('2'));
        assert_eq!(fx.app.focus(), Focus::Branches);
        fx.app.on_key(KeyCode::Left);
        assert_eq!(fx.app.focus(), Focus::Status);
    }

    #[test]
    fn up_down_arrows_scroll_diff_line_by_line_without_moving_hunk() {
        let mut fx = two_hunk_fixture();
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        assert_eq!(fx.app.diff_scroll(), 0);
        fx.app.on_key(KeyCode::Down);
        assert_eq!(fx.app.diff_scroll(), 1);
        assert_eq!(
            fx.app.hunk(),
            0,
            "arrow scroll must not move hunk selection"
        );
        fx.app.on_key(KeyCode::Down);
        assert_eq!(fx.app.diff_scroll(), 2);
        fx.app.on_key(KeyCode::Up);
        assert_eq!(fx.app.diff_scroll(), 1);
        // j/k still jump by hunk (and snap scroll to the hunk top).
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.hunk(), 1);
        assert!(fx.app.diff_scroll() > 1);
    }

    /// Repo with `a.txt` dirtied and `b.txt` left clean.
    fn clean_fixture() -> (tempfile::TempDir, App) {
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "a\n", "init");
        commit_file(&repo, "b.txt", "b\n", "init");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(repo.workdir().unwrap().join("a.txt"))
                .unwrap();
            f.write_all(b"dirty\n").unwrap();
        }
        let path = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let app = App::new(JobQueue::spawn(path).unwrap());
        (dir, app)
    }

    /// Settle until the browsable tree holds `n` entries.
    fn wait_for_files(app: &mut App, n: usize) -> Vec<StatusEntry> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if app.file_list().len() == n {
                return app.file_list().to_vec();
            }
            assert!(
                Instant::now() < deadline,
                "timed out; last: {:?}",
                app.file_list()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn clean_tracked_files_join_the_tree_after_changed_ones() {
        let (_dir, mut app) = clean_fixture();
        let list = wait_for_files(&mut app, 2);
        assert_eq!(list[0].path, "a.txt");
        assert_eq!(list[0].state, FileState::Unstaged);
        assert_eq!(list[1].path, "b.txt");
        assert_eq!(list[1].state, FileState::Clean);
    }

    #[test]
    fn enter_on_clean_file_loads_whole_file() {
        use git_tui_core::diff::LineKind;
        let (_dir, mut app) = clean_fixture();
        wait_for_files(&mut app, 2);
        fx_select(&mut app);
        let d = wait_for_diff(&mut app, "b.txt");
        assert!(app.diff_whole_file());
        let texts: Vec<&str> = d
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(texts, vec!["b"]);
        assert!(d
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .all(|l| l.kind == LineKind::Context));
    }

    /// Move selection to the clean `b.txt` (index 1 in the clean fixture).
    fn fx_select(app: &mut App) {
        app.on_key(KeyCode::Char('j'));
        assert_eq!(app.selected_file().unwrap().path, "b.txt");
    }

    #[test]
    fn space_on_clean_file_reports_error() {
        let (_dir, mut app) = clean_fixture();
        wait_for_files(&mut app, 2);
        fx_select(&mut app);
        app.on_key(KeyCode::Char(' '));
        let err = app.error().unwrap_or("").to_string();
        assert!(err.contains("unchanged"), "got: {err}");
    }

    #[test]
    fn space_on_whole_file_hunk_errors_instead_of_staging() {
        let (_dir, mut app) = clean_fixture();
        wait_for_files(&mut app, 2);
        fx_select(&mut app);
        app.on_key(KeyCode::Enter);
        assert_eq!(app.mode(), Mode::FullDiff);
        wait_for_diff(&mut app, "b.txt");
        app.on_key(KeyCode::Char(' '));
        let err = app.error().unwrap_or("").to_string();
        assert!(err.contains("nothing to stage"), "got: {err}");
    }

    #[test]
    fn enter_on_changed_file_shows_diff_automatically() {
        let mut fx = harness(&["a.txt"]);
        // Changed file: the loaded view is the diff, not the whole file.
        let d = wait_for_diff(&mut fx.app, "a.txt");
        assert!(!fx.app.diff_whole_file());
        assert!(!d.hunks.is_empty());
        // Fullscreen shows that same diff.
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        assert!(!fx.app.diff_whole_file());
        assert_eq!(fx.app.diff().unwrap().path, "a.txt");
    }

    #[test]
    fn enter_on_clean_file_shows_whole_file_automatically() {
        use git_tui_core::diff::LineKind;
        let (_dir, mut app) = clean_fixture();
        wait_for_files(&mut app, 2);
        fx_select(&mut app);
        // No changes: whole workdir file loads on its own.
        let d = wait_for_diff(&mut app, "b.txt");
        assert!(app.diff_whole_file());
        assert!(d
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .all(|l| { l.kind == LineKind::Context }));
        // Fullscreen shows that same whole-file view.
        app.on_key(KeyCode::Enter);
        assert_eq!(app.mode(), Mode::FullDiff);
        assert!(app.diff_whole_file());
        assert_eq!(app.diff().unwrap().path, "b.txt");
    }

    #[test]
    fn empty_diff_falls_back_to_whole_file_automatically() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "hello\nworld\n", "init");
        // Mode-only change: status lists the file, but the content diff
        // has zero hunks — the viewer must show the whole file, not
        // "(no changes)".
        std::fs::set_permissions(
            repo.workdir().unwrap().join("a.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let path = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let mut fx = Fixture {
            _dir: dir,
            app: App::new(JobQueue::spawn(path).unwrap()),
        };
        wait_for(&mut fx.app, |st| st.files.iter().any(|e| e.path == "a.txt"));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            fx.app.poll();
            if fx.app.diff_whole_file() {
                if let Some(d) = fx.app.diff() {
                    if d.path == "a.txt" && !d.hunks.is_empty() {
                        break;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "whole-file fallback never loaded; whole={} diff={:?}",
                fx.app.diff_whole_file(),
                fx.app.diff()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let texts: Vec<&str> = fx
            .app
            .diff()
            .unwrap()
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(texts, vec!["hello", "world"]);
    }

    #[test]
    fn q_in_modal_types_literal_and_quits_after_esc() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        fx.app.on_key(KeyCode::Char('q'));
        assert!(!fx.app.should_quit());
        assert_eq!(fx.app.draft(), "q");
        fx.app.on_key(KeyCode::Char('Q'));
        assert!(!fx.app.should_quit(), "Q in modal must type, not quit");
        assert_eq!(fx.app.draft(), "qQ");
        fx.app.on_key(KeyCode::Esc);
        // Lowercase `q` no longer quits the app (workspace closes the
        // project instead); uppercase `Q` quits everything.
        fx.app.on_key(KeyCode::Char('q'));
        assert!(!fx.app.should_quit());
        fx.app.on_key(KeyCode::Char('Q'));
        assert!(fx.app.should_quit());
    }

    /// Repo with an `origin` remote (bare, on disk) that nothing has been
    /// pushed to yet: pressing `P` must offer to set the upstream.
    fn sync_harness() -> (tempfile::TempDir, tempfile::TempDir, App) {
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "a\n", "init");
        let origin_dir = tempfile::TempDir::new().unwrap();
        let bare = origin_dir.path().join("origin.git");
        git2::Repository::init_bare(&bare).unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
        let path = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let app = App::new(JobQueue::spawn(path).unwrap());
        (dir, origin_dir, app)
    }

    /// Settle until upstream tracking state arrives (preloaded at startup).
    fn wait_for_sync(app: &mut App) -> git_tui_core::sync::SyncStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if let Some(st) = app.sync() {
                return st.clone();
            }
            assert!(Instant::now() < deadline, "timed out waiting for sync");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn p_pull_failure_surfaces_error_and_clears_syncing() {
        // Harness repos have no remote (and dirty files, so `git pull`
        // fails one way or another depending on the user's pull config).
        // What matters: the failure lands in the error line and the
        // "pulling…" indicator clears instead of sticking forever.
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('p'));
        assert_eq!(fx.app.syncing(), Some("pulling…"));
        let err = wait_for_error(&mut fx.app);
        assert!(!err.is_empty(), "expected a pull failure message");
        assert!(fx.app.syncing().is_none());
    }

    #[test]
    fn push_without_remotes_opens_publish_modal_and_enter_publishes() {
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "a\n", "init");
        let origin_dir = tempfile::TempDir::new().unwrap();
        let bare = origin_dir.path().join("origin.git");
        git2::Repository::init_bare(&bare).unwrap();
        let path = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let mut app = App::new(JobQueue::spawn(path).unwrap());
        let st = wait_for_sync(&mut app);
        assert!(st.remotes.is_empty());
        app.on_key(KeyCode::Char('P'));
        assert_eq!(app.mode(), Mode::SetRemote);
        for c in bare.to_str().unwrap().chars() {
            app.on_key(KeyCode::Char(c));
        }
        app.on_key(KeyCode::Enter);
        assert_eq!(app.mode(), Mode::Normal);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if app.sync().is_some_and(|s| s.upstream.is_some()) {
                break;
            }
            assert!(Instant::now() < deadline, "publish never recorded upstream");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.sync().unwrap().upstream.as_deref(), Some("origin/main"));
        let remote = git2::Repository::open_bare(&bare).unwrap();
        assert!(remote.find_reference("refs/heads/main").is_ok());
        drop(dir);
    }

    #[test]
    fn push_without_upstream_opens_set_upstream_modal() {
        let (_dir, _origin, mut app) = sync_harness();
        wait_for_sync(&mut app);
        app.on_key(KeyCode::Char('P'));
        assert_eq!(app.mode(), Mode::SetUpstream);
        assert_eq!(app.draft(), "origin");
        app.on_key(KeyCode::Enter);
        assert_eq!(app.mode(), Mode::Normal);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if app.sync().is_some_and(|s| s.upstream.is_some()) {
                break;
            }
            assert!(Instant::now() < deadline, "push -u never recorded upstream");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.sync().unwrap().upstream.as_deref(), Some("origin/main"));
    }

    #[test]
    fn push_with_upstream_pushes_directly_without_modal() {
        let (_dir, _origin, mut app) = sync_harness();
        wait_for_sync(&mut app);
        // Establish the upstream first.
        app.on_key(KeyCode::Char('P'));
        assert_eq!(app.mode(), Mode::SetUpstream);
        app.on_key(KeyCode::Enter);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if app.sync().is_some_and(|s| s.upstream.is_some()) {
                break;
            }
            assert!(Instant::now() < deadline, "push -u never recorded upstream");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.syncing().is_none());
        // A fresh `P` now pushes straight away, no modal.
        app.on_key(KeyCode::Char('P'));
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.syncing(), Some("pushing…"));
    }

    #[test]
    fn esc_in_sync_modal_returns_without_syncing() {
        let (_dir, _origin, mut app) = sync_harness();
        wait_for_sync(&mut app);
        app.on_key(KeyCode::Char('P'));
        assert_eq!(app.mode(), Mode::SetUpstream);
        app.on_key(KeyCode::Esc);
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.syncing().is_none());
        assert!(app.sync().unwrap().upstream.is_none());
    }

    #[test]
    fn sync_modal_from_fullscreen_returns_to_fullscreen() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        // Harness repos have no remotes: `P` opens the publish modal.
        fx.app.on_key(KeyCode::Char('P'));
        assert_eq!(fx.app.mode(), Mode::SetRemote);
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
    }

    /// Two-hunk fixture: 40 lines, changes at line 5 and line 35.
    fn two_hunk_fixture() -> Fixture {
        let (dir, repo) = init_repo();
        let base = (1..=40).map(|i| format!("line {i}\n")).collect::<String>();
        commit_file(&repo, "a.txt", &base, "init");
        let dirty = base.replacen("line 5\n", "line 5 CHANGED\n", 1).replacen(
            "line 35\n",
            "line 35 CHANGED\n",
            1,
        );
        std::fs::write(repo.workdir().unwrap().join("a.txt"), &dirty).unwrap();
        let path = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let mut fx = Fixture {
            _dir: dir,
            app: App::new(JobQueue::spawn(path).unwrap()),
        };
        wait_for(&mut fx.app, |st| st.files.iter().any(|e| e.path == "a.txt"));
        wait_for_diff(&mut fx.app, "a.txt");
        fx
    }

    fn wait_for_diff(app: &mut App, path: &str) -> git_tui_core::diff::FileDiff {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if let Some(d) = app.diff() {
                if d.path == path && !d.hunks.is_empty() {
                    return d.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for diff of {path}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn enter_esc_then_walk_files_in_both_directions() {
        let mut fx = harness(&["a.txt", "b.txt", "c.txt"]);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::Normal);
        for expected in ["b.txt", "c.txt"] {
            fx.app.on_key(KeyCode::Char('j'));
            assert_eq!(
                fx.app.selected_file().unwrap().path,
                expected,
                "down-walk broke after Esc"
            );
        }
        for expected in ["b.txt", "a.txt"] {
            fx.app.on_key(KeyCode::Char('k'));
            assert_eq!(
                fx.app.selected_file().unwrap().path,
                expected,
                "up-walk broke after Esc"
            );
        }
        // The preview must follow the cursor again too.
        let d = wait_for_diff(&mut fx.app, "a.txt");
        assert_eq!(d.path, "a.txt");
    }

    #[test]
    fn slash_opens_finder_and_esc_cancels() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('/'));
        assert_eq!(fx.app.mode(), Mode::FindFile);
        assert_eq!(fx.app.draft(), "");
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::Normal);
    }

    #[test]
    fn finder_typing_filters_and_enter_jumps_to_match() {
        let mut fx = harness(&["a.txt", "b.txt"]);
        fx.app.on_key(KeyCode::Char('/'));
        for c in "b".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        assert_eq!(fx.app.finder_matches(), vec![1]);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::Normal);
        assert_eq!(fx.app.focus(), Focus::Status);
        assert_eq!(fx.app.selected(), 1);
        assert_eq!(fx.app.selected_file().unwrap().path, "b.txt");
    }

    #[test]
    fn finder_arrows_move_and_clamp() {
        let mut fx = harness(&["a.txt", "b.txt", "c.txt"]);
        fx.app.on_key(KeyCode::Char('/'));
        assert_eq!(fx.app.finder_matches().len(), 3);
        fx.app.on_key(KeyCode::Down);
        fx.app.on_key(KeyCode::Down);
        assert_eq!(fx.app.finder_cursor(), 2);
        fx.app.on_key(KeyCode::Down);
        assert_eq!(fx.app.finder_cursor(), 2, "clamps at end");
        fx.app.on_key(KeyCode::Up);
        assert_eq!(fx.app.finder_cursor(), 1);
    }

    #[test]
    fn finder_enter_with_no_matches_stays_open() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('/'));
        for c in "zzz".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        assert!(fx.app.finder_matches().is_empty());
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FindFile);
    }

    #[test]
    fn slash_in_fullscreen_opens_finder_and_esc_returns_to_fullscreen() {
        let mut fx = harness(&["a.txt", "b.txt"]);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        fx.app.on_key(KeyCode::Char('/'));
        assert_eq!(fx.app.mode(), Mode::FindFile);
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
    }

    #[test]
    fn finder_enter_in_fullscreen_jumps_to_match_and_stays_fullscreen() {
        let mut fx = harness(&["a.txt", "b.txt"]);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        fx.app.on_key(KeyCode::Char('/'));
        for c in "b".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        assert_eq!(fx.app.finder_matches(), vec![1]);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        assert_eq!(fx.app.selected_file().unwrap().path, "b.txt");
        let d = wait_for_diff(&mut fx.app, "b.txt");
        assert_eq!(d.path, "b.txt");
    }

    #[test]
    fn tab_cycles_focus_through_all_panels() {
        let mut fx = harness(&["a.txt"]);
        assert_eq!(fx.app.focus(), Focus::Status);
        fx.app.on_key(KeyCode::Tab);
        assert_eq!(fx.app.focus(), Focus::Branches);
        fx.app.on_key(KeyCode::Tab);
        assert_eq!(fx.app.focus(), Focus::Log);
        fx.app.on_key(KeyCode::Tab);
        assert_eq!(fx.app.focus(), Focus::Stash);
        // Tab skips the right-side Diff preview: Stash wraps to Status.
        fx.app.on_key(KeyCode::Tab);
        assert_eq!(fx.app.focus(), Focus::Status);
        // Diff is reached via `5` / Shift+Right, not via Tab.
        fx.app.on_key(KeyCode::Char('4'));
        assert_eq!(fx.app.focus(), Focus::Stash);
        fx.app.on_key(KeyCode::Char('3'));
        assert_eq!(fx.app.focus(), Focus::Log);
        fx.app.on_key(KeyCode::Char('2'));
        assert_eq!(fx.app.focus(), Focus::Branches);
        fx.app.on_key(KeyCode::Char('1'));
        assert_eq!(fx.app.focus(), Focus::Status);
        fx.app.on_key(KeyCode::Char('5'));
        assert_eq!(fx.app.focus(), Focus::Diff);
        // Tab from Diff drops back to the file list.
        fx.app.on_key(KeyCode::Tab);
        assert_eq!(fx.app.focus(), Focus::Status);
    }

    #[test]
    fn shift_right_from_files_focuses_diff_preview() {
        let mut fx = harness(&["a.txt"]);
        assert_eq!(fx.app.focus(), Focus::Status);
        // Plain Right expands folders and must stay in the file list.
        fx.app.on_key(KeyCode::Right);
        assert_eq!(fx.app.focus(), Focus::Status);
        // Shift+Right jumps to the right-side diff tab.
        fx.app.on_key_with_modifiers(KeyCode::Right, true);
        assert_eq!(fx.app.focus(), Focus::Diff);
        // Shift+Left (Left already owns focus_status) jumps back.
        fx.app.on_key_with_modifiers(KeyCode::Left, true);
        assert_eq!(fx.app.focus(), Focus::Status);
    }

    #[test]
    fn diff_focus_scrolls_preview_without_moving_selection() {
        let mut fx = two_hunk_fixture();
        assert_eq!(fx.app.focus(), Focus::Status);
        fx.app.on_key(KeyCode::Char('5'));
        assert_eq!(fx.app.focus(), Focus::Diff);
        let sel = fx.app.selected();
        assert_eq!(fx.app.diff_scroll(), 0);
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.diff_scroll(), 1);
        assert_eq!(fx.app.selected(), sel, "file cursor must not move");
        fx.app.on_key(KeyCode::Down);
        assert_eq!(fx.app.diff_scroll(), 2);
        fx.app.on_key(KeyCode::Char('k'));
        assert_eq!(fx.app.diff_scroll(), 1);
        // Left jumps back to the file list (focus_status owns Left).
        fx.app.on_key(KeyCode::Left);
        assert_eq!(fx.app.focus(), Focus::Status);
        // Enter from the preview opens fullscreen too.
        fx.app.on_key(KeyCode::Char('5'));
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
    }

    #[test]
    fn diff_preview_scroll_clamps_at_content_end() {
        let mut fx = two_hunk_fixture();
        fx.app.on_key(KeyCode::Char('5'));
        assert_eq!(fx.app.focus(), Focus::Diff);
        let max = crate::ui::diff_unified_len(fx.app.diff().unwrap())
            .saturating_sub(1)
            .min(u16::MAX as usize) as u16;
        assert!(max > 2, "fixture must have scrollable content");
        for _ in 0..500 {
            fx.app.on_key(KeyCode::Char('j'));
        }
        assert_eq!(fx.app.diff_scroll(), max);
        for _ in 0..10 {
            fx.app.on_key(KeyCode::PageDown);
        }
        assert_eq!(fx.app.diff_scroll(), max, "PageDown must clamp too");
        for _ in 0..500 {
            fx.app.on_key(KeyCode::Char('k'));
        }
        assert_eq!(fx.app.diff_scroll(), 0);
        for _ in 0..10 {
            fx.app.on_key(KeyCode::PageUp);
        }
        assert_eq!(fx.app.diff_scroll(), 0, "PageUp must not underflow");
    }

    #[test]
    fn fullscreen_scroll_clamps_at_content_end() {
        let mut fx = two_hunk_fixture();
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        let max = crate::ui::diff_rows(fx.app.diff().unwrap())
            .len()
            .saturating_sub(1)
            .min(u16::MAX as usize) as u16;
        assert!(max > 2, "fixture must have scrollable content");
        for _ in 0..500 {
            fx.app.on_key(KeyCode::Down);
        }
        assert_eq!(fx.app.diff_scroll(), max);
        for _ in 0..500 {
            fx.app.on_key(KeyCode::Up);
        }
        assert_eq!(fx.app.diff_scroll(), 0);
    }

    #[test]
    fn moving_file_selection_loads_its_diff() {
        let mut fx = harness(&["a.txt", "b.txt"]);
        // Select b.txt; its diff should load automatically.
        fx.app.on_key(KeyCode::Char('j'));
        let d = wait_for_diff(&mut fx.app, "b.txt");
        assert_eq!(d.path, "b.txt");
    }

    #[test]
    fn fully_staged_file_loads_staged_diff() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char(' '));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            fx.app.poll();
            if fx.app.diff_viewing_staged() == Some(true) {
                if let Some(d) = fx.app.diff() {
                    if !d.hunks.is_empty() {
                        return;
                    }
                }
            }
            assert!(Instant::now() < deadline, "staged diff never loaded");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn hunk_nav_clamps_and_snaps_scroll() {
        let mut fx = two_hunk_fixture();
        assert!(fx.app.diff().unwrap().hunks.len() >= 2);
        assert_eq!(fx.app.hunk(), 0);
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::FullDiff);
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.hunk(), 1);
        // Snaps the selected hunk to the top of the view.
        assert!(fx.app.diff_scroll() > 0);
        fx.app.on_key(KeyCode::Char('j'));
        assert_eq!(fx.app.hunk(), 1, "clamps at last hunk");
        fx.app.on_key(KeyCode::Char('k'));
        assert_eq!(fx.app.hunk(), 0);
    }

    #[test]
    fn space_in_diff_stages_selected_hunk() {
        let mut fx = two_hunk_fixture();
        let before = fx.app.diff().unwrap().hunks.len();
        fx.app.on_key(KeyCode::Enter);
        fx.app.on_key(KeyCode::Char(' '));
        // Partial staging: status shows Both, diff reloads with one less hunk.
        let st = wait_for(&mut fx.app, |st| {
            st.files
                .iter()
                .any(|e| e.path == "a.txt" && e.state == FileState::BothStagedAndUnstaged)
        });
        let _ = st;
        let d = wait_for_diff(&mut fx.app, "a.txt");
        assert_eq!(d.hunks.len(), before - 1);
    }

    #[test]
    fn space_in_staged_diff_view_errors() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char(' '));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            fx.app.poll();
            if fx.app.diff_viewing_staged() == Some(true) {
                break;
            }
            assert!(Instant::now() < deadline, "staged diff never loaded");
            std::thread::sleep(Duration::from_millis(10));
        }
        fx.app.on_key(KeyCode::Enter);
        fx.app.on_key(KeyCode::Char(' '));
        assert!(fx.app.error().is_some(), "expected staged-hunk error");
    }

    fn wait_for_branches(app: &mut App, min_count: usize) -> Vec<BranchInfo> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if let Some(b) = app.branches() {
                if b.len() >= min_count {
                    return b.to_vec();
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for branches");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn focus_branches(fx: &mut Fixture) {
        fx.app.on_key(KeyCode::Char('2'));
        assert_eq!(fx.app.focus(), Focus::Branches);
    }

    /// Move branch selection to the named branch (sort-order independent).
    fn select_branch(fx: &mut Fixture, name: &str) {
        let branches = wait_for_branches(&mut fx.app, 1);
        let pos = branches
            .iter()
            .position(|b| b.name == name)
            .unwrap_or_else(|| panic!("branch {name} missing in {branches:?}"));
        while fx.app.branch_selected() < pos {
            fx.app.on_key(KeyCode::Char('j'));
        }
        while fx.app.branch_selected() > pos {
            fx.app.on_key(KeyCode::Char('k'));
        }
    }

    #[test]
    fn panels_load_eagerly_at_startup_without_focusing() {
        let mut fx = harness(&["a.txt"]);
        // No focus keys pressed: branches, log, and stash must arrive on
        // their own so every pane shows data on launch.
        let branches = wait_for_branches(&mut fx.app, 1);
        assert!(branches.iter().any(|b| b.name == "main" && b.is_head));
        let entries = wait_for_log(&mut fx.app, 1);
        assert_eq!(entries[0].summary, "init");
        let stash = wait_for_stash(&mut fx.app, 0);
        assert!(stash.is_empty());
    }

    #[test]
    fn branches_load_when_panel_focused() {
        let mut fx = harness(&["a.txt"]);
        // Eager at startup; focusing must keep working.
        focus_branches(&mut fx);
        let branches = wait_for_branches(&mut fx.app, 1);
        assert!(branches.iter().any(|b| b.name == "main" && b.is_head));
    }

    #[test]
    fn create_branch_via_modal() {
        let mut fx = harness(&["a.txt"]);
        focus_branches(&mut fx);
        wait_for_branches(&mut fx.app, 1);
        fx.app.on_key(KeyCode::Char('a'));
        assert_eq!(fx.app.mode(), Mode::NewBranch);
        for c in "feat".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::Normal);
        let branches = wait_for_branches(&mut fx.app, 2);
        assert!(branches.iter().any(|b| b.name == "feat"));
    }

    #[test]
    fn checkout_branch_via_enter() {
        let mut fx = harness(&["a.txt"]);
        focus_branches(&mut fx);
        wait_for_branches(&mut fx.app, 1);
        // Create feat, then move selection to it and check out.
        fx.app.on_key(KeyCode::Char('a'));
        for c in "feat".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        wait_for_branches(&mut fx.app, 2);
        select_branch(&mut fx, "feat");
        fx.app.on_key(KeyCode::Enter);
        let st = wait_for(&mut fx.app, |st| st.branch == "feat");
        assert_eq!(st.branch, "feat");
    }

    #[test]
    fn delete_branch_via_key() {
        let mut fx = harness(&["a.txt"]);
        focus_branches(&mut fx);
        wait_for_branches(&mut fx.app, 1);
        fx.app.on_key(KeyCode::Char('a'));
        for c in "gone".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        wait_for_branches(&mut fx.app, 2);
        // Select "gone" and delete it.
        select_branch(&mut fx, "gone");
        fx.app.on_key(KeyCode::Char('D'));
        let branches = wait_for_branches(&mut fx.app, 1);
        // Wait until "gone" disappears (list reloads after MutationDone).
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            fx.app.poll();
            if let Some(b) = fx.app.branches() {
                if !b.iter().any(|x| x.name == "gone") {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "gone never deleted: {branches:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn delete_checked_out_branch_surfaces_error() {
        let mut fx = harness(&["a.txt"]);
        focus_branches(&mut fx);
        wait_for_branches(&mut fx.app, 1);
        // "main" is checked out; deleting it must fail loudly.
        fx.app.on_key(KeyCode::Char('D'));
        let err = wait_for_error(&mut fx.app);
        assert!(err.contains("checked out"), "got: {err}");
    }

    fn wait_for_log(app: &mut App, min_count: usize) -> Vec<CommitInfo> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if let Some(entries) = app.log() {
                if entries.len() >= min_count {
                    return entries.to_vec();
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for log");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn log_loads_newest_first_when_focused() {
        let mut fx = harness(&["a.txt"]);
        // Eager at startup; focusing must keep working.
        fx.app.on_key(KeyCode::Char('3'));
        let entries = wait_for_log(&mut fx.app, 1);
        assert_eq!(entries[0].summary, "init");
    }

    #[test]
    fn new_commit_appears_at_top_of_log() {
        let mut fx = harness(&["a.txt"]);
        // Stage + commit, then open the log.
        fx.app.on_key(KeyCode::Char(' '));
        wait_for(&mut fx.app, |st| {
            st.files
                .iter()
                .any(|e| e.path == "a.txt" && e.state == FileState::Staged)
        });
        fx.app.on_key(KeyCode::Char('c'));
        for c in "second".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        fx.app.on_key(KeyCode::Char('3'));
        let entries = wait_for_log(&mut fx.app, 2);
        assert_eq!(entries[0].summary, "second");
        assert_eq!(entries[1].summary, "init");
    }

    fn wait_for_stash(app: &mut App, count: usize) -> Vec<StashEntry> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if let Some(entries) = app.stash() {
                if entries.len() == count {
                    return entries.to_vec();
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for stash");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn focus_stash(fx: &mut Fixture) {
        fx.app.on_key(KeyCode::Char('4'));
        assert_eq!(fx.app.focus(), Focus::Stash);
    }

    #[test]
    fn stash_push_cleans_status_and_lists_entry() {
        let mut fx = harness(&["a.txt"]);
        focus_stash(&mut fx);
        wait_for_stash(&mut fx.app, 0);
        fx.app.on_key(KeyCode::Char('a'));
        assert_eq!(fx.app.mode(), Mode::StashPush);
        for c in "wip".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::Normal);
        let entries = wait_for_stash(&mut fx.app, 1);
        assert!(
            entries[0].message.contains("wip"),
            "got {:?}",
            entries[0].message
        );
        // Status is clean after the push.
        let st = wait_for(&mut fx.app, |st| st.files.is_empty());
        assert!(st.files.is_empty());
    }

    #[test]
    fn stash_pop_restores_changes() {
        let mut fx = harness(&["a.txt"]);
        focus_stash(&mut fx);
        wait_for_stash(&mut fx.app, 0);
        fx.app.on_key(KeyCode::Char('a'));
        for c in "wip".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        wait_for_stash(&mut fx.app, 1);
        // Pop it back.
        fx.app.on_key(KeyCode::Enter);
        wait_for_stash(&mut fx.app, 0);
        let st = wait_for(&mut fx.app, |st| st.files.iter().any(|e| e.path == "a.txt"));
        assert_eq!(st.files[0].state, FileState::Unstaged);
    }

    #[test]
    fn stash_drop_removes_entry() {
        let mut fx = harness(&["a.txt"]);
        focus_stash(&mut fx);
        wait_for_stash(&mut fx.app, 0);
        fx.app.on_key(KeyCode::Char('a'));
        for c in "wip".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        wait_for_stash(&mut fx.app, 1);
        fx.app.on_key(KeyCode::Char('D'));
        wait_for_stash(&mut fx.app, 0);
    }

    #[test]
    fn overridden_bindings_take_effect() {
        use crate::config::Config;
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "base\n", "init");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(repo.workdir().unwrap().join("a.txt"))
                .unwrap();
            f.write_all(b"dirty\n").unwrap();
        }
        let path = repo.workdir().unwrap().to_path_buf();
        drop(repo);
        let keys = crate::config::KeyBindings {
            stage: vec![KeyCode::Char('s')],
            ..Default::default()
        };
        let config = Config {
            keys,
            ..Default::default()
        };
        let mut fx = Fixture {
            _dir: dir,
            app: App::new_with_config(JobQueue::spawn(path).unwrap(), config),
        };
        wait_for(&mut fx.app, |st| st.files.iter().any(|e| e.path == "a.txt"));
        // Space is no longer bound: nothing happens.
        fx.app.on_key(KeyCode::Char(' '));
        std::thread::sleep(Duration::from_millis(200));
        fx.app.poll();
        assert!(fx.app.error().is_none());
        let st = fx.app.status().unwrap();
        assert_eq!(st.files[0].state, FileState::Unstaged);
        // "s" stages.
        fx.app.on_key(KeyCode::Char('s'));
        let st = wait_for(&mut fx.app, |st| {
            st.files
                .iter()
                .any(|e| e.path == "a.txt" && e.state == FileState::Staged)
        });
        assert_eq!(st.files[0].state, FileState::Staged);
    }

    #[test]
    fn shift_a_in_commit_box_starts_generation_and_no_staged_errors() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        assert_eq!(fx.app.mode(), Mode::Committing);
        // Legacy single-KeyCode path: `A` means Shift+A.
        fx.app.on_key(KeyCode::Char('A'));
        assert!(fx.app.is_generating());
        let err = wait_for_error(&mut fx.app);
        assert!(
            err.contains("nothing staged") || err.contains("API key"),
            "got: {err}"
        );
        assert!(!fx.app.is_generating());
        assert_eq!(fx.app.mode(), Mode::Committing, "stay in the box to edit");
    }

    #[test]
    fn shift_a_without_shift_types_literal_a() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        // Real modifier path: no Shift means a literal `A` (caps lock).
        fx.app.on_key_with_modifiers(KeyCode::Char('A'), false);
        assert!(!fx.app.is_generating());
        assert_eq!(fx.app.draft(), "A");
        // Lowercase always types.
        fx.app.on_key_with_modifiers(KeyCode::Char('a'), false);
        assert_eq!(fx.app.draft(), "Aa");
        // Shift+A generates instead of typing.
        fx.app.on_key_with_modifiers(KeyCode::Char('a'), true);
        assert!(fx.app.is_generating());
        assert_eq!(fx.app.draft(), "Aa", "generation must not type into the draft");
    }

    #[test]
    fn generated_message_fills_draft_and_moves_cursor_to_end() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        assert_eq!(fx.app.mode(), Mode::Committing);
        fx.app.generating = true;
        fx.app.apply(git_tui_core::jobqueue::AsyncResult::GeneratedMessage(
            "feat: add thing".into(),
        ));
        assert_eq!(fx.app.draft(), "feat: add thing");
        assert_eq!(fx.app.draft_cursor(), fx.app.draft().chars().count());
        assert!(!fx.app.is_generating());
        assert!(fx.app.error().is_none());
    }

    #[test]
    fn generated_message_arriving_after_esc_is_ignored() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('c'));
        fx.app.generating = true;
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::Normal);
        fx.app.generating = true;
        fx.app.apply(git_tui_core::jobqueue::AsyncResult::GeneratedMessage(
            "feat: late".into(),
        ));
        assert!(!fx.app.is_generating());
        assert_eq!(fx.app.draft(), "", "closed box must not be filled");
    }

    #[test]
    fn shift_a_in_file_list_opens_llm_settings_prefilled() {
        let mut fx = harness(&["a.txt"]);
        assert_eq!(fx.app.mode(), Mode::Normal);
        fx.app.on_key(KeyCode::Char('A'));
        assert_eq!(fx.app.mode(), Mode::LlmSettings);
        assert_eq!(fx.app.llm_selected(), 0);
        // Prefilled from the session config (defaults here).
        assert_eq!(fx.app.llm_field_value(0), "openai");
        assert_eq!(fx.app.llm_field_value(1), "gpt-4o-mini");
    }

    #[test]
    fn llm_settings_tab_switches_fields_and_typing_edits() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('A'));
        fx.app.on_key(KeyCode::Tab);
        assert_eq!(fx.app.llm_selected(), 1);
        for c in "x-model".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        assert!(fx.app.llm_field_value(1).contains("x-model"));
        fx.app.on_key(KeyCode::Up);
        assert_eq!(fx.app.llm_selected(), 0);
        // Row 0 kept its prefilled value (stash/unstash round-trip).
        assert_eq!(fx.app.llm_field_value(0), "openai");
    }

    #[test]
    fn llm_settings_esc_cancels_without_applying() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('A'));
        fx.app.on_key(KeyCode::Tab);
        fx.app.on_key(KeyCode::Tab);
        for c in "junk".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Esc);
        assert_eq!(fx.app.mode(), Mode::Normal);
        assert_eq!(fx.app.llm.provider, "openai", "cancel must not apply");
        assert!(fx.app.notice().is_none());
    }

    #[test]
    fn llm_settings_enter_with_bad_provider_stays_open() {
        let mut fx = harness(&["a.txt"]);
        fx.app.on_key(KeyCode::Char('A'));
        fx.app.clear_draft();
        for c in "skynet".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::LlmSettings, "bad input stays open");
        assert!(fx.app.error().unwrap_or("").contains("unknown provider"));
        assert_eq!(fx.app.llm.provider, "openai", "bad input not applied");
    }

    #[test]
    fn llm_settings_enter_saves_and_persists_to_file() {
        let mut fx = harness(&["a.txt"]);
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("config.toml");
        fx.app.set_config_path(Some(cfg_path.clone()));
        fx.app.on_key(KeyCode::Char('A'));
        // Provider row: replace "openai" with "ollama".
        fx.app.clear_draft();
        for c in "ollama".chars() {
            fx.app.on_key(KeyCode::Char(c));
        }
        fx.app.on_key(KeyCode::Enter);
        assert_eq!(fx.app.mode(), Mode::Normal);
        assert_eq!(fx.app.llm.provider, "ollama");
        assert!(fx.app.notice().unwrap_or("").contains("saved"));
        // Round-trips through the real config file.
        let cfg = Config::load_from_path(&cfg_path).unwrap();
        assert_eq!(cfg.llm.provider, "ollama");
    }
}
