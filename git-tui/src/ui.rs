//! Phase 4: status + diff panels (ratatui).

use crate::app::{App, Focus, Mode};
use crate::config::Theme;
use crate::syntax::{highlight_line, HiToken};
use crate::words::{word_diff, WordSeg};
use git_tui_core::branch::BranchInfo;
use git_tui_core::diff::{FileDiff, LineKind};
use git_tui_core::log::CommitInfo;
use git_tui_core::stash::StashEntry;
use git_tui_core::status::{FileState, StatusEntry};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

fn state_glyph(state: FileState, theme: Theme) -> (&'static str, Color) {
    match state {
        FileState::Staged => ("S", theme.staged),
        FileState::Unstaged => ("M", theme.unstaged),
        FileState::Untracked => ("?", theme.untracked),
        FileState::Conflicted => ("C", theme.conflicted),
        FileState::BothStagedAndUnstaged => ("B", theme.both_staged),
        // Clean files need no marker; the tree position says it all.
        FileState::Clean => (" ", theme.hint),
    }
}

fn focused_border(focused: bool, theme: Theme) -> Style {
    if focused {
        Style::default()
            .fg(theme.border_focused)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.border_unfocused)
    }
}

/// LazyVim-style panel frame: rounded corners + title that glows when
/// focused (like Telescope/border highlights in LazyVim).
fn panel_block(focused: bool, theme: Theme, title: String) -> Block<'static> {
    let title_style = if focused {
        Style::default()
            .fg(theme.border_focused)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.hint)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(focused, theme))
        // Opaque LazyVim background: no terminal wallpaper bleed-through.
        .style(Style::default().bg(theme.bg).fg(theme.fg))
        .title(title)
        .title_style(title_style)
}

/// Selection wash (Telescope-style): readable fg on a tinted bg.
fn selection_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.fg)
        .bg(theme.selection_bg)
        .add_modifier(Modifier::BOLD)
}

/// Render the whole screen: a full-width vertical stack (status, files
/// tree, unified diff preview, branches, commits, stash), footer hints,
/// then any modal or the fullscreen diff overlay on top.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Solid LazyVim base: paint every cell once so transparent spans
    // (gutters, padding, unfocused chrome) never show the wallpaper.
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme().bg)),
        area,
    );
    let footer_len = if app.error().is_some() { 2 } else { 1 };
    let layout = compute_layout(area, footer_len);

    render_status_panel(frame, layout.status, app);
    render_files_panel(frame, layout.files, app);
    render_diff_preview_panel(frame, layout.diff, app);
    render_branches_panel(frame, layout.branches, app);
    render_commits_panel(frame, layout.commits, app);
    render_stash_panel(frame, layout.stash, app);
    render_footer(frame, layout.footer, app);

    match app.mode() {
        Mode::Committing => render_input_modal(frame, area, app, " Commit message "),
        Mode::NewBranch => render_input_modal(frame, area, app, " New branch name "),
        Mode::StashPush => render_input_modal(frame, area, app, " Stash message "),
        Mode::FindFile => render_finder_modal(frame, area, app),
        Mode::FullDiff => render_fullscreen_diff(frame, area, app),
        Mode::Normal => {}
    }
}

/// Geometry of the screen. Pure function of the area so tests can predict
/// panel corners with the same math the renderer uses.
pub(crate) struct ScreenLayout {
    pub status: Rect,
    pub files: Rect,
    pub diff: Rect,
    pub branches: Rect,
    pub commits: Rect,
    pub stash: Rect,
    pub footer: Rect,
}

pub(crate) fn compute_layout(area: Rect, footer_h: u16) -> ScreenLayout {
    let footer_h = footer_h.min(area.height);
    let body_h = area.height - footer_h;
    let footer = Rect {
        x: area.x,
        y: area.y + body_h,
        width: area.width,
        height: footer_h,
    };
    let rail_w = (area.width / 10 * 3 + area.width % 10 * 3 / 10)
        .min(44)
        .min(area.width);
    let rail = Rect {
        x: area.x,
        y: area.y,
        width: rail_w,
        height: body_h,
    };
    let preview = Rect {
        x: area.x + rail_w,
        y: area.y,
        width: area.width - rail_w,
        height: body_h,
    };
    let sizes = [3, body_h.saturating_sub(16).max(3), 5, 4, 4];
    let mut rects = Vec::with_capacity(5);
    let mut y = rail.y;
    let end = rail.y + rail.height;
    for want in sizes {
        let h = (y + want).min(end).saturating_sub(y);
        rects.push(Rect {
            x: rail.x,
            y,
            width: rail.width,
            height: h,
        });
        y += h;
    }
    ScreenLayout {
        status: rects[0],
        files: rects[1],
        branches: rects[2],
        commits: rects[3],
        stash: rects[4],
        diff: preview,
        footer,
    }
}

/// Minimal-scroll follow: keep `selected` visible inside a `visible`-row
/// window, reusing the persisted offset. Pure: the caller stores the result.
fn follow_selection(selected: usize, visible: usize, current: usize) -> usize {
    if visible == 0 {
        return current;
    }
    if selected < current {
        selected
    } else if selected >= current + visible {
        selected + 1 - visible
    } else {
        current
    }
}

fn render_status_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let theme = app.theme();
    // Info strip only: the files panel below owns the Status-focus glow
    // (that is where the cursor lives), so this border stays dim.
    let block = panel_block(false, theme, "[1]-Status".to_string());
    let line = match app.status() {
        None => Line::raw("loading…"),
        Some(st) if st.files.is_empty() => Line::from(vec![
            Span::styled(
                "✓ ",
                Style::default()
                    .fg(theme.branch_current)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{} → {}", app.repo_name(), st.branch)),
        ]),
        // Count first: the rail is narrow and the tail can clip.
        Some(st) => Line::from(vec![
            Span::styled(
                "● ",
                Style::default()
                    .fg(theme.unstaged)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "{} → {} ({})",
                app.repo_name(),
                st.branch,
                st.files.len()
            )),
        ]),
    };
    frame.render_widget(Paragraph::new(line).block(block), area);
}

/// A row of the files tree: a directory header or a file (indexed into
/// `RepoStatus::files`). Collapsed headers render folded (`▶`) and hide
/// their children; selection stays file-based, so staging keys behave
/// exactly as in the flat list. When the selected file is hidden inside
/// a collapsed dir, its shallowest collapsed ancestor header takes the
/// highlight so the cursor never disappears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileRow<'a> {
    Dir { path: &'a str, depth: usize },
    File { index: usize, depth: usize },
}

/// Cumulative ancestor prefixes: "a/b/c/f" -> ["a", "a/b", "a/b/c"].
fn ancestors(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for (i, b) in path.bytes().enumerate() {
        if b == b'/' {
            out.push(&path[..i]);
        }
    }
    out
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(crate) fn file_rows(files: &[StatusEntry]) -> Vec<FileRow<'_>> {
    use std::collections::HashSet;
    let mut emitted: HashSet<&str> = HashSet::new();
    let mut rows = Vec::new();
    for (index, f) in files.iter().enumerate() {
        let dirs = ancestors(&f.path);
        for dir in &dirs {
            if emitted.insert(*dir) {
                let depth = dir.bytes().filter(|&b| b == b'/').count();
                rows.push(FileRow::Dir { path: dir, depth });
            }
        }
        rows.push(FileRow::File {
            index,
            depth: dirs.len(),
        });
    }
    rows
}

