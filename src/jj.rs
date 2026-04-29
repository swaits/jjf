use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result};

const SEP: u8 = 0x1f;

// Custom oneline template: drop the timestamp and commit id (rarely useful in
// the picker, and the commit-id hex steals real estate), keep change-id with
// shortest-unique-prefix highlighting, bookmarks, conflict/empty labels, and
// the first line of the description.
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

pub struct Row {
    pub change_id_short: String,
    pub change_id_prefix: String,
    pub commit_id_short: String,
    pub commit_id_prefix: String,
    pub plain: String,
    pub styled: Vec<u8>,
}

pub fn capture_log() -> Result<Vec<Row>> {
    let output = Command::new("jj")
        .args([
            "--ignore-working-copy",
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
        if let Some(row) = parse_row(raw) {
            rows.push(row);
        }
    }
    Ok(rows)
}

fn parse_row(bytes: &[u8]) -> Option<Row> {
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
        return None;
    }
    parts.push(&bytes[start..]);

    let graph = parts[0];
    let change_id_short = strip_ansi(parts[1]);
    let change_id_prefix = strip_ansi(parts[2]);
    let commit_id_short = strip_ansi(parts[3]);
    let commit_id_prefix = strip_ansi(parts[4]);
    let payload = parts[5];

    if change_id_short.is_empty() {
        return None;
    }

    let mut styled = Vec::with_capacity(graph.len() + payload.len());
    styled.extend_from_slice(graph);
    styled.extend_from_slice(payload);

    let plain = strip_ansi(&styled);

    Some(Row {
        change_id_short,
        change_id_prefix,
        commit_id_short,
        commit_id_prefix,
        plain,
        styled,
    })
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

pub fn supports_revisions(subcommand: &str) -> Result<bool> {
    let output = Command::new("jj")
        .arg(subcommand)
        .arg("--help")
        .output()
        .context("failed to spawn jj for preflight")?;
    if !output.status.success() {
        // unknown subcommand or other error — let the main flow surface jj's error
        return Ok(true);
    }
    let help = String::from_utf8_lossy(&output.stdout);
    Ok(help.contains("REVSET"))
}

pub fn exec(subcommand: &str, passthrough: &[String], ids: &[String]) -> Result<ExitStatus> {
    let revset = ids.join("|");
    let status = Command::new("jj")
        .arg(subcommand)
        .args(passthrough)
        .arg("-r")
        .arg(&revset)
        .status()
        .context("failed to spawn jj")?;
    Ok(status)
}

pub fn command_line(subcommand: &str, passthrough: &[String], ids: &[String]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(4 + passthrough.len());
    parts.push("jj".into());
    parts.push(subcommand.into());
    for a in passthrough {
        parts.push(shell_quote(a));
    }
    if !ids.is_empty() {
        parts.push("-r".into());
        parts.push(shell_quote(&ids.join("|")));
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
