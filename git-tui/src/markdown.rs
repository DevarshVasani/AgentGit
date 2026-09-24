//! Rendered Markdown preview for the terminal (Zed / browser-like).
//!
//! Raw `.md` source is syntax-colored elsewhere; this module parses it
//! with `pulldown-cmark` and emits `ratatui` lines: bold headings,
//! wrapped paragraphs, bullets / numbered lists, task checkboxes,
//! blockquotes, fenced code (re-highlighted via `syntax`), tables,
//! links, image placeholders and horizontal rules.

use crate::config::Theme;
use crate::syntax::highlight_line;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Markdown source files (mirrors the `mdx|mkd` grammar fallback in
/// `syntax.rs`).
pub fn is_markdown_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".markdown")
        || lower.ends_with(".mkd")
        || lower.ends_with(".mdx")
}

/// Map a fenced-code info string to a fake path so the existing
/// syntect highlighter picks a grammar (`rust` -> `x.rs`, …).
fn lang_to_path(lang: &str) -> &str {
    match lang.to_ascii_lowercase().as_str() {
        "rs" | "rust" => "x.rs",
        "py" | "python" => "x.py",
        "js" | "javascript" | "mjs" | "cjs" => "x.js",
        "ts" | "typescript" | "tsx" | "mts" | "cts" | "jsx" => "x.js",
        "json" => "x.json",
        "toml" => "x.toml",
        "yaml" | "yml" => "x.yaml",
        "sh" | "bash" | "zsh" => "x.sh",
        "diff" | "patch" => "x.diff",
        "go" => "x.go",
        "c" => "x.c",
        "cpp" | "c++" | "cc" => "x.cpp",
        "java" => "x.java",
        "rb" | "ruby" => "x.rb",
        "html" | "xml" => "x.html",
        "css" => "x.css",
        "md" | "markdown" => "x.md",
        _ => "x.txt",
    }
}

#[derive(Debug, Clone)]
struct ListCtx {
    ordered: bool,
    next_num: u64,
}

#[derive(Debug, Default)]
struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    cur_row: Vec<String>,
    cur_cell: String,
    in_head: bool,
    aligns: Vec<Alignment>,
}

struct Renderer {
    theme: Theme,
    /// Inner render width (for full-width rules / fitting tables).
    /// 0 means unknown (tests / narrow fallback).
    width: usize,
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    bold: usize,
    italic: usize,
    strike: usize,
    link_depth: usize,
    img_src: Vec<String>,
    heading: Option<u32>,
    quote_depth: usize,
    lists: Vec<ListCtx>,
    pending_item_prefix: Option<String>,
    code_block_lang: Option<String>,
    code_block_buf: String,
    table: Option<Table>,
    table_cell_active: bool,
}

impl Renderer {
    fn new(theme: Theme, width: usize) -> Self {
        Self {
            theme,
            width,
            lines: Vec::new(),
            cur: Vec::new(),
            bold: 0,
            italic: 0,
            strike: 0,
            link_depth: 0,
            img_src: Vec::new(),
            heading: None,
            quote_depth: 0,
            lists: Vec::new(),
            pending_item_prefix: None,
            code_block_lang: None,
            code_block_buf: String::new(),
            table: None,
            table_cell_active: false,
        }
    }

    /// Browser-like heading style: GitHub renders every level bold;
    /// H1/H2 gain a full-width bottom border. No `#` markers shown.
    fn heading_style(&self) -> Style {
        Style::default()
            .fg(self.theme.syntax_function)
            .add_modifier(Modifier::BOLD)
    }