fn render_files_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let theme = app.theme();
    let focused = app.focus() == Focus::Status;
    let Some(st) = app.status() else {
        frame.render_widget(
            Paragraph::new("loading…").block(panel_block(focused, theme, "[2]-Files".to_string())),
            area,
        );
        return;
    };
    if st.files.is_empty() {
        frame.render_widget(
            Paragraph::new("(clean working tree)").block(panel_block(
                focused,
                theme,
                "[2]-Files (0)".to_string(),
            )),
            area,
        );
        return;
    }
    let rows = file_rows(&st.files);
    // Fold collapsed subtrees: drop every row hiding under a collapsed
    // dir, but keep the collapsed header itself (rendered as `▶`).
    let rows: Vec<&FileRow> = rows
        .iter()
        .filter(|row| {
            let r: &FileRow = row;
            let path: &str = match r {
                FileRow::Dir { path, .. } => path,
                FileRow::File { index, .. } => st.files[*index].path.as_str(),
            };
            !ancestors(path).iter().any(|a| app.is_collapsed(a))
        })
        .collect();
    let sel = app.selected().min(st.files.len() - 1);
    // The selected file's own row, or — when it is hidden inside a
    // collapsed dir — its shallowest collapsed ancestor header, which
    // is always visible (its own ancestors are all expanded).
    let sel_row = rows
        .iter()
        .position(|row| {
            let r: &FileRow = row;
            matches!(r, FileRow::File { index, .. } if *index == sel)
        })
        .or_else(|| {
            ancestors(st.files[sel].path.as_str())
                .into_iter()
                .find(|a| app.is_collapsed(a))
                .and_then(|header| {
                    rows.iter().position(|row| {
                        let r: &FileRow = row;
                        matches!(r, FileRow::Dir { path, .. } if *path == header)
                    })
                })
        })
        .unwrap_or(0);
    let visible = area.height.saturating_sub(2) as usize;
    let off = follow_selection(sel_row, visible, app.files_scroll());
    app.set_files_scroll(off);
    let items: Vec<ListItem> = rows
        .iter()
        .skip(off)
        .take(visible)
        .map(|row| match row {
            FileRow::Dir { path, depth } => {
                let indent = "  ".repeat(*depth);
                let glyph = if app.is_collapsed(path) { "▶" } else { "▼" };
                ListItem::new(Line::from(vec![Span::styled(
                    format!("{indent}{glyph} {}/", basename(path)),
                    Style::default().fg(theme.hint).add_modifier(Modifier::BOLD),
                )]))
            }
            FileRow::File { index, depth } => {
                let f = &st.files[*index];
                let (glyph, color) = state_glyph(f.state, theme);
                let indent = "  ".repeat(*depth);
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{indent}{glyph} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(basename(&f.path).to_string(), Style::default().fg(theme.fg)),
                ]))
            }
        })
        .collect();
    let list = List::new(items)
        .block(panel_block(
            focused,
            theme,
            format!("[2]-Files ({} of {})", sel + 1, st.files.len()),
        ))
        .highlight_style(selection_style(theme))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select((!rows.is_empty() && visible > 0).then(|| sel_row.saturating_sub(off)));
    frame.render_stateful_widget(list, area, &mut state);
}

/// One side of a side-by-side row: a gutter number plus word segments.
/// `Blank` is the empty counterpart of a one-sided (add/del-only) row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SideKind {
    Context,
    Del,
    Add,
    Blank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Side {
    pub no: Option<u32>,
    pub segs: Vec<WordSeg>,
    pub kind: SideKind,
}

/// A rendered diff row: a hunk separator or a paired old/new line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiffRow {
    Header { index: usize },
    Split { left: Side, right: Side },
}

/// Pair hunk lines into VS Code-style rows, tracking old/new line numbers
/// from the hunk starts. Del/add runs pair up positionally; leftover dels
/// (or adds) get a blank counterpart; context fills both sides.
pub(crate) fn diff_rows(diff: &FileDiff) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (index, hunk) in diff.hunks.iter().enumerate() {
        rows.push(DiffRow::Header { index });
        let mut old_no = hunk.old_start;
        let mut new_no = hunk.new_start;
        let mut i = 0;
        while i < hunk.lines.len() {
            let mut dels = Vec::new();
            while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Del {
                dels.push(&hunk.lines[i]);
                i += 1;
            }
            let mut adds = Vec::new();
            while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Add {
                adds.push(&hunk.lines[i]);
                i += 1;
            }
            if dels.is_empty() && adds.is_empty() {
                let line = &hunk.lines[i];
                i += 1;
                // The @@ header is already its own row; anything else here
                // is context shown on both sides.
                if line.kind == LineKind::HunkHeader {
                    continue;
                }
                let segs = vec![WordSeg {
                    text: line.text.clone(),
                    changed: false,
                }];
                rows.push(DiffRow::Split {
                    left: Side {
                        no: Some(old_no),
                        segs: segs.clone(),
                        kind: SideKind::Context,
                    },
                    right: Side {
                        no: Some(new_no),
                        segs,
                        kind: SideKind::Context,
                    },
                });
                old_no += 1;
                new_no += 1;
                continue;
            }
            let n = dels.len().max(adds.len());
            for k in 0..n {
                let has_old = dels.get(k).is_some();
                let has_new = adds.get(k).is_some();
                let old_text = dels.get(k).map(|l| l.text.as_str()).unwrap_or("");
                let new_text = adds.get(k).map(|l| l.text.as_str()).unwrap_or("");
                let (old_segs, new_segs) = word_diff(old_text, new_text);
                rows.push(DiffRow::Split {
                    left: Side {
                        no: has_old.then_some(old_no),
                        segs: old_segs,
                        kind: if has_old {
                            SideKind::Del
                        } else {
                            SideKind::Blank
                        },
                    },
                    right: Side {
                        no: has_new.then_some(new_no),
                        segs: new_segs,
                        kind: if has_new {
                            SideKind::Add
                        } else {
                            SideKind::Blank
                        },
                    },
                });
                if has_old {
                    old_no += 1;
                }
                if has_new {
                    new_no += 1;
                }
            }
        }
    }
    rows
}

/// Rendered row offset where hunk `index` starts (its header row), so hunk
/// navigation can snap the view to the selected hunk.
pub(crate) fn hunk_start_row(diff: &FileDiff, index: usize) -> u16 {
    diff_rows(diff)
        .iter()
        .position(|r| matches!(r, DiffRow::Header { index: i } if *i == index))
        .unwrap_or(0)
        .min(u16::MAX as usize) as u16
}

/// Gutter width: right-aligned numbers, at least 4 digits wide.
fn gutter_width(rows: &[DiffRow]) -> usize {
    let mut digits = 4;
    for row in rows {
        if let DiffRow::Split { left, right } = row {
            for no in left.no.iter().chain(right.no.iter()) {
                digits = digits.max(no.to_string().len());
            }
        }
    }
    digits
}

/// Render one side (gutter + text) padded to exactly `width` cells.
/// LazyVim-style: syntax-highlighted foreground (treesitter-like colors)
/// on a tinted diff wash, with exactly the changed words getting a
/// stronger wash. Whole-file views are all-`Context` with no wash, so they
/// read like a LazyVim buffer: line numbers + full syntax colors.
fn render_side(
    side: &Side,
    path: &str,
    width: usize,
    gutter_w: usize,
    theme: Theme,
) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthChar;
    let mut remaining = width;
    let mut out = Vec::new();
    for span in render_side_unclipped(side, path, width, gutter_w, theme) {
        let mut text = String::new();
        for ch in span.content.chars() {
            let cells = ch.width().unwrap_or(0);
            if cells > remaining {
                text.push_str(&" ".repeat(remaining));
                remaining = 0;
                break;
            }
            text.push(ch);
            remaining -= cells;
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
        if remaining == 0 {
            break;
        }
    }
    out
}

