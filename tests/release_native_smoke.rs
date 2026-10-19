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

/// Held while a PTY is opened and its child forked; see `spawn_mellow_with_env`.
static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    panic!(
        "Native binary did not enter alternate screen and query cursor within 5 seconds; output={:?}",
        String::from_utf8_lossy(captured)
    );
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
    spawn_mellow_with_env(binary, cwd, file, home, size, mouse, &[])
}

fn spawn_mellow_with_env(
    binary: &Path,
    cwd: &Path,
    file: Option<&Path>,
    home: &Path,
    size: (u16, u16),
    mouse: bool,
    overrides: &[(&str, &str)],
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

    let config = home.join("config");
    let state = home.join("state");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&state).unwrap();
    // Construct a private child environment before fork. setenv in the child
    // can allocate or acquire a lock inherited from another test thread.
    // Formatter overrides belong to this child, never process-global tests.
    let mut child_env: std::collections::BTreeMap<_, _> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("MELLOW_"))
        .collect();
    for (key, value) in [
        ("HOME", home.as_os_str()),
        ("XDG_CONFIG_HOME", config.as_os_str()),
        ("XDG_STATE_HOME", state.as_os_str()),
        ("SHELL", std::ffi::OsStr::new("/bin/sh")),
        ("TERM", std::ffi::OsStr::new("xterm-256color")),
    ] {
        child_env.insert(key.into(), value.to_os_string());
    }
    for &(key, value) in overrides {
        child_env.insert(key.into(), value.into());
    }
    let environment: Vec<CString> = child_env
        .into_iter()
        .map(|(key, value)| {
            let mut entry = key.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).unwrap()
        })
        .collect();
    let mut envp: Vec<_> = environment.iter().map(|value| value.as_ptr()).collect();
    envp.push(ptr::null());

    // Tests fork on parallel threads. Without close-on-exec, a child started for
    // one test keeps another test's PTY master open, so closing that master never
    // hangs up its terminal. Mark both ends before any other spawn can fork.
    let _spawning = SPAWN_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    for fd in [master_fd, slave_fd] {
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }

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

            libc::execve(binary_c.as_ptr(), argv.as_ptr(), envp.as_ptr());
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
fn live_start_page_replaces_the_first_run_guide() {
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let size = (100, 32);
    let (_child, mut master) = spawn_mellow_with(
        &binary_path,
        workspace.path(),
        None,
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
        "the start page",
        |rows| {
            rows.iter().any(|row| row.contains("START"))
                && rows.iter().any(|row| row.contains("LEARN"))
        },
    );
    // A first run in a fresh home would also qualify for the guide; it must not
    // be drawn over the start page, which already shows the first keys.
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(600),
    );
    let rows = rendered_rows(&captured, size);
    assert!(
        !rows.iter().any(|row| row.contains("no modes to learn")),
        "first-run guide covers the start page:\n{}",
        rows.join("\n")
    );
    assert!(
        rows.iter().any(|row| row.contains('╭')),
        "start page card is missing:\n{}",
        rows.join("\n")
    );
}

/// Waits for `pid` to disappear; kills it and returns false if it outlives the deadline.
fn process_exits_within(pid: libc::pid_t, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(pid, libc::SIGKILL) };
    false
}

#[test]
fn release_binary_exits_after_its_terminal_hangs_up() {
    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("hangup.txt");
    fs::write(&file, "keep\n").unwrap();

    // Mellow leads the terminal's session, as when a terminal window runs it
    // directly. Closing the PTY is that window closing.
    let (mut child, mut master) = spawn_mellow(
        binary_path.as_path(),
        workspace.path(),
        Some(&file),
        home.path(),
    );
    let mut captured = Vec::new();
    let mut queries = 0usize;
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "Ln 1, Col",
        |screen| screen.contains("Ln 1, Col"),
    );
    drop(master);

    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline && !child.try_reap() {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !child.try_reap() {
        unsafe { libc::kill(child.pid, libc::SIGKILL) };
    }
    assert!(
        child.reaped,
        "Mellow kept running after its terminal hung up"
    );
}

#[test]
fn release_binary_exits_when_its_launching_shell_ends() {
    use std::os::unix::fs::PermissionsExt;

    let binary_path = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("shell.txt");
    fs::write(&file, "keep\n").unwrap();

    // A shell leads the session and runs Mellow in the foreground, as an SSH login
    // does. The inner shell records Mellow's pid, then becomes Mellow.
    let pid_file = workspace.path().join("mellow.pid");
    let launcher = workspace.path().join("launcher.sh");
    fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nsh -c 'echo $$ > \"$0\"; exec \"$@\"' \"{}\" \"{}\" \"$@\"\nexit $?\n",
            pid_file.display(),
            binary_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();

    let (_child, mut master) = spawn_mellow(&launcher, workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0usize;
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "Ln 1, Col",
        |screen| screen.contains("Ln 1, Col"),
    );
    let pid: libc::pid_t = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Dropping the connection hangs up the terminal and takes the shell with it.
    drop(master);
    assert!(
        process_exits_within(pid, Duration::from_secs(6)),
        "Mellow kept running after its launching shell ended"
    );
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
    type_keys(&mut master, &mut captured, &mut queries, b"New file");
    let rows = wait_for_rows(
        &mut master,
        &mut captured,
        &mut queries,
        size,
        "the palette",
        |rows| find_on_screen(rows, "New file").is_some(),
    );
    // The query line also says New file. Click the actual command row.
    let (row, line) = rows
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains("New file") && !line.contains("> New file"))
        .unwrap();
    let byte = line.find("New file").unwrap();
    let column = line[..byte].chars().count() as u16;
    let row = row as u16;
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

