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

## Status

Planning/scaffolding phase. No usable builds yet.