fn render_side_unclipped(
    side: &Side,
    path: &str,
    width: usize,
    gutter_w: usize,
    theme: Theme,
) -> Vec<Span<'static>> {
    let gutter_text = match side.no {
        Some(no) => format!("{:>gutter_w$} ", no),
        None => " ".repeat(gutter_w + 1),
    };
    let (bg, word_bg) = match side.kind {
        // Plain code rows sit on the opaque editor background. Deleted rows
        // carry a red wash (stronger on changed words); added rows carry a
        // single very light green wash so syntax colors stay readable —
        // green is never painted twice on the same cell.
        SideKind::Context => (Some(theme.bg), None),
        SideKind::Del => (Some(theme.diff_del_bg), Some(theme.diff_del_word_bg)),
        SideKind::Add => (Some(theme.diff_add_bg), None),
        SideKind::Blank => (Some(theme.bg), None),
    };
    let mut spans = vec![Span::styled(
        gutter_text,
        Style::default().fg(theme.line_nr).bg(theme.bg),
    )];
    let mut used = gutter_w + 1;
    // Blank counterparts stay empty even if pairing left stray segments.
    let segs: &[WordSeg] = match side.kind {
        SideKind::Blank => &[],
        _ => &side.segs,
    };
    if segs.is_empty() {
        let pad = width.saturating_sub(used);
        if pad > 0 {
            let mut style = Style::default();
            if let Some(bg) = bg {
                style = style.bg(bg);
            }
            spans.push(Span::styled(" ".repeat(pad), style));
        }
        return spans;
    }
    // Syntax colors for the whole line, then re-split by word-diff boundaries
    // so changed words keep their stronger wash without losing syntax fg.
    let full_text: String = segs.iter().map(|s| s.text.as_str()).collect();
    let hi = highlight_line(path, &full_text, theme);
    let syntax_chars = expand_tokens(&hi);
    let changed_chars = expand_changed(segs);
    // Both derive from `full_text`, so lengths match; truncate defensively
    // rather than panicking on grapheme edge cases.
    let n = syntax_chars.len().min(changed_chars.len());
    let mut idx = 0;
    while idx < n {
        let (fg, modifier) = syntax_chars[idx];
        let changed_flag = changed_chars[idx];
        let mut j = idx + 1;
        while j < n
            && syntax_chars[j].0 == fg
            && syntax_chars[j].1 == modifier
            && changed_chars[j] == changed_flag
        {
            j += 1;
        }
        let text: String = full_text_chars(&full_text, idx, j);
        let mut style = Style::default().fg(fg).add_modifier(modifier);
        // Changed runs take the stronger word wash when the kind has one
        // (deletions); kinds without it (additions) keep the single line
        // wash so green is painted exactly once.
        let wash = if changed_flag { word_bg.or(bg) } else { bg };
        if let Some(wash) = wash {
            style = style.bg(wash);
        }
        // Washed code reads brighter: bold keeps the syntax hue while
        // lifting it off the tinted background (gutter stays dim).
        if matches!(side.kind, SideKind::Del | SideKind::Add) {
            style = style.add_modifier(Modifier::BOLD);
        }
        used += text.width();
        spans.push(Span::styled(text, style));
        idx = j;
    }
    // Pad to the full cell width so the wash covers the whole half-pane.
    // Context/blank rows paint the opaque editor background.
    let pad = width.saturating_sub(used);
    if pad > 0 {
        let mut style = Style::default();
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        spans.push(Span::styled(" ".repeat(pad), style));
    }
    spans
}

/// Flatten highlight tokens to per-char (fg, modifier) for merging with
/// word-diff changed flags.
fn expand_tokens(tokens: &[HiToken]) -> Vec<(Color, Modifier)> {
    let mut out = Vec::new();
    for t in tokens {
        for _ in t.text.chars() {
            out.push((t.fg, t.modifier));
        }
    }
    out
}

/// Per-char changed flags from word-diff segments.
fn expand_changed(segs: &[WordSeg]) -> Vec<bool> {
    let mut out = Vec::new();
    for s in segs {
        for _ in s.text.chars() {
            out.push(s.changed);
        }
    }
    out
}

/// Slice `full_text` by char indices [start, end).
fn full_text_chars(full_text: &str, start: usize, end: usize) -> String {
    full_text.chars().skip(start).take(end - start).collect()
}

/// A unified preview line: single text column with a `-`/`+` marker.
fn unified_line(
    marker: &'static str,
    marker_style: Style,
    side: &Side,
    path: &str,
    gutter_w: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut spans = vec![Span::styled(marker, marker_style)];
    spans.extend(render_side(
        side,
        path,
        width.saturating_sub(2),
        gutter_w,
        theme,
    ));
    Line::from(spans)
}

/// How many unified lines a row expands to (headers count as one).
fn unified_row_count(row: &DiffRow) -> usize {
    match row {
        DiffRow::Header { .. } => 1,
        DiffRow::Split { left, right } => match (&left.kind, &right.kind) {
            (SideKind::Context, _) => 1,
            (SideKind::Del, SideKind::Add) => 2,
            (SideKind::Del, _) => 1,
            (_, SideKind::Add) => 1,
            _ => 0,
        },
    }
}

/// Render one DiffRow into 0-2 unified lines (syntax-highlighted).
fn render_unified_row(
    diff: &FileDiff,
    row: &DiffRow,
    gutter_w: usize,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let path = diff.path.as_str();
    match row {
        DiffRow::Header { index } => vec![Line::from(vec![Span::styled(
            format!("  {}", diff.hunks[*index].header),
            Style::default().fg(theme.hint),
        )])],
        DiffRow::Split { left, right } => match (&left.kind, &right.kind) {
            (SideKind::Context, _) => vec![unified_line(
                "  ",
                Style::default(),
                left,
                path,
                gutter_w,
                width,
                theme,
            )],
            (SideKind::Del, SideKind::Add) => vec![
                unified_line(
                    "- ",
                    Style::default()
                        .fg(theme.conflicted)
                        .add_modifier(Modifier::BOLD),
                    left,
                    path,
                    gutter_w,
                    width,
                    theme,
                ),
                unified_line(
                    "+ ",
                    Style::default()
                        .fg(theme.staged)
                        .add_modifier(Modifier::BOLD),
                    right,
                    path,
                    gutter_w,
                    width,
                    theme,
                ),
            ],
            (SideKind::Del, _) => vec![unified_line(
                "- ",
                Style::default()
                    .fg(theme.conflicted)
                    .add_modifier(Modifier::BOLD),
                left,
                path,
                gutter_w,
                width,
                theme,
            )],
            (_, SideKind::Add) => vec![unified_line(
                "+ ",
                Style::default()
                    .fg(theme.staged)
                    .add_modifier(Modifier::BOLD),
                right,
                path,
                gutter_w,
                width,
                theme,
            )],
            _ => vec![],
        },
    }
}

/// Flatten side-by-side rows into single-column unified lines for the
/// inline preview: context stays one line, del/add pairs become two.
/// Only the visible window (`skip`, `take`) is syntax-highlighted so
/// opening a large file stays fast.
fn render_unified_lines(
    diff: &FileDiff,
    theme: Theme,
    width: usize,
    skip: usize,
    take: usize,
) -> (Vec<Line<'static>>, usize) {
    let rows = diff_rows(diff);
    let gutter_w = gutter_width(&rows);
    let total: usize = rows.iter().map(unified_row_count).sum();
    let mut out = Vec::new();
    let mut idx = 0;
    for row in &rows {
        for line in render_unified_row(diff, row, gutter_w, width, theme) {
            if idx >= skip && out.len() < take {
                out.push(line);
            } else if idx >= skip + take {
                // Still need `total` (computed above); skip highlighting rest
                // by breaking early — but `render_unified_row` already ran for
                // this row; remaining rows are untouched.
                // Fall through to fast count: total already known.
            }
            idx += 1;
            if idx >= skip + take && out.len() >= take {
                // No more visible lines; stop highlighting further rows.
                // Total was precomputed, so we can return early.
                return (out, total);
            }
        }
    }
    (out, total)
}