/// Configure only a local mock and a dummy key. No provider credentials from
/// the developer machine are needed for this extracted-binary regression.
fn mock_ai_config(home: &Path, provider: &str, endpoint: &str) {
    let directory = home.join("config/mellow");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("ai.conf"), format!(
        "provider = {provider}\nendpoint = {endpoint}\nmodel = audit-mock\napi_key = synthetic-audit-key\ninline_suggestions = false\n"
    )).unwrap();
}

fn accept_mock_request(listener: &std::net::TcpListener) -> std::net::TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // BSD/macOS accepts can inherit O_NONBLOCK from the listener.
                // A bounded blocking fixture read must wait for request bytes,
                // rather than race the client's write immediately after accept.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "mock provider received no request within its deadline"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("mock listener failed: {error}"),
        }
    }
}

fn read_mock_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        assert!(
            request.len() < 128 * 1024,
            "mock request exceeds fixture bound"
        );
        let read = stream.read(&mut chunk).unwrap_or_else(|error| {
            let header_end = request.windows(4).position(|part| part == b"\r\n\r\n");
            panic!(
                "mock read failed after {} bytes, header_end={header_end:?}: {error}",
                request.len()
            )
        });
        assert!(read > 0, "mock request ended before its body");
        request.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .expect("fixture requires bounded Content-Length");
            if request.len() >= header_end + 4 + length {
                return request;
            }
        }
    }
}

fn begin_selected_ai_request(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize) {
    write_all(master, &[0x01, 0x0b]); // Select All, Ask AI.
    wait_for_screen_state(
        master,
        captured,
        queries,
        "AI context disclosure before submission",
        |screen| {
            screen.contains("Sends")
                && screen.contains("file name and language")
                && screen.contains("Enter send")
        },
    );
}

#[test]
fn release_binary_ai_proposal_disclosure_reject_accept_and_grouped_undo_redo() {
    use std::net::TcpListener;
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("proposal.txt");
    let original = "before_name\nbefore_value\n";
    let replacement = "after_name 🙂\nafter_value e\u{301}\nthird_line\n";
    fs::write(&file, original).unwrap();
    fs::write(
        workspace.path().join("unselected.txt"),
        "unselected_synthetic_private_content",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    mock_ai_config(
        home.path(),
        "custom",
        &format!("http://{}/chat", listener.local_addr().unwrap()),
    );
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let mut stream = accept_mock_request(&listener);
            requests.push(read_mock_http_request(&mut stream));
            let proposal = serde_json::json!({"summary":"Synthetic reviewed multi-line edit", "replacement":replacement}).to_string();
            let body =
                serde_json::json!({"choices":[{"message":{"content":proposal}}]}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    let (_child, mut master) =
        spawn_mellow(&smoke_binary(), workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    for accept in [false, true] {
        begin_selected_ai_request(&mut master, &mut captured, &mut queries);
        assert_eq!(fs::read_to_string(&file).unwrap(), original);
        write_all(&mut master, b"review this synthetic edit\r");
        wait_for_screen_state(
            &mut master,
            &mut captured,
            &mut queries,
            "review before any edit",
            |screen| {
                screen.contains("Suggested change")
                    && screen.contains("before_name")
                    && screen.contains("after_name")
                    && screen.contains("Accept")
                    && screen.contains("Reject")
            },
        );
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            original,
            "review wrote disk before acceptance"
        );
        write_all(&mut master, if accept { b"\r" } else { b"r" });
        wait_for_screen_state(
            &mut master,
            &mut captured,
            &mut queries,
            "explicit proposal decision",
            |screen| {
                screen.contains(if accept {
                    "Suggestion applied"
                } else {
                    "Suggestion rejected"
                })
            },
        );
        if !accept {
            write_all(&mut master, &[0x13]);
            wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, original);
        }
    }
    write_all(&mut master, &[0x13]);
    wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, replacement);
    write_all(&mut master, &[0x1a, 0x13]);
    wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, original);
    write_all(&mut master, &[0x19, 0x13]);
    wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, replacement);
    for request in server.join().unwrap() {
        let header_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        let envelope: serde_json::Value = serde_json::from_slice(&request[header_end..]).unwrap();
        let user: serde_json::Value =
            serde_json::from_str(envelope["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(user["context"], original);
        assert_eq!(user["replacement_allowed"], true);
        assert!(user["file"].as_str().unwrap().ends_with("proposal.txt"));
        assert!(
            !String::from_utf8_lossy(&request).contains("unselected_synthetic_private_content")
        );
    }
    assert!(!String::from_utf8_lossy(&captured).contains("synthetic-audit-key"));
}

#[test]
fn release_binary_ai_stream_cancel_aborts_socket_and_late_result_cannot_overwrite_edit() {
    use std::net::TcpListener;
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("cancel.txt");
    fs::write(&file, "original_target\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    mock_ai_config(
        home.path(),
        "custom",
        &format!("http://{}/chat", listener.local_addr().unwrap()),
    );
    let (cancelled_sender, cancelled_receiver) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut stream = accept_mock_request(&listener);
        read_mock_http_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let prefix = "{\"summary\":\"streaming_pending\",\"replacement\":\"";
        writeln!(
            stream,
            "data: {}\n",
            serde_json::json!({"choices":[{"delta":{"content":prefix}}]})
        )
        .unwrap();
        stream.flush().unwrap();
        cancelled_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        // The peer must close the active transport without waiting for a
        // provider to finish generating. Then deliberately attempt late output.
        let mut byte = [0u8; 1];
        let closed = stream.read(&mut byte).unwrap() == 0;
        let late = "late_provider_overwrite\\n\"}";
        serde_json::from_str::<serde_json::Value>(&format!("{prefix}{late}")).unwrap();
        let _ = writeln!(
            stream,
            "data: {}\n\ndata: [DONE]\n",
            serde_json::json!({"choices":[{"delta":{"content":late},"finish_reason":"stop"}]})
        );
        closed
    });
    let (_child, mut master) =
        spawn_mellow(&smoke_binary(), workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    begin_selected_ai_request(&mut master, &mut captured, &mut queries);
    write_all(&mut master, b"suggest a synthetic replacement\r");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "actual in-flight stream preview",
        |screen| {
            screen.contains("streaming_pending") && screen.contains("Esc cancels local transport")
        },
    );
    write_all(&mut master, b"\x1b");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "explicit local transport cancellation",
        |screen| screen.contains("AI local transport cancelled"),
    );
    cancelled_sender.send(()).unwrap();
    write_all(
        &mut master,
        "\x1b[200~local edit 🙂\n\x1b[201~\x13".as_bytes(),
    );
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &file,
        "local edit 🙂\n",
    );
    assert!(
        server.join().unwrap(),
        "cancellation left provider socket open"
    );
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(700),
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "local edit 🙂\n");
    let screen = rendered_rows(&captured, (80, 24)).join("\n");
    assert!(!screen.contains("Suggested change"));
    assert!(!screen.contains("late_provider_overwrite"));
    write_all(&mut master, &[0x1a, 0x13]);
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &file,
        "original_target\n",
    );
    assert!(!String::from_utf8_lossy(&captured).contains("synthetic-audit-key"));
}

