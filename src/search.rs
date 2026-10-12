use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use regex::{Captures, Regex, RegexBuilder};
use unicode_segmentation::UnicodeSegmentation;

const MAX_PROJECT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PROJECT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PROJECT_RESULTS: usize = 2_000;
pub const MAX_REPLACE_PREVIEW: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub start_byte: usize,
    pub end_byte: usize,
    pub row: usize,
    pub col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub line_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementMatch {
    pub search: SearchMatch,
    pub replacement: String,
    pub matched_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSearchResult {
    pub path: PathBuf,
    pub row: usize,
    pub col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub line_preview: String,
    pub matched_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSearchReport {
    pub results: Vec<ProjectSearchResult>,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    pub truncated: bool,
    /// Files searched from unsaved editor text instead of disk.
    pub unsaved_files: usize,
    /// Files not searched, so "no results" is never mistaken for complete.
    pub skipped_large: usize,
    pub skipped_binary: usize,
    pub skipped_unreadable: usize,
    pub skipped_filtered: usize,
    /// Bounded effective include/exclude policy, supplied by the worker.
    pub scope_summary: String,
}

pub fn search_text(
    source: &str,
    query: &str,
    options: SearchOptions,
    limit: usize,
) -> Result<Vec<SearchMatch>> {
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let regex = compile_pattern(query, options)?;
    let mut lines = LineCursor::new(source);
    Ok(regex
        .find_iter(source)
        .take(limit)
        .map(|matched| lines.search_match(matched.start(), matched.end()))
        .collect())
}

pub fn replacement_matches(
    source: &str,
    query: &str,
    replacement: &str,
    options: SearchOptions,
    limit: usize,
) -> Result<Vec<ReplacementMatch>> {
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let regex = compile_pattern(query, options)?;
    let mut lines = LineCursor::new(source);
    let mut output = Vec::new();

    for captures in regex.captures_iter(source).take(limit) {
        let Some(matched) = captures.get(0) else {
            continue;
        };
        let replacement_text = if options.regex {
            expand_replacement(&captures, replacement)
        } else {
            replacement.to_owned()
        };
        output.push(ReplacementMatch {
            search: lines.search_match(matched.start(), matched.end()),
            replacement: replacement_text,
            matched_text: matched.as_str().to_owned(),
        });
    }

    Ok(output)
}

/// Searches `files` (relative to `root`). Files with an entry in `unsaved`
/// are searched as the editor has them, not as saved on disk.
#[cfg(test)]
pub fn search_project(
    root: &Path,
    files: &[PathBuf],
    query: &str,
    options: SearchOptions,
    unsaved: &std::collections::HashMap<PathBuf, String>,
) -> Result<ProjectSearchReport> {
    let request = ProjectSearchRequest {
        root: root.to_path_buf(),
        files: Some(files.to_vec()),
        query: query.to_owned(),
        options,
        unsaved: unsaved.clone(),
        scope: ProjectSearchScope::default(),
    };
    search_project_request(&request, &AtomicBool::new(false), |_| {})
}

#[derive(Debug, Clone)]
pub struct ProjectSearchScope {
    /// Glob filters over relative paths. `*` matches any substring, `?` one character.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_results: usize,
    pub max_files: usize,
}

impl Default for ProjectSearchScope {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_bytes: MAX_PROJECT_FILE_BYTES,
            max_total_bytes: MAX_PROJECT_TOTAL_BYTES,
            max_results: MAX_PROJECT_RESULTS,
            max_files: 5_000,
        }
    }
}

