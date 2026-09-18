//! LazyVim-style syntax highlighting for the file viewer.
//!
//! LazyVim = tokyo-night colors + treesitter grammars. Here we approximate
//! that with `syntect` Sublime grammars and a hand-built tmTheme derived
//! from our [`crate::config::Theme`], so `default` and `tokyo-night` both
//! get readable code colors instead of a flat gray wash.
//!
//! Design notes:
//! - `SyntaxSet` is global (large, ~MBs) via `OnceLock`.
//! - The syntect `Theme` is built per call from the app `Theme` (cheap).
//! - `highlight_line` highlights one line in isolation (fresh state). This
//!   covers keywords/strings/comments/functions for ~95% of lines. Multi-line
//!   block comments spanning lines lose state — accepted v1 tradeoff to keep
//!   per-frame rendering cheap (only visible lines are highlighted).
//! - `highlight_file_lines` reuses one `HighlightLines` across lines so a
//!   whole-file view keeps block-comment/string state correctly.

use crate::config::Theme;
use ratatui::style::{Color as RatColor, Modifier};
use std::str::FromStr;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, FontStyle, ScopeSelectors, StyleModifier, Theme as SynTheme, ThemeSettings,
};
use syntect::parsing::SyntaxSet;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

fn rat_to_syn(c: RatColor) -> SynColor {
    match c {
        RatColor::Rgb(r, g, b) => SynColor { r, g, b, a: 255 },
        RatColor::White => SynColor {
            r: 192,
            g: 202,
            b: 245,
            a: 255,
        },
        RatColor::Black => SynColor {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        },
        RatColor::Red => SynColor {
            r: 247,
            g: 118,
            b: 142,
            a: 255,
        },
        RatColor::Green => SynColor {
            r: 158,
            g: 206,
            b: 106,
            a: 255,
        },
        RatColor::Yellow => SynColor {
            r: 224,
            g: 175,
            b: 104,
            a: 255,
        },
        RatColor::Blue => SynColor {
            r: 122,
            g: 162,
            b: 247,
            a: 255,
        },
        RatColor::Magenta => SynColor {
            r: 187,
            g: 154,
            b: 247,
            a: 255,
        },
        RatColor::Cyan => SynColor {
            r: 125,
            g: 207,
            b: 255,
            a: 255,
        },
        RatColor::Gray => SynColor {
            r: 169,
            g: 177,
            b: 214,
            a: 255,
        },
        RatColor::DarkGray => SynColor {
            r: 86,
            g: 95,
            b: 137,
            a: 255,
        },
        RatColor::LightRed => SynColor {
            r: 247,
            g: 118,
            b: 142,
            a: 255,
        },
        RatColor::LightGreen => SynColor {
            r: 158,
            g: 206,
            b: 106,
            a: 255,
        },
        RatColor::LightYellow => SynColor {
            r: 224,
            g: 175,
            b: 104,
            a: 255,
        },
        RatColor::LightBlue => SynColor {
            r: 122,
            g: 162,
            b: 247,
            a: 255,
        },
        RatColor::LightMagenta => SynColor {
            r: 187,
            g: 154,
            b: 247,
            a: 255,
        },
        RatColor::LightCyan => SynColor {
            r: 125,
            g: 207,
            b: 255,
            a: 255,
        },
        RatColor::Indexed(_) => SynColor {
            r: 192,
            g: 202,
            b: 245,
            a: 255,
        },
        RatColor::Reset => SynColor {
            r: 192,
            g: 202,
            b: 245,
            a: 255,
        },
    }
}

fn syn_to_rat(c: SynColor) -> RatColor {
    RatColor::Rgb(c.r, c.g, c.b)
}

fn rule(
    selector: &str,
    fg: RatColor,
    font_style: Option<FontStyle>,
) -> syntect::highlighting::ThemeItem {
    syntect::highlighting::ThemeItem {
        scope: ScopeSelectors::from_str(selector).unwrap_or_default(),
        style: StyleModifier {
            foreground: Some(rat_to_syn(fg)),
            background: None,
            font_style,
        },
    }
}

/// Build a syntect theme from the app theme: LazyVim groups.
fn syntect_theme(theme: Theme) -> SynTheme {
    let fg = rat_to_syn(theme.fg);
    SynTheme {
        name: Some("git-tui-lazyvim".into()),
        author: Some("git-tui".into()),
        settings: ThemeSettings {
            foreground: Some(fg),
            background: None,
            gutter: None,
            gutter_foreground: None,
            line_highlight: None,
            ..Default::default()
        },
        scopes: vec![
            rule("comment", theme.syntax_comment, Some(FontStyle::ITALIC)),
            rule("comment.line", theme.syntax_comment, Some(FontStyle::ITALIC)),
            rule("comment.block", theme.syntax_comment, Some(FontStyle::ITALIC)),
            rule("string", theme.syntax_string, None),
            rule("string.quoted", theme.syntax_string, None),
            rule(
                "constant.numeric, constant.language, constant.character",
                theme.syntax_number,
                None,
            ),
            rule(
                "keyword, keyword.control, keyword.operator, storage, storage.type, storage.modifier",
                theme.syntax_keyword,
                Some(FontStyle::ITALIC),
            ),
            rule(
                "entity.name.function, support.function, variable.function, entity.name.method",
                theme.syntax_function,
                None,
            ),
            rule(
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, support.type, support.class",
                theme.syntax_type,
                None,
            ),
        ],
    }
}

