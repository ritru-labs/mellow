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

/// Runs `command` with `text` on stdin from the file's folder (so project
/// settings like rustfmt.toml or pyproject.toml apply) and returns stdout.
/// Any failure leaves the caller's text untouched.
pub fn run(command: &[String], text: &str, path: &Path) -> Result<String> {
    let Some((program, args)) = command.split_first() else {
        bail!("empty formatter command");
    };
    let mut process = Command::new(program);
    process
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = path.parent().filter(|dir| dir.is_dir()) {
        process.current_dir(dir);
    }
    let mut child = process
        .spawn()
        .with_context(|| format!("could not start {program}"))?;
    // Messages name the tool, not its full path.
    let program = Path::new(program).file_name().map_or_else(
        || program.clone(),
        |name| name.to_string_lossy().into_owned(),
    );

    // Feed stdin and drain both pipes on threads, so a large file can never
    // deadlock against a full pipe.
    let mut stdin = child.stdin.take().context("formatter stdin")?;
    let input = text.to_owned();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().context("formatter stdout")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let mut stderr = child.stderr.take().context("formatter stderr")?;
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} took longer than {} seconds", TIMEOUT.as_secs());
        }
        thread::sleep(Duration::from_millis(5));
    };
    let _ = writer.join();
    let output = reader.join().unwrap_or_default();
    let errors = errors.join().unwrap_or_default();

    if !status.success() {
        let message = String::from_utf8_lossy(&errors);
        let first = message
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("it reported an error");
        bail!("{program}: {first}");
    }
    let formatted = String::from_utf8(output).context("formatter output is not UTF-8")?;
    if formatted.trim().is_empty() && !text.trim().is_empty() {
        bail!("{program} returned nothing");
    }
    Ok(formatted)
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