/// Inline single-column diff preview of the selected file. Not focusable;
/// PgUp/PgDn scroll it, Enter opens the fullscreen side-by-side view.
fn render_diff_preview_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let theme = app.theme();
    let Some(diff) = app.diff() else {
        let title = if app.has_files() {
            " Diff (loading…) ".to_string()
        } else {
            " Diff ".to_string()
        };
        frame.render_widget(
            Paragraph::new("").block(panel_block(false, theme, title)),
            area,
        );
        return;
    };
    let title = if app.diff_whole_file() {
        format!(" File: {} ", diff.path)
    } else if app.diff_viewing_staged() == Some(true) {
        format!(" Diff: {} (staged) ", diff.path)
    } else {
        format!(" Diff: {} (unstaged) ", diff.path)
    };
    if diff.hunks.is_empty() {
        frame.render_widget(
            Paragraph::new("(no changes)").block(panel_block(false, theme, title)),
            area,
        );
        return;
    }
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    // Total (cheap, no highlighting) for the "more lines" hint…
    let total: usize = diff_rows(diff).iter().map(unified_row_count).sum();
    let off = (app.diff_scroll() as usize).min(total);
    // …then highlight only the visible window so large files stay fast.
    let take = if total.saturating_sub(off) > inner_h && inner_h > 0 {
        inner_h.saturating_sub(1)
    } else {
        inner_h
    };
    let (mut shown, _) = render_unified_lines(diff, theme, inner_w, off, take.max(1));
    let remaining = total.saturating_sub(off + shown.len());
    if remaining > 0 && !shown.is_empty() {
        shown.push(Line::from(vec![Span::styled(
            format!("… {remaining} more lines — enter for full screen"),
            Style::default().fg(theme.hint),
        )]));
        // Keep exactly `inner_h` lines; drop the oldest visible line, not the
        // newest, so context above stays stable.
        if shown.len() > inner_h {
            shown.remove(0);
        }
    }
    frame.render_widget(
        Paragraph::new(shown).block(panel_block(false, theme, title)),
        area,
    );
}

/// Fullscreen side-by-side diff overlay (Enter opens, Esc closes).
fn render_fullscreen_diff(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Clear, area);
    render_diff_pane(frame, area, app);
}

fn render_diff_pane(frame: &mut Frame, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let theme = app.theme();
    let Some(diff) = app.diff() else {
        let title = if app.has_files() {
            " Full diff (loading…) ".to_string()
        } else {
            " Full diff ".to_string()
        };
        frame.render_widget(
            Paragraph::new("").block(panel_block(true, theme, title)),
            area,
        );
        return;
    };
    // LazyVim buffer header: file icon + path + mode, like `LazyVim ● file`.
    let title = if app.diff_whole_file() {
        format!(" Full file: {} ", diff.path)
    } else if app.diff_viewing_staged() == Some(true) {
        format!(" Full diff: {} (staged) ", diff.path)
    } else {
        format!(" Full diff: {} (unstaged) ", diff.path)
    };
    if diff.hunks.is_empty() {
        frame.render_widget(
            Paragraph::new("(no changes)").block(panel_block(true, theme, title)),
            area,
        );
        return;
    }
    // One Line per row: left half + divider + right half, so both panes
    // scroll together under a single scroll offset. Only the visible window
    // is syntax-highlighted (large whole-file views stay fast).
    let inner = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let half = inner.saturating_sub(1) / 2;
    let rows = diff_rows(diff);
    let gutter_w = gutter_width(&rows);
    let off = (app.diff_scroll() as usize).min(rows.len());
    let path = diff.path.as_str();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner_h);
    for row in rows.iter().skip(off).take(inner_h.max(1)) {
        match row {
            DiffRow::Header { index } => {
                let selected = *index == app.hunk();
                let header_style = if selected {
                    Style::default()
                        .fg(theme.hunk_header)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.hint)
                };
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "{} {}",
                        if selected { ">" } else { " " },
                        diff.hunks[*index].header
                    ),
                    header_style,
                )]));
            }
            DiffRow::Split { left, right } => {
                // Whole-file opens are one LazyVim buffer: each code line is
                // painted once, full width — never mirrored into both halves.
                if app.diff_whole_file() {
                    if left.kind == SideKind::Context {
                        lines.push(Line::from(render_side(left, path, inner, gutter_w, theme)));
                    } else {
                        // Defensive (production whole-file diffs are all
                        // context): stack old/new full-width so no side is
                        // silently dropped.
                        lines.push(Line::from(render_side(left, path, inner, gutter_w, theme)));
                        lines.push(Line::from(render_side(right, path, inner, gutter_w, theme)));
                    }
                    continue;
                }
                let mut spans = render_side(left, path, half, gutter_w, theme);
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(theme.line_nr).bg(theme.bg),
                ));
                spans.extend(render_side(
                    right,
                    path,
                    inner.saturating_sub(half + 1),
                    gutter_w,
                    theme,
                ));
                lines.push(Line::from(spans));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel_block(true, theme, title)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    if let Some(err) = app.error() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "Error: ",
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(err, Style::default().fg(theme.error)),
            ])),
            chunks[0],
        );
        frame.render_widget(footer_hints(app, theme), chunks[1]);
    } else {
        frame.render_widget(footer_hints(app, theme), area);
    }
}

fn footer_hints(app: &App, theme: Theme) -> Paragraph<'static> {
    let hints = match app.mode() {
        Mode::Committing => "Enter commit · Esc cancel",
        Mode::NewBranch => "Enter create branch · Esc cancel",
        Mode::StashPush => "Enter stash · Esc cancel",
        Mode::FullDiff => {
            "j/k hunk · ↑/↓ scroll · space stage hunk · PgUp/PgDn page · esc close · q quit"
        }
        Mode::Normal if app.focus() == Focus::Branches => {
            "enter checkout · a new branch · D delete · tab commits · q quit"
        }
        Mode::Normal if app.focus() == Focus::Log => "j/k scroll · tab stash · r refresh · q quit",
        Mode::Normal if app.focus() == Focus::Stash => {
            "enter pop · a stash · D drop · tab files · q quit"
        }
        Mode::FindFile => "type to filter · ↑/↓ move · enter open · esc cancel",
        Mode::Normal => {
            "space stage/unstage · on ▶ dir stages all · c commit · / find · enter full diff · r refresh · q quit"
        }
    };
    Paragraph::new(Line::styled(hints, Style::default().fg(theme.hint)))
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

