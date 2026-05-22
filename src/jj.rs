use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result};

const SEP: u8 = 0x1f;

// Custom oneline template emitting field-separated values. We run jj with
// its native `--graph` so the merge connectors (`├─┬─╮`, `╰─╯`) render
// exactly as they do in `jj log` — the picker treats those connector-only
// rows as decoration and keeps the cursor on commit rows.
//
// The template MUST emit a single line per commit (no embedded `\n`,
// `ui.log-word-wrap=false`), or jj's graph drawer would inject continuation
// rows into our one-line invariant.
//
// Fields (\x1f-separated, leading `\x1f` separates jj's graph chrome from
// our fields):
//   0  graph chrome (everything jj prepended before the first \x1f)
//   1  change_id.short()                 — full 12-char change id
//   2  change_id.shortest().prefix()     — shortest unique prefix
//   3  commit_id.short()                 — full 12-char commit id
//   4  commit_id.shortest().prefix()     — shortest unique prefix
//   5  payload — change-id-prefix-highlighted, bookmarks, conflict/empty,
//                description.first_line()
const TEMPLATE: &str = concat!(
    "\"\\x1f\" ++ ",
    "change_id.short() ++ \"\\x1f\" ++ ",
    "change_id.shortest().prefix() ++ \"\\x1f\" ++ ",
    "commit_id.short() ++ \"\\x1f\" ++ ",
    "commit_id.shortest().prefix() ++ \"\\x1f\" ++ ",
    "(separate(\" \", \
        format_short_change_id(self.change_id()), \
        bookmarks, \
        if(conflict, label(\"conflict\", \"conflict\")), \
        if(empty, label(\"empty\", \"(empty)\")), \
        description.first_line() \
    ))",
);

/// Either a commit row (selectable) or a connector row (decoration only —
/// `├─┬─╮`, `│ │`, `~`, …). Connectors have empty `change_id_short`.
pub struct Row {
    pub change_id_short: String,
    pub change_id_prefix: String,
    pub commit_id_short: String,
    pub commit_id_prefix: String,
    pub plain: String,
    pub styled: Vec<u8>,
}

impl Row {
    pub fn is_connector(&self) -> bool {
        self.change_id_short.is_empty()
    }
}

pub fn capture_log() -> Result<Vec<Row>> {
    let output = Command::new("jj")
        .args([
            "--ignore-working-copy",
            // Force off any user `ui.log-word-wrap = true` — wrapping would
            // split a description into a continuation row that looks
            // syntactically identical to a graph connector row, breaking the
            // one-row-per-commit invariant the picker relies on.
            "--config",
            "ui.log-word-wrap=false",
            "log",
            "--color=always",
            "-r",
            "all()",
            "--limit",
            "1000",
            "-T",
            TEMPLATE,
        ])
        .stderr(Stdio::inherit())
        .output()
        .context("failed to spawn jj — is it installed and on PATH?")?;

    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let mut rows = Vec::new();
    for raw in output.stdout.split(|&b| b == b'\n') {
        if raw.is_empty() {
            continue;
        }
        rows.push(parse_row(raw));
    }
    Ok(rows)
}

/// Parse one line of `jj log --graph` output. A line with our 5 `\x1f`
/// separators is a commit row; anything else is a connector / `~` /
/// continuation row that we keep as decoration but render unselectable.
fn parse_row(bytes: &[u8]) -> Row {
    let mut parts: Vec<&[u8]> = Vec::with_capacity(6);
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == SEP {
            parts.push(&bytes[start..i]);
            start = i + 1;
            if parts.len() == 5 {
                break;
            }
        }
    }
    if parts.len() < 5 {
        // Connector / `~` / pure-graph row — keep as decoration only.
        let styled = bytes.to_vec();
        let plain = strip_ansi(&styled);
        return Row {
            change_id_short: String::new(),
            change_id_prefix: String::new(),
            commit_id_short: String::new(),
            commit_id_prefix: String::new(),
            plain,
            styled,
        };
    }
    parts.push(&bytes[start..]);

    let graph = parts[0];
    let change_id_short = strip_ansi(parts[1]);
    let change_id_prefix = strip_ansi(parts[2]);
    let commit_id_short = strip_ansi(parts[3]);
    let commit_id_prefix = strip_ansi(parts[4]);
    let payload = parts[5];

    let mut styled = Vec::with_capacity(graph.len() + payload.len());
    styled.extend_from_slice(graph);
    styled.extend_from_slice(payload);
    let plain = strip_ansi(&styled);

    Row {
        change_id_short,
        change_id_prefix,
        commit_id_short,
        commit_id_prefix,
        plain,
        styled,
    }
}

