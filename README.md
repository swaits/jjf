# jjf

A fuzzy revision picker for [jujutsu (jj)](https://github.com/jj-vcs/jj).

Wraps any jj subcommand with an interactive picker over `jj log`. Type to
filter, hit Enter, and `jjf` runs your command with `-r '<id>'` filled in
from the revision you picked. The right pane shows a live `jj show
--summary` of whatever's under the cursor; the bottom row shows the exact
command that's about to run.

```
❯ vx                                          ┌────────────────────────────────
  ○  tovs chore(release): prepare 0.1.0       │  - Add LICENSE (MIT)
  @  xnkx feat(ux): more refinements          │  - Add README with install,
  ○  ssnu feat(ux): batch UX improvements ●──▶│   usage, keybindings…
  ○  lyxo fix(history): drop jjf entry        │
  ○  tqvr feat: two-column layout             │  M  Cargo.toml
  ○  yksz feat: default to 'show'             │  A  LICENSE
  ○  lsqz feat: broaden picker revset         │  A  README.md
  ○  nzwx feat: initial implementation
▶ jj describe -m 'fix typo' -r 'ss'
[8/8]  type filter · ↑↓/^N^P nav · tab select · enter run · ^U clear · esc quit
```

## Install

```sh
cargo install jjf
```

Requires Rust 1.85+ (edition 2024) and `jj` 0.40+ on `PATH`.

## Usage

```sh
jjf <jj-subcommand> [args...]
```

Some examples:

```sh
jjf describe -m 'fix: validate input'
jjf abandon
jjf edit
jjf squash
jjf rebase -d main
```

`jjf` runs `jj log`, lets you fuzzy-pick a revision (or several), then
invokes the subcommand with `-r '<id>'` appended. Subcommands that don't
accept revisions (`status`, `log`, `git fetch`, etc.) are detected
upfront and rejected with a friendly message — no wasted picker cycle.

With shell integration installed (see below), `jjf` with no arguments
defaults to `jjf show` so it doubles as a quick rev browser.

## Smart prefix matching

Type the bold portion of any change ID shown in `jj log` and that
revision is highlighted instantly. Falls back to fuzzy-matching the
description text:

| Query matches                       | Score        |
| ----------------------------------- | ------------ |
| Prefix of `change_id.shortest()`    | 1,000,000    |
| Prefix of full short change ID (12) | 100,000      |
| Prefix of `commit_id.shortest()`    | 10,000       |
| Prefix of full short commit ID (12) | 1,000        |
| Fuzzy match of description text     | nucleo score |

Empty filter shows all revisions in original `jj log` order.

## Keybindings

| Key                                                 | Action                              |
| --------------------------------------------------- | ----------------------------------- |
| Any printable char                                  | Append to filter                    |
| `↑` / `↓`, `Ctrl-N` / `Ctrl-P`, `Ctrl-J` / `Ctrl-K` | Move cursor                         |
| `PageUp` / `PageDown`                               | Page up / down                      |
| `Home` / `End`, `Ctrl-A` / `Ctrl-E`                 | Jump to first / last                |
| `Tab`                                               | Toggle selection on cursor row      |
| `Enter`                                             | Confirm: selected rows, else cursor |
| `Backspace`, `Ctrl-H`                               | Delete char from filter             |
| `Ctrl-U`                                            | Clear filter                        |
| `Ctrl-W`                                            | Delete word from filter             |
| `Esc`, `Ctrl-C`                                     | Cancel                              |

Multi-select with `Tab` joins picks into a `-r 'A|B|C'` revset union.
Subcommands that require exactly one revision (`describe`, `edit`) will
surface jj's own error if multi-selected.

## Shell integration

Add to your shell rc:

```sh
# bash
eval "$(jjf init bash)"

# zsh
eval "$(jjf init zsh)"

# fish
jjf init fish | source

# nushell — save once, source from your config.nu
jjf init nu | save -f ~/.config/nushell/jjf.nu
# then add:  source ~/.config/nushell/jjf.nu
```

With this in place:

- **History rewriting** — `jjf describe -m 'foo'` records the resolved
  `jj describe -m 'foo' -r '<id>'` in your shell history rather than the
  `jjf …` invocation. Up-arrow recalls the resolved command, ready to
  re-run or edit.
- **`jjf` alone** runs `jjf show` so it's a quick rev browser.
- **`jjf init …`, `jjf -h`, `jjf --version`** bypass the picker and
  pass through to the binary directly.

| Shell   | History append           | Original `jjf …` entry                        |
| ------- | ------------------------ | --------------------------------------------- |
| bash    | `history -s`             | deleted via `history -d`                      |
| zsh     | `print -s`               | suppressed via `zshaddhistory` hook           |
| fish    | `builtin history append` | deleted via `fish_postexec` hook              |
| nushell | `history import`         | remains (sqlite history; no clean delete API) |

Without shell integration, `jjf` still works — it just execs `jj`
directly and your shell history shows the original `jjf …` invocation.

## How it works

`jjf` invokes `jj log --ignore-working-copy --color=always -r 'all()'`
with a custom template that emits per-revision metadata (short change
ID, shortest unique change-ID prefix, short commit ID, shortest unique
commit-ID prefix) plus a custom oneline payload, separated by `\x1f`
sentinels.

The picker is a hand-rolled TUI rendered via direct ANSI to `/dev/tty`
(no `ratatui`, no `ansi-to-tui`). Two-column layout when the terminal
is at least 80 cols wide: picker on the left, `jj show --summary` of
the cursor row on the right (cached per revision). On Enter, the
resolved `jj <sub> [args] -r '<id>'` is either exec'd directly or
printed to stdout (in `--emit` mode, used by the shell wrappers).

The `--ignore-working-copy` flag avoids snapshotting during the picker
phase, which prevents races with prompt/editor integrations that
auto-snapshot (e.g. starship's `jj` module).

## Dependencies

Runtime crates: `crossterm`, `nucleo-matcher`, `anyhow`, `libc`. Total
transitive dep count is around 50; release binary is ~700 KB stripped.

## Acknowledgments

The idea for `jjf` came from a [bash + fzf script of the same
concept](https://oppi.li/posts/jjj/) (also discussed on
[lobste.rs](https://lobste.rs/s/exlogg/jjj)) by the author at oppi.li.
`jjf` is an independent Rust implementation — **none** of the original
script's code was viewed or used by the author — but the core insight (wrap any jj subcommand
with an fzf-style picker over `jj log`) is theirs and worth crediting.

## License

MIT — see [LICENSE](LICENSE).
