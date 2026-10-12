mod ai;
mod app;
mod brand;
mod buffer;
mod claude;
mod command;
mod config_change;
mod conflict;
mod cursor;
mod file_ops;
mod format;
mod git;
mod history;
mod input;
mod keymap;
mod lsp;
mod pty;
mod recovery;
mod search;
mod session;
mod settings;
mod snippets;
mod syntax;
mod syntax_tree;
mod terminal;
mod theme;
mod ui;
mod visual;
mod workspace;

use std::path::PathBuf;

use anyhow::Result;
use app::App;
use clap::Parser;
use terminal::TerminalSession;

#[derive(Debug, Parser)]
#[command(
    name = "mellow",
    version,
    about = brand::ABOUT,
    after_help = brand::AFTER_HELP
)]
struct Cli {
    /// Files to open, or one project directory. '-' reads an unnamed draft from stdin.
    paths: Vec<PathBuf>,

    /// Open path:line[:column] (one-based).
    #[arg(long, value_name = "PATH:LINE[:COLUMN]")]
    goto: Option<String>,

    /// Wait until editing ends (Mellow always runs in the foreground).
    #[arg(long)]
    wait: bool,

    /// Explicitly trust this workspace for language servers, formatters and tasks.
    #[arg(long)]
    trust: bool,

    /// Disable terminal mouse capture (useful when you prefer terminal-native selection).
    #[arg(long)]
    no_mouse: bool,
}

fn main() -> Result<()> {
    terminal::install_panic_hook();
    terminal::install_signal_handlers()?;
    exit_when_session_ends();
    let cli = Cli::parse();
    let mut paths = cli.paths;
    let goto = cli.goto.as_deref().map(parse_goto).transpose()?;
    if let Some((path, _, _)) = &goto {
        paths.insert(0, path.clone());
    }
    let cursor = goto.as_ref().map(|(_, row, col)| (*row, *col));
    let stdin = if paths.iter().any(|p| p.as_os_str() == "-") {
        use std::io::Read;
        let mut text = String::new();
        std::io::stdin()
            .take(100 * 1024 * 1024 + 1)
            .read_to_string(&mut text)?;
        anyhow::ensure!(
            text.len() <= 100 * 1024 * 1024,
            "stdin exceeds the 100 MiB open limit"
        );
        paths.retain(|p| p.as_os_str() != "-");
        restore_interactive_input()?;
        Some(text)
    } else {
        None
    };
    let first = if paths.is_empty() {
        None
    } else {
        Some(paths.remove(0))
    };
    let mut app = App::startup(first)?;
    for path in paths {
        anyhow::ensure!(!path.is_dir(), "additional paths must be files");
        app.open_path_in_tab(path)?;
    }
    app.configure_cli(cli.trust, None, stdin);
    if let Some((path, _, _)) = goto {
        app.open_path_in_tab(path)?;
        app.configure_cli(cli.trust, cursor, None);
    }
    let mut terminal = TerminalSession::new(!cli.no_mouse)?;
    app.run(terminal.terminal_mut())
}

/// Ends Mellow once its terminal session is over. An SSH disconnect or a closed
/// window hangs up the terminal: the launching shell usually exits, and if it
/// ignores hangups the terminal still stops being ours. Either way the event
/// library spins inside its own reads of the dead terminal, where the main loop
/// never gets to act, so this watches from a separate thread. Unsaved text is
/// already in the crash journal.
fn exit_when_session_ends() {
    use std::time::{Duration, Instant};
    let launcher = unsafe { libc::getppid() };
    let had_terminal = has_controlling_terminal();
    std::thread::spawn(move || {
        let mut requested_at: Option<Instant> = None;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if unsafe { libc::getppid() } != launcher
                || (had_terminal && !has_controlling_terminal())
            {
                std::process::exit(0);
            }
            // The main loop saves and stops within a frame of a signal; only a
            // dead terminal keeps it running past this grace period.
            if terminal::termination_requested()
                && requested_at.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(2)
            {
                std::process::exit(0);
            }
        }
    });
}

/// Whether this process still has a controlling terminal. A hangup removes it, and
/// then `/dev/tty` fails with ENXIO; any other failure is not taken as a hangup.
fn has_controlling_terminal() -> bool {
    let fd = unsafe {
        libc::open(
            c"/dev/tty".as_ptr(),
            libc::O_RDONLY | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if fd >= 0 {
        unsafe { libc::close(fd) };
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ENXIO)
}

/// Stdin supplied the draft. Terminal negotiation and subsequent key input must
/// use the controlling terminal, never the exhausted pipe.
fn restore_interactive_input() -> Result<()> {
    use std::{
        ffi::{CStr, OsStr},
        os::{
            fd::AsRawFd,
            unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        },
    };
    // Open the concrete output TTY device. On Darwin, kqueue cannot register
    // the /dev/tty alias, which otherwise makes Crossterm input initialization
    // fail and keyboard negotiation spin. Redirected output is not a renderer.
    let mut name = [0 as libc::c_char; 1024];
    if unsafe { libc::ttyname_r(libc::STDOUT_FILENO, name.as_mut_ptr(), name.len()) } != 0 {
        anyhow::bail!("stdin editing requires interactive terminal output");
    }
    let path = OsStr::from_bytes(unsafe { CStr::from_ptr(name.as_ptr()) }.to_bytes());
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| anyhow::anyhow!("cannot reopen interactive terminal input: {error}"))?;
    if unsafe { libc::dup2(tty.as_raw_fd(), libc::STDIN_FILENO) } == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn parse_goto(raw: &str) -> Result<(PathBuf, usize, usize)> {
    let (head, last) = raw
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("--goto needs path:line[:column]"))?;
    let last = last.parse::<usize>()?;
    let (path, row, col) = match head.rsplit_once(':') {
        Some((path, row)) if row.parse::<usize>().is_ok() => (path, row.parse::<usize>()?, last),
        _ => (head, last, 1),
    };
    anyhow::ensure!(
        !path.is_empty() && row > 0 && col > 0,
        "--goto positions are one-based"
    );
    Ok((PathBuf::from(path), row, col))
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    #[test]
    fn cli_supports_multiple_paths_goto_wait_stdin_and_explicit_trust() {
        let cli = Cli::try_parse_from([
            "mellow",
            "--wait",
            "--trust",
            "--goto",
            "a.py:12:3",
            "b.yaml",
            "-",
        ])
        .unwrap();
        assert!(cli.wait && cli.trust);
        assert_eq!(cli.paths.len(), 2);
        assert_eq!(
            parse_goto(cli.goto.as_deref().unwrap()).unwrap(),
            (PathBuf::from("a.py"), 12, 3)
        );
        assert!(parse_goto("a.py:0").is_err());
    }
}
