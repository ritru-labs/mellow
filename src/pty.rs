use std::{
    env,
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    ptr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

const SCROLLBACK_LINES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCell {
    pub contents: String,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub wide_continuation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalScreenSnapshot {
    pub rows: Vec<Vec<TerminalCell>>,
    pub cursor: Option<(u16, u16)>,
    pub alternate_screen: bool,
    pub bracketed_paste: bool,
    pub application_cursor: bool,
}

pub struct PtySession {
    id: usize,
    master: File,
    child_pid: libc::pid_t,
    shell: String,
    parser: vt100::Parser,
    exited: bool,
}

impl PtySession {
    pub fn spawn_shell(id: usize, cwd: &Path, cols: u16, rows: u16) -> Result<Self> {
        let shell = env::var_os("SHELL")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        Self::spawn_program(id, Path::new(&shell), &["-i"], cwd, cols, rows)
    }

    fn spawn_program(
        id: usize,
        program: &Path,
        args: &[&str],
        cwd: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        let program_c = CString::new(program.as_os_str().as_bytes())
            .context("shell path contains an embedded NUL byte")?;
        let cwd_c = CString::new(cwd.as_os_str().as_bytes())
            .context("terminal working directory contains an embedded NUL byte")?;
        let arg_c: Vec<CString> = args
            .iter()
            .map(|arg| {
                CString::new(*arg).context("terminal argument contains an embedded NUL byte")
            })
            .collect::<Result<_>>()?;
        // Built before fork: between fork and exec the child of a threaded
        // process may only call async-signal-safe functions, and setenv
        // (which allocates) is not one of them.
        let env_c = child_environment();

        let mut master_fd = -1;
        let mut slave_fd = -1;
        // SAFETY: openpty initializes both descriptors on success. libc exposes
        // different pointer mutability for the BSD/macOS and Linux signatures.
        #[cfg(target_os = "macos")]
        let opened = unsafe {
            let mut size = libc::winsize {
                ws_row: rows.max(1),
                ws_col: cols.max(1),
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
                ws_row: rows.max(1),
                ws_col: cols.max(1),
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
        if opened != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to allocate PTY");
        }
        // openpty does not set close-on-exec. Without it every process Mellow
        // started later (Git, language servers, other shells) inherited this
        // terminal's master, so closing the pane could leave its jobs running.
        for fd in [master_fd, slave_fd] {
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }

        let mut argv: Vec<*const libc::c_char> = Vec::with_capacity(arg_c.len() + 2);
        argv.push(program_c.as_ptr());
        argv.extend(arg_c.iter().map(|arg| arg.as_ptr()));
        argv.push(ptr::null());
        let mut envp: Vec<*const libc::c_char> = Vec::with_capacity(env_c.len() + 1);
        envp.extend(env_c.iter().map(|entry| entry.as_ptr()));
        envp.push(ptr::null());

        // SAFETY: the child performs descriptor/session setup and immediately execs.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            unsafe {
                libc::close(master_fd);
                libc::close(slave_fd);
            }
            return Err(std::io::Error::last_os_error()).context("failed to fork PTY shell");
        }

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
                libc::execve(program_c.as_ptr(), argv.as_ptr(), envp.as_ptr());
                libc::_exit(127);
            }
        }

        unsafe {
            libc::close(slave_fd);
        }
        let master = unsafe { File::from_raw_fd(master_fd) };

        Ok(Self {
            id,
            master,
            child_pid: pid,
            shell: program.to_string_lossy().into_owned(),
            parser: vt100::Parser::new(rows.max(1), cols.max(1), SCROLLBACK_LINES),
            exited: false,
        })
    }

    pub const fn id(&self) -> usize {
        self.id
    }

    #[cfg(test)]
    fn master_fd(&self) -> libc::c_int {
        self.master.as_raw_fd()
    }

    pub fn label(&self) -> &str {
        Path::new(&self.shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&self.shell)
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if self.refresh_exit_status()? {
            bail!("terminal session has exited");
        }
        self.master
            .write_all(bytes)
            .context("failed to write to PTY")
    }

    #[cfg(test)]
    pub fn interrupt(&mut self) -> Result<()> {
        self.write_bytes(&[0x03])
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to resize PTY");
        }
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }

    pub fn read_available(&mut self) -> Result<usize> {
        let mut total = 0usize;
        loop {
            let mut poll_fd = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll_fd, 1, 0) };
            if ready < 0 {
                return Err(std::io::Error::last_os_error()).context("PTY poll failed");
            }
            if ready == 0 || poll_fd.revents & libc::POLLIN == 0 {
                break;
            }

            let mut buffer = [0u8; 4096];
            match self.master.read(&mut buffer) {
                Ok(0) => {
                    self.exited = true;
                    break;
                }
                Ok(read) => {
                    total += read;
                    self.parser.process(&buffer[..read]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("failed to read PTY output"),
            }
        }

        let _ = self.refresh_exit_status();
        Ok(total)
    }

    /// Moves the view `delta` lines into history (positive) or back towards
    /// live output (negative); returns how far back the view now is.
    pub fn scroll_view(&mut self, delta: isize) -> usize {
        let current = self.parser.screen().scrollback();
        let target = current.saturating_add_signed(delta);
        self.parser.screen_mut().set_scrollback(target);
        self.parser.screen().scrollback()
    }

    /// Lines of history above the live screen currently in view (0 = live).
    pub fn scroll_offset(&self) -> usize {
        self.parser.screen().scrollback()
    }

    pub fn follow_output(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    /// The text currently on the terminal screen (including a scrolled view).
    pub fn screen_text(&self) -> String {
        let contents = self.parser.screen().contents();
        contents.trim_end_matches('\n').to_owned()
    }

    /// True while a program other than the shell owns the terminal (a build,
    /// a server, an editor...): closing would kill it.
    pub fn has_running_job(&self) -> bool {
        if self.exited {
            return false;
        }
        let group = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        group > 0 && group != self.child_pid
    }

    #[cfg(test)]
    pub fn visible_text(&self, max_lines: usize) -> String {
        let contents = self.parser.screen().contents();
        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(max_lines.max(1));
        lines[start..].join("\n")
    }

    pub fn screen_snapshot(&self, max_rows: u16, max_cols: u16) -> TerminalScreenSnapshot {
        let screen = self.parser.screen();
        let (screen_rows, screen_cols) = screen.size();
        let rows = screen_rows.min(max_rows);
        let cols = screen_cols.min(max_cols);
        let mut output = Vec::with_capacity(rows as usize);

        for row in 0..rows {
            let mut cells = Vec::with_capacity(cols as usize);
            for col in 0..cols {
                let cell = screen.cell(row, col);
                cells.push(match cell {
                    Some(cell) => TerminalCell {
                        contents: cell.contents().to_owned(),
                        fg: terminal_color(cell.fgcolor()),
                        bg: terminal_color(cell.bgcolor()),
                        bold: cell.bold(),
                        dim: cell.dim(),
                        italic: cell.italic(),
                        underline: cell.underline(),
                        inverse: cell.inverse(),
                        wide_continuation: cell.is_wide_continuation(),
                    },
                    None => TerminalCell {
                        contents: String::new(),
                        fg: TerminalColor::Default,
                        bg: TerminalColor::Default,
                        bold: false,
                        dim: false,
                        italic: false,
                        underline: false,
                        inverse: false,
                        wide_continuation: false,
                    },
                });
            }
            output.push(cells);
        }

        TerminalScreenSnapshot {
            rows: output,
            cursor: (!screen.hide_cursor()).then_some(screen.cursor_position()),
            alternate_screen: screen.alternate_screen(),
            bracketed_paste: screen.bracketed_paste(),
            application_cursor: screen.application_cursor(),
        }
    }

    pub fn write_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        let application_cursor = self.parser.screen().application_cursor();
        let Some(bytes) = key_event_bytes_with_mode(key, application_cursor) else {
            return Ok(false);
        };
        self.write_bytes(&bytes)?;
        Ok(true)
    }

    pub fn write_paste(&mut self, text: &str) -> Result<()> {
        if self.parser.screen().bracketed_paste() {
            self.write_bytes(b"\x1b[200~")?;
            self.write_bytes(text.as_bytes())?;
            self.write_bytes(b"\x1b[201~")
        } else {
            self.write_bytes(text.as_bytes())
        }
    }

    pub fn write_mouse_event(
        &mut self,
        event: crossterm::event::MouseEvent,
        col: u16,
        row: u16,
    ) -> Result<bool> {
        let screen = self.parser.screen();
        let Some(bytes) = mouse_event_bytes(
            event,
            col,
            row,
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding(),
        ) else {
            return Ok(false);
        };
        self.write_bytes(&bytes)?;
        Ok(true)
    }

    /// True when the program in the terminal asked for mouse events (an
    /// editor, a pager); the wheel then belongs to it, not to scrollback.
    pub fn wants_mouse(&self) -> bool {
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    pub fn is_exited(&mut self) -> bool {
        self.refresh_exit_status().unwrap_or(self.exited)
    }

    fn refresh_exit_status(&mut self) -> Result<bool> {
        if self.exited {
            return Ok(true);
        }

        let mut status = 0;
        let result = unsafe { libc::waitpid(self.child_pid, &mut status, libc::WNOHANG) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.exited = true;
                return Ok(true);
            }
            return Err(error).context("failed to query PTY child");
        }
        if result == self.child_pid {
            self.exited = true;
        }
        Ok(self.exited)
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if self.exited {
            return;
        }

        unsafe {
            libc::kill(-self.child_pid, libc::SIGHUP);
        }
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            if self.refresh_exit_status().unwrap_or(false) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        unsafe {
            let _ = libc::kill(-self.child_pid, libc::SIGKILL);
            let _ = libc::kill(self.child_pid, libc::SIGKILL);
        }

        // Never block indefinitely during teardown. PTY/job-control semantics differ
        // across Unix platforms, so reap opportunistically for a bounded interval.
        let kill_deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < kill_deadline {
            match self.refresh_exit_status() {
                Ok(true) => return,
                Ok(false) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
            }
        }
        self.exited = true;
    }
}

