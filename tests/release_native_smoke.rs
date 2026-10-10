#![cfg(unix)]

use std::{
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    ptr,
    time::{Duration, Instant},
};

use tempfile::tempdir;

struct ChildGuard {
    pid: libc::pid_t,
    reaped: bool,
    exit_status: Option<i32>,
    initial_lflag: libc::tcflag_t,
}

impl ChildGuard {
    fn new(pid: libc::pid_t, initial_lflag: libc::tcflag_t) -> Self {
        Self {
            pid,
            reaped: false,
            exit_status: None,
            initial_lflag,
        }
    }

    fn try_reap(&mut self) -> bool {
        if self.reaped {
            return true;
        }
        let mut status = 0;
        let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if result == self.pid {
            self.reaped = true;
            self.exit_status = Some(status);
            true
        } else {
            false
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        unsafe {
            let _ = libc::kill(-self.pid, libc::SIGKILL);
            let _ = libc::kill(self.pid, libc::SIGKILL);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if self.try_reap() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn write_all(master: &mut File, bytes: &[u8]) {
    master.write_all(bytes).expect("failed to write PTY input");
    master.flush().expect("failed to flush PTY input");
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// Marker appended to the captured stream each time a device-attributes query
/// is answered; it is not a valid terminal sequence, so the screen ignores it.
const ATTRIBUTES_ANSWERED: &[u8] = b"\x1b]9999;mellow-smoke-da\x07";

fn answered_attributes(captured: &[u8]) -> usize {
    count_occurrences(captured, ATTRIBUTES_ANSWERED)
}

fn pump(
    master: &mut File,
    captured: &mut Vec<u8>,
    answered_queries: &mut usize,
    duration: Duration,
) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let mut poll_fd = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll_fd, 1, 25) };
        if ready <= 0 || poll_fd.revents & libc::POLLIN == 0 {
            continue;
        }

        let mut buffer = [0u8; 8192];
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => captured.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => panic!("failed to read PTY output: {error}"),
        }

        let total_queries = count_occurrences(captured, b"\x1b[6n");
        while *answered_queries < total_queries {
            write_all(master, b"\x1b[1;1R");
            *answered_queries += 1;
        }
        // Answer primary device attributes like a plain VT220-class terminal
        // without the kitty keyboard protocol, so startup detection is fast and
        // the legacy keyboard path is what this smoke test exercises.
        let attribute_queries = count_occurrences(captured, b"\x1b[c");
        while answered_attributes(captured) < attribute_queries {
            write_all(master, b"\x1b[?62c");
            captured.extend_from_slice(ATTRIBUTES_ANSWERED);
        }
    }
}

fn wait_for_screen_state(
    master: &mut File,
    captured: &mut Vec<u8>,
    queries: &mut usize,
    expected: &str,
    ready: impl Fn(&str) -> bool,
) {
    // Match the existing release-acceptance startup budget, but require the
    // actual rendered state before sending keys rather than sleeping blindly.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        pump(master, captured, queries, Duration::from_millis(100));
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(captured);
        if ready(&parser.screen().contents()) {
            return;
        }
    }
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(captured);
    panic!(
        "Expected rendered state {expected:?}; fixture screen: {:?}",
        parser.screen().contents()
    );
}

fn wait_for_startup(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        pump(master, captured, queries, Duration::from_millis(100));
        if count_occurrences(captured, b"\x1b[?1049h") > 0 && *queries > 0 {
            return;
        }
    }
    panic!("Native binary did not enter alternate screen and query cursor within 5 seconds");
}

fn wait_for_saved_text(
    master: &mut File,
    captured: &mut Vec<u8>,
    queries: &mut usize,
    path: &Path,
    expected: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        pump(master, captured, queries, Duration::from_millis(100));
        if fs::read_to_string(path).is_ok_and(|text| text == expected) {
            return;
        }
    }
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        expected,
        "Save did not reach the expected bytes within 3 seconds"
    );
}

fn spawn_mellow(binary: &Path, cwd: &Path, file: Option<&Path>, home: &Path) -> (ChildGuard, File) {
    spawn_mellow_with(binary, cwd, file, home, (80, 24), false)
}

