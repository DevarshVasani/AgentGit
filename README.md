# git-tui

A fast, keyboard-driven git TUI for Linux, focused on the commit workflow:
status → diff → stage (whole files or single hunks) → commit.

## Vision

**What this is:** a terminal-native tool that makes reviewing and composing
commits faster than `git add -p`. Status, diff, staging, and committing are the
first-class workflow; branches, log, stash, and interactive rebase follow once
that loop is solid.

**What this is not:**
- Not a full-featured git GUI replacement (no push/pull UI in v0.1, no blame,
  no merge-PR-review features)
- Not cross-platform — Linux-first, terminal-only
- Not a wrapper around the `git` CLI — it uses libgit2 (`git2`) directly

## Layout

- `git-tui-core` — all git operations and the async job engine (no TUI deps)
- `git-tui` — ratatui frontend

Docs: [architecture](docs/architecture.md) ·
[phase 1 checklist](docs/phase-1-checklist.md)

## Config

`~/.config/git-tui/config.toml` (or `$XDG_CONFIG_HOME/git-tui/config.toml`
when set). No file means defaults. Unknown actions, key names, or sections
fail fast with the offending value instead of being silently ignored.

```toml
[theme]
name = "tokyo-night"  # or "default"

[keys]
stage = "s"
quit = ["q", "Q"]
```

The theme can also be forced for one run (overrides the file):

```sh
git-tui --theme tokyo-night
```

Key names: single characters, plus `space`, `tab`, `enter`, `esc`,
`backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`, `pageup`,
`pagedown`, `home`, `end`. Action names are listed in
`git-tui/src/config.rs` (`ACTIONS`): `nav_down`, `nav_up`, `stage`, `commit`,
`refresh`, `quit`, `focus_next`, `focus_status`, `focus_branches`,
`focus_log`, `focus_stash`, `scroll_up`, `scroll_down`, `branch_new`,
`branch_delete`, `checkout`, `stash_pop`, `stash_push`, `stash_drop`,
`find_files`.

Keyboard flow: `j`/`k` move in the file tree (the selected file's unified
diff previews inline), `/` fuzzy-finds a file (`enter` jumps to it),
`enter` opens it fullscreen side-by-side (`esc` closes) — files with no
changes show the whole file automatically, changed files show the diff —
`space` stages, `c` commits, `1`–`4`/`tab` switch panels.

## Status

Working TUI: `cargo build --release` produces `target/release/git-tui`;
`cargo test --workspace` covers core + TUI.