    fn base_style(&self) -> Style {
        let mut s = Style::default().fg(self.theme.fg);
        if self.heading.is_some() {
            return self.heading_style();
        } else {
            if self.bold > 0 {
                s = s.add_modifier(Modifier::BOLD);
            }
            if self.italic > 0 || self.quote_depth > 0 {
                s = s.add_modifier(Modifier::ITALIC);
            }
            if self.strike > 0 {
                s = s.add_modifier(Modifier::CROSSED_OUT);
            }
            if !self.img_src.is_empty() {
                // Browser shows the image itself; in the terminal the
                // alt text stands in, dimmed italic with a 🖼 marker
                // (the `🖼 ` prefix is pushed on image start).
                s = Style::default()
                    .fg(self.theme.hint)
                    .add_modifier(Modifier::ITALIC);
            }
            if self.link_depth > 0 {
                // Browser-like link: blue underlined text, URL hidden.
                s = Style::default()
                    .fg(self.theme.syntax_type)
                    .add_modifier(Modifier::UNDERLINED);
                if self.bold > 0 {
                    s = s.add_modifier(Modifier::BOLD);
                }
                if self.italic > 0 {
                    s = s.add_modifier(Modifier::ITALIC);
                }
            }
        }
        s
    }

    fn flush_cur(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        if self.quote_depth > 0 {
            spans.push(Span::styled(
                format!("{} ", "▎".repeat(self.quote_depth)),
                Style::default().fg(self.theme.hint),
            ));
        }
        spans.extend(std::mem::take(&mut self.cur));
        self.lines.push(Line::from(spans));
    }

    fn blank_line(&mut self) {
        self.flush_cur();
        if !self.lines.is_empty() && !self.lines.last().map(|l| l.width() == 0).unwrap_or(false) {
            self.lines.push(Line::from(""));
        }
    }