fn ask_shell_through_palette(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize) {
    write_all(master, &[0x10]);
    wait_for_screen_state(master, captured, queries, "command palette", |screen| {
        screen.contains("Commands")
    });
    write_all(master, b"Ask AI for a shell command");
    pump(master, captured, queries, Duration::from_millis(100));
    write_all(master, b"\r");
    wait_for_screen_state(master, captured, queries, "AI prompt", |screen| {
        screen.contains("Describe the command") || screen.contains("Ask AI")
    });
    write_all(master, b"suggest a command\r");
}

fn smoke_binary() -> std::path::PathBuf {
    std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into())
}

fn start_editor(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize) {
    wait_for_startup(master, captured, queries);
    // Alternate-screen negotiation precedes the first application frame. On a
    // loaded runner a fixed delay can capture only its prefix, before Welcome
    // is drawn, and incorrectly decide that onboarding needs no dismissal.
    // Ratatui emits this frame in row order; its complete footer follows the
    // onboarding overlay as well as the underlying editor content.
    wait_for_screen_state(
        master,
        captured,
        queries,
        "complete initial editor or onboarding frame",
        |screen| {
            // Restored sessions initially use a welcome notification in place
            // of the ordinary line/column footer. Both finish the first frame.
            let footer_complete =
                (screen.contains("Ln ") && screen.contains(", Col ") && screen.contains("F1 Help"))
                    || (screen.contains("Ctrl+P commands") && screen.contains("F1 help"));
            footer_complete
                && (!screen.contains("Welcome to Mellow · crafted")
                    || screen.contains("Start typing, or press Enter"))
        },
    );
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(captured);
    if parser.screen().contents().contains("Welcome to Mellow") {
        write_all(master, b"\x1b");
    }
    wait_for_screen_state(
        master,
        captured,
        queries,
        "editing after onboarding",
        |screen| !screen.contains("Welcome to Mellow") && screen.contains("Ln"),
    );
}

fn palette_command(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize, name: &str) {
    write_all(master, &[0x10]);
    wait_for_screen_state(master, captured, queries, "palette", |screen| {
        screen.contains("Commands")
    });
    write_all(master, name.as_bytes());
    pump(master, captured, queries, Duration::from_millis(100));
    write_all(master, b"\r");
    pump(master, captured, queries, Duration::from_millis(150));
}

fn fixture_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Synthetic clipboard adapters keep the host clipboard untouched while the
/// executable still exercises its real subprocess/native-clipboard route.
fn fixture_clipboard(root: &Path, text: &str) -> String {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let clipboard = root.join("clipboard.txt");
    fs::write(&clipboard, text).unwrap();
    for name in ["pbcopy", "wl-copy"] {
        fixture_executable(
            &bin.join(name),
            &format!(
                "printf invoked > '{}'; /bin/cat > '{}'",
                root.join(format!("{name}.invoked")).display(),
                clipboard.display()
            ),
        );
    }
    for name in ["pbpaste", "wl-paste"] {
        fixture_executable(
            &bin.join(name),
            &format!(
                "printf invoked > '{}'; /bin/cat '{}'",
                root.join(format!("{name}.invoked")).display(),
                clipboard.display()
            ),
        );
    }
    let path = format!("{}:/usr/bin:/bin", bin.display());
    // Fail on a broken fixture rather than mistaking it for editor behavior.
    for adapter in ["pbpaste", "pbcopy", "wl-paste", "wl-copy"] {
        let status = std::process::Command::new(adapter)
            .env("PATH", &path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .expect("could not start synthetic clipboard adapter");
        assert!(status.success(), "synthetic clipboard adapter failed");
        fs::remove_file(root.join(format!("{adapter}.invoked"))).unwrap();
    }
    fs::write(&clipboard, text).unwrap();
    path
}

#[test]
fn release_binary_unicode_multicursor_terminal_and_clipboard_paste_are_one_undo() {
    for bracketed in [true, false] {
        let workspace = tempdir().unwrap();
        let home = tempdir().unwrap();
        let file = workspace.path().join("multicursor.txt");
        fs::write(&file, "aa\nbb\n").unwrap();
        let path = fixture_clipboard(workspace.path(), "🦀e\u{301}");
        let (_child, mut master) = spawn_mellow_with_env(
            &smoke_binary(),
            workspace.path(),
            Some(&file),
            home.path(),
            (80, 24),
            false,
            &[("PATH", &path)],
        );
        let mut captured = Vec::new();
        let mut queries = 0;
        start_editor(&mut master, &mut captured, &mut queries);
        palette_command(&mut master, &mut captured, &mut queries, "Add cursor below");
        if bracketed {
            write_all(&mut master, "\x1b[200~🦀e\u{301}\x1b[201~".as_bytes());
        } else {
            write_all(&mut master, &[0x16]); // Ctrl+V through the clipboard adapter.
        }
        wait_for_screen_state(
            &mut master,
            &mut captured,
            &mut queries,
            "paste at both cursors",
            |screen| {
                screen.contains("🦀e\u{301}aa")
                    && count_occurrences(screen.as_bytes(), "🦀e\u{301}".as_bytes()) == 2
            },
        );
        write_all(&mut master, &[0x13]);
        wait_for_saved_text(
            &mut master,
            &mut captured,
            &mut queries,
            &file,
            "🦀e\u{301}aa\n🦀e\u{301}bb\n",
        );
        write_all(&mut master, &[0x1a, 0x13]); // One Undo, then Save.
        wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, "aa\nbb\n");
        write_all(&mut master, &[0x19, 0x13]); // Redo restores all cursor edits.
        wait_for_saved_text(
            &mut master,
            &mut captured,
            &mut queries,
            &file,
            "🦀e\u{301}aa\n🦀e\u{301}bb\n",
        );
    }
}