/// Starts the binary at `size` (columns, rows). `mouse` leaves mouse reporting
/// on, as a user's terminal would have it.
fn spawn_mellow_with(
    binary: &Path,
    cwd: &Path,
    file: Option<&Path>,
    home: &Path,
    size: (u16, u16),
    mouse: bool,
) -> (ChildGuard, File) {
    let binary_c = CString::new(binary.as_os_str().as_bytes()).unwrap();
    let cwd_c = CString::new(cwd.as_os_str().as_bytes()).unwrap();
    let file_c = file.map(|file| CString::new(file.as_os_str().as_bytes()).unwrap());
    let no_mouse_c = CString::new("--no-mouse").unwrap();
    // Built before fork: the child must not allocate.
    let mut argv = vec![binary_c.as_ptr()];
    argv.extend(file_c.as_ref().map(|file| file.as_ptr()));
    if !mouse {
        argv.push(no_mouse_c.as_ptr());
    }
    argv.push(ptr::null());

    let home_c = CString::new(home.as_os_str().as_bytes()).unwrap();
    let config = home.join("config");
    let state = home.join("state");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&state).unwrap();
    let config_c = CString::new(config.as_os_str().as_bytes()).unwrap();
    let state_c = CString::new(state.as_os_str().as_bytes()).unwrap();

    let home_key = CString::new("HOME").unwrap();
    let config_key = CString::new("XDG_CONFIG_HOME").unwrap();
    let state_key = CString::new("XDG_STATE_HOME").unwrap();
    let term_key = CString::new("TERM").unwrap();
    let term_value = CString::new("xterm-256color").unwrap();

    let mut master_fd = -1;
    let mut slave_fd = -1;

    #[cfg(target_os = "macos")]
    let opened = unsafe {
        let mut size = libc::winsize {
            ws_row: size.1,
            ws_col: size.0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        )
    };

    #[cfg(not(target_os = "macos"))]
    let opened = unsafe {
        let size = libc::winsize {
            ws_row: size.1,
            ws_col: size.0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            ptr::null_mut(),
            ptr::null(),
            &size,
        )
    };

    assert_eq!(
        opened,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );

    let mut initial_termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave_fd, initial_termios.as_mut_ptr()) },
        0
    );
    let initial_lflag = unsafe { initial_termios.assume_init() }.c_lflag;
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());

    if pid == 0 {
        unsafe {
            libc::close(master_fd);
            if libc::setsid() < 0 {
                libc::_exit(126);
            }
            let _ = libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0);
            if libc::dup2(slave_fd, libc::STDIN_FILENO) < 0
                || libc::dup2(slave_fd, libc::STDOUT_FILENO) < 0
                || libc::dup2(slave_fd, libc::STDERR_FILENO) < 0
            {
                libc::_exit(126);
            }
            if slave_fd > libc::STDERR_FILENO {
                libc::close(slave_fd);
            }
            if libc::chdir(cwd_c.as_ptr()) != 0 {
                libc::_exit(126);
            }

            libc::setenv(home_key.as_ptr(), home_c.as_ptr(), 1);
            libc::setenv(config_key.as_ptr(), config_c.as_ptr(), 1);
            libc::setenv(state_key.as_ptr(), state_c.as_ptr(), 1);
            libc::setenv(term_key.as_ptr(), term_value.as_ptr(), 1);

            libc::execv(binary_c.as_ptr(), argv.as_ptr());
            libc::_exit(127);
        }
    }

    unsafe {
        libc::close(slave_fd);
    }

    let master = unsafe { File::from_raw_fd(master_fd) };
    (ChildGuard::new(pid, initial_lflag), master)
}

