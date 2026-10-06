use std::{
    fs,
    path::{Path, PathBuf},
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
pub fn search_project(
    root: &Path,
    files: &[PathBuf],
    query: &str,
    options: SearchOptions,
    unsaved: &std::collections::HashMap<PathBuf, String>,
) -> Result<ProjectSearchReport> {
    if query.is_empty() {
        return Ok(ProjectSearchReport::default());
    }

    let regex = compile_pattern(query, options)?;
    let mut report = ProjectSearchReport::default();

    for relative in files {
        if report.results.len() >= MAX_PROJECT_RESULTS
            || report.bytes_scanned >= MAX_PROJECT_TOTAL_BYTES
        {
            report.truncated = true;
            break;
        }

        let loaded;
        let source: &str = if let Some(text) = unsaved.get(relative) {
            report.unsaved_files += 1;
            text
        } else {
            let absolute = root.join(relative);
            let metadata = match fs::metadata(&absolute) {
                Ok(metadata) if metadata.is_file() => metadata,
                _ => {
                    report.skipped_unreadable += 1;
                    continue;
                }
            };
            if metadata.len() > MAX_PROJECT_FILE_BYTES {
                report.skipped_large += 1;
                continue;
            }
            if report.bytes_scanned.saturating_add(metadata.len()) > MAX_PROJECT_TOTAL_BYTES {
                report.truncated = true;
                break;
            }
            let Ok(bytes) = fs::read(&absolute) else {
                report.skipped_unreadable += 1;
                continue;
            };
            match String::from_utf8(bytes) {
                Ok(text) => {
                    loaded = text;
                    &loaded
                }
                Err(_) => {
                    report.skipped_binary += 1;
                    continue;
                }
            }
        };

        report.files_scanned = report.files_scanned.saturating_add(1);
        report.bytes_scanned = report.bytes_scanned.saturating_add(source.len() as u64);

        let mut lines = LineCursor::new(source);
        for matched in regex.find_iter(source) {
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

            if report.results.len() >= MAX_PROJECT_RESULTS {
                report.truncated = true;
                break;
            }
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
}

impl<'a> LineCursor<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            row: 0,
            line_start: 0,
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
        let col = self.source[self.line_start..byte].graphemes(true).count();
        (self.row, col)
    }

    /// The current line, as `str::lines` would yield it.
    fn line_preview(&self) -> String {
        let rest = &self.source[self.line_start..];
        let line = rest.split('\n').next().unwrap_or("");
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