#[test]
fn release_binary_rectangle_paste_distributes_rows_and_undo_restores_all() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("rectangle.txt");
    fs::write(&file, "aa\nbb\n").unwrap();
    let (_child, mut master) = spawn_mellow_with(
        &smoke_binary(),
        workspace.path(),
        Some(&file),
        home.path(),
        (80, 24),
        true,
    );
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    // Alt+Shift mouse drag is the supported rectangle gesture. Select one
    // text column across the first two rows, using SGR modifier bits 4 + 8.
    write_all(&mut master, b"\x1b[<12;5;3M\x1b[<44;6;4M\x1b[<12;6;4m");
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(150),
    );
    write_all(&mut master, "\x1b[200~🦀\n尾\x1b[201~".as_bytes());
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
        "🦀a\n尾b\n",
    );
    write_all(&mut master, &[0x1a, 0x13]);
    wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, "aa\nbb\n");
}

#[test]
fn release_binary_linked_snippet_paste_and_restart_history_restore_are_undoable() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("snippet.py");
    fs::write(&file, "").unwrap();
    let (mut child, mut master) =
        spawn_mellow(&smoke_binary(), workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Insert snippet template",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "snippet picker",
        |screen| screen.contains("Linked variable"),
    );
    write_all(&mut master, b"\x1b[B\x1b[B\x1b[B\r");
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(150),
    );
    write_all(
        &mut master,
        "\x1b[200~名字\x1b[201~\t\x1b[200~42\x1b[201~\t".as_bytes(),
    );
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(150),
    );
    write_all(&mut master, &[0x13]);
    let expected = "名字 = 42\nprint(名字)\n";
    wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, expected);
    write_all(&mut master, &[0x11]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !child.try_reap() {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(50),
        );
        assert!(Instant::now() < deadline, "saved snippet could not quit");
    }
    let (_child, mut master) =
        spawn_mellow(&smoke_binary(), workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Local history / restore checkpoint",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "persistent checkpoints",
        |screen| screen.contains("Private local history") && screen.contains("before-save"),
    );
    write_all(&mut master, b"\x1b[B\r"); // Restore pre-save empty file without writing it.
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "unsaved restored checkpoint",
        |screen| screen.contains("Checkpoint restored"),
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), expected);
    write_all(&mut master, &[0x1a, 0x13]);
    wait_for_saved_text(&mut master, &mut captured, &mut queries, &file, expected);
}

fn trust_workspace(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize) {
    palette_command(master, captured, queries, "Trust workspace execution");
    wait_for_screen_state(
        master,
        captured,
        queries,
        "explicit trust review",
        |screen| screen.contains("Press Enter to explicitly trust"),
    );
    write_all(master, b"\r");
    wait_for_screen_state(master, captured, queries, "workspace trusted", |screen| {
        screen.contains("Workspace trusted")
    });
}

fn formatter_settings(home: &Path) {
    let directory = home.join("config/mellow");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("settings.conf"), "format_on_save = true\n").unwrap();
}

