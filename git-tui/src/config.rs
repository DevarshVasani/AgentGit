//! Phase 5: TOML config — keybinding overrides and theme selection.
//!
//! `~/.config/git-tui/config.toml`. Missing file means defaults; anything
//! present overrides just that piece. Invalid names fail fast with the file
//! path and the offending value.

use anyhow::{Context, Result};
use crossterm::event::KeyCode;
use ratatui::style::Color;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Canonical action names for `[keys]`.
pub const ACTIONS: &[&str] = &[
    "nav_down",
    "nav_up",
    "stage",
    "commit",
    "refresh",
    "quit",
    "focus_next",
    "focus_status",
    "focus_branches",
    "focus_log",
    "focus_stash",
    "scroll_up",
    "scroll_down",
    "branch_new",
    "branch_delete",
    "checkout",
    "stash_pop",
    "stash_push",
    "stash_drop",
    "find_files",
];

/// Key names accepted in `[keys]` besides single characters.
pub fn parse_key(name: &str) -> Result<KeyCode> {
    match name {
        "space" => Ok(KeyCode::Char(' ')),
        "tab" => Ok(KeyCode::Tab),
        "enter" => Ok(KeyCode::Enter),
        "esc" => Ok(KeyCode::Esc),
        "backspace" => Ok(KeyCode::Backspace),
        "delete" => Ok(KeyCode::Delete),
        "insert" => Ok(KeyCode::Insert),
        "up" => Ok(KeyCode::Up),
        "down" => Ok(KeyCode::Down),
        "left" => Ok(KeyCode::Left),
        "right" => Ok(KeyCode::Right),
        "pageup" => Ok(KeyCode::PageUp),
        "pagedown" => Ok(KeyCode::PageDown),
        "home" => Ok(KeyCode::Home),
        "end" => Ok(KeyCode::End),
        s if s.chars().count() == 1 => Ok(KeyCode::Char(s.chars().next().unwrap())),
        _ => anyhow::bail!("unknown key name: {name:?}"),
    }
}

/// One action's bindings (defaults preserve legacy behavior).
#[derive(Debug, Clone)]
pub struct KeyBindings {
    pub nav_down: Vec<KeyCode>,
    pub nav_up: Vec<KeyCode>,
    pub stage: Vec<KeyCode>,
    pub commit: Vec<KeyCode>,
    pub refresh: Vec<KeyCode>,
    pub quit: Vec<KeyCode>,
    pub focus_next: Vec<KeyCode>,
    pub focus_status: Vec<KeyCode>,
    pub focus_branches: Vec<KeyCode>,
    pub focus_log: Vec<KeyCode>,
    pub focus_stash: Vec<KeyCode>,
    pub scroll_up: Vec<KeyCode>,
    pub scroll_down: Vec<KeyCode>,
    pub branch_new: Vec<KeyCode>,
    pub branch_delete: Vec<KeyCode>,
    pub checkout: Vec<KeyCode>,
    pub stash_pop: Vec<KeyCode>,
    pub stash_push: Vec<KeyCode>,
    pub stash_drop: Vec<KeyCode>,
    pub find_files: Vec<KeyCode>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        use KeyCode::*;
        Self {
            nav_down: vec![Char('j'), Down],
            nav_up: vec![Char('k'), Up],
            stage: vec![Char(' '), Char('s')],
            commit: vec![Char('c')],
            refresh: vec![Char('r')],
            quit: vec![Char('q')],
            focus_next: vec![KeyCode::Tab],
            focus_status: vec![Char('1'), KeyCode::Left],
            focus_branches: vec![Char('2')],
            focus_log: vec![Char('3')],
            focus_stash: vec![Char('4')],
            scroll_up: vec![KeyCode::PageUp],
            scroll_down: vec![KeyCode::PageDown],
            branch_new: vec![Char('a')],
            branch_delete: vec![Char('D')],
            checkout: vec![KeyCode::Enter],
            stash_pop: vec![KeyCode::Enter],
            stash_push: vec![Char('a')],
            stash_drop: vec![Char('D')],
            find_files: vec![Char('/')],
        }
    }
}