fn mouse_event_bytes(
    event: crossterm::event::MouseEvent,
    col: u16,
    row: u16,
    mode: vt100::MouseProtocolMode,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

    if mode == vt100::MouseProtocolMode::None || encoding != vt100::MouseProtocolEncoding::Sgr {
        return None;
    }

    let allowed = match mode {
        vt100::MouseProtocolMode::None => false,
        vt100::MouseProtocolMode::Press => {
            matches!(
                event.kind,
                MouseEventKind::Down(_)
                    | MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight
            )
        }
        vt100::MouseProtocolMode::PressRelease => {
            matches!(
                event.kind,
                MouseEventKind::Down(_)
                    | MouseEventKind::Up(_)
                    | MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight
            )
        }
        vt100::MouseProtocolMode::ButtonMotion => {
            matches!(
                event.kind,
                MouseEventKind::Down(_)
                    | MouseEventKind::Up(_)
                    | MouseEventKind::Drag(_)
                    | MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight
            )
        }
        vt100::MouseProtocolMode::AnyMotion => true,
    };
    if !allowed {
        return None;
    }

    let (mut code, release) = match event.kind {
        MouseEventKind::Down(MouseButton::Left) => (0u16, false),
        MouseEventKind::Down(MouseButton::Middle) => (1, false),
        MouseEventKind::Down(MouseButton::Right) => (2, false),
        MouseEventKind::Up(MouseButton::Left) => (0, true),
        MouseEventKind::Up(MouseButton::Middle) => (1, true),
        MouseEventKind::Up(MouseButton::Right) => (2, true),
        MouseEventKind::Drag(MouseButton::Left) => (32, false),
        MouseEventKind::Drag(MouseButton::Middle) => (33, false),
        MouseEventKind::Drag(MouseButton::Right) => (34, false),
        MouseEventKind::Moved => (35, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };

    if event.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }

    let suffix = if release { 'm' } else { 'M' };
    Some(
        format!(
            "\x1b[<{code};{};{}{suffix}",
            col.saturating_add(1),
            row.saturating_add(1)
        )
        .into_bytes(),
    )
}