#[test]
fn release_binary_workspace_trust_does_not_execute_tools_for_outside_files_or_aliases() {
    use std::os::unix::fs::symlink;
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let home = tempdir().unwrap();
    let initial = workspace.path().join("ordinary.txt");
    fs::write(&initial, "ordinary\n").unwrap();
    let direct = outside.path().join("external.py");
    let aliased = outside.path().join("other.py");
    let alias = workspace.path().join("alias.py");
    fs::write(&direct, "direct_external\n").unwrap();
    fs::write(&aliased, "aliased_external\n").unwrap();
    symlink(&aliased, &alias).unwrap();
    let lsp_marker = workspace.path().join("lsp-started");
    let formatter_marker = workspace.path().join("formatter-started");
    let lsp = workspace.path().join("dummy-lsp");
    let formatter = workspace.path().join("dummy-formatter");
    fixture_executable(
        &lsp,
        &format!(
            "printf started > {}\n/bin/cat > /dev/null",
            shell_quote(lsp_marker.to_str().unwrap())
        ),
    );
    fixture_executable(
        &formatter,
        &format!(
            "printf started > {}\n/bin/cat",
            shell_quote(formatter_marker.to_str().unwrap())
        ),
    );
    formatter_settings(home.path());
    let lsp_program = lsp.to_string_lossy().into_owned();
    let formatter_program = formatter.to_string_lossy().into_owned();
    let (_child, mut master) = spawn_mellow_with_env(
        &smoke_binary(),
        workspace.path(),
        Some(&initial),
        home.path(),
        (80, 24),
        false,
        &[
            ("MELLOW_LSP_PYTHON", &lsp_program),
            ("MELLOW_FORMAT_PYTHON", &formatter_program),
        ],
    );
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    trust_workspace(&mut master, &mut captured, &mut queries);
    for (open_path, disk_path, content) in [
        (direct.as_path(), direct.as_path(), "direct_external"),
        (alias.as_path(), aliased.as_path(), "aliased_external"),
    ] {
        write_all(&mut master, &[0x0f]);
        wait_for_screen_state(
            &mut master,
            &mut captured,
            &mut queries,
            "outside file picker",
            |screen| screen.contains("Open file"),
        );
        write_all(&mut master, open_path.to_str().unwrap().as_bytes());
        wait_for_screen_state(
            &mut master,
            &mut captured,
            &mut queries,
            "complete explicit outside path echoed",
            |screen| screen.contains(open_path.file_name().unwrap().to_str().unwrap()),
        );
        // Enter during asynchronous discovery explicitly asks the user to wait
        // and try again. A symlink is intentionally absent from completion
        // candidates, but its explicit path remains openable. Exercise that
        // workflow without assuming the worker is ready after a fixed sleep.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = rendered_rows(&captured, (80, 24)).join("\n");
            if !screen.contains("Open file") {
                assert!(
                    screen.contains(content),
                    "wrong outside file opened: {screen}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "explicit outside path did not open: {screen}"
            );
            write_all(&mut master, b"\r");
            pump(
                &mut master,
                &mut captured,
                &mut queries,
                Duration::from_millis(100),
            );
        }
        write_all(&mut master, b"\x1b[200~edited_\x1b[201~\x13");
        wait_for_saved_text(
            &mut master,
            &mut captured,
            &mut queries,
            disk_path,
            &format!("edited_{content}\n"),
        );
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(350),
        );
        assert!(
            !lsp_marker.exists(),
            "trusted workspace launched LSP for outside path {}",
            open_path.display()
        );
        assert!(
            !formatter_marker.exists(),
            "trusted workspace formatted outside path {}",
            open_path.display()
        );
    }
    // Positive controls prove both per-process overrides are executable and
    // the trusted workspace is enabled, rather than accepting absent markers
    // from a broken fixture or globally disabled execution.
    let internal = workspace.path().join("internal.py");
    fs::write(&internal, "internal_control\n").unwrap();
    write_all(&mut master, &[0x0f]);
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "inside file picker",
        |screen| screen.contains("Open file"),
    );
    write_all(&mut master, b"./internal.py");
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(300),
    );
    write_all(&mut master, b"\r");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "inside file opened",
        |screen| screen.contains("internal_control") && !screen.contains("Open file"),
    );
    wait_for_marker(&mut master, &mut captured, &mut queries, &lsp_marker);
    write_all(&mut master, b"\x1b[200~edited_\x1b[201~\x13");
    wait_for_marker(&mut master, &mut captured, &mut queries, &formatter_marker);
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &internal,
        "edited_internal_control\n",
    );
}

fn wait_for_marker(master: &mut File, captured: &mut Vec<u8>, queries: &mut usize, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !marker.exists() {
        pump(master, captured, queries, Duration::from_millis(50));
        assert!(
            Instant::now() < deadline,
            "subprocess marker {} absent after three seconds; screen={:?}",
            marker.file_name().unwrap().to_string_lossy(),
            rendered_rows(captured, (80, 24))
        );
    }
}

#[test]
fn release_binary_formatter_palette_is_responsive_stale_result_and_cancel_do_not_save() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("format.sh");
    fs::write(&file, "original\n").unwrap();
    let formatter = workspace.path().join("formatter.sh");
    let started = workspace.path().join("started");
    fixture_executable(
        &formatter,
        &format!(
            "printf started > '{}'\n/bin/sleep 2\nprintf 'FORMATTED\\n'",
            started.display()
        ),
    );
    formatter_settings(home.path());
    let formatter_string = formatter.to_string_lossy().into_owned();
    let (_child, mut master) = spawn_mellow_with_env(
        &smoke_binary(),
        workspace.path(),
        Some(&file),
        home.path(),
        (80, 24),
        false,
        &[("MELLOW_FORMAT_SHELL", &formatter_string)],
    );
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    trust_workspace(&mut master, &mut captured, &mut queries);
    write_all(&mut master, b"\x1b[200~edited \x1b[201~\x13");
    wait_for_marker(&mut master, &mut captured, &mut queries, &started);
    write_all(&mut master, b"\x1b[200~LATE \x1b[201~");
    let interaction = Instant::now();
    write_all(&mut master, &[0x10]);
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "palette during formatter",
        |screen| screen.contains("Commands"),
    );
    assert!(
        interaction.elapsed() < Duration::from_secs(1),
        "formatter blocked palette input"
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "stale formatter rejected",
        |screen| screen.contains("Formatting discarded"),
    );
    // Escape cancels a pending formatter too; close the palette after stale
    // output has already been rejected so it cannot mask the stale check.
    type_keys(&mut master, &mut captured, &mut queries, b"\x1b");
    assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");
    fs::remove_file(&started).unwrap();
    write_all(&mut master, &[0x13]);
    wait_for_marker(&mut master, &mut captured, &mut queries, &started);
    write_all(&mut master, b"\x1b");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "cancelled formatter",
        |screen| screen.contains("save cancelled"),
    );
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(2300),
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "original\n",
        "cancelled save wrote to disk"
    );
}

