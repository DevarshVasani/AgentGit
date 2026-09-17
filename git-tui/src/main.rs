mod app;
mod config;
mod fuzzy;
mod syntax;
mod ui;
mod words;
mod workspace;

use anyhow::{Context, Result};
use config::{Config, Theme};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::ffi::OsString;
use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;
use workspace::Workspace;

fn main() -> Result<()> {
    // Stage 5 — raw mode, explained:
    // A terminal normally line-buffers input and echoes it (cooked mode):
    // your program only sees a line after Enter. A TUI needs every keypress
    // immediately and without echo, so it enables raw mode. The terminal is
    // global state: if we exit (or panic) without restoring it, the user's
    // shell is left broken (no echo, no line editing). Hence the panic hook
    // below + the explicit restore after `run` returns.
    install_panic_hook();

    // Missing/invalid config fails fast here (path + value in the error);
    // missing file falls back to defaults inside `Config::load`.
    // Precedence: `--theme` flag > config file > default.
    // Positional paths and `--repo` flags select the projects; empty means
    // the current directory. Each path is resolved to its enclosing repo.
    let cli = parse_args(std::env::args_os().skip(1))?;
    let mut config = Config::load().context("cannot load config")?;
    if let Some(name) = cli.theme {
        config.theme = Theme::by_name(&name).with_context(|| format!("unknown theme {name:?}"))?;
    }
    let mut workspace = Workspace::open(cli.paths, config)?;

    enable_raw_mode().context("cannot enable raw mode")?;
    let mut out = stdout();
    // Alternate screen = the terminal saves the current view and shows a
    // scratch buffer; on `LeaveAlternateScreen` the shell contents reappear.
    // All drawing from here on is just ANSI escape sequences (cursor moves,
    // colors) emitted by ratatui — you never write them by hand.
    execute!(out, EnterAlternateScreen).context("cannot enter alternate screen")?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend).context("cannot create terminal")?;

    let res = run(&mut terminal, &mut workspace);

    restore_terminal(&mut terminal);
    res
}

/// Panic-hook pattern (ratatui docs): a panicking TUI must leave cooked mode
/// and the alternate screen *before* the default hook prints the panic,
/// otherwise the panic message itself is unreadable and the shell stays raw.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

/// Some terminals report Shift+letter as lowercase + SHIFT instead of the
/// uppercase char, which would misclassify Shift+q (`Q`, quit-all) as `q`
/// (close-project). Normalize `a`-`z` + SHIFT to `A`-`Z` so `KeyCode`-only
/// bindings stay reliable regardless of the terminal.
fn normalize_key(code: KeyCode, modifiers: KeyModifiers) -> KeyCode {
    if modifiers.contains(KeyModifiers::SHIFT) {
        if let KeyCode::Char(c) = code {
            if c.is_ascii_lowercase() {
                return KeyCode::Char(c.to_ascii_uppercase());
            }
        }
    }
    code
}

struct Cli {
    theme: Option<String>,
    paths: Vec<PathBuf>,
}

const USAGE: &str =
    "usage: git-tui [--theme <default|tokyo-night>] [--repo <path>]... [<path>...] [-- <path>...]";

fn parse_args(args: impl IntoIterator<Item = impl Into<OsString>>) -> Result<Cli> {
    let mut cli = Cli {
        theme: None,
        paths: Vec::new(),
    };
    let mut args = args.into_iter().map(Into::into);
    while let Some(arg) = args.next() {
        if arg == "--" {
            cli.paths.extend(args.map(PathBuf::from));
            break;
        } else if arg == "-h" || arg == "--help" {
            println!("{USAGE}");
            std::process::exit(0);
        } else if arg == "--theme" {
            let name = args.next().context("--theme needs a value")?;
            cli.theme = Some(
                name.into_string()
                    .map_err(|_| anyhow::anyhow!("theme must be UTF-8"))?,
            );
        } else if let Some(name) = arg.to_str().and_then(|s| s.strip_prefix("--theme=")) {
            anyhow::ensure!(!name.is_empty(), "--theme needs a value");
            cli.theme = Some(name.to_string());
        } else if arg == "--repo" {
            let path = args.next().context("--repo needs a value")?;
            anyhow::ensure!(
                !path.is_empty() && !path.as_encoded_bytes().starts_with(b"-"),
                "--repo needs a value"
            );
            cli.paths.push(path.into());
        } else if arg.as_encoded_bytes().starts_with(b"--repo=") {
            let path: OsString = arg.to_string_lossy()["--repo=".len()..].into();
            anyhow::ensure!(!path.is_empty(), "--repo needs a value");
            cli.paths.push(path.into());
        } else if arg.as_encoded_bytes().starts_with(b"-") {
            anyhow::bail!("{USAGE} (unexpected argument {arg:?})");
        } else {
            cli.paths.push(arg.into());
        }
    }
    Ok(cli)
}