impl ProjectSearchScope {
    fn summary(&self) -> String {
        let describe = |patterns: &[String], fallback: &str| {
            if patterns.is_empty() {
                return fallback.to_owned();
            }
            let mut label = patterns
                .iter()
                .take(2)
                .map(|pattern| {
                    pattern
                        .chars()
                        .take(40)
                        .map(|ch| if ch.is_control() { '�' } else { ch })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(", ");
            if patterns.len() > 2 {
                label.push_str(&format!(" (+{})", patterns.len() - 2));
            }
            label
        };
        format!(
            "include {}; exclude {}",
            describe(&self.include, "all"),
            describe(&self.exclude, "none")
        )
    }

    /// Read project filters on the worker, with explicit allocation limits.
    fn from_project(root: &Path, fallback: &Self) -> Result<Self> {
        let path = root.join(".mellow/search.json");
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut scope = fallback.clone();
        match options.open(&path) {
            Ok(file) => {
                let metadata = file.metadata()?;
                anyhow::ensure!(
                    metadata.is_file() && metadata.len() <= 64 * 1024,
                    "search.json must be a regular file of at most 64 KiB"
                );
                let mut bytes = Vec::new();
                file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
                anyhow::ensure!(bytes.len() <= 64 * 1024, "search.json exceeded 64 KiB");
                let config: serde_json::Value =
                    serde_json::from_slice(&bytes).context("invalid .mellow/search.json")?;
                anyhow::ensure!(config.is_object(), "search.json must be a JSON object");
                for (key, target) in [
                    ("include", &mut scope.include),
                    ("exclude", &mut scope.exclude),
                ] {
                    if let Some(value) = config.get(key) {
                        let values = value
                            .as_array()
                            .context("search include/exclude must be arrays")?;
                        anyhow::ensure!(
                            values.len() <= 64,
                            "search include/exclude allow at most 64 patterns"
                        );
                        *target = values
                            .iter()
                            .map(|value| {
                                value
                                    .as_str()
                                    .map(str::to_owned)
                                    .context("search filter must be a string")
                            })
                            .collect::<Result<_>>()?;
                    }
                }
                for (key, target) in [
                    ("max_file_bytes", &mut scope.max_file_bytes),
                    ("max_total_bytes", &mut scope.max_total_bytes),
                ] {
                    if let Some(value) = config.get(key) {
                        *target = value
                            .as_u64()
                            .context("search byte limits must be positive integers")?;
                    }
                }
                for (key, target) in [
                    ("max_results", &mut scope.max_results),
                    ("max_files", &mut scope.max_files),
                ] {
                    if let Some(value) = config.get(key) {
                        *target = usize::try_from(
                            value
                                .as_u64()
                                .context("search count limits must be positive integers")?,
                        )
                        .context("search count exceeds platform limit")?;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read .mellow/search.json"),
        }
        anyhow::ensure!(
            (1..=64 * 1024 * 1024).contains(&scope.max_file_bytes),
            "max_file_bytes must be 1..67108864"
        );
        anyhow::ensure!(
            (1..=1024 * 1024 * 1024).contains(&scope.max_total_bytes),
            "max_total_bytes must be 1..1073741824"
        );
        anyhow::ensure!(
            (1..=20_000).contains(&scope.max_results),
            "max_results must be 1..20000"
        );
        anyhow::ensure!(
            (1..=100_000).contains(&scope.max_files),
            "max_files must be 1..100000"
        );
        anyhow::ensure!(
            scope
                .include
                .iter()
                .chain(&scope.exclude)
                .all(|pattern| pattern.len() <= 256),
            "search filter patterns must be at most 256 bytes"
        );
        Ok(scope)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectSearchRequest {
    pub root: PathBuf,
    /// None discovers files on the worker, honoring Git ignore rules.
    pub files: Option<Vec<PathBuf>>,
    pub query: String,
    pub options: SearchOptions,
    pub unsaved: HashMap<PathBuf, String>,
    pub scope: ProjectSearchScope,
}

#[derive(Debug)]
pub struct ProjectSearchUpdate {
    pub generation: u64,
    pub finished: bool,
    pub report: std::result::Result<ProjectSearchReport, String>,
}

struct SearchJob {
    generation: u64,
    request: ProjectSearchRequest,
    cancelled: Arc<AtomicBool>,
}

/// A single background worker coalesces queued requests. Cancelled generations
/// cannot replace the current panel, and never create another scanning thread.
pub struct ProjectSearchWorker {
    jobs: Sender<SearchJob>,
    updates: Receiver<ProjectSearchUpdate>,
    generation: u64,
    cancelled: Option<Arc<AtomicBool>>,
}

impl Default for ProjectSearchWorker {
    fn default() -> Self {
        let (jobs, pending) = mpsc::channel::<SearchJob>();
        let (results, updates) = mpsc::sync_channel(4);
        thread::Builder::new()
            .name("mellow-project-search".to_owned())
            .spawn(move || {
                while let Ok(mut job) = pending.recv() {
                    for newer in pending.try_iter() {
                        job = newer;
                    }
                    if job.cancelled.load(Ordering::Relaxed) {
                        continue;
                    }
                    let result =
                        ProjectSearchScope::from_project(&job.request.root, &job.request.scope)
                            .and_then(|scope| {
                                job.request.scope = scope;
                                search_project_request(&job.request, &job.cancelled, |report| {
                                    let _ = results.try_send(ProjectSearchUpdate {
                                        generation: job.generation,
                                        finished: false,
                                        report: Ok(report.clone()),
                                    });
                                })
                            })
                            .map_err(|error| error.to_string());
                    let mut final_update = ProjectSearchUpdate {
                        generation: job.generation,
                        finished: true,
                        report: result,
                    };
                    while !job.cancelled.load(Ordering::Relaxed) {
                        match results.try_send(final_update) {
                            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => break,
                            Err(mpsc::TrySendError::Full(update)) => {
                                final_update = update;
                                thread::sleep(Duration::from_millis(2));
                            }
                        }
                    }
                }
            })
            .expect("start project-search worker");
        Self {
            jobs,
            updates,
            generation: 0,
            cancelled: None,
        }
    }
}

impl ProjectSearchWorker {
    pub fn submit(&mut self, request: ProjectSearchRequest) -> u64 {
        self.cancel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancelled = Some(Arc::clone(&cancelled));
        let _ = self.jobs.send(SearchJob {
            generation: self.generation,
            request,
            cancelled,
        });
        self.generation
    }
    pub fn cancel(&mut self) {
        if let Some(cancelled) = self.cancelled.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.generation = self.generation.wrapping_add(1);
    }
    pub fn poll(&self) -> Vec<ProjectSearchUpdate> {
        self.updates
            .try_iter()
            .filter(|update| update.generation == self.generation)
            .collect()
    }
}

impl Drop for ProjectSearchWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn scope_patterns(patterns: &[String]) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|glob| {
            let mut expression = String::from("^");
            for ch in glob.chars() {
                match ch {
                    '*' => expression.push_str(".*"),
                    '?' => expression.push('.'),
                    other => expression.push_str(&regex::escape(&other.to_string())),
                }
            }
            expression.push('$');
            Regex::new(&expression).context("invalid search file filter")
        })
        .collect()
}

fn search_project_request(
    request: &ProjectSearchRequest,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(&ProjectSearchReport),
) -> Result<ProjectSearchReport> {
    if request.query.is_empty() {
        return Ok(ProjectSearchReport::default());
    }
    let regex = compile_pattern(&request.query, request.options)?;
    let root_identity = fs::canonicalize(&request.root).context("resolve search workspace")?;
    let include = scope_patterns(&request.scope.include)?;
    let exclude = scope_patterns(&request.scope.exclude)?;
    let mut report = ProjectSearchReport {
        scope_summary: request.scope.summary(),
        ..ProjectSearchReport::default()
    };
    let mut files = match &request.files {
        Some(files) => files.clone(),
        None => {
            let discovered = crate::workspace::discover_file_list_cancellable(
                &request.root,
                request.scope.max_files,
                cancelled,
            )?;
            report.truncated = discovered.truncated;
            discovered.files
        }
    };
    files.extend(request.unsaved.keys().cloned());
    files.sort_by_key(|path| (!request.unsaved.contains_key(path), path.clone()));
    files.dedup();
    if files.len() > request.scope.max_files {
        report.truncated = true;
        files.truncate(request.scope.max_files);
    }
    for (index, relative) in files.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("search cancelled");
        }
        if report.results.len() >= request.scope.max_results
            || report.bytes_scanned >= request.scope.max_total_bytes
        {
            report.truncated = true;
            break;
        }
        let name = relative.to_string_lossy();
        if !include.is_empty() && !include.iter().any(|pattern| pattern.is_match(&name))
            || exclude.iter().any(|pattern| pattern.is_match(&name))
        {
            report.skipped_filtered += 1;
            continue;
        }
        // Paths outside discovery scope, including `..`, never escape the project.
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            report.skipped_unreadable += 1;
            continue;
        }
        let loaded;
        let source: &str = if let Some(text) = request.unsaved.get(relative) {
            if text.len() as u64 > request.scope.max_file_bytes {
                report.skipped_large += 1;
                continue;
            }
            report.unsaved_files += 1;
            text
        } else {
            let absolute = match fs::canonicalize(request.root.join(relative)) {
                Ok(path) if path.starts_with(&root_identity) => path,
                _ => {
                    report.skipped_unreadable += 1;
                    continue;
                }
            };
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
            }
            let Ok(mut file) = options.open(&absolute) else {
                report.skipped_unreadable += 1;
                continue;
            };
            let metadata = match file.metadata() {
                Ok(metadata) if metadata.is_file() => metadata,
                _ => {
                    report.skipped_unreadable += 1;
                    continue;
                }
            };
            if metadata.len() > request.scope.max_file_bytes {
                report.skipped_large += 1;
                continue;
            }
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut failed = false;
            loop {
                if cancelled.load(Ordering::Relaxed) {
                    anyhow::bail!("search cancelled");
                }
                match file.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        bytes.extend_from_slice(&chunk[..count]);
                        if bytes.len() as u64 > request.scope.max_file_bytes {
                            break;
                        }
                    }
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                report.skipped_unreadable += 1;
                continue;
            }
            if bytes.len() as u64 > request.scope.max_file_bytes {
                report.skipped_large += 1;
                continue;
            }
            match String::from_utf8(bytes) {
                Ok(text) if !text.contains('\0') => {
                    loaded = text;
                    &loaded
                }
                _ => {
                    report.skipped_binary += 1;
                    continue;
                }
            }
        };
        if report.bytes_scanned.saturating_add(source.len() as u64) > request.scope.max_total_bytes
        {
            report.truncated = true;
            break;
        }
        report.files_scanned += 1;
        report.bytes_scanned += source.len() as u64;
        let mut lines = LineCursor::new(source);
        for matched in regex.find_iter(source) {
            if cancelled.load(Ordering::Relaxed) {
                anyhow::bail!("search cancelled");
            }
            let location = lines.search_match(matched.start(), matched.end());
            report.results.push(ProjectSearchResult {
                path: relative.clone(),
                row: location.row,
                col: location.col,
                end_row: location.end_row,
                end_col: location.end_col,
                line_preview: location.line_preview,
                matched_text: matched.as_str().to_owned(),
            });
            if report.results.len() >= request.scope.max_results {
                report.truncated = true;
                break;
            }
        }
        if index % 32 == 0 {
            progress(&report);
        }
    }
    Ok(report)
}