#[test]
fn release_binary_formatter_deadline_includes_descendants_holding_pipes() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("deadline.sh");
    fs::write(&file, "original\n").unwrap();
    let formatter = workspace.path().join("formatter.sh");
    let escaped = workspace.path().join("descendant-escaped");
    fixture_executable(
        &formatter,
        &format!(
            "(/bin/sleep 13; printf escaped > '{}') &\nexit 0",
            escaped.display()
        ),
    );
    formatter_settings(home.path());
    let formatter_string = formatter.to_string_lossy().into_owned();
    let (_child, mut master) = spawn_mellow_with_env(
        &smoke_binary(),
        workspace.path(),
        Some(&file),
        home.path(),
        (80, 24),
        false,
        &[("MELLOW_FORMAT_SHELL", &formatter_string)],
    );
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    trust_workspace(&mut master, &mut captured, &mut queries);
    let operation = Instant::now();
    write_all(&mut master, b"\x1b[200~edited \x1b[201~\x13");
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(100),
        );
        if fs::read_to_string(&file).is_ok_and(|text| text == "edited original\n") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "inherited formatter pipe outlived its deadline"
        );
    }
    assert!(operation.elapsed() < Duration::from_secs(12));
    let rows = rendered_rows(&captured, (80, 24));
    assert!(
        rows.iter()
            .any(|row| row.contains("timed out") || row.contains("deadline")),
        "missing formatter failure disclosure: {rows:?}"
    );
    // Had the descendant escaped process-group cleanup, it would now write.
    while operation.elapsed() < Duration::from_secs(14) {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(100),
        );
    }
    assert!(
        !escaped.exists(),
        "formatter descendant survived timeout cleanup"
    );
}

#[test]
fn release_binary_safe_ai_shell_stays_in_review_copy_and_close_never_execute() {
    use std::net::TcpListener;
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("shell-review.txt");
    fs::write(&file, "unchanged\n").unwrap();
    let path = fixture_clipboard(workspace.path(), "");
    let marker = workspace.path().join("must-not-exist");
    let suggestion = format!("printf audit > '{}'", marker.display());
    let expected_clipboard = suggestion.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    mock_ai_config(
        home.path(),
        "custom",
        &format!("http://{}/chat", listener.local_addr().unwrap()),
    );
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "mock provider received no request"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("mock provider: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = [0u8; 16384];
        let _ = stream.read(&mut request).unwrap();
        let body = serde_json::json!({"choices":[{"message":{"content":suggestion}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let (_child, mut master) = spawn_mellow_with_env(
        &smoke_binary(),
        workspace.path(),
        Some(&file),
        home.path(),
        (80, 24),
        false,
        &[("PATH", &path)],
    );
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    // An existing shell with a partial command must never receive the answer.
    write_all(&mut master, &[0x14]);
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(250),
    );
    write_all(&mut master, b"printf partial");
    write_all(&mut master, &[0x14]);
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(150),
    );
    ask_shell_through_palette(&mut master, &mut captured, &mut queries);
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "shell read-only review",
        |screen| screen.contains("printf audit") && screen.contains("copy answer"),
    );
    server.join().unwrap();
    assert!(!marker.exists());
    write_all(&mut master, b"c");
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &workspace.path().join("clipboard.txt"),
        &expected_clipboard,
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("clipboard.txt")).unwrap(),
        expected_clipboard,
        "copy fixture called={}; screen={:?}",
        workspace.path().join("pbcopy.invoked").exists(),
        rendered_rows(&captured, (80, 24))
    );
    assert!(!marker.exists(), "copy action executed shell suggestion");
    write_all(&mut master, b"\r");
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(300),
    );
    assert!(!marker.exists(), "close action executed shell suggestion");
    assert_eq!(fs::read_to_string(&file).unwrap(), "unchanged\n");
}

#[test]
fn release_binary_save_all_multiple_files_and_navigation_back_forward() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let first = workspace.path().join("first.txt");
    let second = workspace.path().join("second.txt");
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    let (_child, mut master) =
        spawn_mellow(&smoke_binary(), workspace.path(), Some(&first), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    write_all(&mut master, b"\x1b[200~A \x1b[201~\x0f");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "open file picker",
        |screen| screen.contains("Open file"),
    );
    write_all(&mut master, b"./second.txt");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "resolved second file path",
        |screen| count_occurrences(screen.as_bytes(), b"second.txt") >= 2,
    );
    write_all(&mut master, b"\r");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "second buffer opened",
        |screen| screen.contains("1 │ second"),
    );
    write_all(&mut master, b"\x1b[200~B \x1b[201~");
    palette_command(&mut master, &mut captured, &mut queries, "Save All files");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "two dirty file review",
        |screen| {
            screen.contains("2 dirty files")
                && screen.contains("first.txt")
                && screen.contains("second.txt")
        },
    );
    assert_eq!(fs::read_to_string(&first).unwrap(), "first\n");
    assert_eq!(fs::read_to_string(&second).unwrap(), "second\n");
    write_all(&mut master, b"\r");
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &first,
        "A first\n",
    );
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &second,
        "B second\n",
    );
    palette_command(&mut master, &mut captured, &mut queries, "Go to line");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "line navigation",
        |screen| screen.contains("Go to line"),
    );
    write_all(&mut master, b"2\r");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "line two",
        |screen| screen.contains("Ln 2, Col 1"),
    );
    palette_command(&mut master, &mut captured, &mut queries, "Navigation back");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "previous caret",
        |screen| screen.contains("Ln 1, Col 3"),
    );
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Navigation forward",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "forward caret",
        |screen| screen.contains("Ln 2, Col 1"),
    );
}

#[test]
fn release_binary_terminal_doctor_omits_paste_contents_and_config_review_does_not_save() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("config.json");
    fs::write(&file, "{\"spec\":{\"replicas\":1}}\n").unwrap();
    let (_child, mut master) =
        spawn_mellow(&smoke_binary(), workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Terminal Doctor / key recorder",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "terminal diagnostics",
        |screen| screen.contains("Terminal Doctor") && screen.contains("Keyboard:"),
    );
    write_all(
        &mut master,
        b"\x0f\x1b[200~synthetic-private-paste-4937\x1b[201~",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "recorded delivered paste",
        |screen| screen.contains("Delivered bracketed paste") && screen.contains("content omitted"),
    );
    assert!(
        !String::from_utf8_lossy(&captured).contains("synthetic-private-paste-4937"),
        "diagnostics retained pasted content"
    );
    write_all(&mut master, b"\x1b");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "editor after doctor",
        |screen| !screen.contains("Terminal Doctor"),
    );
    write_all(
        &mut master,
        b"\x01\x1b[200~{\"spec\":{\"replicas\":3},\"token\":\"synthetic-secret-719\"}\n\x1b[201~",
    );
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Safe configuration change",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "semantic configuration review",
        |screen| screen.contains("spec.replicas") && screen.contains("[redacted]"),
    );
    let rows = rendered_rows(&captured, (80, 24));
    assert!(
        !rows.iter().any(|row| row.contains("synthetic-secret-719")),
        "semantic review disclosed a secret value"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "{\"spec\":{\"replicas\":1}}\n",
        "review saved without explicit action"
    );
    type_keys(&mut master, &mut captured, &mut queries, b"\x1b");
    write_all(&mut master, b"\x01\x1b[200~{\"a\":1,\"a\":2}\x1b[201~");
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Safe configuration change",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "duplicate keys rejected",
        |screen| screen.contains("Configuration validation failed"),
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "{\"spec\":{\"replicas\":1}}\n"
    );
}