fn terminal_color(color: vt100::Color) -> TerminalColor {
    match color {
        vt100::Color::Default => TerminalColor::Default,
        vt100::Color::Idx(index) => TerminalColor::Indexed(index),
        vt100::Color::Rgb(r, g, b) => TerminalColor::Rgb(r, g, b),
    }
}

fn key_event_bytes_with_mode(
    key: crossterm::event::KeyEvent,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let cursor = |normal: &'static [u8], application: &'static [u8]| {
        if application_cursor {
            application.to_vec()
        } else {
            normal.to_vec()
        }
    };

    let mut bytes = match key.code {
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => cursor(b"\x1b[D", b"\x1bOD"),
        KeyCode::Right => cursor(b"\x1b[C", b"\x1bOC"),
        KeyCode::Up => cursor(b"\x1b[A", b"\x1bOA"),
        KeyCode::Down => cursor(b"\x1b[B", b"\x1bOB"),
        KeyCode::Home => cursor(b"\x1b[H", b"\x1bOH"),
        KeyCode::End => cursor(b"\x1b[F", b"\x1bOF"),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Char(ch) if ctrl && ch.is_ascii() => {
            let lower = ch.to_ascii_lowercase() as u8;
            if lower.is_ascii_lowercase() {
                vec![lower & 0x1f]
            } else {
                return None;
            }
        }
        KeyCode::Char(ch) => {
            let mut buffer = [0u8; 4];
            ch.encode_utf8(&mut buffer).as_bytes().to_vec()
        }
        _ => return None,
    };

    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(test)]
