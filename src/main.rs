mod init;
mod jj;
mod tui;

use std::process::ExitCode;

use anyhow::{Context, Result};

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("jjf: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<u8> {
    let mut args = std::env::args().skip(1);
    let Some(first) = args.next() else {
        usage();
        return Ok(2);
    };

    match first.as_str() {
        "init" => run_init(args),
        "--emit" => {
            let rest: Vec<String> = args.collect();
            if rest.is_empty() {
                eprintln!("usage: jjf --emit <jj-subcommand> [args...]");
                return Ok(2);
            }
            run_pick(&rest, /* emit = */ true)
        }
        _ => {
            let mut all = Vec::with_capacity(1 + args.size_hint().0);
            all.push(first);
            all.extend(args);
            run_pick(&all, /* emit = */ false)
        }
    }
}

fn usage() {
    eprintln!("usage: jjf <jj-subcommand> [args...]");
    eprintln!("       jjf --emit <jj-subcommand> [args...]   (print resolved command, don't run)");
    eprintln!("       jjf init <bash|zsh|fish|nu>            (print shell wrapper)");
}

fn run_init(mut args: impl Iterator<Item = String>) -> Result<u8> {
    let Some(shell) = args.next() else {
        eprintln!("usage: jjf init <bash|zsh|fish|nu>");
        return Ok(2);
    };
    match init::snippet(&shell) {
        Some(s) => {
            print!("{s}");
            Ok(0)
        }
        None => {
            eprintln!("jjf: unknown shell '{shell}' — supported: bash, zsh, fish, nu");
            Ok(2)
        }
    }
}

fn run_pick(args: &[String], emit: bool) -> Result<u8> {
    let target = jj::resolve_target(args)?;

    // Case 1: user already pinned the revision — a revset flag, or a bare
    // positional for a positional-revset leaf like `jj show`. Bypass the
    // picker entirely and run their command verbatim. Avoids double-`-r` and
    // respects what they explicitly asked for.
    if target.user_supplied_revset {
        if emit {
            let cmd = jj::command_line(&target, &[]);
            println!("{cmd}");
            return Ok(0);
        }
        echo_command(&target, &[]);
        let status = jj::exec(&target, &[])?;
        return Ok(status.code().unwrap_or(1) as u8);
    }

    // Case 2: leaf accepts no revset flag at all (e.g. `jj operation`,
    // `jj workspace list`). Refuse to run the picker.
    if target.flag.is_none() {
        let leaf = target.leaf.join(" ");
        eprintln!(
            "jjf: 'jj {leaf}' takes no revset flag (-r/--to) — run 'jj {leaf}' directly."
        );
        return Ok(2);
    }

    let rows = jj::capture_log()?;
    if rows.is_empty() {
        eprintln!("jjf: no revisions in default revset");
        return Ok(0);
    }

    // In --emit mode, fish/bash/zsh/nu wrappers capture our stdout via $(...).
    // crossterm's cursor::position() writes its DSR query to stdout; if stdout is
    // a pipe, the query never reaches the terminal and ratatui times out. Redirect
    // fd 1 to /dev/tty for the picker phase, then restore it before printing.
    let saved_stdout = if emit {
        Some(redirect_stdout_to_tty()?)
    } else {
        None
    };

    let tui_result = tui::run(rows, &target);

    if let Some(saved) = saved_stdout {
        restore_stdout(saved)?;
    }

    let Some(ids) = tui_result? else {
        return Ok(130);
    };
    if ids.is_empty() {
        return Ok(130);
    }

    if emit {
        let cmd = jj::command_line(&target, &ids);
        println!("{cmd}");
        Ok(0)
    } else {
        echo_command(&target, &ids);
        let status = jj::exec(&target, &ids)?;
        Ok(status.code().unwrap_or(1) as u8)
    }
}

#[cfg(unix)]
fn redirect_stdout_to_tty() -> Result<libc::c_int> {
    use std::os::fd::AsRawFd;
    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if saved < 0 {
        return Err(std::io::Error::last_os_error()).context("dup stdout");
    }
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("open /dev/tty for stdout redirect")?;
    if unsafe { libc::dup2(tty.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
        unsafe { libc::close(saved) };
        return Err(std::io::Error::last_os_error()).context("dup2 tty -> stdout");
    }
    Ok(saved)
}

#[cfg(unix)]
fn restore_stdout(saved: libc::c_int) -> Result<()> {
    if unsafe { libc::dup2(saved, libc::STDOUT_FILENO) } < 0 {
        unsafe { libc::close(saved) };
        return Err(std::io::Error::last_os_error()).context("dup2 saved -> stdout");
    }
    unsafe { libc::close(saved) };
    Ok(())
}

fn echo_command(target: &jj::RevsetTarget, ids: &[String]) {
    use std::io::IsTerminal;
    let cmd = jj::command_line(target, ids);
    if std::io::stdout().is_terminal() {
        println!("\x1b[2m$\x1b[0m {cmd}");
    } else {
        println!("{cmd}");
    }
}