/// Event loop, exactly: input → dispatch → poll → render.
/// Polling (not blocking) on input keeps status refreshes and the render
/// loop live; draining the job queue never blocks either.
///
/// Stage 5 — the render-loop pattern: unlike a request/response CLI (run
/// once, print, exit), a TUI loops forever: `poll` input with a timeout
/// (here 100ms so a frame still renders with no input) → update state →
/// `draw` the whole screen → repeat. `draw` diffs the previous frame and
/// emits only the changed ANSI sequences.
fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    workspace: &mut Workspace,
) -> Result<()> {
    loop {
        if event::poll(Duration::from_millis(100)).context("cannot poll input")? {
            if let Event::Key(key) = event::read().context("cannot read input")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // Ctrl-C safety hatch: works even if the user rebinds `quit`
                // away from `q`, and in text modals where `q` is literal.
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c' | 'C'))
                {
                    workspace.request_quit();
                } else {
                    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                    workspace.on_key_with_modifiers(
                        normalize_key(key.code, key.modifiers),
                        // `normalize_key` folds Shift+a into `A`; the commit
                        // box needs the original Shift state so a literal
                        // `A` (caps lock) still types normally.
                        shift || matches!(normalize_key(key.code, key.modifiers), KeyCode::Char('A')),
                    );
                }
            }
        }
        workspace.poll();
        terminal
            .draw(|f| ui::render_workspace(f, workspace))
            .context("cannot render")?;
        if workspace.should_quit() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_means_no_override() {
        assert!(parse_args(args(&[])).unwrap().theme.is_none());
    }

    #[test]
    fn theme_flag_space_and_equals_forms() {
        assert_eq!(
            parse_args(args(&["--theme", "tokyo-night"])).unwrap().theme,
            Some("tokyo-night".into())
        );
        assert_eq!(
            parse_args(args(&["--theme=default"])).unwrap().theme,
            Some("default".into())
        );
    }

    #[test]
    fn theme_flag_needs_a_value() {
        assert!(parse_args(args(&["--theme"])).is_err());
        assert!(parse_args(args(&["--theme="])).is_err());
    }

    #[test]
    fn positional_repo_paths_are_accepted() {
        assert!(parse_args(args(&["some-repo", "other-repo"])).is_ok());
    }

    #[test]
    fn multiple_repo_flags_and_positionals_are_accepted() {
        assert!(parse_args(args(&[
            "first",
            "--repo",
            "second",
            "--theme=tokyo-night",
            "--repo=third",
            "--repo",
            "fourth",
            "fifth",
        ]))
        .is_ok());
    }

    #[test]
    fn repo_flag_needs_a_value() {
        for input in [
            vec!["--repo"],
            vec!["--repo="],
            vec!["--repo", ""],
            vec!["--repo", "--theme=default"],
            vec!["--repo", "--"],
        ] {
            assert!(parse_args(args(&input)).is_err(), "{input:?}");
        }
    }

    #[test]
    fn unknown_flags_are_rejected() {
        for input in [vec!["--bogus"], vec!["-x"], vec!["repo", "--bogus=value"]] {
            assert!(parse_args(args(&input)).is_err(), "{input:?}");
        }
    }

    #[test]
    fn separator_treats_remaining_arguments_as_paths() {
        assert!(parse_args(args(&[
            "first",
            "--",
            "--repo",
            "--theme=default",
            "--help",
            "--",
            "-last",
        ]))
        .is_ok());
    }

    #[test]
    fn cli_theme_overrides_file_theme() {
        // Precedence CLI > file: file says default, CLI says tokyo-night.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[theme]\nname = \"default\"\n").unwrap();
        let mut config = Config::load_from_path(&path).unwrap();
        let cli = parse_args(args(&["--theme", "tokyo-night"])).unwrap();
        if let Some(name) = cli.theme {
            config.theme = Theme::by_name(&name).unwrap();
        }
        assert_eq!(
            config.theme.border_focused,
            ratatui::style::Color::Rgb(122, 162, 247)
        );
    }

    #[test]
    fn shift_lowercase_normalizes_to_uppercase() {
        use crossterm::event::KeyModifiers;
        assert_eq!(
            normalize_key(KeyCode::Char('q'), KeyModifiers::SHIFT),
            KeyCode::Char('Q')
        );
        assert_eq!(
            normalize_key(KeyCode::Char('q'), KeyModifiers::empty()),
            KeyCode::Char('q')
        );
        assert_eq!(
            normalize_key(KeyCode::Char('Q'), KeyModifiers::SHIFT),
            KeyCode::Char('Q')
        );
    }
}