/// Named UI palette. `default` preserves the legacy hardcoded colors.
/// Syntax colors follow LazyVim (tokyo-night + treesitter): comments are
/// dim italic gray, strings green, keywords magenta italic, functions blue,
/// types cyan, numbers orange.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub hint: Color,
    pub error: Color,
    pub staged: Color,
    pub unstaged: Color,
    pub untracked: Color,
    pub conflicted: Color,
    pub both_staged: Color,
    pub hunk_header: Color,
    pub commit_id: Color,
    pub branch_current: Color,
    #[allow(dead_code)]
    pub context: Color,
    /// Side-by-side tints: deleted lines get a red wash with a stronger
    /// background on exactly the changed words; added lines get a single
    /// very light green wash so syntax colors stay easily readable
    /// (green is never painted twice on the same cell).
    pub diff_del_bg: Color,
    pub diff_add_bg: Color,
    pub diff_del_word_bg: Color,
    // --- LazyVim-style code view ---
    /// Opaque editor background (LazyVim Normal bg). Every panel paints it
    /// so the terminal wallpaper never bleeds through the code view.
    pub bg: Color,
    /// Main editor foreground (LazyVim fg #c0caf5).
    pub fg: Color,
    /// Dim line numbers / gutter.
    pub line_nr: Color,
    /// Telescope-style selection wash.
    pub selection_bg: Color,
    /// Syntax groups.
    pub syntax_comment: Color,
    pub syntax_string: Color,
    pub syntax_keyword: Color,
    pub syntax_function: Color,
    pub syntax_type: Color,
    pub syntax_number: Color,
}

impl Theme {
    pub fn default_theme() -> Self {
        Self {
            border_focused: Color::White,
            border_unfocused: Color::DarkGray,
            hint: Color::DarkGray,
            error: Color::Red,
            staged: Color::Green,
            unstaged: Color::Yellow,
            untracked: Color::DarkGray,
            conflicted: Color::Red,
            both_staged: Color::Magenta,
            hunk_header: Color::Cyan,
            commit_id: Color::Yellow,
            branch_current: Color::Green,
            context: Color::DarkGray,
            diff_del_bg: Color::Rgb(55, 25, 32),
            diff_add_bg: Color::Rgb(22, 45, 30),
            diff_del_word_bg: Color::Rgb(78, 32, 42),
            // LazyVim-ish on top of legacy ANSI base.
            bg: Color::Black,
            fg: Color::White,
            line_nr: Color::DarkGray,
            selection_bg: Color::Rgb(40, 42, 60),
            syntax_comment: Color::DarkGray,
            syntax_string: Color::Green,
            syntax_keyword: Color::Magenta,
            syntax_function: Color::Blue,
            syntax_type: Color::Cyan,
            syntax_number: Color::Yellow,
        }
    }

    /// https://github.com/tokyo-night/tokyo-night.nvim — Storm palette.
    pub fn tokyo_night() -> Self {
        let rgb = Color::Rgb;
        Self {
            border_focused: rgb(122, 162, 247), // blue #7aa2f7
            border_unfocused: rgb(59, 66, 97),  // #3b4261
            hint: rgb(86, 95, 137),             // comment #565f89
            error: rgb(247, 118, 142),          // red #f7768e
            staged: rgb(158, 206, 106),         // green #9ece6a
            unstaged: rgb(224, 175, 104),       // yellow #e0af68
            untracked: rgb(86, 95, 137),        // comment #565f89
            conflicted: rgb(247, 118, 142),     // red #f7768e
            both_staged: rgb(187, 154, 247),    // magenta #bb9af7
            hunk_header: rgb(125, 207, 255),    // cyan #7dcfff
            commit_id: rgb(224, 175, 104),      // yellow #e0af68
            branch_current: rgb(158, 206, 106), // green #9ece6a
            context: rgb(86, 95, 137),          // comment #565f89
            diff_del_bg: rgb(60, 32, 42),
            diff_add_bg: rgb(28, 48, 38),
            diff_del_word_bg: rgb(82, 38, 50),
            // LazyVim editor colors (tokyo-night Storm).
            bg: rgb(36, 40, 59),                 // #24283b Storm bg
            fg: rgb(192, 202, 245),              // #c0caf5
            line_nr: rgb(59, 66, 97),            // #3b4261 dim gutter
            selection_bg: rgb(40, 52, 94),       // #28365e Telescope selection
            syntax_comment: rgb(86, 95, 137),    // #565f89 italic
            syntax_string: rgb(158, 206, 106),   // #9ece6a
            syntax_keyword: rgb(187, 154, 247),  // #bb9af7 italic
            syntax_function: rgb(122, 162, 247), // #7aa2f7
            syntax_type: rgb(125, 207, 255),     // #7dcfff
            syntax_number: rgb(255, 158, 100),   // #ff9e64 orange
        }
    }

    pub fn by_name(name: &str) -> Result<Self> {
        match name {
            "default" => Ok(Self::default_theme()),
            "tokyo-night" => Ok(Self::tokyo_night()),
            _ => anyhow::bail!("unknown theme {name:?} (expected \"default\" or \"tokyo-night\")"),
        }
    }
}

/// Resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub keys: KeyBindings,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keys: KeyBindings::default(),
            theme: Theme::default_theme(),
        }
    }
}

impl Config {
    /// Load from an explicit path. Missing file means defaults.
    pub fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("bad config {}", path.display()))
    }

    /// Load from `$XDG_CONFIG_HOME/git-tui/config.toml`, falling back to
    /// `~/.config/git-tui/config.toml`. Missing file means defaults.
    pub fn load() -> Result<Self> {
        match Self::default_path() {
            Some(path) => Self::load_from_path(&path),
            None => Ok(Self::default()),
        }
    }

    pub fn default_path() -> Option<PathBuf> {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("git-tui").join("config.toml"));
            }
        }
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("git-tui")
                .join("config.toml")
        })
    }

    fn from_toml(text: &str) -> Result<Self> {
        #[derive(Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct FileConfig {
            #[serde(default)]
            keys: HashMap<String, OneOrMany>,
            #[serde(default)]
            theme: ThemeSection,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }

        #[derive(Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ThemeSection {
            #[serde(default)]
            name: Option<String>,
        }

        let file: FileConfig = toml::from_str(text).context("cannot parse TOML")?;
        let mut cfg = Self::default();
        for (action, value) in &file.keys {
            let names: &[String] = match value {
                OneOrMany::One(s) => std::slice::from_ref(s),
                OneOrMany::Many(v) => v,
            };
            let mut keys = Vec::with_capacity(names.len());
            for name in names {
                keys.push(parse_key(name).with_context(|| format!("bad key for [{action}]"))?);
            }
            if keys.is_empty() {
                anyhow::bail!("no keys listed for [{action}]");
            }
            let k = &mut cfg.keys;
            match action.as_str() {
                "nav_down" => k.nav_down = keys,
                "nav_up" => k.nav_up = keys,
                "stage" => k.stage = keys,
                "commit" => k.commit = keys,
                "refresh" => k.refresh = keys,
                "quit" => k.quit = keys,
                "focus_next" => k.focus_next = keys,
                "focus_status" => k.focus_status = keys,
                "focus_branches" => k.focus_branches = keys,
                "focus_log" => k.focus_log = keys,
                "focus_stash" => k.focus_stash = keys,
                "scroll_up" => k.scroll_up = keys,
                "scroll_down" => k.scroll_down = keys,
                "branch_new" => k.branch_new = keys,
                "branch_delete" => k.branch_delete = keys,
                "checkout" => k.checkout = keys,
                "stash_pop" => k.stash_pop = keys,
                "stash_push" => k.stash_push = keys,
                "stash_drop" => k.stash_drop = keys,
                "find_files" => k.find_files = keys,
                _ => anyhow::bail!(
                    "unknown action [{action}] (expected one of: {})",
                    ACTIONS.join(", ")
                ),
            }
        }
        if let Some(name) = file.theme.name {
            cfg.theme = Theme::by_name(&name)?;
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_legacy_behavior() {
        let keys = KeyBindings::default();
        assert!(keys.nav_down.contains(&KeyCode::Char('j')));
        assert!(keys.nav_down.contains(&KeyCode::Down));
        assert!(keys.stage.contains(&KeyCode::Char(' ')));
        assert!(keys.stage.contains(&KeyCode::Char('s')));
        assert!(keys.quit.contains(&KeyCode::Char('q')));
        assert!(keys.focus_next.contains(&KeyCode::Tab));
        assert!(keys.checkout.contains(&KeyCode::Enter));
        assert!(keys.branch_delete.contains(&KeyCode::Char('D')));
        assert!(keys.find_files.contains(&KeyCode::Char('/')));
    }

    #[test]
    fn left_arrow_focuses_status_by_default() {
        let keys = KeyBindings::default();
        assert!(keys.focus_status.contains(&KeyCode::Left));
        // Vertical-only navigation: nothing is bound to Right by default.
        for action in [
            &keys.nav_down,
            &keys.nav_up,
            &keys.stage,
            &keys.focus_branches,
            &keys.focus_log,
            &keys.focus_stash,
        ] {
            assert!(!action.contains(&KeyCode::Right));
        }
    }

    #[test]
    fn key_names_parse() {
        assert_eq!(parse_key("space").unwrap(), KeyCode::Char(' '));
        assert_eq!(parse_key("tab").unwrap(), KeyCode::Tab);
        assert_eq!(parse_key("enter").unwrap(), KeyCode::Enter);
        assert_eq!(parse_key("esc").unwrap(), KeyCode::Esc);
        assert_eq!(parse_key("pageup").unwrap(), KeyCode::PageUp);
        assert_eq!(parse_key("D").unwrap(), KeyCode::Char('D'));
    }

    #[test]
    fn unknown_key_name_errors() {
        assert!(parse_key("ctrl-x").is_err());
        assert!(parse_key("").is_err());
    }

    #[test]
    fn themes_resolve() {
        assert!(Theme::by_name("default").is_ok());
        let tn = Theme::by_name("tokyo-night").unwrap();
        assert_eq!(tn.error, Color::Rgb(247, 118, 142));
        assert_eq!(tn.hunk_header, Color::Rgb(125, 207, 255));
        assert!(Theme::by_name("dracula").is_err());
    }

    #[test]
    fn editor_background_is_opaque() {
        // LazyVim Normal bg: solid so the wallpaper never bleeds through.
        assert_eq!(
            Theme::by_name("tokyo-night").unwrap().bg,
            Color::Rgb(36, 40, 59)
        );
        assert_eq!(Theme::default_theme().bg, Color::Black);
    }

    #[test]
    fn diff_washes_are_dark_enough_for_syntax_colors() {
        // Diffs must stay readable with syntax highlighting: deep red/green
        // washes keep the colored foreground readable (contrast ratio >= 3).
        // Green/teal washes stay dark; red washes stay deep crimson so any
        // syntax fg remains readable on top.
        for theme in [
            Theme::by_name("tokyo-night").unwrap(),
            Theme::default_theme(),
        ] {
            let Color::Rgb(ar, ag, ab) = theme.diff_add_bg else {
                panic!("expected RGB add wash, got {:?}", theme.diff_add_bg);
            };
            assert!(ag > ar, "add wash must stay green-tinted: {ar},{ag},{ab}");
            for wash in [theme.diff_del_bg, theme.diff_del_word_bg] {
                let Color::Rgb(r, g, b) = wash else {
                    panic!("expected RGB wash, got {wash:?}");
                };
                assert!(r > g, "del wash must stay red-tinted: {wash:?}");
                assert!((r + g + b) / 3 < 90, "wash too light: {wash:?}");
            }
        }
    }

    #[test]
    fn default_theme_matches_legacy_colors() {
        let t = Theme::default_theme();
        assert_eq!(t.staged, Color::Green);
        assert_eq!(t.unstaged, Color::Yellow);
        assert_eq!(t.conflicted, Color::Red);
        assert_eq!(t.hunk_header, Color::Cyan);
    }

    // RED: file loading not implemented yet.
    #[test]
    fn missing_file_means_defaults() {
        let cfg = Config::load_from_path(Path::new("/nonexistent/config.toml")).unwrap();
        assert!(cfg.keys.quit.contains(&KeyCode::Char('q')));
    }

    // RED: file loading not implemented yet.
    #[test]
    fn file_overrides_keys_and_theme() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[keys]\nstage = \"s\"\nquit = [\"q\", \"Q\"]\n\n[theme]\nname = \"tokyo-night\"\n",
        )
        .unwrap();
        let cfg = Config::load_from_path(&path).unwrap();
        assert_eq!(cfg.keys.stage, vec![KeyCode::Char('s')]);
        assert!(cfg.keys.quit.contains(&KeyCode::Char('Q')));
        assert_eq!(cfg.theme.error, Color::Rgb(247, 118, 142));
        // Untouched actions keep defaults.
        assert!(cfg.keys.commit.contains(&KeyCode::Char('c')));
    }

    // RED: file loading not implemented yet.
    #[test]
    fn unknown_action_or_key_in_file_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let bad_action = dir.path().join("a.toml");
        std::fs::write(&bad_action, "[keys]\nfly = \"f\"\n").unwrap();
        assert!(Config::load_from_path(&bad_action).is_err());
        let bad_key = dir.path().join("b.toml");
        std::fs::write(&bad_key, "[keys]\nquit = \"ctrl-q\"\n").unwrap();
        assert!(Config::load_from_path(&bad_key).is_err());
    }

    #[test]
    fn misspelled_section_or_theme_field_errors_instead_of_silently_ignored() {
        // `[themes]` (plural) must not silently fall back to defaults.
        let dir = tempfile::TempDir::new().unwrap();
        let plural = dir.path().join("c.toml");
        std::fs::write(&plural, "[themes]\nname = \"tokyo-night\"\n").unwrap();
        assert!(Config::load_from_path(&plural).is_err());
        // `[theme] style = ...` (wrong field) must not be ignored either.
        let field = dir.path().join("d.toml");
        std::fs::write(&field, "[theme]\nstyle = \"tokyo-night\"\n").unwrap();
        assert!(Config::load_from_path(&field).is_err());
    }
}
