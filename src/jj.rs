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