/// Strip CSI escape sequences (ESC `[` … letter) from a byte slice.
/// Non-CSI bytes pass through; result is decoded as UTF-8 (lossy) so the
/// graph chrome (`@`, `○`, `◆`) survives.
pub fn strip_ansi(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Description and file-summary parts of `jj show --summary`, returned
/// separately so the renderer can fit them dynamically into the available
/// vertical space.
pub struct PreviewParts {
    /// Description lines (still 4-space-indented as jj show emits them),
    /// joined by `\n`. No trailing blank.
    pub description: Vec<u8>,
    /// File list (`M path`, `A path`, …) joined by `\n`. No leading blank.
    pub files: Vec<u8>,
}

/// Run `jj show --summary` for a single revision and split the result into
/// description and file-list parts (stripping the `Commit ID:` / `Change ID:`
/// / `Author:` / `Committer:` header).
pub fn show_summary(change_id: &str) -> PreviewParts {
    let out = Command::new("jj")
        .args([
            "--ignore-working-copy",
            "show",
            "--color=always",
            "--summary",
            "-r",
            change_id,
        ])
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => parse_show_summary(&o.stdout),
        _ => PreviewParts {
            description: b"(preview unavailable)".to_vec(),
            files: Vec::new(),
        },
    }
}

fn parse_show_summary(bytes: &[u8]) -> PreviewParts {
    // Skip the metadata header — everything up to and including the first
    // `\n\n` (blank line) is `Commit ID:` / `Change ID:` / `Author:` /
    // `Committer:` and we don't want any of it.
    let body: &[u8] = if let Some(pos) = bytes.windows(2).position(|w| w == b"\n\n") {
        &bytes[pos + 2..]
    } else {
        bytes
    };

    let lines: Vec<&[u8]> = body.split(|&b| b == b'\n').collect();

    // Description block: leading lines that are either 4-space-indented or
    // blank. Stops at the first non-indented non-blank line, which is the
    // first file entry (e.g. `M src/main.rs`).
    let mut desc_end = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() || line.starts_with(b"    ") {
            desc_end = i + 1;
        } else {
            break;
        }
    }
    // Trim trailing blank lines from the description block.
    while desc_end > 0 && lines[desc_end - 1].is_empty() {
        desc_end -= 1;
    }

    // Skip the first description line — it's the title, already shown on
    // the left-pane oneline as `description.first_line()`. Also skip any
    // blank lines immediately after it so the body starts cleanly.
    let mut desc_start = if desc_end > 0 { 1 } else { 0 };
    while desc_start < desc_end && lines[desc_start].is_empty() {
        desc_start += 1;
    }

    let mut description = Vec::new();
    for (i, line) in lines[desc_start..desc_end].iter().enumerate() {
        if i > 0 {
            description.push(b'\n');
        }
        // jj show indents description lines with 4 spaces — strip them so
        // the preview pane doesn't waste a 4-char left margin (and so our
        // wrapper has the full width to wrap into).
        let stripped: &[u8] = line.strip_prefix(b"    ").unwrap_or(line);
        description.extend_from_slice(stripped);
    }

    let mut files = Vec::new();
    let mut first = true;
    for line in lines.iter().skip(desc_end) {
        if line.is_empty() {
            continue;
        }
        if !first {
            files.push(b'\n');
        }
        files.extend_from_slice(line);
        first = false;
    }

    PreviewParts { description, files }
}