#[test]
fn release_binary_launch_edit_save_quit_restores_terminal() {
    // Acceptance also exercises the executable extracted from the archived package.
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let binary = binary_path.as_path();
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("smoke.rs");
    fs::write(&file, "fn main() {\n    println!(\"before\");\n}\n").unwrap();

    let (mut child, mut master) = spawn_mellow(binary, workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut answered_queries = 0usize;

    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut answered_queries,
        "Welcome to Mellow",
        |screen| screen.contains("Welcome to Mellow"),
    );

    let exited_before_screen = child.try_reap();
    assert!(
        captured
            .windows(b"\x1b[?1049h".len())
            .any(|w| w == b"\x1b[?1049h"),
        "Mellow never entered alternate screen. exited={exited_before_screen}, status={:?}, captured={:?}",
        child.exit_status,
        String::from_utf8_lossy(&captured)
    );
    assert!(
        answered_queries > 0,
        "terminal cursor-position query was never observed"
    );

    // Observe onboarding dismissal before typing; a delayed Escape must not
    // cause the following edit to be consumed by the welcome overlay.
    write_all(&mut master, b"\x1b");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut answered_queries,
        "modeless fixture editor",
        |screen| {
            screen.contains("fn main() {")
                && screen.contains("smoke.rs")
                && screen.contains("Ln 1, Col 1")
                && !screen.contains("Welcome to Mellow")
        },
    );

    write_all(&mut master, b"// RC native smoke\r");
    pump(
        &mut master,
        &mut captured,
        &mut answered_queries,
        Duration::from_millis(150),
    );

    // Ctrl+S must persist the edit without requiring any modal command.
    write_all(&mut master, &[0x13]);
    let save_deadline = Instant::now() + Duration::from_secs(3);
    let expected_prefix = "// RC native smoke\nfn main()";
    let mut saved = false;
    while Instant::now() < save_deadline {
        pump(
            &mut master,
            &mut captured,
            &mut answered_queries,
            Duration::from_millis(100),
        );
        if fs::read_to_string(&file).is_ok_and(|contents| contents.starts_with(expected_prefix)) {
            saved = true;
            break;
        }
    }
    assert!(
        saved,
        "Ctrl+S did not persist fixture edit; disk: {:?}",
        fs::read_to_string(&file)
    );

    // Exercise selection, Unicode bracketed paste, dirty-quit protection and
    // undo/redo through the real terminal input loop, then verify disk bytes.
    let previous = fs::read_to_string(&file).unwrap();
    let replacement = "నమస్తే 日本語 👩‍💻 e\u{301}\nsecond line\n";
    write_all(&mut master, &[0x01]); // Ctrl+A
    write_all(
        &mut master,
        format!("\x1b[200~{replacement}\x1b[201~").as_bytes(),
    );
    pump(
        &mut master,
        &mut captured,
        &mut answered_queries,
        Duration::from_millis(200),
    );
    write_all(&mut master, &[0x11]); // A dirty buffer must not quit.
    pump(
        &mut master,
        &mut captured,
        &mut answered_queries,
        Duration::from_millis(150),
    );
    assert!(!child.try_reap(), "Dirty Ctrl+Q silently exited");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        previous,
        "Dirty editing silently saved"
    );
    write_all(&mut master, b"\x1b"); // Cancel quit.
    pump(
        &mut master,
        &mut captured,
        &mut answered_queries,
        Duration::from_millis(150),
    );
    write_all(&mut master, &[0x1a, 0x13]); // Undo and save.
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut answered_queries,
        &file,
        &previous,
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        previous,
        "Undo did not restore the selection replacement"
    );
    write_all(&mut master, &[0x19, 0x13]); // Redo and save.
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut answered_queries,
        &file,
        replacement,
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        replacement,
        "Unicode paste/redo did not preserve bytes"
    );

    // Ctrl+Q must exit cleanly once the buffer is saved.
    write_all(&mut master, &[0x11]);
    let exit_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < exit_deadline && !child.try_reap() {
        pump(
            &mut master,
            &mut captured,
            &mut answered_queries,
            Duration::from_millis(100),
        );
    }
    assert!(child.reaped, "Ctrl+Q did not exit the native RC binary");
    let status = child.exit_status.unwrap();
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "Native binary failed on exit: {status}"
    );

    pump(
        &mut master,
        &mut captured,
        &mut answered_queries,
        Duration::from_millis(100),
    );
    assert!(
        captured
            .windows(b"\x1b[?1049l".len())
            .any(|w| w == b"\x1b[?1049l"),
        "Mellow did not restore the terminal alternate-screen state"
    );
    let mut restored = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(master.as_raw_fd(), restored.as_mut_ptr()) },
        0
    );
    let restored = unsafe { restored.assume_init() };
    let raw_flags = libc::ICANON | libc::ECHO | libc::ISIG;
    assert_eq!(
        restored.c_lflag & raw_flags,
        child.initial_lflag & raw_flags,
        "Terminal raw mode was not restored"
    );
}