#[test]
fn release_binary_cli_multifile_goto_wait_reaches_the_requested_caret() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let first = workspace.path().join("first.txt");
    let second = workspace.path().join("second.txt");
    fs::write(&first, "first\nrow\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    let launcher = workspace.path().join("launch-cli.sh");
    fixture_executable(
        &launcher,
        &format!(
            "exec {} --wait --no-mouse --goto {} {}",
            shell_quote(&smoke_binary().to_string_lossy()),
            shell_quote(&format!("{}:2:2", first.display())),
            shell_quote(&second.to_string_lossy())
        ),
    );
    let (mut child, mut master) = spawn_mellow(&launcher, workspace.path(), None, home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "both CLI files and requested location",
        |screen| {
            screen.contains("first.txt")
                && screen.contains("second.txt")
                && screen.contains("Ln 2, Col 2")
        },
    );
    assert!(!child.try_reap(), "--wait returned before editing finished");
    write_all(&mut master, "\x1b[200~🦀\x1b[201~\x13".as_bytes());
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &first,
        "first\nr🦀ow\n",
    );
    write_all(&mut master, b"\x1b[6;5~");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "second CLI buffer",
        |screen| screen.contains("1 │ second"),
    );
    assert_eq!(fs::read_to_string(&second).unwrap(), "second\n");
    write_all(&mut master, &[0x11]);
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
            "--wait did not return after quitting"
        );
    }
}

#[test]
fn release_binary_cli_piped_stdin_retains_interactive_terminal_and_saves_draft() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let launcher = workspace.path().join("launch-stdin.sh");
    let content = "stdin 🦀\nsecond line\n";
    fixture_executable(
        &launcher,
        &format!(
            "printf '%s' {} | {} --wait --no-mouse -",
            shell_quote(content),
            shell_quote(&smoke_binary().to_string_lossy())
        ),
    );
    let (_child, mut master) = spawn_mellow(&launcher, workspace.path(), None, home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "stdin draft in interactive editor",
        |screen| screen.contains("stdin 🦀") && screen.contains("Untitled"),
    );
    write_all(&mut master, &[0x13]);
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "stdin Save As prompt",
        |screen| screen.contains("Save as") || screen.contains("Save As"),
    );
    write_all(&mut master, b"\r");
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &workspace.path().join("untitled.txt"),
        content,
    );
}

#[test]
fn release_binary_slow_clipboard_keeps_palette_responsive_and_discards_stale_paste() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("clipboard-stale.txt");
    fs::write(&file, "original\n").unwrap();
    let path = fixture_clipboard(workspace.path(), "late clipboard");
    let started = workspace.path().join("clipboard-started");
    for adapter in ["pbpaste", "wl-paste"] {
        fixture_executable(
            &workspace.path().join("bin").join(adapter),
            &format!(
                "printf started > {}\n/bin/sleep 0.8\nprintf 'late clipboard'",
                shell_quote(&started.to_string_lossy())
            ),
        );
    }
    let (_child, mut master) = spawn_mellow_with_env(
        &smoke_binary(),
        workspace.path(),
        Some(&file),
        home.path(),
        (80, 24),
        false,
        &[("PATH", &path)],
    );
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);
    write_all(&mut master, &[0x16]);
    wait_for_marker(&mut master, &mut captured, &mut queries, &started);
    write_all(&mut master, b"\x1b[200~new edit \x1b[201~");
    let interaction = Instant::now();
    write_all(&mut master, &[0x10]);
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "palette during clipboard read",
        |screen| screen.contains("Commands"),
    );
    assert!(
        interaction.elapsed() < Duration::from_millis(600),
        "clipboard blocked editor interaction"
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "stale clipboard discarded",
        |screen| screen.contains("Clipboard result discarded"),
    );
    type_keys(&mut master, &mut captured, &mut queries, b"\x1b");
    write_all(&mut master, &[0x13]);
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &file,
        "new edit original\n",
    );
}

