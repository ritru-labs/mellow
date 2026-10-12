//! Format on save: runs the language's standard formatter (rustfmt, ruff,
//! shfmt, prettier...) over the buffer text before it is written. Formatters
//! read stdin and print the result; Mellow never guesses indentation itself,
//! because in Python or YAML that would change what the file means.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

/// A formatter that never answers would otherwise freeze saving.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Language name (as `Buffer::language` reports it) to the environment
/// variable that overrides its formatter, e.g. `MELLOW_FORMAT_PYTHON`.
fn override_variable(language: &str) -> String {
    let name: String = language
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{}FORMAT_{name}", crate::brand::ENV_PREFIX)
}

/// Overrides set in the environment, read once at startup.
pub fn overrides_from_env() -> HashMap<String, String> {
    LANGUAGES
        .iter()
        .filter_map(|language| {
            let value = std::env::var(override_variable(language)).ok()?;
            let value = value.trim();
            (!value.is_empty()).then(|| (language.to_string(), value.to_owned()))
        })
        .collect()
}

const LANGUAGES: &[&str] = &[
    "Rust",
    "Python",
    "Shell",
    "YAML",
    "JSON",
    "TOML",
    "Terraform",
    "Go",
    "JavaScript",
    "TypeScript",
    "React JSX",
    "React TSX",
    "CSS",
    "HTML",
    "Markdown",
    "C",
    "C++",
    "Java",
];

/// Candidate commands per language, best first. `{path}` becomes the file's
/// path so formatters can pick the right parser and project settings.
fn candidates(language: &str) -> &'static [&'static str] {
    match language {
        "Rust" => &["rustfmt --edition 2024 --emit stdout"],
        "Python" => &[
            "ruff format --stdin-filename {path} -",
            "black -q --stdin-filename {path} -",
        ],
        "Shell" => &["shfmt -filename {path}"],
        "YAML" => &["prettier --stdin-filepath {path}", "yamlfmt -"],
        "JSON" => &["prettier --stdin-filepath {path}", "jq ."],
        "TOML" => &["taplo fmt -"],
        "Terraform" => &["terraform fmt -"],
        "Go" => &["gofmt"],
        "JavaScript" | "TypeScript" | "React JSX" | "React TSX" | "CSS" | "HTML" | "Markdown" => {
            &["prettier --stdin-filepath {path}"]
        }
        "C" | "C++" | "Java" => &["clang-format --assume-filename={path}"],
        _ => &[],
    }
}

/// The formatter that would run for this file, or why there is none.
pub fn formatter_for(
    language: &str,
    path: &Path,
    overrides: &HashMap<String, String>,
) -> Result<Vec<String>, String> {
    let chosen = match overrides.get(language) {
        Some(command) => command.as_str(),
        None => {
            let known = candidates(language);
            if known.is_empty() {
                return Err(format!("no formatter is known for {language}"));
            }
            match known
                .iter()
                .find(|command| on_path(command.split_whitespace().next().unwrap_or("")))
            {
                Some(command) => command,
                None => {
                    let programs: Vec<&str> = known
                        .iter()
                        .filter_map(|command| command.split_whitespace().next())
                        .collect();
                    return Err(format!("{} is not installed", programs.join(" or ")));
                }
            }
        }
    };
    let path = path.to_string_lossy();
    Ok(chosen
        .split_whitespace()
        .map(|word| word.replace("{path}", &path))
        .collect())
}

fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Runs a formatter under the same deadline for input, process exit and pipe
/// draining. The caller should run this on a worker and bind its result to the
/// original buffer identity and revision before applying it.
pub fn run_cancellable(
    command: &[String],
    text: &str,
    path: &Path,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    run_with_limits(command, text, path, cancelled, TIMEOUT, OUTPUT_LIMIT)
}

const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
const ERROR_LIMIT: usize = 1024 * 1024;

/// Bounded command output. An unsuccessful exit is data, so task runners can
/// show diagnostics; cancellation, deadlines and invalid output are errors.
pub struct ProcessOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(test)]
pub fn run(command: &[String], text: &str, path: &Path) -> Result<String> {
    run_cancellable(
        command,
        text,
        path,
        &std::sync::atomic::AtomicBool::new(false),
    )
}

fn run_with_limits(
    command: &[String],
    text: &str,
    path: &Path,
    cancelled: &std::sync::atomic::AtomicBool,
    timeout: Duration,
    output_limit: usize,
) -> Result<String> {
    let result = run_process(command, text, path, cancelled, timeout, output_limit)?;
    let program = command.first().map(String::as_str).unwrap_or("formatter");
    let program = Path::new(program)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    if !result.success {
        let first = result
            .stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("it reported an error");
        let first: String = first
            .chars()
            .filter(|ch| !ch.is_control())
            .take(512)
            .collect();
        bail!("{program}: {first}");
    }
    if result.stdout.trim().is_empty() && !text.trim().is_empty() {
        bail!("{program} returned nothing");
    }
    Ok(result.stdout)
}