    fn ensure_item_prefix(&mut self) {
        if let Some(prefix) = self.pending_item_prefix.take() {
            if self.cur.is_empty() {
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                if !indent.is_empty() {
                    self.cur.push(Span::raw(indent));
                }
                self.cur.push(Span::styled(
                    prefix,
                    Style::default()
                        .fg(self.theme.hint)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                // Continuation text inside the same item: separate with space.
                self.cur.push(Span::raw(" "));
            }
        } else if self.quote_depth > 0 && self.cur.is_empty() {
            // Quote prefix is added in flush_cur; nothing to do here.
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.code_block_lang.is_some() {
            self.code_block_buf.push_str(text);
            return;
        }
        if let Some(t) = self.table.as_mut() {
            if self.table_cell_active {
                t.cur_cell.push_str(text);
                return;
            }
        }
        self.ensure_item_prefix();
        // Split on newlines inside a Text event (can happen with
        // soft-break-less source) so each becomes its own visual line.
        let mut parts = text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                self.cur
                    .push(Span::styled(part.to_string(), self.base_style()));
            }
            if parts.peek().is_some() {
                self.flush_cur();
            }
        }
    }

    fn push_code_inline(&mut self, code: &str) {
        if self.code_block_lang.is_some() {
            self.code_block_buf.push_str(code);
            return;
        }
        if let Some(t) = self.table.as_mut() {
            if self.table_cell_active {
                t.cur_cell.push_str(&format!("`{code}`"));
                return;
            }
        }
        self.ensure_item_prefix();
        // Browser-like inline code: tinted pill, not just green text.
        self.cur.push(Span::styled(
            code.to_string(),
            Style::default()
                .fg(self.theme.syntax_string)
                .bg(self.theme.selection_bg),
        ));
    }

    /// Browser-like fenced block: shaded background with syntax colors,
    /// no ``` fences (GitHub shows a plain shaded block).
    fn finish_code_block(&mut self) {
        let lang = self.code_block_lang.take().unwrap_or_default();
        let buf = std::mem::take(&mut self.code_block_buf);
        self.flush_cur();
        let code_bg = self.theme.selection_bg;
        // Blank shaded line above/below gives the rounded-block feel.
        self.lines.push(Line::from(Span::styled(
            " ".to_string(),
            Style::default().bg(code_bg),
        )));
        let path = lang_to_path(lang.split_whitespace().next().unwrap_or(""));
        let mut any = false;
        for line in buf.lines() {
            any = true;
            if line.is_empty() {
                self.lines.push(Line::from(Span::styled(
                    " ".to_string(),
                    Style::default().bg(code_bg),
                )));
                continue;
            }
            let mut spans = Vec::new();
            for tok in highlight_line(path, line, self.theme) {
                spans.push(Span::styled(
                    tok.text,
                    Style::default()
                        .fg(tok.fg)
                        .bg(code_bg)
                        .add_modifier(tok.modifier),
                ));
            }
            // Pad the row so the wash spans the full width like a
            // browser <pre> background.
            spans.push(Span::styled(" ".to_string(), Style::default().bg(code_bg)));
            self.lines.push(Line::from(spans));
        }
        if !any {
            self.lines.push(Line::from(Span::styled(
                " ".to_string(),
                Style::default().bg(code_bg),
            )));
        }
        self.lines.push(Line::from(Span::styled(
            " ".to_string(),
            Style::default().bg(code_bg),
        )));
        self.lines.push(Line::from(""));
    }

    /// Browser-like table: full box borders, bold header, per-column
    /// alignment (`:---`, `:---:`, `---:`), fitted to the render width
    /// so rows never soft-wrap mid-border (GitHub scrolls instead).
    fn finish_table(&mut self) {
        let Some(t) = self.table.take() else { return };
        self.flush_cur();
        if t.headers.is_empty() && t.rows.is_empty() {
            return;
        }
        let ncols = t
            .headers
            .len()
            .max(t.rows.iter().map(|r| r.len()).max().unwrap_or(0));
        if ncols == 0 {
            return;
        }
        let mut aligns = t.aligns.clone();
        aligns.resize(ncols, Alignment::None);
        // Truncate giant cells first (browser scrolls; we cap).
        let trunc = |s: &str| -> String {
            const MAX: usize = 40;
            if UnicodeWidthStr::width(s) <= MAX {
                return s.to_string();
            }
            let mut out = String::new();
            let mut used = 0;
            for ch in s.chars() {
                let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if used + w + 1 > MAX {
                    break;
                }
                out.push(ch);
                used += w;
            }
            out.push('…');
            out
        };
        let mut norm_headers: Vec<String> = t.headers.iter().map(|h| trunc(h)).collect();
        norm_headers.resize(ncols, String::new());
        let mut norm_rows: Vec<Vec<String>> = t
            .rows
            .into_iter()
            .map(|r| {
                let mut r: Vec<String> = r.iter().map(|c| trunc(c)).collect();
                r.resize(ncols, String::new());
                r
            })
            .collect();
        let mut widths = vec![3usize; ncols];
        for (i, h) in norm_headers.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(h.as_str()).max(3));
        }
        for r in &norm_rows {
            for (i, c) in r.iter().enumerate() {
                widths[i] = widths[i].max(UnicodeWidthStr::width(c.as_str()));
            }
        }
        // Fit to the render width: borders cost 3*ncols+1 cells.
        let avail = self.width.saturating_sub(3 * ncols + 1).max(ncols * 3);
        let total: usize = widths.iter().sum();
        if total > avail {
            let mut budget = avail;
            for (i, w) in widths.iter_mut().enumerate() {
                let fair = (avail / ncols).max(3);
                let give = (*w)
                    .min(fair)
                    .min(budget.saturating_sub(3 * (ncols - i - 1)));
                *w = give.max(3);
                budget = budget.saturating_sub(*w);
            }
            // Re-truncate to the fitted widths.
            let fit = |s: &str, w: usize| -> String {
                if UnicodeWidthStr::width(s) <= w {
                    return s.to_string();
                }
                let mut out = String::new();
                let mut used = 0;
                for ch in s.chars() {
                    let chw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if used + chw + 1 > w {
                        break;
                    }
                    out.push(ch);
                    used += chw;
                }
                out.push('…');
                out
            };
            for (i, h) in norm_headers.iter_mut().enumerate() {
                *h = fit(h, widths[i]);
            }
            for r in norm_rows.iter_mut() {
                for (i, c) in r.iter_mut().enumerate() {
                    *c = fit(c, widths[i]);
                }
            }
        }
        let border = Style::default().fg(self.theme.line_nr);
        let header_style = Style::default()
            .fg(self.theme.fg)
            .add_modifier(Modifier::BOLD);
        let cell_style = Style::default().fg(self.theme.fg);
        // Pad one cell per alignment (None/Left pad right, Right pad
        // left, Center split) — like the browser, not like `:---:` text.
        let pad_cell = |text: &str, w: usize, align: Alignment| -> (String, String) {
            let cw = UnicodeWidthStr::width(text).min(w);
            let pad = w.saturating_sub(cw);
            match align {
                Alignment::Right => (" ".repeat(pad), String::new()),
                Alignment::Center => {
                    let l = pad / 2;
                    (" ".repeat(l), " ".repeat(pad - l))
                }
                _ => (String::new(), " ".repeat(pad)),
            }
        };
        let emit_row = |cells: &[String], style: Style, lines: &mut Vec<Line<'static>>| {
            let mut spans = vec![Span::styled("│ ", border)];
            for (i, c) in cells.iter().enumerate() {
                let (lp, rp) = pad_cell(c, widths[i], aligns[i]);
                spans.push(Span::raw(lp));
                spans.push(Span::styled(c.clone(), style));
                spans.push(Span::raw(rp));
                spans.push(Span::styled(" │ ", border));
            }
            lines.push(Line::from(spans));
        };
        let rule = |l: char, m: char, r: char| -> String {
            let mut s = String::from(l);
            for (i, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(*w + 2));
                if i + 1 < widths.len() {
                    s.push(m);
                } else {
                    s.push(r);
                }
            }
            s
        };
        self.lines
            .push(Line::from(Span::styled(rule('┌', '┬', '┐'), border)));
        emit_row(&norm_headers, header_style, &mut self.lines);
        self.lines
            .push(Line::from(Span::styled(rule('├', '┼', '┤'), border)));
        for r in std::mem::take(&mut norm_rows) {
            emit_row(&r, cell_style, &mut self.lines);
        }
        self.lines
            .push(Line::from(Span::styled(rule('└', '┴', '┘'), border)));
        self.lines.push(Line::from(""));
    }
}

