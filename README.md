# git-tui

A fast, keyboard-driven git TUI for Linux, focused on the commit workflow:
status → diff → stage (whole files or single hunks) → commit.

## Vision

**What this is:** a terminal-native tool that makes reviewing and composing
commits faster than `git add -p`. Status, diff, staging, and committing are the
first-class workflow; branches, log, stash, and interactive rebase follow once
that loop is solid.

**What this is not:**
- Not a full-featured git GUI replacement (no blame, no merge-PR-review
  features)
- Not cross-platform — Linux-first, terminal-only
- Not a pure-libgit2 purist for network ops — local ops use libgit2
  (`git2`) directly, while push/pull shell out to the `git` CLI
  (lazygit-style) so your ssh keys, agent, and credential helpers work
  unchanged

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
quit = "Q"
project_close = "q"
sync_pull = "p"
sync_push = "P"

[llm]
provider = "openai"  # openai | openrouter | ollama | anthropic | gemini | custom
model = "gpt-4o-mini"
api_key = "sk-..."  # or leave empty and export $OPENAI_API_KEY ($ANTHROPIC_API_KEY,
                    # $GEMINI_API_KEY, $OPENROUTER_API_KEY, or generic $LLM_API_KEY)
# base_url = "http://localhost:11434/v1"  # only for provider = "custom" (or to override)
```

AI commit messages: stage with `space`, press `c` for the commit box,
then `Shift+A` generates a Conventional-Commits message from the staged
(index vs HEAD) diffs — every staged file is referenced in the prompt.
Connect the LLM either through the TUI (press `A` in the file list for
the setup form: provider, model, API key) or via the file below;
`ollama` needs no key.

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
 `find_files`, `project_next`, `project_prev`, `project_open`, `project_close`,
 `sync_pull`, `sync_push`, `llm_settings`.

Keyboard flow: `j`/`k` move in the file tree (the selected file's unified
diff previews inline), `/` fuzzy-finds a file (`enter` jumps to it),
`enter` opens it fullscreen side-by-side (`esc` closes) — files with no
changes show the whole file automatically, changed files show the diff —
`space` stages, `c` commits (`Shift+A` inside the commit box generates the
message from staged files, `A` in the file list opens the LLM setup),
`p` pulls, `P` pushes,
`1`–`4`/`tab` switch panels. Text boxes
(commit message, new branch, stash message, jump-to-path, file finder)
are fully editable: `←`/`→` move the cursor, `Home`/`End` jump,
`backspace`/`Del` delete, and long lines scroll horizontally so the
cursor is always visible.

## Multiple projects

Pass several repositories and switch between them without leaving the
viewer (herder-style):

```sh
git-tui ~/projects/api ~/projects/web
git-tui --repo ~/projects/api --repo ~/projects/web
```

Each project keeps its own status, diff, selection, and staging state.
`[` / `]` cycle projects (rebindable via `project_prev` / `project_next`);
`q` (`project_close`) closes the current project (last one quits the app),
`Q` (`quit`) quits the whole application;
`o` (`project_open`) opens a directory browser without leaving the viewer.
It starts at the current project; just start typing to filter the current
folder's list (no prefix key), `enter` opens the highlighted folder (`.`
opens the shown folder itself), `↑`/`↓` move, `→` descends, `←` goes up
(`backspace` edits the query, or goes up when it is empty), `tab` jumps
to a typed path, and `esc` clears the query first, then closes. Repo roots
show a `[repo]` badge, open tabs `[open]`.
Edge cases stay in the browser as errors (missing path, bare repos, files);
a plain directory offers `enter` to `git init` it first, and an
already-open repo just switches to its tab. Empty repos (no commits yet)
open fine.
the project bar on top shows every repo with its dirty-file count and
stays visible even in fullscreen diff.

## Sync: push, pull, publish

Lazygit-style remote workflow (`p` / `P`, rebindable via `sync_pull` /
`sync_push`); works from the file list and from fullscreen diff:

- `p` pulls the current branch (`git pull`, so your `pull.rebase` /
  `pull.ff` config decides merge vs rebase).
- `P` pushes: straight to the upstream when one exists; otherwise it
  prompts for the remote (`git push -u <remote> <branch>`).
- Fresh `git init` with no remote at all: `P` prompts for the `origin`
  URL instead — create the empty repo on GitHub, paste its URL, and the
  branch is published (`git remote add origin <url>` + `push -u`).

The status panel tracks the upstream (`main → origin/main ↑2↓1`) and
shows `pushing…` / `pulling…` while a sync job runs; failures land on
the error line with the remote's message. Prompts are disabled for sync
commands, so missing credentials fail fast instead of hanging. Push,
pull, and publish also refresh status, diff, branches, log, and stash
when they complete.

## Status

Working TUI: `cargo build --release` produces `target/release/git-tui`;
`cargo test --workspace` covers core + TUI.