#[test]
fn release_binary_recovers_unsaved_unicode_after_crash() {
    let binary = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("recovery.txt");
    fs::write(&file, "original\n").unwrap();
    let (mut child, mut master) = spawn_mellow(&binary, workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    wait_for_startup(&mut master, &mut captured, &mut queries);
    write_all(&mut master, b"\x1b");
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(150),
    );
    write_all(&mut master, "\x1b[200~unsaved 日本語\n\x1b[201~".as_bytes());
    let journal_dir = home.path().join("state/mellow/recovery");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut journaled = false;
    while Instant::now() < deadline {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(100),
        );
        journaled = journal_contains(&journal_dir, "unsaved 日本語");
        if journaled {
            break;
        }
    }
    assert!(journaled, "Unsaved edit was not journaled before the crash");
    assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");
    assert_eq!(unsafe { libc::kill(child.pid, libc::SIGKILL) }, 0);
    // Keep reading the pty: on macOS a killed process cannot finish exiting
    // while its unread terminal output is still queued.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !child.try_reap() {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(50),
        );
    }
    assert!(child.reaped);
    let (mut recovered, mut master) =
        spawn_mellow(&binary, workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    wait_for_startup(&mut master, &mut captured, &mut queries);
    write_all(&mut master, b"r");
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(150),
    );
    write_all(&mut master, &[0x13]);
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &file,
        "unsaved 日本語\noriginal\n",
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "unsaved 日本語\noriginal\n"
    );
    write_all(&mut master, &[0x11]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !recovered.try_reap() {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(100),
        );
    }
    assert!(
        recovered.reaped,
        "Recovered buffer could not quit after save"
    );
    let status = recovered.exit_status.unwrap();
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
}

/// Only finished journals count: Mellow writes `.journal-*.tmp`, syncs it and
/// then renames it, so a crash before the rename rightly recovers nothing.
fn journal_contains(journal_dir: &Path, text: &str) -> bool {
    fs::read_dir(journal_dir).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
            .any(|entry| fs::read_to_string(entry.path()).is_ok_and(|body| body.contains(text)))
    })
}

fn wait_for_journal(
    master: &mut File,
    captured: &mut Vec<u8>,
    queries: &mut usize,
    journal_dir: &Path,
    text: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        pump(master, captured, queries, Duration::from_millis(100));
        if journal_contains(journal_dir, text) {
            return;
        }
    }
    panic!("{text} was not journaled");
}

/// Audit F1: two unnamed drafts, a crash, and both come back.
#[test]
fn release_binary_recovers_every_unnamed_draft_after_crash() {
    let binary = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let journal_dir = home.path().join("state/mellow/recovery");
    let (mut child, mut master) = spawn_mellow(&binary, workspace.path(), None, home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    let settle = |master: &mut File, captured: &mut Vec<u8>, queries: &mut usize| {
        pump(master, captured, queries, Duration::from_millis(200));
    };
    wait_for_startup(&mut master, &mut captured, &mut queries);
    write_all(&mut master, b"\x1b");
    settle(&mut master, &mut captured, &mut queries);

    write_all(&mut master, b"\x1b[200~FIRST_UNSAVED_DRAFT\x1b[201~");
    wait_for_journal(
        &mut master,
        &mut captured,
        &mut queries,
        &journal_dir,
        "FIRST_UNSAVED_DRAFT",
    );
    write_all(&mut master, &[0x0e]); // Ctrl+N: a clean second Untitled tab
    settle(&mut master, &mut captured, &mut queries);
    assert!(
        journal_contains(&journal_dir, "FIRST_UNSAVED_DRAFT"),
        "a clean new tab erased the first draft's journal"
    );
    write_all(&mut master, b"\x1b[200~SECOND_UNSAVED_DRAFT\x1b[201~");
    wait_for_journal(
        &mut master,
        &mut captured,
        &mut queries,
        &journal_dir,
        "SECOND_UNSAVED_DRAFT",
    );
    assert!(journal_contains(&journal_dir, "FIRST_UNSAVED_DRAFT"));

    assert_eq!(unsafe { libc::kill(child.pid, libc::SIGKILL) }, 0);
    // Keep reading the pty: on macOS a killed process cannot finish exiting
    // while its unread terminal output is still queued.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !child.try_reap() {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(50),
        );
    }
    assert!(child.reaped);

    let (mut recovered, mut master) = spawn_mellow(&binary, workspace.path(), None, home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    wait_for_startup(&mut master, &mut captured, &mut queries);
    for (index, name) in ["untitled.txt", "untitled-2.txt"].into_iter().enumerate() {
        if index > 0 {
            write_all(&mut master, b"\x1b[6;5~"); // Ctrl+PageDown: next tab
            settle(&mut master, &mut captured, &mut queries);
        }
        write_all(&mut master, b"r");
        settle(&mut master, &mut captured, &mut queries);
        write_all(&mut master, &[0x13]); // Ctrl+S opens Save As with a suggested name
        settle(&mut master, &mut captured, &mut queries);
        write_all(&mut master, b"\r");
        let target = workspace.path().join(name);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !target.exists() {
            pump(
                &mut master,
                &mut captured,
                &mut queries,
                Duration::from_millis(100),
            );
        }
    }

    let mut saved: Vec<String> = ["untitled.txt", "untitled-2.txt"]
        .iter()
        .map(|name| fs::read_to_string(workspace.path().join(name)).unwrap_or_default())
        .collect();
    saved.sort();
    assert_eq!(saved, ["FIRST_UNSAVED_DRAFT", "SECOND_UNSAVED_DRAFT"]);

    write_all(&mut master, &[0x11]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !recovered.try_reap() {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(100),
        );
    }
    assert!(recovered.reaped, "could not quit after saving both drafts");
}