fn find_syntax<'a>(ss: &'a SyntaxSet, path: &str) -> &'a syntect::parsing::SyntaxReference {
    if let Ok(Some(s)) = ss.find_syntax_for_file(path) {
        return s;
    }
    if let Some(ext) = path.rsplit('.').next() {
        // syntect's default set ships no TypeScript/JSX grammar; fall back
        // to JavaScript, which covers keywords/strings/comments for the
        // whole TS/JS family (.ts/.tsx/.mts/.cts/.jsx/.mjs/.cjs).
        let mapped: &str = match ext.to_ascii_lowercase().as_str() {
            "ts" | "mts" | "cts" | "tsx" | "jsx" | "mjs" | "cjs" => "js",
            _ => ext,
        };
        if let Some(s) = ss.find_syntax_by_extension(mapped) {
            return s;
        }
    }
    ss.find_syntax_plain_text()
}

/// One highlighted token: text + fg + modifier (italic/bold from tmTheme).
#[derive(Debug, Clone)]
pub struct HiToken {
    pub text: String,
    pub fg: RatColor,
    pub modifier: Modifier,
}

fn convert_ranges(ranges: Vec<(syntect::highlighting::Style, &str)>) -> Vec<HiToken> {
    ranges
        .into_iter()
        .map(|(st, text)| {
            let mut modifier = Modifier::empty();
            if st.font_style.contains(FontStyle::ITALIC) {
                modifier |= Modifier::ITALIC;
            }
            if st.font_style.contains(FontStyle::BOLD) {
                modifier |= Modifier::BOLD;
            }
            if st.font_style.contains(FontStyle::UNDERLINE) {
                modifier |= Modifier::UNDERLINED;
            }
            HiToken {
                text: text.to_string(),
                fg: syn_to_rat(st.foreground),
                modifier,
            }
        })
        .filter(|t| !t.text.is_empty())
        .collect()
}

/// Highlight a single line in isolation (fresh parser state).
pub fn highlight_line(path: &str, text: &str, theme: Theme) -> Vec<HiToken> {
    if text.is_empty() {
        return Vec::new();
    }
    let ss = syntaxes();
    let syn = syntect_theme(theme);
    let syntax = find_syntax(ss, path);
    let mut hl = HighlightLines::new(syntax, &syn);
    match hl.highlight_line(text, ss) {
        Ok(ranges) => {
            let out = convert_ranges(ranges);
            if out.is_empty() {
                vec![HiToken {
                    text: text.to_string(),
                    fg: theme.fg,
                    modifier: Modifier::empty(),
                }]
            } else {
                out
            }
        }
        Err(_) => vec![HiToken {
            text: text.to_string(),
            fg: theme.fg,
            modifier: Modifier::empty(),
        }],
    }
}

#[allow(dead_code)]
/// Highlight whole-file lines with shared parser state (correct multi-line
/// comments/strings). Returns one token vec per input line.
pub fn highlight_file_lines(path: &str, lines: &[&str], theme: Theme) -> Vec<Vec<HiToken>> {
    let ss = syntaxes();
    let syn = syntect_theme(theme);
    let syntax = find_syntax(ss, path);
    let mut hl = HighlightLines::new(syntax, &syn);
    lines
        .iter()
        .map(|text| match hl.highlight_line(text, ss) {
            Ok(ranges) => {
                let out = convert_ranges(ranges);
                if out.is_empty() && !text.is_empty() {
                    vec![HiToken {
                        text: (*text).to_string(),
                        fg: theme.fg,
                        modifier: Modifier::empty(),
                    }]
                } else {
                    out
                }
            }
            Err(_) => vec![HiToken {
                text: (*text).to_string(),
                fg: theme.fg,
                modifier: Modifier::empty(),
            }],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;

    #[test]
    fn typescript_files_get_keyword_and_comment_colors() {
        // syntect's default set ships no TypeScript/TSX grammar; .ts files
        // must still highlight (via the JavaScript grammar), not render flat.
        let theme = Theme::tokyo_night();
        for path in ["a.ts", "a.tsx", "a.mts", "a.jsx", "a.mjs"] {
            let toks = highlight_line(path, "const x = 1; // hi", theme);
            assert!(
                toks.iter()
                    .any(|t| t.text.contains("const") && t.fg == theme.syntax_keyword),
                "{path}: `const` should carry the keyword color: {toks:?}"
            );
            assert!(
                toks.iter().any(|t| t.fg == theme.syntax_comment),
                "{path}: comment should carry the comment color: {toks:?}"
            );
        }
    }

    #[test]
    fn rust_keywords_get_keyword_color() {
        let theme = Theme::tokyo_night();
        let toks = highlight_line("src/main.rs", "fn main() { let x = 1; }", theme);
        let joined: String = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, "fn main() { let x = 1; }");
        // `fn` should carry the keyword (magenta) color.
        let fn_tok = toks.iter().find(|t| t.text == "fn").expect("fn token");
        assert_eq!(fn_tok.fg, theme.syntax_keyword);
    }

    #[test]
    fn line_comment_gets_comment_color() {
        let theme = Theme::tokyo_night();
        let toks = highlight_line("a.py", "# hello world", theme);
        assert!(!toks.is_empty());
        assert!(
            toks.iter().all(|t| t.fg == theme.syntax_comment),
            "comment should be uniform: {toks:?}"
        );
    }

    #[test]
    fn unknown_extension_falls_back_to_fg() {
        let theme = Theme::tokyo_night();
        let toks = highlight_line("file.unknownext123", "hello", theme);
        let joined: String = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, "hello");
    }

    #[test]
    fn file_lines_preserve_every_line() {
        let theme = Theme::default_theme();
        let out = highlight_file_lines("a.rs", &["fn a() {}", "// c", ""], theme);
        assert_eq!(out.len(), 3);
        let joined: String = out[0].iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, "fn a() {}");
    }
}
