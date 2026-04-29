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
        "--emit" => match args.next() {
            Some(sub) => {
                let passthrough: Vec<String> = args.collect();
                run_pick(&sub, &passthrough, /* emit = */ true)
            }
            None => {
                eprintln!("usage: jjf --emit <jj-subcommand> [args...]");
                Ok(2)
            }
        },
        sub => {
            let passthrough: Vec<String> = args.collect();
            run_pick(sub, &passthrough, /* emit = */ false)
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

fn run_pick(subcommand: &str, passthrough: &[String], emit: bool) -> Result<u8> {
    if !jj::supports_revisions(subcommand)? {
        eprintln!(
            "jjf: 'jj {subcommand}' doesn't take revisions — run 'jj {subcommand}' directly."
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

    let tui_result = tui::run(rows, subcommand, passthrough);

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
        let cmd = jj::command_line(subcommand, passthrough, &ids);
        println!("{cmd}");
        Ok(0)
    } else {
        echo_command(subcommand, passthrough, &ids);
        let status = jj::exec(subcommand, passthrough, &ids)?;
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

fn echo_command(subcommand: &str, passthrough: &[String], ids: &[String]) {
    use std::io::IsTerminal;
    let cmd = jj::command_line(subcommand, passthrough, ids);
    if std::io::stdout().is_terminal() {
        println!("\x1b[2m$\x1b[0m {cmd}");
    } else {
        println!("{cmd}");
    }
}
