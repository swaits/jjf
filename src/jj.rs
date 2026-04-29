use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result};

const SEP: u8 = 0x1f;

const TEMPLATE: &str = concat!(
    "\"\\x1f\" ++ ",
    "change_id.short() ++ \"\\x1f\" ++ ",
    "change_id.shortest().prefix() ++ \"\\x1f\" ++ ",
    "commit_id.short() ++ \"\\x1f\" ++ ",
    "commit_id.shortest().prefix() ++ \"\\x1f\" ++ ",
    "builtin_log_oneline",
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

/// Run `jj show --summary` for a single revision and return the body
/// (description + diff summary) with the metadata header stripped.
pub fn show_summary(change_id: &str) -> Vec<u8> {
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
        Ok(o) if o.status.success() => clip_description(&strip_show_header(&o.stdout), 3),
        _ => b"(preview unavailable)".to_vec(),
    }
}

/// `jj show` output starts with `Commit ID:` / `Change ID:` / `Author:` /
/// `Committer:` lines, then a blank line, then the description, then file
/// list. Skip everything up to and including the first blank line.
fn strip_show_header(bytes: &[u8]) -> Vec<u8> {
    if let Some(pos) = bytes.windows(2).position(|w| w == b"\n\n") {
        bytes[pos + 2..].to_vec()
    } else {
        bytes.to_vec()
    }
}

/// Truncate the (4-space-indented) description block to `max_desc` lines,
/// appending a dim ellipsis to the last kept line if anything was clipped.
/// Lines after the description (blank separator + `M`/`A`/`D` file list)
/// pass through unchanged so the file summary remains visible.
fn clip_description(bytes: &[u8], max_desc: usize) -> Vec<u8> {
    let lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    // Description lines are the leading lines that start with 4 spaces.
    let mut desc_total = 0;
    for line in &lines {
        if line.starts_with(b"    ") {
            desc_total += 1;
        } else {
            break;
        }
    }

    let kept = desc_total.min(max_desc);
    let mut out = Vec::with_capacity(bytes.len());
    let mut first = true;
    for line in lines.iter().take(kept) {
        if !first {
            out.push(b'\n');
        }
        out.extend_from_slice(line);
        first = false;
    }
    if desc_total > max_desc {
        out.extend_from_slice(b" \x1b[2m\xe2\x80\xa6\x1b[0m");
    }
    for line in lines.iter().skip(desc_total) {
        if !first {
            out.push(b'\n');
        }
        out.extend_from_slice(line);
        first = false;
    }
    out
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