/// Run an optional local tool with a single deadline across input, child exit
/// and output draining. Empty output and nonzero exits are valid results.
/// Callers must run this off the UI thread and sanitize diagnostics for display.
#[cfg(unix)]
pub fn run_process(
    command: &[String],
    text: &str,
    path: &Path,
    cancelled: &std::sync::atomic::AtomicBool,
    timeout: Duration,
    output_limit: usize,
) -> Result<ProcessOutput> {
    use std::os::unix::{io::AsRawFd, process::CommandExt};
    use std::sync::atomic::Ordering;
    let Some((program, args)) = command.split_first() else {
        bail!("empty formatter command");
    };
    if cancelled.load(Ordering::Relaxed) {
        bail!("formatting cancelled");
    }
    let deadline = Instant::now() + timeout;
    let mut process = Command::new(program);
    process
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process.process_group(0);
    if let Some(dir) = path.parent().filter(|dir| dir.is_dir()) {
        process.current_dir(dir);
    }
    let child = process
        .spawn()
        .with_context(|| format!("could not start {program}"))?;
    let program = Path::new(program)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let mut group = FormatterProcess(child);
    let mut stdin = group.0.stdin.take();
    let mut stdout = group.0.stdout.take();
    let mut stderr = group.0.stderr.take();
    for fd in [
        stdin.as_ref().map(AsRawFd::as_raw_fd),
        stdout.as_ref().map(AsRawFd::as_raw_fd),
        stderr.as_ref().map(AsRawFd::as_raw_fd),
    ]
    .into_iter()
    .flatten()
    {
        // SAFETY: each descriptor belongs to a live child pipe; flags do not
        // affect ownership and remain valid until its Option is cleared.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error()).context("formatter nonblocking pipes");
        }
    }
    let mut written = 0;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut status = None;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            bail!("formatting cancelled");
        }
        if Instant::now() >= deadline {
            bail!(
                "{program} exceeded its {} ms deadline (including inherited pipes)",
                timeout.as_millis()
            );
        }
        let progress = (written, output.len(), errors.len());
        if let Some(pipe) = stdin.as_mut() {
            if written < text.len() {
                let end = (written + 8192).min(text.len());
                match pipe.write(&text.as_bytes()[written..end]) {
                    Ok(count) => written += count,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            || error.kind() == std::io::ErrorKind::Interrupted => {}
                    // A formatter may choose not to consume all input. Its
                    // exit status and output still determine success.
                    Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                        stdin = None;
                    }
                    Err(error) => return Err(error).context("formatter input"),
                }
            }
            if written == text.len() {
                stdin = None;
            }
        }
        drain_pipe(&mut stdout, &mut output, output_limit, "output")?;
        drain_pipe(&mut stderr, &mut errors, ERROR_LIMIT, "error output")?;
        if status.is_none() {
            status = group.0.try_wait()?;
        }
        if let Some(status) = status
            && stdout.is_none()
            && stderr.is_none()
        {
            return Ok(ProcessOutput {
                success: status.success(),
                stdout: String::from_utf8(output).context("tool output is not UTF-8")?,
                stderr: String::from_utf8_lossy(&errors).into_owned(),
            });
        }
        if progress == (written, output.len(), errors.len()) {
            thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(unix)]