/// Which flag a leaf jj subcommand uses to accept a revset, picked in
/// priority order: `-r`/`--revision[s]` first, then `-t, --to`. `--from` is
/// intentionally not a fallback — its semantics ("filter source revisions")
/// differ enough from "the revision the picker chose" that silently injecting
/// it would surprise users.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevsetFlag {
    Revision,
    To,
}

impl RevsetFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            RevsetFlag::Revision => "-r",
            RevsetFlag::To => "--to",
        }
    }
}

/// A jj invocation jjf has analyzed and is ready to dispatch on. `leaf` is
/// the resolved subcommand path (preserving the user's tokens, including
/// aliases like `bm` — jj resolves them again at exec time). `passthrough`
/// is everything after the leaf. `flag` is `None` when the leaf accepts no
/// revset; in that case jjf refuses to run the picker. `user_supplied_revset`
/// is true when the passthrough already pins down the revision (a revset
/// flag, or a bare positional for a positional-revset leaf like `jj show`),
/// in which case jjf bypasses the picker and runs the command verbatim.
#[derive(Debug, Clone)]
pub struct RevsetTarget {
    pub leaf: Vec<String>,
    pub passthrough: Vec<String>,
    pub flag: Option<RevsetFlag>,
    pub user_supplied_revset: bool,
}

pub fn resolve_target(args: &[String]) -> Result<RevsetTarget> {
    debug_assert!(!args.is_empty(), "resolve_target requires at least one arg");

    let first_help = match run_help(std::slice::from_ref(&args[0]))? {
        Some(h) => h,
        None => {
            // Unknown subcommand or jj help failed for some reason. Fall
            // through optimistically so jj surfaces its own error message at
            // exec time.
            return Ok(RevsetTarget {
                leaf: vec![args[0].clone()],
                passthrough: args[1..].to_vec(),
                flag: Some(RevsetFlag::Revision),
                // No help to consult — fall back to a plain revset-flag scan.
                user_supplied_revset: has_revset_flag(&args[1..]),
            });
        }
    };

    let (leaf, leaf_help) = resolve_leaf(args, first_help)?;
    let passthrough = args[leaf.len()..].to_vec();
    let flag = parse_revset_flag(&leaf_help);
    let user_supplied_revset = passthrough_pins_revset(&passthrough, &leaf_help);

    Ok(RevsetTarget {
        leaf,
        passthrough,
        flag,
        user_supplied_revset,
    })
}

fn resolve_leaf(args: &[String], first_help: String) -> Result<(Vec<String>, String)> {
    let mut leaf = vec![args[0].clone()];
    let mut help = first_help;
    for arg in &args[1..] {
        if arg.starts_with('-') {
            break;
        }
        let children = parse_commands_section(&help);
        if !children.contains(arg.as_str()) {
            break;
        }
        let mut candidate = leaf.clone();
        candidate.push(arg.clone());
        match run_help(&candidate)? {
            Some(next_help) => {
                leaf = candidate;
                help = next_help;
            }
            None => break,
        }
    }
    Ok((leaf, help))
}