/// Telescope-style fuzzy file finder (`/`): query line on top, ranked
/// matches below. Enter jumps the file cursor to the chosen match.
fn render_finder_modal(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let popup = centered_rect(area, 72, 14);
    frame.render_widget(Clear, popup);
    // Clear wipes to the terminal default; repaint the opaque base first.
    frame.render_widget(Block::default().style(Style::default().bg(theme.bg)), popup);
    let matches = app.finder_matches();
    let cursor = app.finder_cursor();
    let block = panel_block(
        true,
        theme,
        format!(
            " Find files ({} match{}) ",
            matches.len(),
            if matches.len() == 1 { "" } else { "es" }
        ),
    );
    let inner_w = popup.width.saturating_sub(2) as usize;
    let inner_h = popup.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner_h);
    // Query line.
    lines.push(Line::from(vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(theme.border_focused)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.draft().to_string(), Style::default().fg(theme.fg)),
    ]));
    // Match window follows the cursor (stateless: cursor only moves ±1 and
    // resets to 0 on every keystroke).
    let rows = inner_h.saturating_sub(1);
    let start = cursor
        .saturating_sub(rows.saturating_sub(1))
        .min(matches.len());
    if matches.is_empty() {
        lines.push(Line::styled(
            "(no matches)",
            Style::default().fg(theme.hint),
        ));
    }
    for (row, &index) in matches.iter().skip(start).take(rows).enumerate() {
        let selected = start + row == cursor;
        let (text, line_style) = match app.file_entry(index) {
            Some(entry) => {
                let (glyph, _) = state_glyph(entry.state, theme);
                (
                    format!("{} {}", glyph, entry.path),
                    if selected {
                        selection_style(theme)
                    } else {
                        Style::default().fg(theme.fg).bg(theme.bg)
                    },
                )
            }
            None => (
                "(gone)".to_string(),
                Style::default().fg(theme.hint).bg(theme.bg),
            ),
        };
        let marker = if selected { "> " } else { "  " };
        let used = marker.width() + text.width();
        let pad = inner_w.saturating_sub(used);
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), line_style),
            Span::styled(text, line_style),
            Span::styled(" ".repeat(pad), line_style),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).block(block), popup);
    // Cursor just after the query text.
    let cursor_x = popup.x + 1 + 2 + app.draft().width() as u16;
    let cursor_y = popup.y + 1;
    if cursor_x < popup.x + popup.width.saturating_sub(1) {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_input_modal(frame: &mut Frame, area: Rect, app: &App, title: &'static str) {
    let popup = centered_rect(area, 60, 3);
    frame.render_widget(Clear, popup);
    let input = Paragraph::new(app.draft()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title),
    );
    frame.render_widget(input, popup);
    // Cursor just after the draft text (single-line input).
    let cursor_x = popup.x + 1 + app.draft().len() as u16;
    let cursor_y = popup.y + 1;
    if cursor_x < popup.x + popup.width.saturating_sub(1) {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn branch_list_items(branches: &[BranchInfo], theme: Theme) -> Vec<ListItem<'static>> {
    branches
        .iter()
        .map(|b| {
            let (marker, style) = if b.is_head {
                (
                    "* ",
                    Style::default()
                        .fg(theme.branch_current)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(theme.hint))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(
                    b.name.clone(),
                    if b.is_head {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("  {}", b.tip_summary),
                    Style::default().fg(theme.hint),
                ),
            ]))
        })
        .collect()
}

fn render_branches_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let theme = app.theme();
    let focused = app.focus() == Focus::Branches;
    let Some(branches) = app.branches() else {
        frame.render_widget(
            Paragraph::new("loading…").block(panel_block(
                focused,
                theme,
                "[2]-Local branches".to_string(),
            )),
            area,
        );
        return;
    };
    if branches.is_empty() {
        frame.render_widget(
            Paragraph::new("(none)").block(panel_block(
                focused,
                theme,
                "[2]-Local branches (0)".to_string(),
            )),
            area,
        );
        return;
    }
    let sel = app.branch_selected().min(branches.len() - 1);
    let visible = area.height.saturating_sub(2) as usize;
    let off = follow_selection(sel, visible, app.branch_scroll());
    app.set_branch_scroll(off);
    let list = List::new(branch_list_items(branches, theme))
        .block(panel_block(
            focused,
            theme,
            format!("[2]-Local branches ({} of {})", sel + 1, branches.len()),
        ))
        .highlight_style(selection_style(theme))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select((visible > 0).then(|| sel.saturating_sub(off)));
    frame.render_stateful_widget(list, area, &mut state);
}

fn log_lines(entries: &[CommitInfo], theme: Theme) -> Vec<Line<'static>> {
    entries
        .iter()
        .map(|e| {
            Line::from(vec![
                Span::styled(
                    e.id.clone(),
                    Style::default()
                        .fg(theme.commit_id)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(e.summary.clone()),
                Span::styled(format!("  — {}", e.author), Style::default().fg(theme.hint)),
            ])
        })
        .collect()
}

fn render_commits_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let theme = app.theme();
    let focused = app.focus() == Focus::Log;
    let Some(entries) = app.log() else {
        frame.render_widget(
            Paragraph::new("loading…").block(panel_block(
                focused,
                theme,
                "[3]-Commits".to_string(),
            )),
            area,
        );
        return;
    };
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new("(no commits yet)").block(panel_block(
                focused,
                theme,
                "[3]-Commits (0)".to_string(),
            )),
            area,
        );
        return;
    }
    frame.render_widget(
        Paragraph::new(log_lines(entries, theme))
            .block(panel_block(
                focused,
                theme,
                format!("[3]-Commits ({})", entries.len()),
            ))
            .scroll((app.log_scroll(), 0)),
        area,
    );
}

fn stash_list_items(entries: &[StashEntry], theme: Theme) -> Vec<ListItem<'static>> {
    entries
        .iter()
        .map(|e| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("stash@{} ", e.index),
                    Style::default()
                        .fg(theme.commit_id)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(e.message.clone()),
            ]))
        })
        .collect()
}

