mod app;
mod config;
mod fuzzy;
mod syntax;
mod ui;
mod words;

use anyhow::{Context, Result};
use app::App;
use config::{Config, Theme};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use git_tui_core::jobqueue::JobQueue;
use git_tui_core::repo::Repo;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::time::Duration;

fn main() -> Result<()> {
    // Stage 5 — raw mode, explained:
    // A terminal normally line-buffers input and echoes it (cooked mode):
    // your program only sees a line after Enter. A TUI needs every keypress
    // immediately and without echo, so it enables raw mode. The terminal is
    // global state: if we exit (or panic) without restoring it, the user's
    // shell is left broken (no echo, no line editing). Hence the panic hook
    // below + the explicit restore after `run` returns.
    install_panic_hook();

    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let root = Repo::discover(&cwd)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .workdir()
        .context("bare repositories are not supported")?;
    let queue = JobQueue::spawn(&root).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Missing/invalid config fails fast here (path + value in the error);
    // missing file falls back to defaults inside `Config::load`.
    // Precedence: `--theme` flag > config file > default.
    let cli = parse_args(std::env::args().skip(1))?;
    let mut config = Config::load().context("cannot load config")?;
    if let Some(name) = cli.theme {
        config.theme = Theme::by_name(&name).with_context(|| format!("unknown theme {name:?}"))?;
    }
    let mut app = App::new_with_config(queue, config);
    if let Some(name) = root
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
    {
        app.set_repo_name(name);
    }

    enable_raw_mode().context("cannot enable raw mode")?;
    let mut out = stdout();
    // Alternate screen = the terminal saves the current view and shows a
    // scratch buffer; on `LeaveAlternateScreen` the shell contents reappear.
    // All drawing from here on is just ANSI escape sequences (cursor moves,
    // colors) emitted by ratatui — you never write them by hand.
    execute!(out, EnterAlternateScreen).context("cannot enter alternate screen")?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend).context("cannot create terminal")?;

    let res = run(&mut terminal, &mut app);

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

/// Parsed CLI flags. Only `--theme` for now; the app takes no positionals.
struct Cli {
    theme: Option<String>,
}

const USAGE: &str = "usage: git-tui [--theme <default|tokyo-night>]";

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli> {
    let mut cli = Cli { theme: None };
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            println!("{USAGE}");
            std::process::exit(0);
        } else if arg == "--theme" {
            let name = args.next().context("--theme needs a value")?;
            cli.theme = Some(name);
        } else if let Some(name) = arg.strip_prefix("--theme=") {
            anyhow::ensure!(!name.is_empty(), "--theme needs a value");
            cli.theme = Some(name.to_string());
        } else {
            anyhow::bail!("{USAGE} (unexpected argument {arg:?})");
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
fn run(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) -> Result<()> {
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
                    app.request_quit();
                } else {
                    app.on_key(key.code);
                }
            }
        }
        app.poll();
        terminal
            .draw(|f| ui::render(f, app))
            .context("cannot render")?;
        if app.should_quit() {
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
    fn unexpected_positional_errors() {
        assert!(parse_args(args(&["some-repo"])).is_err());
        assert!(parse_args(args(&["--bogus"])).is_err());
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
}
