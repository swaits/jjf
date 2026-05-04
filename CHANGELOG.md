# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] — 2026-05-04

Subcommand dispatch is now help-driven instead of substring-matching
"REVSET", so multi-level subcommands and non-`-r` revset flags work.

### Fixed

- `jjf bm main` (where `bm` is a user alias for `bookmark move`) now
  correctly emits `jj bm main --to <picked>` instead of `-r <picked>`,
  which jj rejected. jjf parses the leaf's `Options:` block and picks
  `-r`/`--revision[s]` first, then `-t, --to`. `--from` is intentionally
  not a fallback (different semantics).
- `jjf tag set v0.2.0` no longer errors with "doesn't take revisions".
  jjf now walks into multi-level subcommands by parsing each parent's
  `Commands:` block, so `tag set`, `bookmark move`, `git push`, etc.
  resolve to their actual leaf and read its real flag set.

### Added

- Bypass mode when the user already passed a revset flag in their args.
  `jjf describe -r @-` now runs verbatim instead of double-injecting
  `-r`. Triggers on any of `-r`, `--revision`, `--revisions`, `-t`,
  `--to` in passthrough.
- Refusal message for leaves with no revset flag now includes the
  resolved leaf path (e.g. `jjf workspace list` → "'jj workspace list'
  takes no revset flag — run 'jj workspace list' directly") so users
  see exactly which command jjf landed on.
- Unit tests for the help parsers and command-line construction
  against captured fixtures of `jj tag set --help`,
  `jj bookmark move --help`, and `jj tag --help`.

[0.3.0]: https://github.com/swaits/jjf/releases/tag/v0.3.0

## [0.2.0] — 2026-04-29

Picker now renders jj's real graph (merge connectors and all), with the
cursor/filter aware of decorative rows.

### Changed

- Graph rendering now uses jj's native `--graph` output instead of a
  custom one-row-per-commit reconstruction. Merge connectors
  (`├─┬─╮`, `├─╯`, `╰─╯`) appear exactly as `jj log` draws them, so the
  picker is a 1:1 visual match for the real log.
- Cursor row arrow glyphs changed from `├──►` to `●──▶` so the
  selection arrow doesn't visually clash with the graph chrome.
- Hint row at the bottom of the picker drops trailing key/label pairs
  (`esc quit`, `^U clear`, …) until it fits on one row. Previously a
  narrow terminal would wrap the hint and push the search bar off the
  top of the screen.

### Added

- Connector / decorative graph rows (`├─╮`, `╰─╯`, `~`, …) are kept in
  the picker for visual fidelity with `jj log`. Cursor navigation
  (arrows, Page{Up,Down}, Home, End, Ctrl-N/P/J/K, Ctrl-A/E) skips over
  them; selection is silently ignored on connector rows.
- Filtering (any non-empty query) excludes connector rows, since they
  can't match a query and a stray `├─╮` between filtered hits would
  carry no useful information.

### Fixed

- Long descriptions no longer split across rows when the user has
  `ui.log-word-wrap = true` configured globally — the internal
  `jj log` invocation now forces `ui.log-word-wrap=false` so a single
  commit always renders on a single picker row.

[0.2.0]: https://github.com/swaits/jjf/releases/tag/v0.2.0

## [0.1.0] — 2026-04-29

Initial release.

### Added

- Fuzzy picker over `jj log` for any jj subcommand that accepts `-r`.
- Smart prefix matching: typing the bold-highlighted portion of a change
  ID jumps to that revision instantly. Falls back to nucleo fuzzy
  matching against description text.
- Multi-select via `Tab`; selections joined into `-r 'A|B|C'` revset
  union.
- Two-column layout (when terminal ≥ 80 cols): picker on left, live
  `jj show --summary` preview of the cursor row on right, cached per
  revision.
- Live command preview row showing the exact `jj …` invocation that
  Enter will run.
- Preflight check via `jj <sub> --help`: subcommands that don't accept
  revisions (`status`, `log`, etc.) are rejected upfront with a
  friendly message.
- `--ignore-working-copy` on the internal `jj log` call so the picker
  doesn't race with prompt/editor auto-snapshot integrations.
- Shell integration via `jjf init <bash|zsh|fish|nu>`:
  - rewrites shell history so up-arrow recalls the resolved `jj …`
    command rather than the `jjf …` invocation,
  - drops the `jjf …` entry on cancel/error so cancelled invocations
    don't pollute history,
  - defaults `jjf` (no args) to `jjf show`,
  - passes meta-commands and bare flags through to the binary directly.
- Vim-style and emacs-style navigation: `↑↓`, `Ctrl-N/P`, `Ctrl-J/K`,
  `PageUp/PageDown`, `Home/End`, `Ctrl-A/E`.
- Light-terminal detection via `COLORFGBG`: cursor-row highlight uses
  reverse video on light themes (where the dark gray bg is invisible).
- Synchronized output (DEC mode 2026) around each redraw to eliminate
  tearing on Kitty / WezTerm / foot / recent iTerm.

[0.1.0]: https://github.com/swaits/jjf/releases/tag/v0.1.0