fn compile_pattern(query: &str, options: SearchOptions) -> Result<Regex> {
    let body = if options.regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let pattern = if options.whole_word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };

    RegexBuilder::new(&pattern)
        .case_insensitive(!options.case_sensitive)
        .multi_line(true)
        .build()
        .with_context(|| format!("invalid search pattern: {query}"))
}

fn expand_replacement(captures: &Captures<'_>, replacement: &str) -> String {
    let mut expanded = String::new();
    captures.expand(replacement, &mut expanded);
    expanded
}

/// Turns byte offsets into rows and columns for matches visited in order.
/// It only moves forward, so locating every match costs one pass over the
/// text instead of a rescan from the start per match.
struct LineCursor<'a> {
    source: &'a str,
    offset: usize,
    row: usize,
    line_start: usize,
    column_line_start: Option<usize>,
    column_starts: Vec<usize>,
}

impl<'a> LineCursor<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            row: 0,
            line_start: 0,
            column_line_start: None,
            column_starts: Vec::new(),
        }
    }

    fn search_match(&mut self, start_byte: usize, end_byte: usize) -> SearchMatch {
        let (row, col) = self.position(start_byte);
        let line_preview = self.line_preview();
        let (end_row, end_col) = self.position(end_byte);
        SearchMatch {
            start_byte,
            end_byte,
            row,
            col,
            end_row,
            end_col,
            line_preview,
        }
    }

    fn position(&mut self, byte: usize) -> (usize, usize) {
        let byte = byte.min(self.source.len());
        if byte < self.offset {
            *self = Self::new(self.source);
        }
        for (index, value) in self.source.as_bytes()[self.offset..byte].iter().enumerate() {
            if *value == b'\n' {
                self.row += 1;
                self.line_start = self.offset + index + 1;
            }
        }
        self.offset = byte;
        if self.column_line_start != Some(self.line_start) {
            let rest = &self.source[self.line_start..];
            let end = rest.find('\n').unwrap_or(rest.len());
            self.column_starts = rest[..end]
                .grapheme_indices(true)
                .map(|(start, _)| start)
                .collect();
            self.column_line_start = Some(self.line_start);
        }
        let col = self
            .column_starts
            .partition_point(|start| *start < byte - self.line_start);
        (self.row, col)
    }

    /// The current line, as `str::lines` would yield it.
    fn line_preview(&self) -> String {
        let rest = &self.source[self.line_start..];
        let line: String = rest
            .graphemes(true)
            .take_while(|grapheme| *grapheme != "\n" && *grapheme != "\r\n")
            .take(512)
            .collect();
        line.trim_end_matches('\r').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::collections::HashMap;
    use tempfile::tempdir;

    /// The straightforward per-match scan the cursor replaces.
    fn reference_position(source: &str, byte: usize) -> (usize, usize) {
        let prefix = &source[..byte];
        let row = prefix.matches('\n').count();
        let line_start = prefix.rfind('\n').map(|index| index + 1).unwrap_or(0);
        (row, source[line_start..byte].graphemes(true).count())
    }

    #[test]
    fn line_cursor_matches_a_full_rescan() {
        let source = "alpha e\u{301}x\r\nbeta x\n\nx 日本 x\r\nlast x";
        for (query, regex) in [("x", false), ("x\r?\n", true), ("\n\n", true)] {
            let options = SearchOptions {
                regex,
                ..SearchOptions::default()
            };
            for found in search_text(source, query, options, usize::MAX).unwrap() {
                let (row, col) = reference_position(source, found.start_byte);
                assert_eq!((found.row, found.col), (row, col), "{query:?} start");
                assert_eq!(
                    (found.end_row, found.end_col),
                    reference_position(source, found.end_byte),
                    "{query:?} end"
                );
                let expected = source.lines().nth(row).unwrap_or("").trim_end_matches('\r');
                assert_eq!(found.line_preview, expected, "{query:?} preview");
            }
        }
    }

    /// Audit F4: 1 MiB with 10,486 single-letter hits took 5.4 s because every
    /// match rescanned the text from the start.
    #[test]
    fn many_matches_in_a_large_file_are_located_in_one_pass() {
        let line = format!("q{}\n", "x".repeat(98));
        let source = line.repeat(20_972);
        let started = std::time::Instant::now();
        let found = search_text(&source, "q", SearchOptions::default(), usize::MAX).unwrap();
        assert_eq!(found.len(), 20_972);
        assert_eq!((found[20_971].row, found[20_971].col), (20_971, 0));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
    }

    use super::*;

    #[test]
    fn literal_search_is_case_insensitive_by_default_and_grapheme_aware() {
        let matches = search_text(
            "Alpha\n🙂 alpha తెలుగు\nALPHA",
            "alpha",
            SearchOptions::default(),
            20,
        )
        .unwrap();

        assert_eq!(matches.len(), 3);
        assert_eq!((matches[1].row, matches[1].col), (1, 2));
    }

    #[test]
    fn case_regex_and_unicode_whole_word_options_are_independent() {
        let options = SearchOptions {
            case_sensitive: true,
            regex: true,
            whole_word: true,
        };
        let matches = search_text("cat scatter cat42 cat", "cat", options, 20).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].col, 0);
        assert_eq!(matches[1].col, 18);
    }

    #[test]
    fn regex_replacement_expands_capture_groups_but_literal_dollar_is_literal() {
        let regex_options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        let replacements =
            replacement_matches("name=alice", r"name=(\w+)", "user:$1", regex_options, 10).unwrap();
        assert_eq!(replacements[0].replacement, "user:alice");

        let literal =
            replacement_matches("price", "price", "$1", SearchOptions::default(), 10).unwrap();
        assert_eq!(literal[0].replacement, "$1");
    }

    #[test]
    fn project_search_is_bounded_skips_binary_and_returns_exact_locations() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"needle🙂\");\n}\n",
        )
        .unwrap();
        fs::write(dir.path().join("README.md"), "Needle docs\n").unwrap();
        fs::write(dir.path().join("binary.bin"), [0xff, 0xfe, 0xfd]).unwrap();

        let report = search_project(
            dir.path(),
            &[
                PathBuf::from("src/main.rs"),
                PathBuf::from("README.md"),
                PathBuf::from("binary.bin"),
            ],
            "needle",
            SearchOptions::default(),
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(report.results.len(), 2);
        assert_eq!(
            report.skipped_binary, 1,
            "binary file is counted, not hidden"
        );
        let rust = report
            .results
            .iter()
            .find(|result| result.path == Path::new("src/main.rs"))
            .unwrap();
        assert_eq!((rust.row, rust.col), (1, 14));
        assert!(rust.line_preview.contains("needle"));
        assert!(!report.truncated);
    }

    #[test]
    fn f08_worker_only_returns_the_current_generation_and_cancels_without_waiting() {
        use std::time::{Duration, Instant};
        let dir = tempdir().unwrap();
        let request = |query: &str| ProjectSearchRequest {
            root: dir.path().to_path_buf(),
            files: Some(vec![PathBuf::from("new.rs")]),
            query: query.to_owned(),
            options: SearchOptions::default(),
            unsaved: HashMap::from([(PathBuf::from("new.rs"), "old new".repeat(100_000))]),
            scope: ProjectSearchScope::default(),
        };
        let mut worker = ProjectSearchWorker::default();
        worker.submit(request("old"));
        let previous_cancel = Arc::clone(worker.cancelled.as_ref().unwrap());
        let start = Instant::now();
        let generation = worker.submit(request("new"));
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "submission waited for filesystem scanning"
        );
        assert!(previous_cancel.load(Ordering::Relaxed));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let updates = worker.poll();
            assert!(updates.iter().all(|update| update.generation == generation));
            if let Some(done) = updates.into_iter().find(|update| update.finished) {
                let report = done.report.unwrap();
                assert!(
                    report
                        .results
                        .iter()
                        .all(|result| result.matched_text == "new")
                );
                assert_eq!(report.results.len(), MAX_PROJECT_RESULTS);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let start = Instant::now();
        worker.cancel();
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(worker.poll().is_empty());
    }

    #[test]
    fn f08_search_scope_filters_limits_and_prioritizes_unsaved_files() {
        let dir = tempdir().unwrap();
        let request = ProjectSearchRequest {
            root: dir.path().to_path_buf(),
            files: Some(vec![PathBuf::from("a.rs"), PathBuf::from("ignored.rs")]),
            query: "needle".to_owned(),
            options: SearchOptions::default(),
            unsaved: HashMap::from([
                (PathBuf::from("z.rs"), "needle".to_owned()),
                (PathBuf::from("ignored.rs"), "needle".to_owned()),
            ]),
            scope: ProjectSearchScope {
                include: vec!["*.rs".to_owned()],
                exclude: vec!["ignored*".to_owned()],
                max_files: 2,
                max_results: 1,
                ..ProjectSearchScope::default()
            },
        };
        let report = search_project_request(&request, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(report.results[0].path, PathBuf::from("z.rs"));
        assert_eq!(report.skipped_filtered, 1);
        assert!(report.truncated);
    }

    #[test]
    fn f08_worker_reports_effective_configured_scope_and_skips() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".mellow")).unwrap();
        fs::write(
            dir.path().join(".mellow/search.json"),
            r#"{"include":["*.rs"],"exclude":["ignored*"],"max_file_bytes":8}"#,
        )
        .unwrap();
        for (name, text) in [
            ("main.rs", "needle"),
            ("notes.md", "needle"),
            ("ignored.rs", "needle"),
            ("large.rs", "needle plus more"),
        ] {
            fs::write(dir.path().join(name), text).unwrap();
        }
        let mut worker = ProjectSearchWorker::default();
        worker.submit(ProjectSearchRequest {
            root: dir.path().to_path_buf(),
            files: Some(
                ["main.rs", "notes.md", "ignored.rs", "large.rs"]
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
            ),
            query: "needle".to_owned(),
            options: SearchOptions::default(),
            unsaved: HashMap::new(),
            scope: ProjectSearchScope::default(),
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(update) = worker.poll().into_iter().find(|update| update.finished) {
                let report = update.report.unwrap();
                assert_eq!(report.scope_summary, "include *.rs; exclude ignored*");
                assert_eq!(report.skipped_filtered, 2);
                assert_eq!(report.skipped_large, 1);
                assert_eq!(report.results.len(), 1);
                return;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn f08_project_search_configuration_is_bounded_and_validated() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".mellow")).unwrap();
        fs::write(dir.path().join(".mellow/search.json"), r#"{"include":["src/*.rs"],"exclude":["*generated*"],"max_results":7,"max_files":20,"max_file_bytes":1024,"max_total_bytes":4096}"#).unwrap();
        let scope =
            ProjectSearchScope::from_project(dir.path(), &ProjectSearchScope::default()).unwrap();
        assert_eq!(scope.include, vec!["src/*.rs"]);
        assert_eq!(scope.max_results, 7);
        fs::write(
            dir.path().join(".mellow/search.json"),
            r#"{"max_results":999999}"#,
        )
        .unwrap();
        assert!(
            ProjectSearchScope::from_project(dir.path(), &ProjectSearchScope::default()).is_err()
        );
        fs::write(
            dir.path().join(".mellow/search.json"),
            r#"{"include":[42]}"#,
        )
        .unwrap();
        assert!(
            ProjectSearchScope::from_project(dir.path(), &ProjectSearchScope::default()).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn f08_project_search_does_not_read_symlinks_outside_its_scope() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let root = dir.path().join("project");
        fs::create_dir(&root).unwrap();
        fs::write(dir.path().join("outside.txt"), "private needle").unwrap();
        fs::write(root.join("inside.txt"), "public needle").unwrap();
        symlink(dir.path().join("outside.txt"), root.join("leak.txt")).unwrap();
        symlink(root.join("inside.txt"), root.join("alias.txt")).unwrap();
        let report = search_project(
            &root,
            &[PathBuf::from("leak.txt"), PathBuf::from("alias.txt")],
            "needle",
            SearchOptions::default(),
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].path, PathBuf::from("alias.txt"));
        assert_eq!(report.skipped_unreadable, 1);
    }

    #[test]
    fn f08_long_line_previews_are_bounded_without_changing_match_positions() {
        let text = "a".repeat(100_000) + "needle";
        let results = search_text(&text, "needle", SearchOptions::default(), 1).unwrap();
        assert_eq!(results[0].col, 100_000);
        assert!(results[0].line_preview.len() <= 512);
    }

    #[test]
    fn f08_search_merges_new_unsaved_documents_into_discovery() {
        let dir = tempdir().unwrap();
        let unsaved =
            HashMap::from([(PathBuf::from("new.rs"), "needle in a new buffer".to_owned())]);
        let report = search_project(
            dir.path(),
            &[],
            "needle",
            SearchOptions::default(),
            &unsaved,
        )
        .unwrap();
        assert_eq!(
            report.results.len(),
            1,
            "new unsaved buffers must be searchable"
        );
        assert_eq!(report.unsaved_files, 1);
    }

    #[test]
    fn f08_unsaved_documents_obey_the_same_file_limits() {
        let dir = tempdir().unwrap();
        let unsaved = HashMap::from([(PathBuf::from("big.rs"), "needle ".repeat(400_000))]);
        let report = search_project(
            dir.path(),
            &[PathBuf::from("big.rs")],
            "needle",
            SearchOptions::default(),
            &unsaved,
        )
        .unwrap();
        assert!(report.results.is_empty());
        assert_eq!(report.skipped_large, 1);
    }

    #[test]
    fn invalid_regex_is_reported_without_panicking() {
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        assert!(search_text("abc", "(", options, 10).is_err());
        assert!(search_project(Path::new("."), &[], "(", options, &HashMap::new()).is_err());
    }

    /// Audit: project search read only disk and could look complete while
    /// skipping files.
    #[test]
    fn project_search_uses_unsaved_text_and_counts_skipped_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("open.rs"), "saved text\n").unwrap();
        fs::write(dir.path().join("big.txt"), "needle ".repeat(400_000)).unwrap();
        let files = [
            PathBuf::from("open.rs"),
            PathBuf::from("big.txt"),
            PathBuf::from("gone.rs"),
        ];
        let unsaved = HashMap::from([(PathBuf::from("open.rs"), "unsaved needle\n".to_owned())]);

        let report = search_project(
            dir.path(),
            &files,
            "needle",
            SearchOptions::default(),
            &unsaved,
        )
        .unwrap();
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].path, PathBuf::from("open.rs"));
        assert_eq!(report.unsaved_files, 1);
        assert_eq!(report.skipped_large, 1);
        assert_eq!(report.skipped_unreadable, 1);
    }
}