// ---------------------------------------------------------------------------
// Live scenarios: what a user does in a terminal, checked against the real
// binary. Each reads the rendered screen (vt100), not the raw bytes.
// ---------------------------------------------------------------------------

/// The rendered text of the screen, one string per row.
fn rendered_rows(captured: &[u8], size: (u16, u16)) -> Vec<String> {
    let mut parser = vt100::Parser::new(size.1, size.0, 0);
    parser.process(captured);
    parser
        .screen()
        .contents()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Pumps the PTY until `ready` accepts the rendered screen, or panics with
/// the screen as it was.
fn wait_for_rows(
    master: &mut File,
    captured: &mut Vec<u8>,
    queries: &mut usize,
    size: (u16, u16),
    what: &str,
    ready: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pump(master, captured, queries, Duration::from_millis(100));
        let rows = rendered_rows(captured, size);
        if ready(&rows) {
            return rows;
        }
        if Instant::now() > deadline {
            panic!("Expected {what}; the screen was:\n{}", rows.join("\n"));
        }
    }
}

/// The (column, row) of the first occurrence of `needle` on the screen.
fn find_on_screen(rows: &[String], needle: &str) -> Option<(u16, u16)> {
    rows.iter().enumerate().find_map(|(row, line)| {
        line.find(needle)
            .map(|byte| (line[..byte].chars().count() as u16, row as u16))
    })
}

/// Sends one key or string and lets the screen settle.
fn type_keys(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize, keys: &[u8]) {
    write_all(master, keys);
    pump(master, captured, queries, Duration::from_millis(300));
}

/// A mouse press and release at a 0-based screen cell (SGR encoding).
fn click_cell(
    master: &mut File,
    captured: &mut Vec<u8>,
    queries: &mut usize,
    column: u16,
    row: u16,
) {
    write_all(
        master,
        format!("\x1b[<0;{};{}M", column + 1, row + 1).as_bytes(),
    );
    pump(master, captured, queries, Duration::from_millis(100));
    write_all(
        master,
        format!("\x1b[<0;{};{}m", column + 1, row + 1).as_bytes(),
    );
    pump(master, captured, queries, Duration::from_millis(300));
}

#[test]
fn live_first_key_on_the_welcome_screen_reaches_the_file() {
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("notes.txt");
    fs::write(&file, "").unwrap();
    let size = (80, 24);
    let (_child, mut master) = spawn_mellow_with(
        &binary_path,
        workspace.path(),
        Some(&file),
        home.path(),
        size,
        false,
    );
    let mut captured = Vec::new();
    let mut queries = 0usize;
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the welcome screen",
        |rows| rows.iter().any(|row| row.contains("Welcome to Mellow")),
    );
    type_keys(&mut master, &mut captured, &mut queries, b"Z");
    let rows = wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "Z on line 1",
        |rows| {
            rows.iter()
                .any(|row| row.starts_with("1 │") && row.contains('Z'))
        },
    );
    assert!(
        !rows.iter().any(|row| row.contains("Welcome to Mellow")),
        "the first key should dismiss the welcome: {rows:?}"
    );
}