fn drain_pipe<P: Read>(
    pipe: &mut Option<P>,
    bytes: &mut Vec<u8>,
    limit: usize,
    label: &str,
) -> Result<()> {
    if let Some(reader) = pipe.as_mut() {
        // Cap each turn as well as the total: a continuously writing child
        // must not starve cancellation and deadline checks.
        let mut chunk = [0u8; 8192];
        for _ in 0..8 {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    *pipe = None;
                    break;
                }
                Ok(count) => {
                    if bytes.len().saturating_add(count) > limit {
                        bail!("formatter {label} exceeded the {limit} byte limit");
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).with_context(|| format!("formatter {label}")),
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
struct FormatterProcess(std::process::Child);

#[cfg(unix)]
impl Drop for FormatterProcess {
    fn drop(&mut self) {
        // Kill the entire group even after its leader exits; descendants can
        // retain stdin/stdout/stderr. No blocking pipe reader threads remain.
        // SAFETY: child PID is positive, and process_group(0) isolated this
        // child's group from the editor's group before executing the tool.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

#[cfg(not(unix))]
pub fn run_process(
    _command: &[String],
    _text: &str,
    _path: &Path,
    _cancelled: &std::sync::atomic::AtomicBool,
    _timeout: Duration,
    _output_limit: usize,
) -> Result<ProcessOutput> {
    bail!(
        "external formatting is unavailable on this platform: process-tree deadlines are not supported"
    )
}

/// Gives formatter output the buffer's line endings (most formatters always
/// print `\n`), so formatting never silently converts a CRLF file.
pub fn match_line_endings(formatted: &str, crlf: bool) -> String {
    let unix = formatted.replace("\r\n", "\n");
    if crlf {
        unix.replace('\n', "\r\n")
    } else {
        unix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    #[test]
    fn bounded_tool_runner_preserves_failed_task_output_and_empty_clipboards() {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let failed = run_process(
            &sh("printf 'x.rs:3:4: failure\\n'; printf details >&2; exit 2"),
            "",
            Path::new("/tmp/x"),
            &cancelled,
            Duration::from_secs(2),
            4096,
        )
        .unwrap();
        assert!(!failed.success);
        assert_eq!(failed.stdout, "x.rs:3:4: failure\n");
        assert_eq!(failed.stderr, "details");
        let empty = run_process(
            &sh("cat >/dev/null"),
            "clipboard",
            Path::new("/tmp/x"),
            &cancelled,
            Duration::from_secs(2),
            4096,
        )
        .unwrap();
        assert!(empty.success);
        assert!(empty.stdout.is_empty());
    }

    #[test]
    fn exited_formatter_with_inherited_output_pipe_obeys_deadline() {
        let started = Instant::now();
        let result = run(
            &sh("sleep 12 & printf formatted; exit 0"),
            "original",
            Path::new("/tmp/x.txt"),
        );
        assert!(
            started.elapsed() < Duration::from_secs(11),
            "inherited pipe bypassed deadline: {:?}",
            started.elapsed()
        );
        assert!(
            result.is_err(),
            "partial formatter output must not be accepted"
        );
    }

    #[test]
    fn input_backpressure_obeys_one_deadline() {
        let started = Instant::now();
        let result = run_with_limits(
            &sh("sleep 30"),
            &"x".repeat(2_000_000),
            Path::new("/tmp/x"),
            &std::sync::atomic::AtomicBool::new(false),
            Duration::from_millis(100),
            OUTPUT_LIMIT,
        );
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn output_is_bounded_before_acceptance() {
        let result = run_with_limits(
            &sh("yes flood"),
            "x",
            Path::new("/tmp/x"),
            &std::sync::atomic::AtomicBool::new(false),
            Duration::from_secs(2),
            4096,
        );
        assert!(result.unwrap_err().to_string().contains("byte limit"));
    }

    #[test]
    fn cancellation_kills_the_formatter_and_its_descendant() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("descendant-was-not-killed");
        let script = format!("(sleep 1; touch '{}') & sleep 30", marker.display());
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = run_cancellable(&sh(&script), "text", Path::new("/tmp/x"), &cancelled);
        canceller.join().unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists(), "descendant survived cancellation");
    }

    #[test]
    fn formatter_error_never_injects_terminal_control_characters() {
        let error = run(
            &sh("printf '\\033[31merror\\033[0m\\n' >&2; exit 1"),
            "text",
            Path::new("/tmp/x"),
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains('\u{1b}'), "{error:?}");
    }

    #[test]
    fn formatter_output_replaces_the_text() {
        let out = run(&sh("tr a-z A-Z"), "fn main() {}\n", Path::new("/tmp/x.rs")).unwrap();
        assert_eq!(out, "FN MAIN() {}\n");
    }

    #[test]
    fn a_failing_formatter_reports_its_first_error_line() {
        let error = run(
            &sh("echo '' >&2; echo 'error: expected one of `;` at line 3' >&2; exit 1"),
            "broken(",
            Path::new("/tmp/x.rs"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("expected one of `;` at line 3"), "{error}");
    }

    #[test]
    fn empty_output_for_real_text_is_refused() {
        assert!(run(&sh("cat >/dev/null"), "x = 1\n", Path::new("/tmp/x.py")).is_err());
    }

    #[test]
    fn large_input_does_not_deadlock() {
        let text = "line of text\n".repeat(200_000); // ~2.6 MB, far beyond a pipe
        let out = run(&sh("cat"), &text, Path::new("/tmp/x.txt")).unwrap();
        assert_eq!(out.len(), text.len());
    }

    #[test]
    fn missing_formatters_are_named() {
        let error = formatter_for("Shell", Path::new("a.sh"), &HashMap::new());
        if let Err(message) = error {
            assert!(message.contains("shfmt"), "{message}");
        }
        assert_eq!(
            formatter_for("Plain Text", Path::new("a.txt"), &HashMap::new()).unwrap_err(),
            "no formatter is known for Plain Text"
        );
    }

    #[test]
    fn overrides_win_and_fill_in_the_path() {
        let overrides = HashMap::from([(
            "Python".to_owned(),
            "black -q --stdin-filename {path} -".to_owned(),
        )]);
        assert_eq!(
            formatter_for("Python", Path::new("/w/app.py"), &overrides).unwrap(),
            ["black", "-q", "--stdin-filename", "/w/app.py", "-"]
        );
        assert_eq!(override_variable("React TSX"), "MELLOW_FORMAT_REACT_TSX");
    }

    #[test]
    fn line_endings_follow_the_buffer() {
        assert_eq!(match_line_endings("a\nb\n", true), "a\r\nb\r\n");
        assert_eq!(match_line_endings("a\r\nb\n", false), "a\nb\n");
    }
}