fn render_stash_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let theme = app.theme();
    let focused = app.focus() == Focus::Stash;
    let Some(entries) = app.stash() else {
        frame.render_widget(
            Paragraph::new("loading…").block(panel_block(focused, theme, "[4]-Stash".to_string())),
            area,
        );
        return;
    };
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new("(no stashes)").block(panel_block(
                focused,
                theme,
                "[4]-Stash (0)".to_string(),
            )),
            area,
        );
        return;
    }
    let sel = app.stash_selected().min(entries.len() - 1);
    let visible = area.height.saturating_sub(2) as usize;
    let off = follow_selection(sel, visible, app.stash_scroll());
    app.set_stash_scroll(off);
    let list = List::new(stash_list_items(entries, theme))
        .block(panel_block(
            focused,
            theme,
            format!("[4]-Stash ({} of {})", sel + 1, entries.len()),
        ))
        .highlight_style(selection_style(theme))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select((visible > 0).then(|| sel.saturating_sub(off)));
    frame.render_stateful_widget(list, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_tui_core::jobqueue::JobQueue;
    use git_tui_core::status::{RepoStatus, StatusEntry};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn test_app() -> (tempfile::TempDir, App) {
        let dir = tempfile::TempDir::new().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let app = App::new(JobQueue::spawn(dir.path()).unwrap());
        (dir, app)
    }

    fn with_files(names: &[(&str, FileState)]) -> (tempfile::TempDir, App) {
        let (dir, mut app) = test_app();
        app.set_status_for_test(RepoStatus {
            branch: "main".into(),
            head_summary: "init".into(),
            files: names
                .iter()
                .map(|(p, s)| StatusEntry {
                    path: p.to_string(),
                    state: *s,
                })
                .collect(),
            tracked_files: vec![],
        });
        (dir, app)
    }

    fn screen(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_file_list_with_branch_and_selection() {
        let (_dir, app) =
            with_files(&[("a.txt", FileState::Unstaged), ("b.txt", FileState::Staged)]);
        let s = screen(&app, 80, 28);
        assert!(s.contains("main"), "branch missing:\n{s}");
        assert!(s.contains("a.txt"), "file missing:\n{s}");
        assert!(s.contains("b.txt"), "file missing:\n{s}");
        assert!(s.contains("[2]-Files"), "panel number missing:\n{s}");
    }

    #[test]
    fn renders_empty_tree_message() {
        let (_dir, app) = with_files(&[]);
        let s = screen(&app, 80, 28);
        assert!(s.contains("clean"), "empty message missing:\n{s}");
        assert!(s.contains("✓"), "clean marker missing:\n{s}");
    }

    #[test]
    fn file_rows_builds_nested_tree_in_first_seen_order() {
        let files = ["b.txt", "packages/agent-worker/src/worker.ts", "docs/x.md"]
            .iter()
            .map(|p| StatusEntry {
                path: p.to_string(),
                state: FileState::Unstaged,
            })
            .collect::<Vec<_>>();
        let rows = file_rows(&files);
        let dir_paths: Vec<(&str, usize)> = rows
            .iter()
            .filter_map(|r| match r {
                FileRow::Dir { path, depth } => Some((*path, *depth)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dir_paths,
            vec![
                ("packages", 0),
                ("packages/agent-worker", 1),
                ("packages/agent-worker/src", 2),
                ("docs", 0),
            ]
        );
        // Root file first with no header, then headers, then files at depth.
        assert_eq!(rows[0], FileRow::File { index: 0, depth: 0 });
        assert_eq!(rows[4], FileRow::File { index: 1, depth: 3 });
        assert_eq!(rows[6], FileRow::File { index: 2, depth: 1 });
        assert_eq!(rows.len(), 7);
    }

    #[test]
    fn collapsed_dir_hides_its_children_and_shows_folded_marker() {
        let (_dir, mut app) = with_files(&[
            ("src/a.rs", FileState::Unstaged),
            ("src/nested/b.rs", FileState::Unstaged),
            ("z.txt", FileState::Unstaged),
        ]);
        app.set_collapsed("src", true);
        let s = screen(&app, 100, 32);
        assert!(s.contains("▶ src/"), "collapsed header missing:\n{s}");
        assert!(!s.contains("a.rs"), "hidden child leaked:\n{s}");
        assert!(!s.contains("b.rs"), "hidden nested child leaked:\n{s}");
        assert!(s.contains("z.txt"), "visible file missing:\n{s}");
    }

    #[test]
    fn collapsing_nested_dir_keeps_sibling_visible() {
        let (_dir, mut app) = with_files(&[
            ("src/a.rs", FileState::Unstaged),
            ("src/nested/b.rs", FileState::Unstaged),
            ("z.txt", FileState::Unstaged),
        ]);
        app.set_collapsed("src/nested", true);
        let s = screen(&app, 100, 32);
        assert!(s.contains("▼ src/"), "expanded parent missing:\n{s}");
        assert!(s.contains("▶ nested/"), "collapsed header missing:\n{s}");
        assert!(s.contains("a.rs"), "visible sibling missing:\n{s}");
        assert!(!s.contains("b.rs"), "hidden nested child leaked:\n{s}");
        assert!(s.contains("z.txt"), "visible file missing:\n{s}");
    }

    #[test]
    fn files_panel_shows_tree_with_basename_and_counter() {
        let (_dir, app) = with_files(&[
            ("src/main.rs", FileState::Unstaged),
            ("src/app.rs", FileState::Staged),
            ("README.md", FileState::Untracked),
        ]);
        let s = screen(&app, 100, 32);
        assert!(s.contains("[2]-Files"), "panel number missing:\n{s}");
        assert!(s.contains("src/"), "dir header missing:\n{s}");
        assert!(s.contains("main.rs"), "basename missing:\n{s}");
        assert!(s.contains("1 of 3"), "counter missing:\n{s}");
        assert!(
            !s.contains("src/main.rs"),
            "full path should collapse to basename:\n{s}"
        );
    }

    #[test]
    fn status_panel_shows_repo_branch_and_dirty_count() {
        let (_dir, app) = with_files(&[("a.txt", FileState::Unstaged)]);
        let s = screen(&app, 80, 28);
        assert!(s.contains("[1]-Status"), "panel number missing:\n{s}");
        assert!(s.contains("repo → main"), "repo/branch missing:\n{s}");
        assert!(s.contains("(1)"), "dirty count missing:\n{s}");
    }

    #[test]
    fn compute_layout_gives_right_preview_most_space() {
        let l = compute_layout(Rect::new(0, 0, 100, 32), 1);
        assert_eq!(l.status, Rect::new(0, 0, 30, 3));
        assert_eq!(l.diff, Rect::new(30, 0, 70, 31));
        assert_eq!(l.files.x, 0);
        assert_eq!(l.files.width, 30);
        assert!(l.files.height >= 12);
        assert_eq!(l.footer, Rect::new(0, 31, 100, 1));
    }

    #[test]
    fn split_sides_clip_long_unicode_lines_to_their_width() {
        let side = Side {
            no: Some(1),
            segs: vec![WordSeg {
                text: "界".repeat(60),
                changed: true,
            }],
            kind: SideKind::Del,
        };
        for width in [0, 2, 6, 20, 31] {
            let spans = render_side(&side, "a.rs", width, 4, Theme::tokyo_night());
            assert_eq!(spans.iter().map(Span::width).sum::<usize>(), width);
        }
    }

    #[test]
    fn diff_washes_preserve_readable_syntax_colors() {
        for theme in [Theme::default_theme(), Theme::tokyo_night()] {
            for kind in [SideKind::Del, SideKind::Add] {
                let side = Side {
                    no: Some(1),
                    segs: vec![WordSeg {
                        text: "fn main() {}".into(),
                        changed: true,
                    }],
                    kind,
                };
                let spans = render_side(&side, "main.rs", 40, 4, theme);
                let keyword = spans.iter().find(|s| s.content == "fn").unwrap();
                let Color::Rgb(r, g, b) = keyword.style.bg.unwrap() else {
                    panic!("RGB wash required")
                };
                assert!(
                    r.max(g).max(b) < 100,
                    "wash must be dark enough for syntax colors"
                );
                assert_ne!(keyword.style.fg, keyword.style.bg);
            }
        }
    }

    #[test]
    fn renders_commit_modal_with_draft() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.on_key(KeyCode::Char('c'));
        app.on_key(KeyCode::Char('h'));
        app.on_key(KeyCode::Char('i'));
        let s = screen(&app, 40, 10);
        assert!(s.contains("Commit"), "modal title missing:\n{s}");
        assert!(s.contains("hi"), "draft missing:\n{s}");
    }

    #[test]
    fn renders_error_line() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Conflicted)]);
        app.on_key(KeyCode::Char(' '));
        let s = screen(&app, 40, 10);
        assert!(s.contains("conflicted"), "error missing:\n{s}");
    }

    fn sample_diff() -> git_tui_core::diff::FileDiff {
        use git_tui_core::diff::{DiffLine, Hunk, LineKind};
        git_tui_core::diff::FileDiff {
            path: "a.txt".into(),
            hunks: vec![
                Hunk {
                    header: "@@ -1,3 +1,3 @@".into(),
                    old_start: 1,
                    new_start: 1,
                    lines: vec![
                        DiffLine {
                            kind: LineKind::Context,
                            text: "same".into(),
                        },
                        DiffLine {
                            kind: LineKind::Del,
                            text: "hello world".into(),
                        },
                        DiffLine {
                            kind: LineKind::Add,
                            text: "hello WORLD".into(),
                        },
                    ],
                },
                Hunk {
                    header: "@@ -30,2 +30,2 @@".into(),
                    old_start: 30,
                    new_start: 30,
                    lines: vec![DiffLine {
                        kind: LineKind::Add,
                        text: "brand new".into(),
                    }],
                },
            ],
        }
    }

    fn render_buf(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Single-hunk diff small enough to fit the inline preview whole.
    fn mini_diff() -> git_tui_core::diff::FileDiff {
        use git_tui_core::diff::{DiffLine, Hunk, LineKind};
        git_tui_core::diff::FileDiff {
            path: "a.txt".into(),
            hunks: vec![Hunk {
                header: "@@ -1,2 +1,2 @@".into(),
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        kind: LineKind::Context,
                        text: "same".into(),
                    },
                    DiffLine {
                        kind: LineKind::Del,
                        text: "hello world".into(),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        text: "hello WORLD".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn renders_diff_pane_with_hunks_and_changed_words() {
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_diff_for_test(mini_diff(), false);
        let s = screen(&app, 70, 14);
        assert!(s.contains("a.txt"), "diff title missing:\n{s}");
        assert!(s.contains("@@ -1,2 +1,2 @@"), "hunk header missing:\n{s}");
        assert!(s.contains("WORLD"), "added line missing:\n{s}");
        assert!(s.contains("unstaged"), "staged label missing:\n{s}");
    }

    #[test]
    fn diff_rows_pair_old_and_new_numbers() {
        use git_tui_core::diff::{DiffLine, FileDiff, Hunk, LineKind};
        let diff = FileDiff {
            path: "a.txt".into(),
            hunks: vec![Hunk {
                header: "@@ -10,3 +20,3 @@".into(),
                old_start: 10,
                new_start: 20,
                lines: vec![
                    DiffLine {
                        kind: LineKind::Context,
                        text: "same".into(),
                    },
                    DiffLine {
                        kind: LineKind::Del,
                        text: "old".into(),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        text: "new".into(),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        text: "extra".into(),
                    },
                ],
            }],
        };
        let rows = diff_rows(&diff);
        assert!(matches!(rows[0], DiffRow::Header { index: 0 }));
        // Context fills both sides with their own numbers.
        let DiffRow::Split { left, right } = &rows[1] else {
            panic!("expected split, got {:?}", rows[1]);
        };
        assert_eq!((left.no, right.no), (Some(10), Some(20)));
        assert_eq!(left.kind, SideKind::Context);
        // Del/add pair shares one row, old on the left, new on the right.
        let DiffRow::Split { left, right } = &rows[2] else {
            panic!("expected split, got {:?}", rows[2]);
        };
        assert_eq!((left.no, right.no), (Some(11), Some(21)));
        assert_eq!(left.kind, SideKind::Del);
        assert_eq!(right.kind, SideKind::Add);
        assert_eq!(
            left.segs
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "old"
        );
        assert_eq!(
            right
                .segs
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "new"
        );
        // Leftover add gets a blank left counterpart; numbering continues.
        let DiffRow::Split { left, right } = &rows[3] else {
            panic!("expected split, got {:?}", rows[3]);
        };
        assert_eq!((left.no, right.no), (None, Some(22)));
        assert_eq!(left.kind, SideKind::Blank);
        assert_eq!(right.kind, SideKind::Add);
    }

    #[test]
    fn hunk_start_row_counts_header_and_paired_rows() {
        let diff = sample_diff();
        assert_eq!(hunk_start_row(&diff, 0), 0);
        // Hunk 0 = header + context row + one paired del/add row.
        assert_eq!(hunk_start_row(&diff, 1), 3);
        // Unknown hunk falls back to the top.
        assert_eq!(hunk_start_row(&diff, 9), 0);
    }

    #[test]
    fn side_by_side_renders_gutters_divider_and_single_add_wash() {
        use crate::config::Theme;
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_diff_for_test(sample_diff(), false);
        // Fullscreen overlay: the side-by-side view.
        app.on_key(KeyCode::Enter);
        let s = screen(&app, 70, 14);
        // Old|new divider and both gutter numbers on the context row.
        assert!(s.contains("│"), "divider missing:\n{s}");
        assert!(s.contains("   1 "), "line numbers missing:\n{s}");
        assert!(s.contains("brand new"), "added line missing:\n{s}");
        // Additions carry one uniform light-green wash: even the changed
        // "WORLD" run keeps the line wash so syntax colors stay readable
        // (green is never painted twice). The shared "hello " prefix carries
        // each side's own wash (del red left, light green right).
        let buf = render_buf(&app, 70, 14);
        let theme = Theme::default_theme();
        let mut found_word = false;
        let mut hello_washes = std::collections::HashSet::new();
        for y in 0..buf.area.height {
            let cells: Vec<String> = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            for x in 0..cells.len().saturating_sub(5) {
                if cells[x..x + 5] == ["W", "O", "R", "L", "D"] {
                    found_word = true;
                    for i in 0..5 {
                        let cell = &buf[(x as u16 + i as u16, y)];
                        assert_eq!(
                            cell.bg,
                            theme.diff_add_bg,
                            "added run must carry the single add wash at ({}, {y})",
                            x + i as usize
                        );
                    }
                }
                if cells[x..x + 5] == ["h", "e", "l", "l", "o"] {
                    hello_washes.insert(buf[(x as u16, y)].bg);
                }
            }
        }
        assert!(found_word, "WORLD not rendered");
        assert!(
            hello_washes.contains(&theme.diff_del_bg),
            "old side missing del wash: {hello_washes:?}"
        );
        assert!(
            hello_washes.contains(&theme.diff_add_bg),
            "new side missing add wash: {hello_washes:?}"
        );
    }

    #[test]
    fn addition_lines_carry_single_light_wash() {
        use crate::config::Theme;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_diff_for_test(mini_diff(), false);
        let buf = render_buf(&app, 70, 14);
        let theme = Theme::default_theme();
        // "WORLD" is a changed run on an added line, but additions are
        // painted once: every cell carries the light line wash (background
        // only, syntax foreground stays readable).
        // NOTE: compare per-cell (box-drawing borders are multi-byte, so
        // byte indices from String::find do not equal cell columns).
        let mut found = false;
        for y in 0..buf.area.height {
            let cells: Vec<String> = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            for x in 0..cells.len().saturating_sub(5) {
                if cells[x..x + 5] == ["W", "O", "R", "L", "D"] {
                    found = true;
                    for i in 0..5 {
                        let cell = &buf[(x as u16 + i as u16, y)];
                        assert_eq!(
                            cell.bg,
                            theme.diff_add_bg,
                            "added run must carry the single add wash at ({}, {y})",
                            x + i as usize
                        );
                    }
                }
            }
        }
        assert!(found, "WORLD not rendered");
    }

    #[test]
    fn selected_hunk_is_marked() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_diff_for_test(sample_diff(), false);
        app.on_key(KeyCode::Enter);
        app.on_key(KeyCode::Char('j'));
        let s = screen(&app, 70, 16);
        assert!(
            s.contains("> @@ -30,2 +30,2 @@"),
            "selected hunk not marked:\n{s}"
        );
    }

    #[test]
    fn esc_closes_fullscreen_back_to_files() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_diff_for_test(sample_diff(), false);
        app.on_key(KeyCode::Enter);
        assert_eq!(app.mode(), Mode::FullDiff);
        let s = screen(&app, 100, 32);
        assert!(s.contains("Full diff"), "fullscreen title missing:\n{s}");
        assert!(s.contains("│"), "fullscreen divider missing:\n{s}");
        app.on_key(KeyCode::Esc);
        assert_eq!(app.mode(), Mode::Normal);
        let s = screen(&app, 100, 32);
        assert!(!s.contains("Full diff"), "overlay should be gone:\n{s}");
        assert!(s.contains("[2]-Files"), "rail should be back:\n{s}");
    }

    #[test]
    fn fullscreen_whole_file_view_has_file_title() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_diff_for_test(mini_diff(), false);
        app.set_whole_file_for_test(true);
        app.on_key(KeyCode::Enter);
        assert_eq!(app.mode(), Mode::FullDiff);
        let s = screen(&app, 100, 32);
        assert!(s.contains("Full file"), "whole-file title missing:\n{s}");
        assert!(s.contains("a.txt"), "filename missing:\n{s}");
    }

    #[test]
    fn focus_ring_highlights_active_pane() {
        use crossterm::event::KeyCode;
        use ratatui::layout::Rect;
        let (_dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        let layout = compute_layout(Rect::new(0, 0, 100, 32), 1);
        // Status focus drives the files list, so the files panel glows
        // while the status strip and diff preview stay dim.
        let buf = render_buf(&app, 100, 32);
        assert_eq!(buf[(layout.files.x, layout.files.y)].fg, Color::White);
        assert_eq!(buf[(layout.status.x, layout.status.y)].fg, Color::DarkGray);
        assert_eq!(buf[(layout.diff.x, layout.diff.y)].fg, Color::DarkGray);
        // Focus branches: branches corner goes bright, files goes dim.
        app.on_key(KeyCode::Tab);
        let buf = render_buf(&app, 100, 32);
        assert_eq!(buf[(layout.branches.x, layout.branches.y)].fg, Color::White);
        assert_eq!(buf[(layout.files.x, layout.files.y)].fg, Color::DarkGray);
    }

    /// End-to-end: a loaded theme (here tokyo-night) must reach the pixels,
    /// not just sit in the config struct.
    #[test]
    fn loaded_theme_reaches_the_screen() {
        use crate::config::{Config, KeyBindings, Theme};
        use crossterm::event::KeyCode;
        use ratatui::layout::Rect;
        let dir = tempfile::TempDir::new().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let config = Config {
            keys: KeyBindings::default(),
            theme: Theme::by_name("tokyo-night").unwrap(),
        };
        let mut app = App::new_with_config(JobQueue::spawn(dir.path()).unwrap(), config);
        app.set_status_for_test(RepoStatus {
            branch: "main".into(),
            head_summary: "init".into(),
            files: vec![StatusEntry {
                path: "a.txt".into(),
                state: FileState::Unstaged,
            }],
            tracked_files: vec!["a.txt".into()],
        });
        // Tokyo-night focused border is blue #7aa2f7, unfocused #3b4261 —
        // neither equals the legacy White/DarkGray. Status focus drives
        // the files list, so the files panel glows first.
        let layout = compute_layout(Rect::new(0, 0, 100, 32), 1);
        let buf = render_buf(&app, 100, 32);
        assert_eq!(
            buf[(layout.files.x, layout.files.y)].fg,
            Color::Rgb(122, 162, 247)
        );
        assert_eq!(
            buf[(layout.diff.x, layout.diff.y)].fg,
            Color::Rgb(59, 66, 97)
        );
        // Focusing branches flips that panel's border to the focused color.
        app.on_key(KeyCode::Tab);
        let buf = render_buf(&app, 100, 32);
        assert_eq!(
            buf[(layout.branches.x, layout.branches.y)].fg,
            Color::Rgb(122, 162, 247)
        );
    }

    fn with_branches() -> (tempfile::TempDir, App) {
        use git_tui_core::branch::BranchInfo;
        let (dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_branches_for_test(vec![
            BranchInfo {
                name: "feat".into(),
                is_head: false,
                tip_summary: "wip".into(),
            },
            BranchInfo {
                name: "main".into(),
                is_head: true,
                tip_summary: "init".into(),
            },
        ]);
        (dir, app)
    }

    #[test]
    fn renders_branches_pane_with_current_marked() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_branches();
        app.on_key(KeyCode::Char('2'));
        let s = screen(&app, 100, 32);
        assert!(s.contains("Local branches"), "pane title missing:\n{s}");
        assert!(s.contains("feat"), "branch missing:\n{s}");
        assert!(s.contains("main"), "branch missing:\n{s}");
        assert!(s.contains("wip"), "tip summary missing:\n{s}");
        assert!(s.contains('*'), "current marker missing:\n{s}");
    }

    #[test]
    fn renders_new_branch_modal() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_branches();
        app.on_key(KeyCode::Char('2'));
        app.on_key(KeyCode::Char('a'));
        app.on_key(KeyCode::Char('x'));
        let s = screen(&app, 70, 12);
        assert!(s.contains("New branch"), "modal title missing:\n{s}");
    }

    fn with_log() -> (tempfile::TempDir, App) {
        use git_tui_core::log::CommitInfo;
        let (dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_log_for_test(vec![
            CommitInfo {
                id: "abc1234".into(),
                summary: "second".into(),
                author: "Test User".into(),
            },
            CommitInfo {
                id: "def5678".into(),
                summary: "init".into(),
                author: "Test User".into(),
            },
        ]);
        (dir, app)
    }

    #[test]
    fn renders_log_pane_newest_first() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_log();
        app.on_key(KeyCode::Char('3'));
        let s = screen(&app, 100, 32);
        assert!(s.contains("Commits"), "pane title missing:\n{s}");
        assert!(s.contains("second"), "entry missing:\n{s}");
        assert!(s.contains("abc1234"), "short id missing:\n{s}");
        let newest = s.find("abc1234").unwrap();
        let older = s.find("def5678").unwrap();
        assert!(newest < older, "newest must come first:\n{s}");
    }

    fn with_stash() -> (tempfile::TempDir, App) {
        use git_tui_core::stash::StashEntry;
        let (dir, mut app) = with_files(&[("a.txt", FileState::Unstaged)]);
        app.set_stash_for_test(vec![StashEntry {
            index: 0,
            message: "On main: wip".into(),
        }]);
        (dir, app)
    }

    #[test]
    fn renders_stash_pane_with_entries() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_stash();
        app.on_key(KeyCode::Char('4'));
        let s = screen(&app, 100, 32);
        assert!(s.contains("Stash"), "pane title missing:\n{s}");
        assert!(s.contains("stash@0"), "entry missing:\n{s}");
        assert!(s.contains("wip"), "message missing:\n{s}");
    }

    /// All-context diff: what a clean file's whole-file view loads.
    fn whole_file_sample() -> git_tui_core::diff::FileDiff {
        use git_tui_core::diff::{DiffLine, Hunk, LineKind};
        git_tui_core::diff::FileDiff {
            path: "main.rs".into(),
            hunks: vec![Hunk {
                header: "@@ -1,3 +1,3 @@".into(),
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        kind: LineKind::Context,
                        text: "fn main() {".into(),
                    },
                    DiffLine {
                        kind: LineKind::Context,
                        text: "println!(\"hi\");".into(),
                    },
                    DiffLine {
                        kind: LineKind::Context,
                        text: "}".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn whole_file_fullscreen_paints_each_line_once() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("main.rs", FileState::Clean)]);
        app.set_diff_for_test(whole_file_sample(), false);
        app.set_whole_file_for_test(true);
        app.on_key(KeyCode::Enter);
        assert_eq!(app.mode(), Mode::FullDiff);
        let s = screen(&app, 100, 32);
        // Single LazyVim buffer: the code line appears exactly once (never
        // mirrored into a second half-pane) and there is no side-by-side
        // divider.
        assert_eq!(
            s.matches("println!").count(),
            1,
            "whole-file line painted more than once:\n{s}"
        );
        // No side-by-side divider: interior cells (outside the rounded
        // panel borders) never contain the column separator.
        for line in s.lines() {
            let chars: Vec<char> = line.chars().collect();
            if chars.len() > 2 {
                let interior: String = chars[1..chars.len() - 1].iter().collect();
                assert!(
                    !interior.contains("\u{2502}"),
                    "single-pane file view must not have a divider:\n{s}"
                );
            }
        }
    }

    #[test]
    fn whole_file_fullscreen_has_opaque_background() {
        use crate::config::Theme;
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[("main.rs", FileState::Clean)]);
        app.set_diff_for_test(whole_file_sample(), false);
        app.set_whole_file_for_test(true);
        app.on_key(KeyCode::Enter);
        let buf = render_buf(&app, 100, 32);
        let theme = Theme::default_theme();
        // Code row is fully opaque: every cell of the `println!` line sits
        // on the editor background (no wallpaper bleed-through).
        let mut found = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if row.contains("println!") {
                found = true;
                for x in 0..buf.area.width {
                    assert_eq!(
                        buf[(x, y)].bg,
                        theme.bg,
                        "transparent cell at ({x}, {y}): {row:?}"
                    );
                }
            }
        }
        assert!(found, "println! line not rendered");
    }

    #[test]
    fn renders_finder_popup_with_filtered_matches() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_files(&[
            ("src/main.rs", FileState::Unstaged),
            ("src/app.rs", FileState::Staged),
        ]);
        app.on_key(KeyCode::Char('/'));
        for c in "main".chars() {
            app.on_key(KeyCode::Char(c));
        }
        let s = screen(&app, 100, 32);
        assert!(s.contains("Find files"), "finder title missing:\n{s}");
        assert!(s.contains("main.rs"), "match missing:\n{s}");
        assert!(s.contains("1 match"), "match count missing:\n{s}");
    }

    #[test]
    fn renders_stash_push_modal() {
        use crossterm::event::KeyCode;
        let (_dir, mut app) = with_stash();
        app.on_key(KeyCode::Char('4'));
        app.on_key(KeyCode::Char('a'));
        app.on_key(KeyCode::Char('w'));
        let s = screen(&app, 70, 12);
        assert!(s.contains("Stash message"), "modal title missing:\n{s}");
    }
}