#[test]
fn live_palette_and_open_file_open_and_close_with_escape() {
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("notes.txt");
    fs::write(&file, "hello\n").unwrap();
    let size = (80, 24);
    let (_child, mut master) = spawn_mellow_with(
        &binary_path,
        workspace.path(),
        Some(&file),
        home.path(),
        size,
        false,
    );
    let mut captured = Vec::new();
    let mut queries = 0usize;
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the editor",
        |rows| rows.iter().any(|row| row.contains("hello")),
    );

    type_keys(&mut master, &mut captured, &mut queries, b"\x10"); // Ctrl+P
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the command palette",
        |rows| rows.iter().any(|row| row.contains("● Commands")),
    );
    type_keys(&mut master, &mut captured, &mut queries, b"\x1b"); // Esc
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the palette closed",
        |rows| !rows.iter().any(|row| row.contains("● Commands")),
    );

    type_keys(&mut master, &mut captured, &mut queries, b"\x0f"); // Ctrl+O
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the Open file list",
        |rows| rows.iter().any(|row| row.contains("● Open file")),
    );
    type_keys(&mut master, &mut captured, &mut queries, b"\x1b");
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "Open file closed",
        |rows| !rows.iter().any(|row| row.contains("● Open file")),
    );
}

#[test]
fn live_quit_with_unsaved_text_asks_and_discard_exits() {
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("notes.txt");
    fs::write(&file, "").unwrap();
    let size = (80, 24);
    let (mut child, mut master) = spawn_mellow_with(
        &binary_path,
        workspace.path(),
        Some(&file),
        home.path(),
        size,
        false,
    );
    let mut captured = Vec::new();
    let mut queries = 0usize;
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the editor",
        |rows| rows.iter().any(|row| row.contains("Welcome to Mellow")),
    );
    type_keys(&mut master, &mut captured, &mut queries, b"Z");
    type_keys(&mut master, &mut captured, &mut queries, b"\x11"); // Ctrl+Q
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the quit question",
        |rows| {
            rows.iter()
                .any(|row| row.contains("Save changes before quitting?"))
        },
    );
    type_keys(&mut master, &mut captured, &mut queries, b"d"); // discard
    let deadline = Instant::now() + Duration::from_secs(3);
    while !child.try_reap() {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(50),
        );
        assert!(
            Instant::now() < deadline,
            "Mellow did not exit after discarding"
        );
    }
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "",
        "discard must not save"
    );
}

#[test]
fn live_mouse_click_runs_a_palette_row_at_120_by_34() {
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("notes.txt");
    fs::write(&file, "hello\n").unwrap();
    let size = (120, 34);
    let (_child, mut master) = spawn_mellow_with(
        &binary_path,
        workspace.path(),
        Some(&file),
        home.path(),
        size,
        true,
    );
    let mut captured = Vec::new();
    let mut queries = 0usize;
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the editor",
        |rows| rows.iter().any(|row| row.contains("hello")),
    );
    type_keys(&mut master, &mut captured, &mut queries, b"\x10"); // Ctrl+P
    let rows = wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the palette",
        |rows| find_on_screen(rows, "New file").is_some(),
    );
    let (column, row) = find_on_screen(&rows, "New file").unwrap();
    click_cell(&mut master, &mut captured, &mut queries, column + 2, row);
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "a new Untitled tab",
        |rows| {
            rows.iter().any(|row| row.contains("Untitled"))
                && !rows.iter().any(|row| row.contains("● Commands"))
        },
    );
}

#[test]
fn live_legacy_terminal_sends_ctrl_shift_o_as_ctrl_o() {
    // Terminals without the kitty keyboard protocol cannot tell Ctrl+Shift+O
    // from Ctrl+O, so Go to symbol is reached from the palette there. This
    // pins that behaviour so a change to it is deliberate.
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("demo.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    let size = (80, 24);
    let (_child, mut master) = spawn_mellow_with(
        &binary_path,
        workspace.path(),
        Some(&file),
        home.path(),
        size,
        false,
    );
    let mut captured = Vec::new();
    let mut queries = 0usize;
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the editor",
        |rows| rows.iter().any(|row| row.contains("fn main")),
    );
    type_keys(&mut master, &mut captured, &mut queries, b"\x0f"); // Ctrl+Shift+O, legacy bytes
    wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "Open file (not symbols)",
        |rows| rows.iter().any(|row| row.contains("● Open file")),
    );
}