#[test]
fn release_binary_rejects_hostile_ai_shell_before_terminal_input() {
    use std::net::TcpListener;
    let binary = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("shell-safety.txt");
    fs::write(&file, "shell safety fixture").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    mock_ai_config(home.path(), "custom", &format!("http://{address}/chat"));
    let marker = workspace.path().join("must-not-exist");
    let hostile = format!("printf audit > '{}'\r# no explicit Enter", marker.display());
    let server = std::thread::spawn(move || {
        let mut stream = accept_mock_request(&listener);
        let mut request = [0u8; 16384];
        let _ = stream.read(&mut request).unwrap();
        let body = serde_json::json!({"choices":[{"message":{"content":hostile}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let (_child, mut master) = spawn_mellow(&binary, workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "onboarding",
        |screen| screen.contains("Welcome to Mellow"),
    );
    write_all(&mut master, b"\x1b");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "editor",
        |screen| screen.contains("shell safety fixture") && !screen.contains("Welcome to Mellow"),
    );
    write_all(&mut master, &[0x14]); // Start a real shell, then return to editor.
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(200),
    );
    write_all(&mut master, &[0x14]);
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(100),
    );
    ask_shell_through_palette(&mut master, &mut captured, &mut queries);
    // Collect the actual execution evidence before checking the rejection UI.
    // The published vulnerable artifact must fail because it creates a marker,
    // not merely because it lacks this candidate's new status message.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut refused = false;
    while Instant::now() < deadline {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(100),
        );
        refused = rendered_rows(&captured, (80, 24))
            .iter()
            .any(|row| row.contains("Control bytes refused"));
        if marker.exists() || refused {
            break;
        }
    }
    assert!(
        !marker.exists(),
        "AI response executed before a separate user action: synthetic marker exists"
    );
    assert!(
        refused,
        "terminal controls were not explicitly refused; marker=false"
    );
    server.join().unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "shell safety fixture");
}

#[test]
fn release_binary_claude_redirect_never_contacts_key_target() {
    use std::net::TcpListener;
    let binary = std::env::var_os("MELLOW_SMOKE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mellow").into());
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("redirect-safety.txt");
    fs::write(&file, "redirect safety fixture").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let target_port = target.local_addr().unwrap().port();
    let source = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = source.local_addr().unwrap();
    mock_ai_config(
        home.path(),
        "claude",
        &format!("http://{address}/v1/messages"),
    );
    let server = std::thread::spawn(move || {
        let mut stream = accept_mock_request(&source);
        let mut request = [0u8; 16384];
        let read = stream.read(&mut request).unwrap();
        assert!(
            String::from_utf8_lossy(&request[..read]).contains("x-api-key: synthetic-audit-key")
        );
        write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://localhost:{target_port}/capture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let (_child, mut master) = spawn_mellow(&binary, workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "onboarding",
        |screen| screen.contains("Welcome to Mellow"),
    );
    write_all(&mut master, b"\x1b");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "editor",
        |screen| {
            screen.contains("redirect safety fixture") && !screen.contains("Welcome to Mellow")
        },
    );
    ask_shell_through_palette(&mut master, &mut captured, &mut queries);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut rejected = false;
    let mut redirected = false;
    let mut forwarded_key = false;
    while Instant::now() < deadline {
        pump(
            &mut master,
            &mut captured,
            &mut queries,
            Duration::from_millis(100),
        );
        match target.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                redirected = true;
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0u8; 16384];
                let read = stream.read(&mut request).unwrap();
                forwarded_key =
                    String::from_utf8_lossy(&request[..read]).contains("synthetic-audit-key");
                let _ = write!(
                    stream,
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("redirect target failed: {error}"),
        }
        rejected = rendered_rows(&captured, (80, 24))
            .iter()
            .any(|row| row.contains("307") || row.contains("Temporary Redirect"));
        if rejected {
            break;
        }
    }
    assert!(
        !redirected,
        "redirect destination received a request; synthetic API key forwarded={forwarded_key}"
    );
    assert!(
        rejected,
        "provider redirect was not explicitly rejected; target_requests=0"
    );
    server.join().unwrap();
    assert!(!String::from_utf8_lossy(&captured).contains("synthetic-audit-key"));
}

#[test]
fn release_binary_interactive_conflict_pills_filter_and_terminal_buffer_flow() {
    let workspace = tempdir().unwrap();
    let home = tempdir().unwrap();
    let file = workspace.path().join("workflow.txt");
    fs::write(
        &file,
        "<<<<<<< HEAD\nzebra\napple\n=======\ntheirs\n>>>>>>> branch\n",
    )
    .unwrap();
    let (_child, mut master) =
        spawn_mellow(&smoke_binary(), workspace.path(), Some(&file), home.path());
    let mut captured = Vec::new();
    let mut queries = 0;
    start_editor(&mut master, &mut captured, &mut queries);

    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "inline conflict pills on editor canvas",
        |screen| screen.contains("[Keep ours]") && screen.contains("[Keep theirs]"),
    );

    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Keep ours in conflict",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "conflict resolved to ours",
        |screen| screen.contains("zebra") && !screen.contains("<<<<<<<"),
    );

    trust_workspace(&mut master, &mut captured, &mut queries);
    write_all(&mut master, &[0x01]); // Ctrl+A select all
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(100),
    );
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Filter selection through shell command",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "filter prompt open",
        |screen| screen.contains("Filter selection through shell"),
    );
    write_all(&mut master, b"\t\r"); // Tab -> sort, Enter -> apply
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "filtered with sort",
        |screen| screen.contains("Filtered with `sort`"),
    );
    write_all(&mut master, &[0x13]); // Ctrl+S save
    wait_for_saved_text(
        &mut master,
        &mut captured,
        &mut queries,
        &file,
        "apple\nzebra\n",
    );

    // Open built-in terminal, emit output, and open terminal output in an editor tab.
    write_all(&mut master, &[0x14]); // Ctrl+T open terminal
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "terminal panel visible",
        |screen| screen.contains("Terminal · "),
    );
    write_all(&mut master, b"echo MELLOW_PTY_SCROLLBACK_OK\n");
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "shell echoed marker",
        |screen| screen.contains("MELLOW_PTY_SCROLLBACK_OK"),
    );
    write_all(&mut master, &[0x14]); // Ctrl+T back to editor
    pump(
        &mut master,
        &mut captured,
        &mut queries,
        Duration::from_millis(100),
    );
    palette_command(
        &mut master,
        &mut captured,
        &mut queries,
        "Open terminal output in editor tab",
    );
    wait_for_screen_state(
        &mut master,
        &mut captured,
        &mut queries,
        "terminal output opened in new tab",
        |screen| {
            screen.contains("Opened terminal output")
                && screen.contains("2 │ MELLOW_PTY_SCROLLBACK_OK")
                && screen.contains("[↗ Tab]")
        },
    );
}