/// Parse + style Markdown into logical (unwrapped) lines.
/// `width` sizes full-width rules (H1/H2 borders, `---`) and fits
/// tables; pass the same inner width the caller will wrap to.
fn parse_markdown(text: &str, theme: Theme, width: usize) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_SMART_PUNCTUATION);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    let parser = Parser::new_ext(text, opts);
    let mut r = Renderer::new(theme, width);

    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    r.flush_cur();
                }
                Tag::Heading { level, .. } => {
                    r.flush_cur();
                    // Browser-like: no `#` markers, just bold text
                    // (H1/H2 gain a bottom border on End).
                    r.heading = Some(level as u32);
                }
                Tag::BlockQuote(_) => {
                    r.flush_cur();
                    r.quote_depth += 1;
                }
                Tag::CodeBlock(kind) => {
                    r.flush_cur();
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    r.code_block_lang = Some(lang);
                    r.code_block_buf.clear();
                }
                Tag::List(num) => {
                    r.flush_cur();
                    r.lists.push(ListCtx {
                        ordered: num.is_some(),
                        next_num: num.unwrap_or(1),
                    });
                }
                Tag::Item => {
                    r.flush_cur();
                    let prefix = if let Some(ctx) = r.lists.last() {
                        if ctx.ordered {
                            format!("{}. ", ctx.next_num)
                        } else {
                            "• ".to_string()
                        }
                    } else {
                        "• ".to_string()
                    };
                    r.pending_item_prefix = Some(prefix);
                }
                Tag::Emphasis => r.italic += 1,
                Tag::Strong => r.bold += 1,
                Tag::Strikethrough => r.strike += 1,
                Tag::Link { .. } => {
                    // Browser hides the URL: link text alone, blue.
                    r.link_depth += 1;
                }
                Tag::Image { dest_url, .. } => {
                    // Browser shows the picture; the terminal shows the
                    // alt text with a 🖼 marker plus a dimmed caption.
                    r.ensure_item_prefix();
                    r.cur.push(Span::styled(
                        "🖼 ".to_string(),
                        Style::default().fg(theme.hint),
                    ));
                    r.img_src.push(dest_url.to_string());
                }
                Tag::Table(aligns) => {
                    r.flush_cur();
                    r.table = Some(Table {
                        aligns,
                        ..Default::default()
                    });
                }
                Tag::TableHead => {
                    if let Some(t) = r.table.as_mut() {
                        t.in_head = true;
                        t.cur_row.clear();
                    }
                }
                Tag::TableRow => {
                    if let Some(t) = r.table.as_mut() {
                        t.cur_row.clear();
                    }
                }
                Tag::TableCell => {
                    if let Some(t) = r.table.as_mut() {
                        t.cur_cell.clear();
                        r.table_cell_active = true;
                    }
                }
                Tag::FootnoteDefinition(name) => {
                    r.flush_cur();
                    // Browser footnote section: `[^n]: text`.
                    r.cur.push(Span::styled(
                        format!("[^{}] ", name),
                        Style::default()
                            .fg(theme.syntax_type)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => r.blank_line(),
                TagEnd::Heading(level) => {
                    r.flush_cur();
                    let lvl = level as u32;
                    r.heading = None;
                    // Browser H1/H2 bottom border, full container width.
                    if lvl <= 2 {
                        let w = r
                            .width
                            .clamp(24, 120)
                            .max(r.lines.last().map(|l| l.width()).unwrap_or(0).max(24));
                        r.lines.push(Line::from(Span::styled(
                            "─".repeat(w.min(r.width.max(24))),
                            Style::default().fg(theme.line_nr),
                        )));
                    }
                    r.lines.push(Line::from(""));
                }
                TagEnd::BlockQuote(_) => {
                    r.flush_cur();
                    r.quote_depth = r.quote_depth.saturating_sub(1);
                    r.lines.push(Line::from(""));
                }
                TagEnd::CodeBlock => r.finish_code_block(),
                TagEnd::List(_) => {
                    r.flush_cur();
                    r.lists.pop();
                    r.lines.push(Line::from(""));
                }
                TagEnd::Item => {
                    r.flush_cur();
                    if let Some(ctx) = r.lists.last_mut() {
                        if ctx.ordered {
                            ctx.next_num += 1;
                        }
                    }
                    r.pending_item_prefix = None;
                }
                TagEnd::Emphasis => r.italic = r.italic.saturating_sub(1),
                TagEnd::Strong => r.bold = r.bold.saturating_sub(1),
                TagEnd::Strikethrough => r.strike = r.strike.saturating_sub(1),
                TagEnd::Link => {
                    // URL stays hidden like the browser (blue text only).
                    r.link_depth = r.link_depth.saturating_sub(1);
                }
                TagEnd::Image => {
                    if let Some(src) = r.img_src.pop() {
                        if !src.is_empty() {
                            r.cur.push(Span::styled(
                                format!(" ({src})"),
                                Style::default().fg(theme.hint),
                            ));
                        }
                    }
                }
                TagEnd::Table => r.finish_table(),
                TagEnd::TableHead => {
                    if let Some(t) = r.table.as_mut() {
                        // TableHead holds cells directly (no inner Row).
                        t.headers = std::mem::take(&mut t.cur_row);
                        t.in_head = false;
                    }
                }
                TagEnd::TableRow => {
                    if let Some(t) = r.table.as_mut() {
                        t.rows.push(std::mem::take(&mut t.cur_row));
                    }
                }
                TagEnd::TableCell => {
                    if let Some(t) = r.table.as_mut() {
                        t.cur_row.push(std::mem::take(&mut t.cur_cell));
                        r.table_cell_active = false;
                    }
                }
                _ => {}
            },
            Event::Text(text) => r.push_text(&text),
            Event::Code(code) => r.push_code_inline(&code),
            Event::Html(html) => {
                r.ensure_item_prefix();
                r.cur.push(Span::styled(
                    html.to_string(),
                    Style::default().fg(theme.hint),
                ));
            }
            Event::SoftBreak => {
                if r.code_block_lang.is_some() {
                    r.code_block_buf.push('\n');
                } else if r.table.as_ref().is_some_and(|_| r.table_cell_active) {
                    if let Some(t) = r.table.as_mut() {
                        t.cur_cell.push(' ');
                    }
                } else {
                    r.cur.push(Span::raw(" "));
                }
            }
            Event::HardBreak => {
                if r.code_block_lang.is_some() {
                    r.code_block_buf.push('\n');
                } else {
                    r.flush_cur();
                }
            }
            Event::Rule => {
                // Browser <hr>: full-width thin rule.
                r.flush_cur();
                let w = r.width.clamp(24, 160);
                r.lines.push(Line::from(Span::styled(
                    "─".repeat(w),
                    Style::default().fg(theme.line_nr),
                )));
                r.lines.push(Line::from(""));
            }
            Event::TaskListMarker(checked) => {
                r.pending_item_prefix = Some(if checked {
                    "☑ ".into()
                } else {
                    "☐ ".into()
                });
            }
            Event::FootnoteReference(name) => {
                r.ensure_item_prefix();
                r.cur.push(Span::styled(
                    format!("[^{name}]"),
                    Style::default()
                        .fg(theme.syntax_type)
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
            _ => {}
        }
    }
    r.flush_cur();
    // Trim trailing blank lines to keep scroll math stable.
    while r.lines.last().map(|l| l.width() == 0).unwrap_or(false) {
        r.lines.pop();
    }
    if r.lines.is_empty() {
        r.lines.push(Line::from(Span::styled(
            "(empty markdown file)",
            Style::default().fg(theme.hint),
        )));
    }
    r.lines
}

/// Soft-wrap logical lines to `width` cells (wide chars never split).
/// Continuation rows inherit the first row's indent so lists/quotes
/// stay aligned.
fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }
    let mut out = Vec::new();
    for line in lines {
        if line.width() == 0 {
            out.push(line);
            continue;
        }
        if line.width() <= width {
            out.push(line);
            continue;
        }
        // Flatten to chars with styles.
        let mut cells: Vec<(char, Style)> = Vec::new();
        for span in line.spans {
            for ch in span.content.chars() {
                cells.push((ch, span.style));
            }
        }
        // Leading indent (spaces only) for continuation rows.
        let indent_len = cells.iter().take_while(|(c, _)| *c == ' ').count();
        let indent: String = " ".repeat(indent_len);
        let mut cur: Vec<Span<'static>> = Vec::new();
        let mut cur_str = String::new();
        let mut cur_style: Option<Style> = None;
        let mut used = 0usize;
        let mut first_row = true;
        let flush_run =
            |cur_str: &mut String, cur_style: &mut Option<Style>, cur: &mut Vec<Span<'static>>| {
                if !cur_str.is_empty() {
                    cur.push(Span::styled(
                        std::mem::take(cur_str),
                        cur_style.unwrap_or_default(),
                    ));
                }
            };
        for (ch, style) in cells {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + w > width {
                flush_run(&mut cur_str, &mut cur_style, &mut cur);
                out.push(Line::from(std::mem::take(&mut cur)));
                cur = Vec::new();
                if !first_row || indent_len > 0 {
                    cur.push(Span::raw(indent.clone()));
                    used = indent_len;
                } else {
                    used = 0;
                }
                first_row = false;
                cur_str.clear();
                cur_style = None;
            }
            if cur_style != Some(style) {
                flush_run(&mut cur_str, &mut cur_style, &mut cur);
                cur_style = Some(style);
            }
            cur_str.push(ch);
            used += w;
        }
        flush_run(&mut cur_str, &mut cur_style, &mut cur);
        if !cur.is_empty() {
            out.push(Line::from(cur));
        }
        // Safety: if indent alone exceeds width, avoid infinite loop —
        // already handled since we push per overflow above.
    }
    out
}