pub fn key_event_bytes(key: crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    key_event_bytes_with_mode(key, false)
}

/// The terminal type the shell is told. Programs then use only colours the
/// real terminal can show; the pane still maps any others down.
fn child_term(tier: crate::theme::ColorTier) -> &'static str {
    match tier {
        crate::theme::ColorTier::Basic => "TERM=xterm",
        _ => "TERM=xterm-256color",
    }
}

/// The shell's environment: Mellow's own, with the terminal type the screen
/// model understands.
fn child_environment() -> Vec<CString> {
    let mut entries: Vec<CString> = env::vars_os()
        .filter(|(key, _)| key != "TERM" && key != "TERM_PROGRAM")
        .filter_map(|(key, value)| {
            let mut entry = key.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).ok()
        })
        .collect();
    entries.extend(
        [
            child_term(crate::theme::cached_color_tier()),
            "TERM_PROGRAM=Mellow",
        ]
        .map(|entry| CString::new(entry).expect("static environment entry")),
    );
    entries
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use tempfile::tempdir;

    use super::*;

    fn pump_until(session: &mut PtySession, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            session.read_available().unwrap();
            let text = session.visible_text(200);
            if text.contains(needle) || Instant::now() >= deadline {
                return text;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Audit: the PTY master leaked into every later child (Git, language
    /// servers, other shells), so closing a terminal could leave its jobs
    /// running. Each shell must also see Mellow's environment and TERM.
    #[test]
    fn the_shell_is_told_an_honest_terminal_type() {
        use crate::theme::ColorTier;
        assert_eq!(super::child_term(ColorTier::Basic), "TERM=xterm");
        assert_eq!(super::child_term(ColorTier::Ansi256), "TERM=xterm-256color");
        assert_eq!(
            super::child_term(ColorTier::TrueColor),
            "TERM=xterm-256color"
        );
    }

    #[test]
    fn terminals_do_not_leak_into_other_programs_and_set_term() {
        let dir = tempdir().unwrap();
        let first = PtySession::spawn_program(
            9,
            Path::new("/bin/sh"),
            &["-c", "sleep 5"],
            dir.path(),
            80,
            24,
        )
        .unwrap();
        let fd = first.master_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "master must be close-on-exec");

        let script = format!(
            "if [ -e /dev/fd/{fd} ]; then echo HOLDS-FIRST; else echo CLEAN; fi; \
             echo \"T=$TERM P=$TERM_PROGRAM\"; [ -n \"$PATH\" ] && echo HAS-PATH; sleep 5"
        );
        let mut second = PtySession::spawn_program(
            10,
            Path::new("/bin/sh"),
            &["-c", &script],
            dir.path(),
            80,
            24,
        )
        .unwrap();
        let text = pump_until(&mut second, "HAS-PATH", Duration::from_secs(5));
        assert!(text.contains("CLEAN"), "{text}");
        let term = super::child_term(crate::theme::cached_color_tier()).trim_start_matches("TERM=");
        assert!(text.contains(&format!("T={term} P=Mellow")), "{text}");
        assert!(text.contains("HAS-PATH"), "{text}");
    }

    /// Audit: scrollback storage existed but could not be read.
    #[test]
    fn scrolled_view_reads_back_through_history_then_follows_output() {
        let dir = tempdir().unwrap();
        let mut session = PtySession::spawn_program(
            7,
            Path::new("/bin/sh"),
            &[
                "-c",
                "i=1; while [ $i -le 300 ]; do echo line-$i; i=$((i+1)); done; sleep 5",
            ],
            dir.path(),
            40,
            10,
        )
        .unwrap();
        let live = pump_until(&mut session, "line-300", Duration::from_secs(5));
        assert!(live.contains("line-300"));
        assert_eq!(session.scroll_offset(), 0);

        assert_eq!(session.scroll_view(100), 100);
        let back = session.screen_text();
        assert!(!back.contains("line-300"), "{back}");
        assert!(back.contains("line-195"), "{back}");

        session.scroll_view(-40);
        assert_eq!(session.scroll_offset(), 60);
        session.follow_output();
        assert_eq!(session.scroll_offset(), 0);
        assert!(session.screen_text().contains("line-300"));
    }

    #[test]
    fn a_foreground_job_is_detected_until_it_ends() {
        let dir = tempdir().unwrap();
        let mut session =
            PtySession::spawn_program(8, Path::new("/bin/sh"), &["-i"], dir.path(), 80, 24)
                .unwrap();
        pump_until(&mut session, "$", Duration::from_secs(2));
        assert!(!session.has_running_job(), "idle shell");

        session.write_bytes(b"sleep 30\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.has_running_job() && Instant::now() < deadline {
            session.read_available().unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(session.has_running_job(), "sleep owns the terminal");

        // Under load a single Ctrl+C can arrive before sleep is ready, so resend it.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut next_interrupt = Instant::now();
        while session.has_running_job() && Instant::now() < deadline {
            if Instant::now() >= next_interrupt {
                session.interrupt().unwrap();
                next_interrupt = Instant::now() + Duration::from_millis(500);
            }
            session.read_available().unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!session.has_running_job(), "back at the prompt");
    }

    #[test]
    fn sgr_mouse_encoder_respects_terminal_mode_and_coordinates() {
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 99,
            row: 99,
            modifiers: KeyModifiers::CONTROL,
        };
        assert_eq!(
            mouse_event_bytes(
                event,
                4,
                2,
                vt100::MouseProtocolMode::PressRelease,
                vt100::MouseProtocolEncoding::Sgr,
            ),
            Some(b"\x1b[<16;5;3M".to_vec())
        );
        assert_eq!(
            mouse_event_bytes(
                event,
                4,
                2,
                vt100::MouseProtocolMode::None,
                vt100::MouseProtocolEncoding::Sgr,
            ),
            None
        );
        assert_eq!(
            mouse_event_bytes(
                event,
                4,
                2,
                vt100::MouseProtocolMode::PressRelease,
                vt100::MouseProtocolEncoding::Default,
            ),
            None
        );
    }

    #[test]
    fn terminal_key_encoding_keeps_ctrl_c_as_sigint_byte() {
        assert_eq!(
            key_event_bytes(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            key_event_bytes(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_event_bytes_with_mode(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn vt_screen_tracks_cursor_color_and_terminal_modes() {
        let dir = tempdir().unwrap();
        let mut session =
            PtySession::spawn_program(1, Path::new("/bin/sh"), &["-i"], dir.path(), 20, 6).unwrap();

        session
            .parser
            .process(b"hello\x1b[2;5H\x1b[31mR\x1b[0m\x1b[?2004h");
        let snapshot = session.screen_snapshot(6, 20);
        assert_eq!(snapshot.rows[0][0].contents, "h");
        assert_eq!(snapshot.rows[1][4].contents, "R");
        assert_eq!(snapshot.rows[1][4].fg, TerminalColor::Indexed(1));
        assert_eq!(snapshot.cursor, Some((1, 5)));
        assert!(snapshot.bracketed_paste);

        session.parser.process(b"\x1b[?1049hALT");
        let alternate = session.screen_snapshot(6, 20);
        assert!(alternate.alternate_screen);
        assert_eq!(alternate.rows[0][0].contents, "A");
        assert_eq!(alternate.rows[0][1].contents, "L");
        assert_eq!(alternate.rows[0][2].contents, "T");
    }

    #[test]
    fn real_pty_shell_keeps_working_directory_and_resizes() {
        let dir = tempdir().unwrap();
        let mut session =
            PtySession::spawn_program(2, Path::new("/bin/sh"), &["-i"], dir.path(), 80, 24)
                .unwrap();
        session.resize(50, 12).unwrap();
        session.write_bytes(b"pwd\nstty size\nexit\n").unwrap();

        let expected = dir.path().to_string_lossy().into_owned();
        let text = pump_until(&mut session, "12 50", Duration::from_secs(2));
        assert!(
            text.contains(&expected),
            "PTY output did not contain cwd: {text:?}"
        );
        assert!(
            text.contains("12 50"),
            "PTY did not observe resize: {text:?}"
        );
    }

    #[test]
    fn ctrl_c_interrupts_foreground_process_without_killing_shell_session() {
        let dir = tempdir().unwrap();
        let mut session =
            PtySession::spawn_program(3, Path::new("/bin/sh"), &["-i"], dir.path(), 80, 24)
                .unwrap();
        session.write_bytes(b"sleep 5\n").unwrap();
        thread::sleep(Duration::from_millis(100));
        session.interrupt().unwrap();
        session
            .write_bytes(b"printf 'AFTER_INTERRUPT\\n'\nexit\n")
            .unwrap();

        let text = pump_until(&mut session, "AFTER_INTERRUPT", Duration::from_secs(2));
        assert!(
            text.contains("AFTER_INTERRUPT"),
            "shell did not recover after Ctrl+C: {text:?}"
        );
    }
}