fn run_help(path: &[String]) -> Result<Option<String>> {
    let output = Command::new("jj")
        .args(path)
        .arg("--help")
        .stderr(Stdio::null())
        .output()
        .context("failed to spawn jj for preflight")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// Parse the `Commands:` block of a jj help output. Returns the set of
/// subcommand names AND their `[aliases: …]` annotations, so a user typing
/// `jjf tag s` (where `s` is the alias for `set`) walks correctly.
fn parse_commands_section(help: &str) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line == "Commands:" {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if line.is_empty() {
            break;
        }
        if !line.starts_with(' ') {
            break;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        // Primary entries have exactly 2-space indent. Anything deeper is
        // a description continuation we don't care about.
        if indent != 2 {
            continue;
        }
        if let Some(name) = trimmed.split_whitespace().next() {
            set.insert(name.to_string());
        }
        scan_aliases(line, &mut set);
    }
    set
}

fn scan_aliases(line: &str, names: &mut std::collections::HashSet<String>) {
    let Some(start) = line.find("[aliases:") else {
        return;
    };
    let after = &line[start + "[aliases:".len()..];
    let Some(end) = after.find(']') else {
        return;
    };
    for a in after[..end].split(',') {
        let n = a.trim();
        if !n.is_empty() {
            names.insert(n.to_string());
        }
    }
}

/// Pick the right revset-accepting flag from a leaf's help, preferring
/// `-r`/`--revision[s]` over `-t, --to`. Falls back to a positional
/// `[REVSET]` argument that exposes `-r` as an alias (this is how `jj show`
/// takes a revision). Returns `None` if the leaf has none of those.
pub fn parse_revset_flag(help: &str) -> Option<RevsetFlag> {
    let opts = parse_options(help);
    for opt in &opts {
        if opt.is_revset
            && opt
                .longs
                .iter()
                .any(|n| n == "--revision" || n == "--revisions")
        {
            return Some(RevsetFlag::Revision);
        }
    }
    for opt in &opts {
        if opt.is_revset && opt.longs.iter().any(|n| n == "--to") {
            return Some(RevsetFlag::To);
        }
    }
    // No revset *option* — fall back to a positional `[REVSET]` argument.
    // `jj show` accepts its revision this way; clap annotates the positional
    // with `[aliases: -r]` precisely so tools can inject it as `-r <rev>`.
    if positional_revset_accepts_r(help) {
        return Some(RevsetFlag::Revision);
    }
    None
}

/// True when the leaf takes its revset as a positional `[REVSET]`/`[REVSETS]`
/// argument that exposes `-r` (or `--revision`) as an alias. Such a positional
/// lives in the `Arguments:` block, which `parse_options` never scans, so
/// without this `jj show` would look like it accepts no revset at all.
///
/// The `-r` alias is required: jjf's dispatch injects the picked revision via
/// a flag, so a bare positional with no alias can't be targeted.
fn positional_revset_accepts_r(help: &str) -> bool {
    let mut in_args = false;
    let mut current_is_revset = false;
    for line in help.lines() {
        if line == "Arguments:" {
            in_args = true;
            continue;
        }
        if !in_args {
            continue;
        }
        // A non-indented non-empty line ends the `Arguments:` block.
        if !line.is_empty() && !line.starts_with(' ') {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        // New positional entry: 2-space indent, `[NAME]` or `<NAME>` (e.g.
        // `[REVSET]`, `<NAMES>...`, `[FILESETS]...`). Deeper indent is a
        // description / continuation line for the current entry.
        if indent == 2 && trimmed.starts_with(['[', '<']) {
            let name = trimmed
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(['[', ']', '<', '>'])
                .trim_end_matches("...")
                .trim_matches(['[', ']', '<', '>']);
            current_is_revset = matches!(name, "REVSET" | "REVSETS");
        }
        // `[aliases: …]` for a positional appears on its description line.
        if current_is_revset && line.contains("[aliases:") {
            let mut aliases = std::collections::HashSet::new();
            scan_aliases(line, &mut aliases);
            if aliases.iter().any(|a| a == "-r" || a == "--revision") {
                return true;
            }
        }
    }
    false
}

/// Whether the user's own args already pin down the revision, so jjf should
/// skip the picker and run their command verbatim. True when the passthrough
/// carries an explicit revset flag, or — for a leaf whose revset is a
/// positional `[REVSET]` argument (`jj show`) — when it carries a bare
/// positional token.
fn passthrough_pins_revset(passthrough: &[String], leaf_help: &str) -> bool {
    if has_revset_flag(passthrough) {
        return true;
    }
    if positional_revset_accepts_r(leaf_help) {
        return passthrough_has_bare_positional(passthrough, &value_flags(leaf_help));
    }
    false
}

/// True when the passthrough already carries an explicit revset flag
/// (`-r`/`--revision`/`--revisions`/`-t`/`--to`), whether as its own token or
/// in long-flag `--flag=value` form.
fn has_revset_flag(passthrough: &[String]) -> bool {
    passthrough.iter().any(|a| {
        let name = a.split('=').next().unwrap_or(a);
        matches!(name, "-r" | "--revision" | "--revisions" | "-t" | "--to")
    })
}

/// Scan a leaf's passthrough args for a bare positional token — one that is
/// neither an option flag nor consumed as the value of one. `value_flags` is
/// the set of option spellings known to take a following value, so `-T tmpl`
/// or `--color always` aren't mistaken for a positional. For a positional-
/// revset leaf, a bare positional means the user already named the revision.
fn passthrough_has_bare_positional(
    passthrough: &[String],
    value_flags: &std::collections::HashSet<String>,
) -> bool {
    let mut i = 0;
    while i < passthrough.len() {
        let tok = passthrough[i].as_str();
        if tok == "--" {
            // Everything after a bare `--` is positional.
            return i + 1 < passthrough.len();
        }
        if let Some(body) = tok.strip_prefix("--") {
            // Long flag. `--flag=value` carries its value inline; otherwise a
            // value-taking flag consumes the next token.
            let name = format!("--{}", body.split('=').next().unwrap_or(body));
            if !body.contains('=') && value_flags.contains(&name) {
                i += 1;
            }
            i += 1;
            continue;
        }
        if tok.starts_with('-') && tok.len() > 1 {
            // Short flag. `-r value` consumes the next token; an attached
            // value (`-rvalue`) or bundled bool shorts (`-sw`) do not.
            if value_flags.contains(tok) {
                i += 1;
            }
            i += 1;
            continue;
        }
        // Not a flag and not consumed as a value — a positional argument.
        return true;
    }
    false
}

/// Every option spelling (short `-x` and long `--long`) that consumes a
/// following value token, gathered across all of a leaf's `Options:` sections.
fn value_flags(help: &str) -> std::collections::HashSet<String> {
    let mut flags = std::collections::HashSet::new();
    for opt in parse_options(help) {
        if opt.takes_value {
            for f in opt.shorts.into_iter().chain(opt.longs) {
                flags.insert(f);
            }
        }
    }
    flags
}

#[derive(Debug)]
struct OptionEntry {
    shorts: Vec<String>,
    longs: Vec<String>,
    /// True when the header carried a `<PLACEHOLDER>` — i.e. the option
    /// consumes a following value token.
    takes_value: bool,
    /// True when the placeholder is `<REVSET>` or `<REVSETS>`.
    is_revset: bool,
}

/// Parse every option section of a leaf's help — the primary `Options:`, any
/// categorized `<Category> Options:`, and `Global Options:` — into a flat list
/// of entries. `[aliases: …]` continuation lines are folded into the owning
/// entry's name lists.
fn parse_options(help: &str) -> Vec<OptionEntry> {
    let mut opts = Vec::new();
    let mut in_options = false;
    let mut current: Option<OptionEntry> = None;

    for line in help.lines() {
        // Section headers are non-indented, non-empty lines. Option entries
        // live only under a header ending in `Options:` — `Arguments:`,
        // `Commands:`, and the intro paragraph switch parsing back off.
        if !line.is_empty() && !line.starts_with(' ') {
            if let Some(o) = current.take() {
                opts.push(o);
            }
            in_options = line.ends_with("Options:");
            continue;
        }
        if !in_options {
            continue;
        }
        if line.is_empty() {
            // Blank lines separate options inside the section; don't end it.
            continue;
        }

        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // New entry: 2-space indent with `-X,` (short + long) OR 6-space
        // indent with `--` (long-only flag, e.g. `--allow-move`).
        let is_new_entry = (indent == 2 && trimmed.starts_with('-'))
            || (indent == 6 && trimmed.starts_with("--"));

        if is_new_entry {
            if let Some(o) = current.take() {
                opts.push(o);
            }
            current = Some(parse_option_header(trimmed));
        } else if let Some(ref mut o) = current {
            // Continuation line. Pull any `[aliases: --foo, -x]` into the
            // current option's name sets so callers see alias spellings too.
            let mut alias_set = std::collections::HashSet::new();
            scan_aliases(line, &mut alias_set);
            for n in alias_set {
                if n.starts_with("--") {
                    o.longs.push(n);
                } else if n.starts_with('-') {
                    o.shorts.push(n);
                }
            }
        }
    }
    if let Some(o) = current {
        opts.push(o);
    }
    opts
}

fn parse_option_header(s: &str) -> OptionEntry {
    let mut shorts = Vec::new();
    let mut longs = Vec::new();
    let mut placeholder: Option<String> = None;
    for token in s.split(|c: char| c == ',' || c.is_whitespace()) {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix("--") {
            // Strip a trailing `=...` if clap ever emits one (it doesn't
            // today, but cheap insurance).
            let name = rest.split('=').next().unwrap_or(rest);
            longs.push(format!("--{name}"));
        } else if t.starts_with('-') {
            // Short flag, e.g. `-r`.
            shorts.push(t.to_string());
        } else if let Some(stripped) = t.strip_prefix('<') {
            // Placeholder: `<REVSET>`, `<REVSETS>`, `<NAMES>...`, etc.
            let inner = stripped.trim_end_matches("...");
            if let Some(end) = inner.find('>') {
                placeholder = Some(inner[..end].to_string());
            }
        }
    }
    let is_revset = matches!(placeholder.as_deref(), Some("REVSET") | Some("REVSETS"));
    OptionEntry {
        shorts,
        longs,
        takes_value: placeholder.is_some(),
        is_revset,
    }
}

pub fn exec(target: &RevsetTarget, ids: &[String]) -> Result<ExitStatus> {
    let mut cmd = Command::new("jj");
    cmd.args(&target.leaf);
    cmd.args(&target.passthrough);
    if !ids.is_empty() {
        if let Some(flag) = target.flag {
            cmd.arg(flag.as_str());
            cmd.arg(ids.join("|"));
        }
    }
    let status = cmd.status().context("failed to spawn jj")?;
    Ok(status)
}

pub fn command_line(target: &RevsetTarget, ids: &[String]) -> String {
    let mut parts: Vec<String> =
        Vec::with_capacity(2 + target.leaf.len() + target.passthrough.len() + 2);
    parts.push("jj".into());
    for w in &target.leaf {
        parts.push(w.clone());
    }
    for a in &target.passthrough {
        parts.push(shell_quote(a));
    }
    if !ids.is_empty() {
        if let Some(flag) = target.flag {
            parts.push(flag.as_str().into());
            parts.push(shell_quote(&ids.join("|")));
        }
    }
    parts.join(" ")
}

pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_-./=:@,".contains(&b))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG_SET_HELP: &str = "Create or update tags

Usage: jj tag set [OPTIONS] <NAMES>...

Arguments:
  <NAMES>...
          Tag names to create or update

Options:
  -r, --revision <REVSET>
          Target revision to point to

          [default: @]
          [aliases: --to]

      --allow-move
          Allow moving existing tags

  -h, --help
          Print help (see a summary with '-h')

Global Options:
  -R, --repository <REPOSITORY>
          Path to repository
";

    const BOOKMARK_MOVE_HELP: &str = "Move existing bookmarks to target revision

Usage: jj bookmark move [OPTIONS] <NAMES|--from <REVSETS>>

Arguments:
  [NAMES]...
          Move bookmarks matching the given name patterns

Options:
  -f, --from <REVSETS>
          Move bookmarks from the given revisions

  -t, --to <REVSET>
          Move bookmarks to this revision

          [default: @]

  -B, --allow-backwards
          Allow moving bookmarks backwards or sideways

  -h, --help
          Print help (see a summary with '-h')

Global Options:
  -R, --repository <REPOSITORY>
          Path to repository
";

    const TAG_PARENT_HELP: &str = "Manage tags

Usage: jj tag [OPTIONS] <COMMAND>

Commands:
  delete  Delete existing tags [aliases: d]
  list    List tags and their targets [aliases: l]
  set     Create or update tags [aliases: s]

Options:
  -h, --help
          Print help (see a summary with '-h')

Global Options:
  -R, --repository <REPOSITORY>
          Path to repository
";

    const SHOW_HELP: &str = "Show commit description and changes in a revision

Usage: jj show [OPTIONS] [REVSET]

Arguments:
  [REVSET]
          Show changes in this revision, compared to its parent(s) [default: @] [aliases: -r]

Options:
  -T, --template <TEMPLATE>
          Render a revision using the given template

  -h, --help
          Print help (see a summary with '-h')

Diff Formatting Options:
  -s, --summary
          For each path, show only whether it was modified, added, or deleted

      --tool <TOOL>
          Generate diff by external command

Global Options:
  -R, --repository <REPOSITORY>
          Path to repository
";

    // A leaf with a positional revset but no `-r` alias jjf could inject.
    const LOG_HELP: &str = "Show revision history

Usage: jj log [OPTIONS] [FILESETS]...

Arguments:
  [FILESETS]...
          Show revisions modifying the given paths

Options:
  -r, --revision <REVSETS>
          Which revisions to show

  -h, --help
          Print help (see a summary with '-h')

Global Options:
  -R, --repository <REPOSITORY>
          Path to repository
";

    #[test]
    fn revset_flag_detects_show_positional() {
        // `jj show` takes its revset as the positional `[REVSET]` argument,
        // annotated `[aliases: -r]`. jjf must inject `-r <picked>`.
        assert_eq!(parse_revset_flag(SHOW_HELP), Some(RevsetFlag::Revision));
    }

    #[test]
    fn revset_flag_log_uses_option_not_positional() {
        // `jj log` has a `[FILESETS]...` positional (not a revset) and a real
        // `-r, --revision` option — the option must drive the result.
        assert_eq!(parse_revset_flag(LOG_HELP), Some(RevsetFlag::Revision));
        assert!(!positional_revset_accepts_r(LOG_HELP));
    }

    #[test]
    fn revset_flag_prefers_revision_over_to_alias() {
        // tag set has `-r, --revision <REVSET>` with `[aliases: --to]`.
        // The Revision-class match must win over the To-class match.
        assert_eq!(parse_revset_flag(TAG_SET_HELP), Some(RevsetFlag::Revision));
    }

    #[test]
    fn revset_flag_falls_back_to_to_when_no_revision() {
        // bookmark move has only `-f, --from <REVSETS>` and `-t, --to <REVSET>`.
        assert_eq!(parse_revset_flag(BOOKMARK_MOVE_HELP), Some(RevsetFlag::To));
    }

    #[test]
    fn revset_flag_none_for_parent_without_options() {
        assert_eq!(parse_revset_flag(TAG_PARENT_HELP), None);
    }

    #[test]
    fn commands_section_includes_names_and_aliases() {
        let cmds = parse_commands_section(TAG_PARENT_HELP);
        for name in ["delete", "list", "set", "d", "l", "s"] {
            assert!(cmds.contains(name), "missing {name} in {cmds:?}");
        }
    }

    #[test]
    fn commands_section_empty_for_leaf() {
        let cmds = parse_commands_section(BOOKMARK_MOVE_HELP);
        assert!(cmds.is_empty(), "expected no commands, got {cmds:?}");
    }

    #[test]
    fn command_line_emits_revision_flag() {
        let target = RevsetTarget {
            leaf: vec!["tag".into(), "set".into()],
            passthrough: vec!["v0.2.0".into()],
            flag: Some(RevsetFlag::Revision),
            user_supplied_revset: false,
        };
        let s = command_line(&target, &["abcd1234".into()]);
        assert_eq!(s, "jj tag set v0.2.0 -r abcd1234");
    }

    #[test]
    fn command_line_emits_to_flag() {
        let target = RevsetTarget {
            leaf: vec!["bm".into()],
            passthrough: vec!["main".into()],
            flag: Some(RevsetFlag::To),
            user_supplied_revset: false,
        };
        let s = command_line(&target, &["abcd1234".into()]);
        assert_eq!(s, "jj bm main --to abcd1234");
    }

    #[test]
    fn command_line_omits_flag_when_no_ids() {
        let target = RevsetTarget {
            leaf: vec!["describe".into()],
            passthrough: vec!["-r".into(), "@".into()],
            flag: Some(RevsetFlag::Revision),
            user_supplied_revset: true,
        };
        // When the user already supplied -r, jjf calls command_line with empty
        // ids — no extra flag should be appended.
        let s = command_line(&target, &[]);
        assert_eq!(s, "jj describe -r @");
    }

    fn pins(passthrough: &[&str], help: &str) -> bool {
        let owned: Vec<String> = passthrough.iter().map(|s| s.to_string()).collect();
        passthrough_pins_revset(&owned, help)
    }

    #[test]
    fn show_bare_positional_pins_revset() {
        // `jjf show @` / `jjf show abc123` — the revision is already named as
        // the positional `[REVSET]`, so jjf must bypass the picker.
        assert!(pins(&["@"], SHOW_HELP));
        assert!(pins(&["abc123"], SHOW_HELP));
        assert!(pins(&["@", "--summary"], SHOW_HELP));
        assert!(pins(&["--summary", "@"], SHOW_HELP));
    }

    #[test]
    fn show_without_revision_runs_picker() {
        // No revision named — jjf should fall through to the picker. Bare
        // `jjf show`, and `jjf show` with only flags, must NOT count as pinned.
        assert!(!pins(&[], SHOW_HELP));
        assert!(!pins(&["--summary"], SHOW_HELP));
        assert!(!pins(&["-s"], SHOW_HELP));
    }

    #[test]
    fn show_option_value_is_not_a_positional() {
        // The value of a value-taking option must not be mistaken for the
        // positional revset: `-T tmpl` / `--tool meld` still want the picker.
        assert!(!pins(&["-T", "mytemplate"], SHOW_HELP));
        assert!(!pins(&["--tool", "meld"], SHOW_HELP));
        // ...but a real positional alongside such an option still pins it.
        assert!(pins(&["-T", "mytemplate", "@"], SHOW_HELP));
    }

    #[test]
    fn show_explicit_revset_flag_pins_revset() {
        // The `-r` alias and the `--revision=@` inline form both count.
        assert!(pins(&["-r", "@"], SHOW_HELP));
        assert!(pins(&["--revision=@"], SHOW_HELP));
    }

    #[test]
    fn dashdash_separates_positionals() {
        assert!(pins(&["--", "@"], SHOW_HELP));
        assert!(!pins(&["--"], SHOW_HELP));
    }

    #[test]
    fn option_leaf_bare_positional_does_not_pin_revset() {
        // `jj tag set v0.2.0` — `v0.2.0` is the `<NAMES>` positional, NOT a
        // revset. Only an explicit `-r`/`--to` flag pins the revision here, so
        // jjf still runs the picker for the target revision.
        assert!(!pins(&["v0.2.0"], TAG_SET_HELP));
        assert!(pins(&["v0.2.0", "-r", "@"], TAG_SET_HELP));
    }

    #[test]
    fn value_flags_span_all_option_sections() {
        // Value-taking flags are gathered from `Options:`, the categorized
        // `Diff Formatting Options:`, and `Global Options:` alike; bool flags
        // and `--help` are excluded.
        let vf = value_flags(SHOW_HELP);
        for f in ["-T", "--template", "--tool", "-R", "--repository"] {
            assert!(vf.contains(f), "missing value flag {f} in {vf:?}");
        }
        for f in ["-s", "--summary", "-h", "--help"] {
            assert!(!vf.contains(f), "{f} should not be a value flag");
        }
    }
}