/// Rendered Markdown preview lines, wrapped to `width`.
/// Falls back to plain lines when the width is degenerate.
pub fn render_markdown(text: &str, theme: Theme, width: usize) -> Vec<Line<'static>> {
    let logical = parse_markdown(text, theme, width.max(20));
    if width < 20 {
        return logical;
    }
    wrap_lines(logical, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tn() -> Theme {
        Theme::tokyo_night()
    }

    #[test]
    fn detects_markdown_paths() {
        for p in ["a.md", "A.MD", "notes.markdown", "doc.mkd", "page.mdx"] {
            assert!(is_markdown_path(p), "{p}");
        }
        assert!(!is_markdown_path("main.rs"));
        assert!(!is_markdown_path("README"));
    }

    #[test]
    fn heading_renders_bold_function_color() {
        let lines = render_markdown("# Hello\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("Hello"), "{text:?}");
        assert!(lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.style.fg == Some(tn().syntax_function)));
    }

    #[test]
    fn emphasis_bold_italic_strike() {
        let lines = render_markdown("**b** *i* ~~s~~ `c`\n", tn(), 80);
        let all: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        assert!(all
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
        assert!(all
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::ITALIC)));
        assert!(all
            .iter()
            .any(|s| s.content == "c" && s.style.fg == Some(tn().syntax_string)));
    }

    #[test]
    fn lists_and_task_markers() {
        let lines = render_markdown("- a\n- [ ] todo\n- [x] done\n1. first\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains('•'), "{text:?}");
        assert!(text.contains('☐'), "{text:?}");
        assert!(text.contains('☑'), "{text:?}");
        assert!(text.contains("1."), "{text:?}");
    }

    #[test]
    fn code_block_is_shaded_without_fences() {
        // Browser-like: shaded <pre> block, no ``` markers.
        let lines = render_markdown("```rust\nfn main() {}\n```\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("fn main"), "{text:?}");
        assert!(
            !text.contains("```"),
            "browser block shows no fences: {text:?}"
        );
        assert!(
            lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|s| s.style.bg == Some(tn().selection_bg)),
            "code block should carry the shaded wash"
        );
    }

    #[test]
    fn table_renders_borders() {
        let lines = render_markdown("| a | b |\n|---|---|\n| 1 | 2 |\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains('│'), "{text:?}");
        assert!(text.contains('a'), "{text:?}");
    }

    #[test]
    fn link_hides_url_like_browser() {
        // Browser shows blue underlined text, not the raw URL.
        let lines = render_markdown("[hi](https://x.y)\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("hi"), "{text:?}");
        assert!(
            !text.contains("https://x.y"),
            "browser hides the URL: {text:?}"
        );
        assert!(
            lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|s| s.content == "hi" && s.style.fg == Some(tn().syntax_type)),
            "link text should be blue"
        );
    }

    #[test]
    fn image_is_placeholder() {
        let lines = render_markdown("![alt](pic.png)\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("alt"), "{text:?}");
    }

    #[test]
    fn blockquote_and_rule() {
        let lines = render_markdown("> quote\n\n---\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains('▎'), "{text:?}");
        assert!(text.contains('─'), "{text:?}");
    }

    #[test]
    fn h1_has_browser_bottom_border_and_no_hashes() {
        let lines = render_markdown("# Hello\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("Hello"), "{text:?}");
        assert!(
            !text.contains('#'),
            "browser headings show no # markers: {text:?}"
        );
        assert!(
            text.contains('─'),
            "H1 should carry a bottom border: {text:?}"
        );
    }

    #[test]
    fn hr_spans_full_width() {
        let lines = render_markdown("---\n", tn(), 60);
        let rule = lines
            .iter()
            .find(|l| l.width() > 24)
            .expect("full-width rule");
        assert!(rule.width() <= 60, "rule must fit: {}", rule.width());
        assert!(
            rule.width() >= 50,
            "rule should span the container: {}",
            rule.width()
        );
    }

    #[test]
    fn table_has_browser_box_and_alignment() {
        let lines = render_markdown("| a | b |\n|:--|--:|\n| 1 | 2 |\n", tn(), 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains('┌'), "{text:?}");
        assert!(text.contains('└'), "{text:?}");
        assert!(text.contains('a'), "{text:?}");
    }

    #[test]
    fn long_lines_wrap_to_width() {
        let long = format!("{}\n", "word ".repeat(40));
        let lines = render_markdown(&long, tn(), 40);
        assert!(lines.len() > 1, "should wrap");
        for l in &lines {
            assert!(l.width() <= 40, "line too wide: {}", l.width());
        }
    }

    #[test]
    fn empty_file_has_placeholder() {
        let lines = render_markdown("", tn(), 80);
        assert!(!lines.is_empty());
    }
}
