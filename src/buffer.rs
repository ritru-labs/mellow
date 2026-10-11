use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::cursor::Cursor;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const HISTORY_LIMIT: usize = 200;
/// How far away a bracket's partner is looked for. The pair is only drawn
/// when on screen, and an unbounded search walked the whole file on every
/// frame (most of the idle CPU with the cursor on a large JSON file's `[`).
const MAX_BRACKET_SCAN_CHARS: usize = 100_000;
/// A pause this long ends a typing run's shared undo step.
const TYPING_GROUP_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punctuation,
}

fn char_class(grapheme: &str) -> CharClass {
    let first = grapheme.chars().next().unwrap_or(' ');
    if first.is_whitespace() {
        CharClass::Space
    } else if first.is_alphanumeric() || first == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
            Self::Cr => "CR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot {
    len: u64,
    modified: Option<SystemTime>,
    content_hash: u64,
    device: u64,
    inode: u64,
    metadata_version: (u32, u32, u32, u64, i64, i64),
}

impl FileSnapshot {
    fn from_metadata_and_bytes(metadata: &fs::Metadata, bytes: &[u8]) -> Self {
        Self::from_metadata_and_hash(metadata, fnv1a(bytes))
    }

    fn from_metadata_and_hash(metadata: &fs::Metadata, content_hash: u64) -> Self {
        #[cfg(unix)]
        let (device, inode, metadata_version) = {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                (
                    metadata.uid(),
                    metadata.gid(),
                    metadata.mode(),
                    metadata.nlink(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                ),
            )
        };
        #[cfg(not(unix))]
        let (device, inode, metadata_version) = (0, 0, (0, 0, 0, 0, 0, 0));

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            content_hash,
            device,
            inode,
            metadata_version,
        }
    }
}

#[derive(Debug, Clone)]
struct BufferSnapshot {
    text: Rope,
    revision: u64,
    pre_cursor: Option<Cursor>,
    post_cursor: Option<Cursor>,
    pre_selection_anchor: Option<Cursor>,
    post_selection_anchor: Option<Cursor>,
}

#[derive(Debug)]
pub struct Buffer {
    /// Path shown to the user. This intentionally preserves the path they opened.
    path: Option<PathBuf>,
    /// Path written on save. Existing symlinks are resolved so an atomic save does
    /// not accidentally replace the symlink itself.
    save_path: Option<PathBuf>,
    text: Rope,
    line_ending: LineEnding,
    has_bom: bool,
    revision: u64,
    saved_revision: u64,
    /// How the most recent save reached the disk.
    pub last_save_mode: SaveMode,
    next_revision: u64,
    disk_snapshot: Option<FileSnapshot>,
    persisted: bool,
    undo: Vec<BufferSnapshot>,
    redo: Vec<BufferSnapshot>,
    /// Unique per buffer instance: names its recovery journal while it has
    /// no path, and tells async replies which buffer they belong to.
    draft_id: u64,
    /// When and where the last typed character landed, so a run of typing
    /// becomes one undo step instead of one per character.
    typing_run: Option<(std::time::Instant, Cursor)>,
    /// Indent with tabs: detected from the file when opened, or chosen.
    tab_indent: bool,
    indent_width: usize,
}

impl Buffer {
    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::empty(None));
        };

        if !path.exists() {
            if fs::symlink_metadata(&path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                bail!(
                    "{} is a broken symlink; refusing to replace it",
                    path.display()
                );
            }
            return Ok(Self::empty(Some(path)));
        }

        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        if metadata.is_dir() {
            bail!("{} is a directory", path.display());
        }
        if !metadata.is_file() {
            bail!("{} is not a regular file", path.display());
        }
        const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
        if metadata.len() > MAX_FILE_SIZE {
            bail!(
                "{} is larger than 100 MB; Mellow opens files up to 100 MB",
                path.display()
            );
        }

        let bytes =
            fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let has_bom = bytes.starts_with(UTF8_BOM);
        let payload = if has_bom {
            &bytes[UTF8_BOM.len()..]
        } else {
            &bytes
        };
        let content = std::str::from_utf8(payload).with_context(|| {
            format!(
                "{} is not UTF-8 text; Mellow opens UTF-8 files only (binary files and other encodings are not supported)",
                path.display()
            )
        })?;

        let save_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let disk_snapshot = Some(FileSnapshot::from_metadata_and_bytes(&metadata, &bytes));

        let mut buffer = Self {
            path: Some(path),
            save_path: Some(save_path),
            text: Rope::from_str(content),
            line_ending: detect_line_ending(content),
            has_bom,
            revision: 0,
            saved_revision: 0,
            last_save_mode: SaveMode::Atomic,
            next_revision: 1,
            disk_snapshot,
            persisted: true,
            undo: Vec::new(),
            redo: Vec::new(),
            draft_id: crate::recovery::new_draft_id(),
            typing_run: None,
            tab_indent: false,
            indent_width: 4,
        };
        buffer.tab_indent = buffer.detect_tab_indent();
        buffer.configure_indentation();
        Ok(buffer)
    }

    pub fn empty(path: Option<PathBuf>) -> Self {
        let mut buffer = Self {
            save_path: path.clone(),
            path,
            text: Rope::new(),
            line_ending: LineEnding::Lf,
            has_bom: false,
            revision: 0,
            saved_revision: 0,
            last_save_mode: SaveMode::Atomic,
            next_revision: 1,
            disk_snapshot: None,
            persisted: false,
            undo: Vec::new(),
            redo: Vec::new(),
            draft_id: crate::recovery::new_draft_id(),
            typing_run: None,
            tab_indent: false,
            indent_width: 4,
        };
        buffer.tab_indent = buffer.detect_tab_indent();
        buffer.configure_indentation();
        buffer
    }

    /// Makefiles need tabs; otherwise follow what most indented lines use.
    fn detect_tab_indent(&self) -> bool {
        let name = self
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(name.as_str(), "makefile" | "gnumakefile") || name.ends_with(".mk") {
            return true;
        }
        let (mut tabs, mut spaces) = (0usize, 0usize);
        for row in 0..self.line_count().min(1_000) {
            let line = self.text.line(row);
            match line.chars().next() {
                Some('\t') => tabs += 1,
                Some(' ') if line.chars().nth(1) == Some(' ') => spaces += 1,
                _ => {}
            }
        }
        tabs > spaces
    }

    pub fn uses_tab_indent(&self) -> bool {
        self.tab_indent
    }

    pub fn set_tab_indent(&mut self, tabs: bool) {
        self.tab_indent = tabs;
    }

    /// One indentation step as text.
    pub fn indent_unit(&self) -> &'static str {
        if self.tab_indent {
            return "\t";
        }
        const SPACES: [&str; 9] = [
            "", " ", "  ", "   ", "    ", "     ", "      ", "       ", "        ",
        ];
        SPACES[self.indent_width.clamp(1, 8)]
    }

    pub fn indent_width(&self) -> usize {
        self.indent_width
    }
    pub fn set_indent_width(&mut self, width: usize) {
        self.indent_width = width.clamp(1, 8);
    }

    fn configure_indentation(&mut self) {
        let configured = crate::settings::load().unwrap_or_default().indent_width;
        let mut inferred = 0;
        for line in self.text.lines().take(1000) {
            let n = line.chars().take_while(|c| *c == ' ').count();
            if n == 0 || n > 64 {
                continue;
            }
            inferred = if inferred == 0 {
                n
            } else {
                gcd_indent(inferred, n)
            };
        }
        self.indent_width = if (1..=8).contains(&inferred) {
            inferred
        } else {
            configured
        };
        if let Some(path) = &self.path {
            let policy = crate::settings::project_indentation(path);
            if let Some(width) = policy.0 {
                self.indent_width = width;
            }
            if let Some(tabs) = policy.1 {
                self.tab_indent = tabs;
            }
        }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// Unique per buffer instance (two tabs never share it).
    pub fn identity(&self) -> u64 {
        self.draft_id
    }

    /// The journal that protects this buffer's unsaved edits.
    pub fn recovery_key(&self) -> crate::recovery::RecoveryKey {
        match &self.path {
            Some(path) => crate::recovery::RecoveryKey::File(path.clone()),
            None => crate::recovery::RecoveryKey::Draft(self.draft_id),
        }
    }

    pub fn display_path(&self) -> String {
        self.path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_owned())
    }

    pub fn language(&self) -> &'static str {
        language_for_path(self.path.as_deref())
    }
}

fn gcd_indent(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// The language of a file name, as `Buffer::language` reports it.
pub fn language_for_path(path: Option<&Path>) -> &'static str {
    let Some(path) = path else {
        return "Plain Text";
    };
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return "Plain Text";
    };
    match ext.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "py" => "Python",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" | "mts" | "cts" => "TypeScript",
        "jsx" => "React JSX",
        "tsx" => "React TSX",
        "json" => "JSON",
        "toml" => "TOML",
        "tf" | "tfvars" => "Terraform",
        "yaml" | "yml" => "YAML",
        "md" | "markdown" => "Markdown",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" => "CSS",
        "sh" | "bash" | "zsh" => "Shell",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" => "C++",
        "go" => "Go",
        "java" => "Java",
        "sql" => "SQL",
        "xml" | "svg" => "XML",
        _ => "Plain Text",
    }
}

impl Buffer {
    pub const fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub const fn has_bom(&self) -> bool {
        self.has_bom
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub const fn is_persisted(&self) -> bool {
        self.persisted
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn byte_len(&self) -> usize {
        self.text.len_bytes()
    }

    pub fn reduced_intelligence_mode(&self) -> bool {
        const LARGE_FILE_INTELLIGENCE_LIMIT: usize = 5 * 1024 * 1024;
        self.byte_len() > LARGE_FILE_INTELLIGENCE_LIMIT
    }

    pub fn contents(&self) -> String {
        self.text.to_string()
    }

    pub fn restore_recovery_text(&mut self, content: &str) {
        self.text = Rope::from_str(content);
        self.line_ending = detect_line_ending(content);
        self.undo.clear();
        self.redo.clear();
        self.revision = self.next_revision;
        self.next_revision = self.next_revision.wrapping_add(1).max(1);
    }

    pub fn line_count(&self) -> usize {
        self.text.len_lines().max(1)
    }

    pub fn line_text(&self, row: usize) -> String {
        if row >= self.text.len_lines() {
            return String::new();
        }
        strip_line_ending(self.text.line(row).to_string())
    }

    pub fn grapheme_count(&self, row: usize) -> usize {
        self.line_text(row).graphemes(true).count()
    }

    pub fn grapheme_at(&self, row: usize, col: usize) -> Option<String> {
        self.line_text(row)
            .graphemes(true)
            .nth(col)
            .map(str::to_owned)
    }

    pub fn utf16_col_for_grapheme(&self, row: usize, grapheme_col: usize) -> usize {
        self.line_text(row)
            .graphemes(true)
            .take(grapheme_col)
            .map(|grapheme| grapheme.encode_utf16().count())
            .sum()
    }

    pub fn grapheme_col_for_utf16(&self, row: usize, utf16_col: usize) -> usize {
        let line = self.line_text(row);
        let mut units = 0usize;
        for (index, grapheme) in line.graphemes(true).enumerate() {
            let next = units.saturating_add(grapheme.encode_utf16().count());
            if utf16_col < next {
                return index;
            }
            units = next;
        }
        line.graphemes(true).count()
    }

    pub fn word_range(&self, row: usize, col: usize) -> Option<(Cursor, Cursor)> {
        let line = self.line_text(row);
        let graphemes: Vec<&str> = line.graphemes(true).collect();
        if graphemes.is_empty() {
            return None;
        }

        let index = col.min(graphemes.len().saturating_sub(1));
        if graphemes[index].chars().all(char::is_whitespace) {
            return None;
        }

        let is_word = |grapheme: &str| grapheme.chars().any(|ch| ch.is_alphanumeric() || ch == '_');
        let target_is_word = is_word(graphemes[index]);

        let mut start = index;
        while start > 0
            && !graphemes[start - 1].chars().all(char::is_whitespace)
            && is_word(graphemes[start - 1]) == target_is_word
        {
            start -= 1;
        }

        let mut end = index + 1;
        while end < graphemes.len()
            && !graphemes[end].chars().all(char::is_whitespace)
            && is_word(graphemes[end]) == target_is_word
        {
            end += 1;
        }

        Some((Cursor::new(row, start), Cursor::new(row, end)))
    }

    pub fn matching_bracket(&self, cursor: Cursor) -> Option<(Cursor, Cursor)> {
        let len = self.text.len_chars();
        if len == 0 {
            return None;
        }
        let index = self.char_index(cursor.row, cursor.col).min(len);
        let mut bracket = None;
        for candidate in [Some(index).filter(|idx| *idx < len), index.checked_sub(1)] {
            let Some(candidate) = candidate else {
                continue;
            };
            let ch = self.text.char(candidate);
            if matches!(ch, '(' | ')' | '[' | ']' | '{' | '}') {
                bracket = Some((candidate, ch));
                break;
            }
        }
        let (origin_index, origin) = bracket?;
        let (partner, forward) = match origin {
            '(' => (')', true),
            '[' => (']', true),
            '{' => ('}', true),
            ')' => ('(', false),
            ']' => ('[', false),
            '}' => ('{', false),
            _ => return None,
        };

        // Stream characters from the rope instead of indexing each one.
        let chars = if forward {
            self.text.chars_at(origin_index + 1)
        } else {
            self.text.chars_at(origin_index).reversed()
        };
        let mut depth = 1usize;
        let mut distance = None;
        for (offset, ch) in chars.take(MAX_BRACKET_SCAN_CHARS).enumerate() {
            if ch == origin {
                depth += 1;
            } else if ch == partner {
                depth -= 1;
                if depth == 0 {
                    distance = Some(offset + 1);
                    break;
                }
            }
        }
        let distance = distance?;
        let target_index = if forward {
            origin_index + distance
        } else {
            origin_index - distance
        };

        Some((
            self.cursor_from_char_index(origin_index),
            self.cursor_from_char_index(target_index),
        ))
    }

    pub fn display_width_before(&self, row: usize, grapheme_col: usize, tab_width: usize) -> usize {
        let line = self.line_text(row);
        let mut width = 0;
        for grapheme in line.graphemes(true).take(grapheme_col) {
            if grapheme == "\t" {
                width += tab_width - (width % tab_width);
            } else {
                width += UnicodeWidthStr::width(grapheme);
            }
        }
        width
    }

    pub fn grapheme_col_at_display_column(
        &self,
        row: usize,
        target_column: usize,
        tab_width: usize,
    ) -> usize {
        let line = self.line_text(row);
        let mut display_column = 0usize;
        let mut grapheme_col = 0usize;

        for grapheme in line.graphemes(true) {
            let width = if grapheme == "\t" {
                tab_width - (display_column % tab_width)
            } else {
                UnicodeWidthStr::width(grapheme).max(1)
            };

            if target_column < display_column + width {
                return grapheme_col;
            }

            display_column += width;
            grapheme_col += 1;
        }

        grapheme_col
    }

    pub fn insert_char(&mut self, cursor: &mut Cursor, ch: char) {
        // Continue the current undo step while typing carries on at the same
        // spot; a pause, a space or any other edit starts a new one.
        let continues = !ch.is_whitespace()
            && self.redo.is_empty()
            && !self.undo.is_empty()
            && self
                .typing_run
                .is_some_and(|(at, after)| after == *cursor && at.elapsed() < TYPING_GROUP_PAUSE);
        if !continues {
            self.checkpoint(Some(*cursor));
        }
        let index = self.char_index(cursor.row, cursor.col);
        self.text.insert_char(index, ch);
        cursor.col = self
            .grapheme_col_for_char_offset(cursor.row, self.char_offset_after(index, cursor.row));
        self.mark_edited(Some(*cursor));
        self.typing_run = Some((std::time::Instant::now(), *cursor));
    }

    /// Where Ctrl+Left lands: the start of the previous word (or run of
    /// punctuation), crossing to the previous line end at column 0.
    pub fn word_left(&self, cursor: Cursor) -> Cursor {
        if cursor.col == 0 {
            if cursor.row == 0 {
                return cursor;
            }
            let row = cursor.row - 1;
            return Cursor::new(row, self.grapheme_count(row));
        }
        let line = self.line_text(cursor.row);
        let classes: Vec<CharClass> = line.graphemes(true).map(char_class).collect();
        let mut col = cursor.col.min(classes.len());
        while col > 0 && classes[col - 1] == CharClass::Space {
            col -= 1;
        }
        if col > 0 {
            let class = classes[col - 1];
            while col > 0 && classes[col - 1] == class {
                col -= 1;
            }
        }
        Cursor::new(cursor.row, col)
    }

    /// Where Ctrl+Right lands: the end of the next word (or run of
    /// punctuation), crossing to the next line start at the line end.
    pub fn word_right(&self, cursor: Cursor) -> Cursor {
        let line = self.line_text(cursor.row);
        let classes: Vec<CharClass> = line.graphemes(true).map(char_class).collect();
        if cursor.col >= classes.len() {
            if cursor.row + 1 >= self.line_count() {
                return Cursor::new(cursor.row, classes.len());
            }
            return Cursor::new(cursor.row + 1, 0);
        }
        let mut col = cursor.col;
        while col < classes.len() && classes[col] == CharClass::Space {
            col += 1;
        }
        if col < classes.len() {
            let class = classes[col];
            while col < classes.len() && classes[col] == class {
                col += 1;
            }
        }
        Cursor::new(cursor.row, col)
    }

    /// Column of the first non-blank character, for smart Home.
    pub fn first_non_blank(&self, row: usize) -> usize {
        self.line_text(row)
            .graphemes(true)
            .take_while(|grapheme| grapheme.chars().all(char::is_whitespace))
            .count()
    }

    pub fn insert_text(&mut self, cursor: &mut Cursor, text: &str) {
        if text.is_empty() {
            return;
        }

        let normalized = normalize_insert_text(text, self.line_ending);
        if normalized.is_empty() {
            return;
        }

        self.checkpoint(Some(*cursor));
        let index = self.char_index(cursor.row, cursor.col);
        self.text.insert(index, &normalized);
        let target = index + normalized.chars().count();
        cursor.row = self.text.char_to_line(target.min(self.text.len_chars()));
        let line_start = self.text.line_to_char(cursor.row);
        cursor.col =
            self.grapheme_col_for_char_offset(cursor.row, target.saturating_sub(line_start));
        self.mark_edited(Some(*cursor));
    }

    pub fn backspace(&mut self, cursor: &mut Cursor) {
        if cursor.col > 0 {
            let end = self.char_index(cursor.row, cursor.col);
            let start = self.char_index(cursor.row, cursor.col - 1);
            if start < end {
                self.checkpoint(Some(*cursor));
                self.text.remove(start..end);
                cursor.col -= 1;
                self.mark_edited(Some(*cursor));
            }
            return;
        }

        if cursor.row == 0 {
            return;
        }

        let previous_row = cursor.row - 1;
        let previous_col = self.grapheme_count(previous_row);
        let current_start = self.text.line_to_char(cursor.row);
        let terminator_len = self.line_terminator_chars(previous_row);
        if terminator_len > 0 {
            self.checkpoint(Some(*cursor));
            self.text
                .remove(current_start.saturating_sub(terminator_len)..current_start);
            cursor.row = previous_row;
            cursor.col = previous_col;
            self.mark_edited(Some(*cursor));
        }
    }

    pub fn delete(&mut self, cursor: &Cursor) {
        let line_graphemes = self.grapheme_count(cursor.row);
        if cursor.col < line_graphemes {
            let start = self.char_index(cursor.row, cursor.col);
            let end = self.char_index(cursor.row, cursor.col + 1);
            if start < end {
                self.checkpoint(Some(*cursor));
                self.text.remove(start..end);
                self.mark_edited(Some(*cursor));
            }
            return;
        }

        if cursor.row + 1 >= self.line_count() {
            return;
        }

        let line_end = self.text.line_to_char(cursor.row) + self.line_content_chars(cursor.row);
        let terminator_len = self.line_terminator_chars(cursor.row);
        if terminator_len > 0 {
            self.checkpoint(Some(*cursor));
            self.text.remove(line_end..line_end + terminator_len);
            self.mark_edited(Some(*cursor));
        }
    }

    #[cfg(test)]
    pub fn undo(&mut self, cursor: &mut Cursor) -> bool {
        let mut selection_anchor = None;
        self.undo_with_selection(cursor, &mut selection_anchor)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_with_selection(
        &mut self,
        cursor: &mut Cursor,
        selection_anchor: &mut Option<Cursor>,
    ) -> bool {
        self.typing_run = None;
        let Some(previous) = self.undo.pop() else {
            return false;
        };

        if self.redo.len() >= HISTORY_LIMIT {
            self.redo.remove(0);
        }
        self.redo.push(BufferSnapshot {
            text: self.text.clone(),
            revision: self.revision,
            pre_cursor: previous.pre_cursor,
            post_cursor: previous.post_cursor,
            pre_selection_anchor: previous.pre_selection_anchor,
            post_selection_anchor: previous.post_selection_anchor,
        });

        self.text = previous.text;
        self.revision = previous.revision;
        if let Some(pos) = previous.pre_cursor {
            *cursor = pos;
        }
        *selection_anchor = previous.pre_selection_anchor;
        true
    }

    #[cfg(test)]
    pub fn redo(&mut self, cursor: &mut Cursor) -> bool {
        let mut selection_anchor = None;
        self.redo_with_selection(cursor, &mut selection_anchor)
    }

    pub fn redo_with_selection(
        &mut self,
        cursor: &mut Cursor,
        selection_anchor: &mut Option<Cursor>,
    ) -> bool {
        self.typing_run = None;
        let Some(next) = self.redo.pop() else {
            return false;
        };

        if self.undo.len() >= HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(BufferSnapshot {
            text: self.text.clone(),
            revision: self.revision,
            pre_cursor: next.pre_cursor,
            post_cursor: next.post_cursor,
            pre_selection_anchor: next.pre_selection_anchor,
            post_selection_anchor: next.post_selection_anchor,
        });

        self.text = next.text;
        self.revision = next.revision;
        if let Some(pos) = next.post_cursor {
            *cursor = pos;
        }
        *selection_anchor = next.post_selection_anchor;
        true
    }

    /// Why saving now would overwrite something this buffer has not seen,
    /// comparing the file's full content. Used before every save.
    pub fn external_conflict(&self) -> Result<Option<String>> {
        self.conflict_with_disk(false)
    }

    /// The same question for the once-a-second poll of open files. When the
    /// size, mtime and file identity are unchanged and the last change is
    /// old enough for any filesystem clock to have ticked, the content is
    /// not re-read: the poll used to read and hash every open file each
    /// second. Saving still compares content, so nothing can be overwritten
    /// on the strength of this check.
    pub fn external_change(&self) -> Result<Option<String>> {
        self.conflict_with_disk(true)
    }

    fn conflict_with_disk(&self, trust_metadata: bool) -> Result<Option<String>> {
        let Some(save_path) = self.save_path.as_ref() else {
            return Ok(None);
        };

        if let Some(display_path) = self.path.as_ref()
            && fs::symlink_metadata(display_path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
        {
            match fs::canonicalize(display_path) {
                Ok(current_target) if current_target != *save_path => {
                    return Ok(Some(format!(
                        "symlink target changed from {} to {}",
                        save_path.display(),
                        current_target.display()
                    )));
                }
                Err(error) => {
                    return Ok(Some(format!(
                        "symlink target can no longer be resolved: {error}"
                    )));
                }
                _ => {}
            }
        }

        let current = match (&self.disk_snapshot, trust_metadata) {
            (Some(expected), true) => unchanged_snapshot(save_path, expected)?,
            _ => None,
        };
        let current = match current {
            Some(snapshot) => Some(snapshot),
            None => capture_file_snapshot(save_path)?,
        };
        match (&self.disk_snapshot, &current) {
            (None, None) => Ok(None),
            (None, Some(_)) => Ok(Some(
                "destination was created by another program after this buffer was opened"
                    .to_owned(),
            )),
            (Some(_), None) => Ok(Some(
                "file was deleted or moved by another program after it was opened".to_owned(),
            )),
            (Some(expected), Some(actual)) if expected != actual => Ok(Some(
                "file identity or content changed on disk since it was opened/saved".to_owned(),
            )),
            _ => Ok(None),
        }
    }

    /// True when the file on disk does not allow writing: no write bits
    /// (`chmod 444`), no write permission for this user, or a read-only
    /// filesystem. A save would still replace such a file (only the folder
    /// needs to be writable), so the editor asks first.
    pub fn read_only_on_disk(&self) -> bool {
        let Some(path) = self.save_path.as_ref() else {
            return false;
        };
        let Ok(metadata) = fs::metadata(path) else {
            return false;
        };
        if metadata.permissions().readonly() {
            return true;
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            if let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
                // SAFETY: c_path is a valid NUL-terminated path.
                return unsafe { libc::access(c_path.as_ptr(), libc::W_OK) } != 0;
            }
        }
        false
    }

    /// How the most recent save reached the disk.
    pub fn last_save_mode(&self) -> SaveMode {
        self.last_save_mode
    }

    pub fn save(&mut self) -> Result<()> {
        if let Some(reason) = self.external_conflict()? {
            bail!("conflict: {reason}");
        }
        self.save_against_snapshot(self.disk_snapshot.clone())
    }

    pub fn force_save(&mut self) -> Result<()> {
        let path = self
            .save_path
            .as_ref()
            .context("this buffer has no path yet; use Save As")?;
        // Explicit overwrite accepts the current version, never a version
        // that appears later while preparing the replacement.
        self.save_against_snapshot(capture_file_snapshot(path)?)
    }

    fn save_against_snapshot(&mut self, expected: Option<FileSnapshot>) -> Result<()> {
        let path = self
            .save_path
            .clone()
            .context("this buffer has no path yet; use Save As")?;

        let (mode, snapshot) =
            atomic_write(&path, self.has_bom, &self.text, Publish::Replace, expected)?;
        self.last_save_mode = mode;
        self.saved_revision = self.revision;
        self.disk_snapshot = Some(snapshot);
        self.persisted = true;
        Ok(())
    }

    pub fn save_as(&mut self, path: PathBuf) -> Result<()> {
        if path.as_os_str().is_empty() {
            bail!("save path cannot be empty");
        }

        if self.path.as_ref() == Some(&path) {
            return self.save();
        }

        if path.exists() {
            bail!(
                "destination {} already exists; choose a different path",
                path.display()
            );
        }

        if fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            bail!(
                "{} is a broken symlink; refusing to replace it",
                path.display()
            );
        }

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        if !parent.as_os_str().is_empty() {
            let metadata = fs::metadata(parent)
                .with_context(|| format!("failed to access parent {}", parent.display()))?;
            if !metadata.is_dir() {
                bail!("{} is not a directory", parent.display());
            }
        }

        let (mode, snapshot) =
            atomic_write(&path, self.has_bom, &self.text, Publish::NewFileOnly, None)?;
        self.last_save_mode = mode;

        self.path = Some(path.clone());
        self.save_path = Some(path.clone());
        self.saved_revision = self.revision;
        self.disk_snapshot = Some(snapshot);
        self.persisted = true;
        Ok(())
    }

    fn checkpoint(&mut self, cursor: Option<Cursor>) {
        self.typing_run = None;
        if self.undo.len() >= HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(BufferSnapshot {
            text: self.text.clone(),
            revision: self.revision,
            pre_cursor: cursor,
            post_cursor: cursor,
            pre_selection_anchor: None,
            post_selection_anchor: None,
        });
        self.redo.clear();
    }

    fn mark_edited(&mut self, cursor: Option<Cursor>) {
        self.revision = self.next_revision;
        self.next_revision = self.next_revision.wrapping_add(1).max(1);
        if let Some(last) = self.undo.last_mut()
            && cursor.is_some()
        {
            last.post_cursor = cursor;
        }
    }

    pub fn char_index(&self, row: usize, grapheme_col: usize) -> usize {
        if self.text.len_chars() == 0 {
            return 0;
        }
        let safe_row = row.min(self.text.len_lines().saturating_sub(1));
        let line_start = self.text.line_to_char(safe_row);
        let line = self.line_text(safe_row);
        let line_graphemes = line.graphemes(true).count();
        if grapheme_col >= line_graphemes {
            if safe_row + 1 >= self.text.len_lines() {
                self.text.len_chars()
            } else {
                line_start + line.chars().count()
            }
        } else {
            let byte_offset = line
                .grapheme_indices(true)
                .nth(grapheme_col)
                .map(|(offset, _)| offset)
                .unwrap_or(line.len());
            line_start + line[..byte_offset].chars().count()
        }
    }

    pub fn cursor_from_char_index(&self, index: usize) -> Cursor {
        let index = index.min(self.text.len_chars());
        let row = if index == self.text.len_chars() && self.text.len_chars() > 0 {
            self.text.len_lines().saturating_sub(1)
        } else {
            self.text.char_to_line(index)
        };
        let line_start = self.text.line_to_char(row);
        let col = self.grapheme_col_for_char_offset(row, index.saturating_sub(line_start));
        Cursor::new(row, col)
    }

    pub fn replace_range(
        &mut self,
        start: Cursor,
        end: Cursor,
        replacement: &str,
        cursor: &mut Cursor,
    ) {
        let (s_pos, e_pos) =
            if start.row < end.row || (start.row == end.row && start.col <= end.col) {
                (start, end)
            } else {
                (end, start)
            };
        let s = self.char_index(s_pos.row, s_pos.col);
        let e = self.char_index(e_pos.row, e_pos.col);
        if s >= e || e > self.text.len_chars() {
            return;
        }

        let normalized = normalize_insert_text(replacement, self.line_ending);
        self.checkpoint(Some(*cursor));
        self.text.remove(s..e);
        if !normalized.is_empty() {
            self.text.insert(s, &normalized);
        }

        let target = s + normalized.chars().count();
        cursor.row = self.text.char_to_line(target.min(self.text.len_chars()));
        let line_start = self.text.line_to_char(cursor.row);
        cursor.col =
            self.grapheme_col_for_char_offset(cursor.row, target.saturating_sub(line_start));
        self.mark_edited(Some(*cursor));
    }

    pub fn replace_ranges_with_cursors(
        &mut self,
        replacements: &[(Cursor, Cursor, String)],
        cursor: Cursor,
    ) -> Vec<Cursor> {
        if replacements.is_empty() {
            return Vec::new();
        }

        let mut indexed = Vec::with_capacity(replacements.len());
        for (start, end, replacement) in replacements {
            let (start, end) = if start <= end {
                (*start, *end)
            } else {
                (*end, *start)
            };
            let start_index = self.char_index(start.row, start.col);
            let end_index = self.char_index(end.row, end.col);
            if start_index > end_index || end_index > self.text.len_chars() {
                continue;
            }
            let normalized = normalize_insert_text(replacement, self.line_ending);
            if start_index == end_index && normalized.is_empty() {
                continue;
            }
            indexed.push((start_index, end_index, normalized));
        }

        indexed.sort_by_key(|(start, end, _)| (*start, *end));
        let mut previous_end = 0usize;
        indexed.retain(|(start, end, _)| {
            let keep = *start >= previous_end;
            if keep {
                previous_end = *end;
            }
            keep
        });
        if indexed.is_empty() {
            return Vec::new();
        }

        self.checkpoint(Some(cursor));
        for (start, end, replacement) in indexed.iter().rev() {
            self.text.remove(*start..*end);
            if !replacement.is_empty() {
                self.text.insert(*start, replacement);
            }
        }

        let mut delta_before: isize = 0;
        let mut cursors = Vec::with_capacity(indexed.len());
        for (start, end, replacement) in &indexed {
            let replacement_len = replacement.chars().count();
            let removed = end.saturating_sub(*start);
            let final_index =
                (*start as isize + delta_before + replacement_len as isize).max(0) as usize;
            cursors.push(self.cursor_from_char_index(final_index));
            delta_before += replacement_len as isize - removed as isize;
        }
        self.mark_edited(cursors.first().copied().or(Some(cursor)));
        cursors
    }

    pub fn replace_ranges(
        &mut self,
        replacements: &[(Cursor, Cursor, String)],
        cursor: Cursor,
    ) -> usize {
        if replacements.is_empty() {
            return 0;
        }

        let mut indexed = Vec::with_capacity(replacements.len());
        for (start, end, replacement) in replacements {
            let (start, end) =
                if start.row < end.row || (start.row == end.row && start.col <= end.col) {
                    (*start, *end)
                } else {
                    (*end, *start)
                };
            let start_index = self.char_index(start.row, start.col);
            let end_index = self.char_index(end.row, end.col);
            if start_index > end_index || end_index > self.text.len_chars() {
                continue;
            }
            if start_index == end_index && replacement.is_empty() {
                continue;
            }
            indexed.push((
                start_index,
                end_index,
                normalize_insert_text(replacement, self.line_ending),
            ));
        }

        if indexed.is_empty() {
            return 0;
        }

        indexed.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
        let mut previous_start = self.text.len_chars().saturating_add(1);
        indexed.retain(|(start, end, _)| {
            let keep = *end <= previous_start;
            if keep {
                previous_start = *start;
            }
            keep
        });

        self.checkpoint(Some(cursor));
        for (start, end, replacement) in &indexed {
            self.text.remove(*start..*end);
            if !replacement.is_empty() {
                self.text.insert(*start, replacement);
            }
        }
        self.mark_edited(Some(cursor));
        indexed.len()
    }

    pub fn set_last_history_state(
        &mut self,
        pre_cursor: Cursor,
        pre_selection_anchor: Option<Cursor>,
        post_cursor: Cursor,
        post_selection_anchor: Option<Cursor>,
    ) {
        if let Some(last) = self.undo.last_mut() {
            last.pre_cursor = Some(pre_cursor);
            last.pre_selection_anchor = pre_selection_anchor;
            last.post_cursor = Some(post_cursor);
            last.post_selection_anchor = post_selection_anchor;
        }
    }

    pub fn get_text_range(&self, start: Cursor, end: Cursor) -> String {
        let (s_pos, e_pos) =
            if start.row < end.row || (start.row == end.row && start.col <= end.col) {
                (start, end)
            } else {
                (end, start)
            };
        let s = self.char_index(s_pos.row, s_pos.col);
        let e = self.char_index(e_pos.row, e_pos.col);
        if s < e && e <= self.text.len_chars() {
            self.text.slice(s..e).to_string()
        } else {
            String::new()
        }
    }

    pub fn delete_range(&mut self, start: Cursor, end: Cursor) {
        let (s_pos, e_pos) =
            if start.row < end.row || (start.row == end.row && start.col <= end.col) {
                (start, end)
            } else {
                (end, start)
            };
        let s = self.char_index(s_pos.row, s_pos.col);
        let e = self.char_index(e_pos.row, e_pos.col);
        if s < e && e <= self.text.len_chars() {
            self.checkpoint(Some(s_pos));
            self.text.remove(s..e);
            self.mark_edited(Some(s_pos));
        }
    }

    pub fn toggle_comment_lines(&mut self, start_row: usize, end_row: usize) -> bool {
        let Some(marker) = line_comment_marker(self.language()) else {
            return false;
        };
        let max_line = self.line_count().saturating_sub(1);
        let start_row = start_row.min(max_line);
        let end_row = end_row.min(max_line);
        let non_blank: Vec<usize> = (start_row..=end_row)
            .filter(|&row| !self.line_text(row).trim().is_empty())
            .collect();
        if non_blank.is_empty() {
            return false;
        }

        let all_commented = non_blank.iter().all(|&row| {
            let line = self.line_text(row);
            line.trim_start().starts_with(marker)
        });

        self.checkpoint(None);
        for row in non_blank.into_iter().rev() {
            let line = self.line_text(row);
            let leading_bytes = line.len() - line.trim_start().len();
            let leading_chars = line[..leading_bytes].chars().count();
            let index = self.text.line_to_char(row) + leading_chars;

            if all_commented {
                let rest = &line[leading_bytes..];
                let mut remove_chars = marker.chars().count();
                if rest[marker.len()..].starts_with(' ') {
                    remove_chars += 1;
                }
                self.text.remove(index..index + remove_chars);
            } else {
                self.text.insert(index, &format!("{marker} "));
            }
        }
        self.mark_edited(None);
        true
    }

    pub fn move_lines_up(&mut self, start_row: usize, end_row: usize) -> bool {
        self.move_line_block(start_row, end_row, -1)
    }

    pub fn move_lines_down(&mut self, start_row: usize, end_row: usize) -> bool {
        self.move_line_block(start_row, end_row, 1)
    }

    fn move_line_block(&mut self, start_row: usize, end_row: usize, direction: i8) -> bool {
        let line_count = self.line_count();
        if line_count <= 1 {
            return false;
        }

        let start = start_row.min(line_count - 1);
        let end = end_row.min(line_count - 1);
        if start > end || (direction < 0 && start == 0) || (direction > 0 && end + 1 >= line_count)
        {
            return false;
        }

        let mut contents: Vec<String> = (0..line_count).map(|row| self.line_text(row)).collect();
        if direction < 0 {
            let previous = contents.remove(start - 1);
            contents.insert(end, previous);
        } else {
            let next = contents.remove(end + 1);
            contents.insert(start, next);
        }

        let endings: Vec<String> = (0..line_count)
            .map(|row| {
                let raw = self.text.line(row).to_string();
                let content_len = self.line_text(row).len();
                raw[content_len..].to_owned()
            })
            .collect();

        let mut rebuilt = String::new();
        for (content, ending) in contents.into_iter().zip(endings) {
            rebuilt.push_str(&content);
            rebuilt.push_str(&ending);
        }

        self.checkpoint(None);
        self.text = Rope::from_str(&rebuilt);
        self.mark_edited(None);
        true
    }

    pub fn duplicate_lines(&mut self, start_row: usize, end_row: usize) -> bool {
        let line_count = self.line_count();
        if line_count == 0 {
            return false;
        }
        let start = start_row.min(line_count - 1);
        let end = end_row.min(line_count - 1);
        if start > end {
            return false;
        }

        let start_index = self.text.line_to_char(start);
        let end_index = if end + 1 < self.text.len_lines() {
            self.text.line_to_char(end + 1)
        } else {
            self.text.len_chars()
        };
        let block = self.text.slice(start_index..end_index).to_string();
        if block.is_empty() {
            return false;
        }

        self.checkpoint(None);
        if end + 1 < self.text.len_lines() {
            self.text.insert(end_index, &block);
        } else {
            let separator = if block.ends_with(self.line_ending.as_str()) {
                String::new()
            } else {
                self.line_ending.as_str().to_owned()
            };
            self.text.insert(end_index, &format!("{separator}{block}"));
        }
        self.mark_edited(None);
        true
    }

    pub fn indent_lines(&mut self, start_row: usize, end_row: usize) {
        self.checkpoint(None);
        let max_line = self.line_count().saturating_sub(1);
        for row in (start_row..=end_row.min(max_line)).rev() {
            let idx = self.char_index(row, 0);
            self.text.insert(idx, self.indent_unit());
        }
        self.mark_edited(None);
    }

    pub fn outdent_lines(&mut self, start_row: usize, end_row: usize) {
        self.checkpoint(None);
        let max_line = self.line_count().saturating_sub(1);
        for row in (start_row..=end_row.min(max_line)).rev() {
            let line = self.line_text(row);
            // One step: a leading tab, or up to four spaces.
            let remove = if line.starts_with('\t') {
                1
            } else {
                line.chars()
                    .take_while(|&c| c == ' ')
                    .take(self.indent_width)
                    .count()
            };
            if remove > 0 {
                let idx = self.char_index(row, 0);
                self.text.remove(idx..idx + remove);
            }
        }
        self.mark_edited(None);
    }

    fn grapheme_col_for_char_offset(&self, row: usize, char_offset: usize) -> usize {
        if char_offset == 0 {
            return 0;
        }
        let line = self.line_text(row);
        let mut consumed_chars = 0;
        let mut graphemes = 0;
        for grapheme in line.graphemes(true) {
            consumed_chars += grapheme.chars().count();
            graphemes += 1;
            if consumed_chars >= char_offset {
                break;
            }
        }
        graphemes
    }

    fn char_offset_after(&self, global_char_index: usize, row: usize) -> usize {
        let line_start = self.text.line_to_char(row);
        global_char_index
            .saturating_add(1)
            .saturating_sub(line_start)
    }

    fn line_content_chars(&self, row: usize) -> usize {
        self.line_text(row).chars().count()
    }

    fn line_terminator_chars(&self, row: usize) -> usize {
        if row >= self.text.len_lines() {
            return 0;
        }
        let raw = self.text.line(row).to_string();
        if raw.ends_with("\r\n") {
            2
        } else if raw.ends_with('\n')
            || raw.ends_with('\r')
            || raw.ends_with('\x0B')
            || raw.ends_with('\x0C')
            || raw.ends_with('\u{0085}')
            || raw.ends_with('\u{2028}')
            || raw.ends_with('\u{2029}')
        {
            1
        } else {
            0
        }
    }
}

fn line_comment_marker(language: &str) -> Option<&'static str> {
    match language {
        "Rust" | "JavaScript" | "TypeScript" | "React JSX" | "React TSX" | "C" | "C++" | "Go"
        | "Java" => Some("//"),
        "Python" | "Shell" | "YAML" | "TOML" | "Terraform" => Some("#"),
        "SQL" => Some("--"),
        _ => None,
    }
}

fn normalize_insert_text(text: &str, line_ending: LineEnding) -> String {
    let lf_only = text.replace("\r\n", "\n").replace('\r', "\n");
    match line_ending {
        LineEnding::Lf => lf_only,
        LineEnding::CrLf => lf_only.replace('\n', "\r\n"),
        LineEnding::Cr => lf_only.replace('\n', "\r"),
    }
}

fn detect_line_ending(content: &str) -> LineEnding {
    let crlf = content.matches("\r\n").count();
    let total_lf = content
        .as_bytes()
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count();
    let lone_lf = total_lf.saturating_sub(crlf);
    let total_cr = content
        .as_bytes()
        .iter()
        .filter(|&&byte| byte == b'\r')
        .count();
    let lone_cr = total_cr.saturating_sub(crlf);

    if crlf >= lone_lf && crlf >= lone_cr && crlf > 0 {
        LineEnding::CrLf
    } else if lone_cr > lone_lf {
        LineEnding::Cr
    } else {
        LineEnding::Lf
    }
}

fn strip_line_ending(mut line: String) -> String {
    if line.ends_with("\r\n") {
        line.truncate(line.len() - 2);
    } else if line.ends_with('\n')
        || line.ends_with('\r')
        || line.ends_with('\x0B')
        || line.ends_with('\x0C')
        || line.ends_with('\u{0085}')
        || line.ends_with('\u{2028}')
        || line.ends_with('\u{2029}')
    {
        line.pop();
    }
    line
}

/// `expected` again if the file's metadata says it cannot have changed;
/// `None` when the content has to be read to know.
fn unchanged_snapshot(path: &Path, expected: &FileSnapshot) -> Result<Option<FileSnapshot>> {
    const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(None);
    };
    let settled = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= SETTLE);
    let same = metadata.is_file()
        && FileSnapshot::from_metadata_and_hash(&metadata, expected.content_hash) == *expected;
    Ok((settled && same).then(|| expected.clone()))
}

fn capture_file_snapshot(path: &Path) -> Result<Option<FileSnapshot>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read metadata for {}", path.display()));
        }
    };
    let metadata = file.metadata()?;

    if !metadata.is_file() {
        bail!("{} is no longer a regular file", path.display());
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed to fingerprint {}", path.display()))?;
    let snapshot = FileSnapshot::from_metadata_and_bytes(&metadata, &bytes);
    if FileSnapshot::from_metadata_and_bytes(&file.metadata()?, &bytes) != snapshot {
        bail!(
            "conflict: file changed while fingerprinting {}",
            path.display()
        );
    }
    Ok(Some(snapshot))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_iter(bytes.iter().copied())
}

fn fnv1a_iter(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// How a save reached the disk. `InPlace` is the fallback for files that
/// cannot be replaced (bind mounts): their content is rewritten directly, so
/// a crash mid-write can damage that file; the recovery journal keeps a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    Atomic,
    InPlace,
}

/// How the finished temporary file takes the destination's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// Save: replace the existing file atomically.
    Replace,
    /// Save As: create the file, and fail if one appeared meanwhile.
    NewFileOnly,
}

fn atomic_write(
    path: &Path,
    has_bom: bool,
    text: &Rope,
    publish: Publish,
    expected: Option<FileSnapshot>,
) -> Result<(SaveMode, FileSnapshot)> {
    let original_snapshot = capture_file_snapshot(path)?;
    if original_snapshot != expected {
        bail!("conflict: file changed before preparing save; original file is unchanged");
    }
    let original = if original_snapshot.is_some() {
        let file = fs::File::open(path)
            .with_context(|| format!("could not inspect save metadata for {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if file.metadata()?.nlink() > 1 {
                bail!(
                    "{} has multiple hard links; atomic save would separate their contents. Use Save As to create an independent file, or explicitly manage the links outside Mellow",
                    path.display()
                );
            }
        }
        Some(file)
    } else {
        None
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_path = parent.join(format!(
        ".{file_name}.mellow-{}-{nonce}.tmp",
        std::process::id()
    ));

    let result = (|| -> Result<(SaveMode, FileSnapshot)> {
        #[allow(unused_mut)]
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Keep the unpublished text private until all metadata is copied.
            options.mode(0o600);
        }

        let mut file = options.open(&temp_path).with_context(|| {
            format!(
                "failed to create temporary save file {}",
                temp_path.display()
            )
        })?;

        if has_bom {
            file.write_all(UTF8_BOM)?;
        }
        text.write_to(&mut file)?;
        file.flush()?;
        file.sync_all()?;

        if let Some(source) = original.as_ref() {
            #[cfg(unix)]
            preserve_save_metadata(source, &file).map_err(|error| anyhow::anyhow!(
                "cannot preserve owner/group, ACLs or extended attributes for {}: {error:#}. Original file is unchanged; use Save As to create an independent file", path.display()))?;
            #[cfg(not(unix))]
            file.set_permissions(source.metadata()?.permissions())?;
            file.sync_all()?;
        }
        // Detect a writer that changed or replaced the target while the save
        // was being prepared. Never knowingly overwrite that newer version.
        if capture_file_snapshot(path)? != original_snapshot {
            bail!("conflict: file changed while preparing save; original file is unchanged");
        }
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            bail!("conflict: save destination became a symlink; original file is unchanged");
        }

        if publish == Publish::NewFileOnly {
            publish_new_file(&temp_path, path)?;
            if let Ok(directory) = fs::File::open(parent) {
                let _ = directory.sync_all();
            }
            return Ok((
                SaveMode::Atomic,
                snapshot_written_file(&file, has_bom, text)?,
            ));
        }

        let mut mode = SaveMode::Atomic;
        match fs::rename(&temp_path, path) {
            Ok(()) => {}
            // A file that is itself a mount point (Docker's single-file
            // `-v ./app.yaml:/app/app.yaml`) cannot be replaced, only
            // rewritten. The complete new text is already safely on disk in
            // the temporary file and in the recovery journal, so rewrite in
            // place rather than refuse to save.
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                file = rewrite_in_place(path, has_bom, text, original_snapshot.as_ref())
                    .with_context(|| {
                        format!("failed to rewrite mounted file {}", path.display())
                    })?;
                let _ = fs::remove_file(&temp_path);
                mode = SaveMode::InPlace;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to replace {}", path.display()));
            }
        }

        // Best-effort directory sync improves durability of the rename on Unix-like systems.
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok((mode, snapshot_written_file(&file, has_bom, text)?))
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

// Bind the saved baseline to the inode and bytes we actually wrote. Reading
// the path after publication could adopt a concurrent writer's new contents
// and incorrectly treat them as this buffer's saved version.
fn snapshot_written_file(file: &fs::File, has_bom: bool, text: &Rope) -> Result<FileSnapshot> {
    let prefix = if has_bom { UTF8_BOM } else { &[] };
    let hash = fnv1a_iter(prefix.iter().copied().chain(text.bytes()));
    Ok(FileSnapshot::from_metadata_and_hash(
        &file.metadata()?,
        hash,
    ))
}

/// Atomic saves preserve supported Unix metadata on the replacement inode.
/// Fail before publication if preserving any part of the contract fails.
#[cfg(unix)]
fn preserve_save_metadata(source: &fs::File, destination: &fs::File) -> Result<()> {
    use std::os::unix::{fs::MetadataExt, io::AsRawFd};
    let before = source.metadata()?;
    let current = destination.metadata()?;
    if before.uid() != current.uid() || before.gid() != current.gid() {
        // SAFETY: destination is a live file descriptor; ownership values came
        // from the still-open original. Failure leaves the original intact.
        if unsafe { libc::fchown(destination.as_raw_fd(), before.uid(), before.gid()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("preserving file owner/group");
        }
    }
    destination.set_permissions(before.permissions())?;
    #[cfg(target_os = "macos")]
    {
        // fcopyfile copies native macOS ACLs and every xattr (including resource
        // forks), without copying content or timestamps over the new text.
        // SAFETY: both file descriptors remain live for this call; null requests
        // copyfile's internally managed state.
        if unsafe {
            libc::fcopyfile(
                source.as_raw_fd(),
                destination.as_raw_fd(),
                std::ptr::null_mut(),
                libc::COPYFILE_ACL | libc::COPYFILE_XATTR,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("preserving ACLs and extended attributes");
        }
    }
    #[cfg(target_os = "linux")]
    copy_linux_xattrs(source, destination)?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("metadata-preserving atomic saves are not supported on this Unix platform");
    let after = destination.metadata()?;
    if (before.uid(), before.gid(), before.mode() & 0o7777)
        != (after.uid(), after.gid(), after.mode() & 0o7777)
    {
        bail!("filesystem changed owner/group or permission bits while preserving metadata");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn copy_linux_xattrs(source: &fs::File, destination: &fs::File) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    const ATTRIBUTE_LIMIT: usize = 16 * 1024 * 1024;
    fn names(fd: i32) -> Result<Vec<std::ffi::CString>> {
        // SAFETY: descriptor is live; null plus zero asks for the list size.
        let length = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0) };
        if length < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOTSUP) {
                return Ok(Vec::new());
            }
            return Err(error).context("listing extended attributes");
        }
        if length as usize > ATTRIBUTE_LIMIT {
            bail!("extended attribute list exceeds the save limit");
        }
        let mut bytes = vec![0u8; length as usize];
        // SAFETY: bytes is writable for its length and descriptor is live.
        let actual = unsafe { libc::flistxattr(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if actual < 0 {
            return Err(std::io::Error::last_os_error()).context("reading extended attribute list");
        }
        bytes.truncate(actual as usize);
        bytes
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| Ok(std::ffi::CString::new(name)?))
            .collect()
    }
    let source_fd = source.as_raw_fd();
    let destination_fd = destination.as_raw_fd();
    let attributes = names(source_fd)?;
    // A parent directory may give the new inode an ACL the original did not
    // have. Remove inherited attributes so the metadata is an exact copy.
    for name in names(destination_fd)?
        .into_iter()
        .filter(|name| !attributes.contains(name))
    {
        // SAFETY: descriptor and NUL-terminated name are live.
        if unsafe { libc::fremovexattr(destination_fd, name.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("clearing inherited extended attributes");
        }
    }
    for name in attributes {
        // Linux POSIX ACLs are system.posix_acl_access xattrs, and are copied
        // through this same path alongside user, security and trusted attrs.
        // SAFETY: live descriptor and valid name, zero buffer requests size.
        let length = unsafe { libc::fgetxattr(source_fd, name.as_ptr(), std::ptr::null_mut(), 0) };
        if length < 0 {
            return Err(std::io::Error::last_os_error()).context("measuring extended attribute");
        }
        if length as usize > ATTRIBUTE_LIMIT {
            bail!("extended attribute exceeds the save limit");
        }
        let mut bytes = vec![0u8; length as usize];
        // SAFETY: destination buffer is writable and source descriptor is live.
        let actual = unsafe {
            libc::fgetxattr(
                source_fd,
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if actual < 0 {
            return Err(std::io::Error::last_os_error()).context("reading extended attribute");
        }
        // Security labels can already match but be non-writable by this user.
        // Retain identical attributes instead of requesting a privileged write.
        let mut current = vec![0u8; actual as usize];
        // SAFETY: destination descriptor/name are live and current is writable.
        let existing = unsafe {
            libc::fgetxattr(
                destination_fd,
                name.as_ptr(),
                current.as_mut_ptr().cast(),
                current.len(),
            )
        };
        if existing == actual && current == bytes[..actual as usize] {
            continue;
        }
        // SAFETY: bytes and name are valid for their lengths; destination is live.
        if unsafe {
            libc::fsetxattr(
                destination_fd,
                name.as_ptr(),
                bytes.as_ptr().cast(),
                actual as usize,
                0,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error()).context("preserving extended attribute");
        }
    }
    Ok(())
}

/// Gives the finished temporary file its final name without ever replacing
/// a file. A hard link fails atomically if the name already exists, which
/// closes the gap between Save As's "does it exist?" check and the write.
fn publish_new_file(temp_path: &Path, path: &Path) -> Result<()> {
    match fs::hard_link(temp_path, path) {
        Ok(()) => {
            let _ = fs::remove_file(temp_path);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => bail!(
            "destination {} already exists; choose a different path",
            path.display()
        ),
        // A check followed by rename would overwrite a destination created
        // between those operations. Refuse unsupported exclusive publication
        // rather than silently weaken Save As's no-overwrite contract.
        Err(error) => Err(error).with_context(|| format!(
            "cannot exclusively publish {}; this filesystem must support hard links for safe Save As; choose another destination",
            path.display()
        )),
    }
}

/// Writes the text into the existing file (same inode, owner and mode).
#[cfg(unix)]
fn rewrite_in_place(
    path: &Path,
    has_bom: bool,
    text: &Rope,
    expected: Option<&FileSnapshot>,
) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // Open before truncating, refuse newly introduced symlinks, and bind the
    // fallback to the same original inode/content as the atomic attempt.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let snapshot = FileSnapshot::from_metadata_and_bytes(&before, &bytes);
    if Some(&snapshot) != expected
        || FileSnapshot::from_metadata_and_bytes(&file.metadata()?, &bytes) != snapshot
    {
        bail!("conflict: mounted file changed before in-place save; original file is unchanged");
    }
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    if has_bom {
        file.write_all(UTF8_BOM)?;
    }
    text.write_to(&mut file)?;
    file.flush()?;
    file.sync_all()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn typed(text: &str) -> (Buffer, Cursor) {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        for ch in text.chars() {
            buffer.insert_char(&mut cursor, ch);
        }
        (buffer, cursor)
    }

    #[cfg(unix)]
    #[test]
    fn saving_a_hard_link_refuses_without_mutating_either_name() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("original.conf");
        let alias = dir.path().join("alias.conf");
        fs::write(&path, "old text").unwrap();
        fs::hard_link(&path, &alias).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        buffer.insert_text(&mut Cursor::default(), "new ");
        let error = buffer.save().unwrap_err().to_string();
        assert!(
            error.contains("hard links") && error.contains("Save As"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "old text");
        assert_eq!(fs::read_to_string(&alias).unwrap(), "old text");
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert!(buffer.is_dirty());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn save_baseline_cannot_adopt_a_writer_after_the_conflict_check() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("concurrent.conf");
        fs::write(&path, "original").unwrap();
        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        buffer.insert_text(&mut Cursor::default(), "my edit ");
        assert!(buffer.external_conflict().unwrap().is_none());
        let accepted = buffer.disk_snapshot.clone();
        // Reproduce a writer between the UI conflict check and save setup.
        fs::write(&path, "other writer").unwrap();
        let error = buffer
            .save_against_snapshot(accepted)
            .unwrap_err()
            .to_string();
        assert!(error.contains("conflict:"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "other writer");
        assert!(buffer.is_dirty());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn saved_snapshot_is_bound_to_the_written_inode_not_a_later_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("saved.conf");
        fs::write(&path, "my saved text").unwrap();
        let written = fs::File::open(&path).unwrap();
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, "external text").unwrap();
        fs::rename(&replacement, &path).unwrap();
        let snapshot =
            snapshot_written_file(&written, false, &Rope::from_str("my saved text")).unwrap();
        assert_eq!(snapshot.content_hash, fnv1a(b"my saved text"));
        assert_ne!(Some(snapshot), capture_file_snapshot(&path).unwrap());
        assert_eq!(fs::read_to_string(path).unwrap(), "external text");
    }

    #[cfg(unix)]
    #[test]
    fn in_place_fallback_refuses_a_replaced_file_or_new_symlink_before_truncation() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        let path = dir.path().join("mounted.conf");
        fs::write(&path, "original").unwrap();
        let expected = capture_file_snapshot(&path).unwrap();
        fs::write(&path, "new external text").unwrap();
        let text = Rope::from_str("my draft");
        assert!(rewrite_in_place(&path, false, &text, expected.as_ref()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "new external text");
        fs::remove_file(&path).unwrap();
        let target = dir.path().join("symlink-target");
        fs::write(&target, "untouched target").unwrap();
        symlink(&target, &path).unwrap();
        assert!(rewrite_in_place(&path, false, &text, expected.as_ref()).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "untouched target");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn atomic_save_preserves_extended_attributes_owner_group_and_mode() {
        use std::os::unix::{fs::MetadataExt, io::AsRawFd};
        let dir = tempdir().unwrap();
        let path = dir.path().join("metadata.conf");
        fs::write(&path, "old text").unwrap();
        let file = fs::File::open(&path).unwrap();
        let name = c"user.audit";
        let value = b"preserve-me";
        // SAFETY: file is live and the name/value buffers are valid.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            },
            0
        );
        let before = file.metadata().unwrap();
        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        buffer.insert_text(&mut Cursor::default(), "new ");
        buffer.save().unwrap();
        let saved = fs::File::open(&path).unwrap();
        let after = saved.metadata().unwrap();
        assert_eq!(
            (before.uid(), before.gid(), before.mode()),
            (after.uid(), after.gid(), after.mode())
        );
        let mut actual = [0u8; 32];
        // SAFETY: file is live and the destination buffer is writable.
        let size = unsafe {
            libc::fgetxattr(
                saved.as_raw_fd(),
                name.as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
                0,
                0,
            )
        };
        assert_eq!(size, value.len() as isize);
        assert_eq!(&actual[..value.len()], value);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn atomic_save_preserves_native_acl_and_nondefault_group() {
        use std::os::unix::{
            fs::{MetadataExt, PermissionsExt},
            io::AsRawFd,
        };
        use std::process::Command;
        let dir = tempdir().unwrap();
        let path = dir.path().join("acl.conf");
        fs::write(&path, "original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let file = fs::File::open(&path).unwrap();
        let original_gid = file.metadata().unwrap().gid();
        let mut groups = [0 as libc::gid_t; 128];
        // SAFETY: groups has capacity for every entry requested.
        let count = unsafe { libc::getgroups(groups.len() as i32, groups.as_mut_ptr()) };
        assert!(count >= 0);
        if let Some(gid) = groups[..count as usize]
            .iter()
            .find(|gid| **gid != original_gid)
        {
            // SAFETY: live descriptor; -1 uid requests unchanged ownership and
            // this group is one of the current user's supplementary groups.
            assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), !0, *gid) }, 0);
        } else {
            eprintln!("no supplementary group available; alternate group branch not exercised");
        }
        let output = Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let acl = |path: &Path| {
            let output = Command::new("/bin/ls")
                .arg("-le")
                .arg(path)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let before_acl = acl(&path);
        assert!(before_acl.contains("everyone allow read"), "{before_acl}");
        let before = fs::metadata(&path).unwrap();
        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        buffer.insert_text(&mut Cursor::default(), "new ");
        buffer.save().unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (before.uid(), before.gid(), before.mode() & 0o7777),
            (after.uid(), after.gid(), after.mode() & 0o7777)
        );
        assert_eq!(acl(&path), before_acl);
        assert_eq!(fs::read_to_string(&path).unwrap(), "new original");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn atomic_save_preserves_linux_extended_attributes() {
        use std::os::unix::io::AsRawFd;
        let dir = tempdir().unwrap();
        let path = dir.path().join("xattr.conf");
        fs::write(&path, "old text").unwrap();
        let file = fs::File::open(&path).unwrap();
        let value = b"preserve-me";
        // SAFETY: descriptor, name and byte buffer are valid.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    c"user.audit".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0
        );
        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        buffer.insert_text(&mut Cursor::default(), "new ");
        buffer.save().unwrap();
        let saved = fs::File::open(&path).unwrap();
        let mut actual = [0u8; 32];
        // SAFETY: descriptor/name are live and actual is writable.
        let size = unsafe {
            libc::fgetxattr(
                saved.as_raw_fd(),
                c"user.audit".as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
            )
        };
        assert_eq!(size, value.len() as isize);
        assert_eq!(&actual[..value.len()], value);
    }

    #[test]
    fn publishing_a_new_file_never_replaces_one_that_appeared_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join(".new.txt.tmp");
        let target = dir.path().join("new.txt");
        std::fs::write(&temp, "mellow draft").unwrap();
        // Another program creates the file after Save As checked for it.
        std::fs::write(&target, "someone else's work").unwrap();

        let error = publish_new_file(&temp, &target).unwrap_err().to_string();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "someone else's work"
        );
    }

    #[test]
    fn save_as_creates_the_file_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fresh.txt");
        let mut buffer = Buffer::empty(None);
        buffer.insert_text(&mut crate::cursor::Cursor::default(), "hello");
        buffer.save_as(target.clone()).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn open_errors_explain_the_limit_without_an_old_version_number() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin1.txt");
        std::fs::write(&path, [0x63, 0x61, 0x66, 0xe9]).unwrap();
        let error = Buffer::open(Some(path)).unwrap_err().to_string();
        assert!(error.contains("not UTF-8 text"), "{error}");
        assert!(!error.contains("v0.1"), "{error}");
    }

    #[test]
    fn word_motion_stops_at_word_and_punctuation_runs() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "    let foo_bar = baz(1);\nnext");
        let right = |col| buffer.word_right(Cursor::new(0, col)).col;
        let left = |col| buffer.word_left(Cursor::new(0, col)).col;
        assert_eq!(right(0), 7, "skips indent, ends after 'let'");
        assert_eq!(right(7), 15, "foo_bar is one word");
        assert_eq!(right(15), 17, "'=' is punctuation");
        assert_eq!(right(21), 22, "'(' run");
        assert_eq!(left(25), 23, "back over ');'");
        assert_eq!(left(15), 8, "back to start of foo_bar");
        assert_eq!(left(8), 4, "back to start of let");
        assert_eq!(buffer.word_right(Cursor::new(0, 25)), Cursor::new(1, 0));
        assert_eq!(buffer.word_left(Cursor::new(1, 0)), Cursor::new(0, 25));
        assert_eq!(buffer.first_non_blank(0), 4);
    }

    #[test]
    fn read_only_files_are_reported_before_saving() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("locked.conf");
        fs::write(&path, "keep\n").unwrap();
        let buffer = Buffer::open(Some(path.clone())).unwrap();
        assert!(!buffer.read_only_on_disk());
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        assert!(buffer.read_only_on_disk());
        assert!(!Buffer::empty(None).read_only_on_disk());
    }

    /// Explicit gate for a real single-file Docker bind mount. The caller must
    /// mount a disposable fixture at /fixture/mounted.txt containing exactly
    /// the marker below. This test never claims a pass when mounts are absent.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires disposable Docker file bind mount and MELLOW_BIND_MOUNT_FILE=/fixture/mounted.txt"]
    fn saving_a_bind_mounted_file_rewrites_it_in_place() {
        use std::os::unix::{
            fs::{MetadataExt, PermissionsExt},
            io::AsRawFd,
        };
        let mounted = PathBuf::from(
            std::env::var_os("MELLOW_BIND_MOUNT_FILE")
                .expect("set MELLOW_BIND_MOUNT_FILE to the disposable bind-mounted fixture"),
        );
        assert_eq!(mounted, Path::new("/fixture/mounted.txt"));
        assert_eq!(
            fs::read_to_string(&mounted).unwrap(),
            "mellow-bind-mount-fixture: original\n"
        );
        fs::set_permissions(&mounted, fs::Permissions::from_mode(0o640)).unwrap();
        let file = fs::File::open(&mounted).unwrap();
        let value = b"preserve-bind-mount";
        // SAFETY: live descriptor and valid name/value buffers.
        let xattr = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                c"user.mellow_bind_mount".as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if xattr != 0 {
            let error = std::io::Error::last_os_error();
            assert_eq!(
                error.raw_os_error(),
                Some(libc::EOPNOTSUPP),
                "unexpected xattr refusal: {error}"
            );
            eprintln!(
                "bind-mount filesystem does not support user xattrs; this attribute branch was not exercised"
            );
        }
        let before = file.metadata().unwrap();
        let probe = mounted.with_file_name("rename-probe.tmp");
        fs::write(&probe, "disposable probe").unwrap();
        let error =
            fs::rename(&probe, &mounted).expect_err("fixture must be an actual file mount point");
        assert_eq!(error.raw_os_error(), Some(libc::EBUSY));
        fs::remove_file(probe).unwrap();
        let mut buffer = Buffer::open(Some(mounted.clone())).unwrap();
        buffer.insert_text(&mut Cursor::default(), "updated ");
        assert!(buffer.is_dirty());
        buffer.save().unwrap();
        assert_eq!(buffer.last_save_mode(), SaveMode::InPlace);
        assert!(!buffer.is_dirty());
        assert_eq!(
            fs::read_to_string(&mounted).unwrap(),
            "updated mellow-bind-mount-fixture: original\n"
        );
        assert!(buffer.external_conflict().unwrap().is_none());
        let saved = fs::File::open(&mounted).unwrap();
        let after = saved.metadata().unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.uid(),
                before.gid(),
                before.mode()
            ),
            (
                after.dev(),
                after.ino(),
                after.uid(),
                after.gid(),
                after.mode()
            )
        );
        if xattr == 0 {
            let mut actual = [0u8; 64];
            // SAFETY: live descriptor and valid name/output buffers.
            let size = unsafe {
                libc::fgetxattr(
                    saved.as_raw_fd(),
                    c"user.mellow_bind_mount".as_ptr(),
                    actual.as_mut_ptr().cast(),
                    actual.len(),
                )
            };
            assert_eq!(size, value.len() as isize);
            assert_eq!(&actual[..value.len()], value);
            eprintln!("actual bind-mount user xattr preserved");
        }
        let leftovers: Vec<_> = fs::read_dir(mounted.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary file left behind");
    }

    /// The poll trusts unchanged metadata instead of re-reading every open
    /// file each second; saving still compares content, so a rewrite that
    /// kept size and mtime can never be overwritten silently.
    #[test]
    fn polling_trusts_settled_metadata_but_saving_checks_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("watched.txt");
        fs::write(&path, "alpha\n").unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        assert!(buffer.external_change().unwrap().is_none());

        // A same-size rewrite that restores mtime still changes Unix ctime.
        // The poll may detect that; the full-content save check must always.
        let file = fs::File::options().write(true).open(&path).unwrap();
        (&file).write_all(b"bravo\n").unwrap();
        file.set_modified(old).unwrap();
        #[cfg(unix)]
        assert!(buffer.external_change().unwrap().is_some());
        assert!(buffer.external_conflict().unwrap().is_some());
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_char(&mut cursor, 'x');
        assert!(buffer.save().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "bravo\n");

        // A fresh change of the same size is read, not trusted.
        let buffer = Buffer::open(Some(path.clone())).unwrap();
        let file = fs::File::options().write(true).open(&path).unwrap();
        (&file).write_all(b"delta\n").unwrap();
        file.set_modified(old).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now())
            .unwrap();
        assert!(buffer.external_change().unwrap().is_some());
        // Deleted files are always noticed.
        fs::remove_file(&path).unwrap();
        assert!(buffer.external_change().unwrap().is_some());
    }

    /// Audit: typing made one undo step per character in a 200-step history.
    #[test]
    fn a_typing_run_undoes_as_one_step_and_breaks_at_spaces() {
        let (mut buffer, mut cursor) = typed("hello world");
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.contents(), "hello");
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.contents(), "");

        let (mut buffer, mut cursor) = typed(&"x".repeat(500));
        assert!(buffer.undo(&mut cursor));
        assert_eq!(
            buffer.contents(),
            "",
            "500 typed characters undo in one step"
        );
        assert!(buffer.redo(&mut cursor));
        assert_eq!(buffer.contents().len(), 500);
    }

    #[test]
    fn a_pause_or_moving_the_cursor_starts_a_new_undo_step() {
        let (mut buffer, mut cursor) = typed("ab");
        buffer.typing_run = buffer
            .typing_run
            .map(|(at, after)| (at - TYPING_GROUP_PAUSE, after));
        buffer.insert_char(&mut cursor, 'c');
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.contents(), "ab", "pause split the run");

        let (mut buffer, _) = typed("ab");
        let mut elsewhere = Cursor::new(0, 0);
        buffer.insert_char(&mut elsewhere, 'z');
        assert!(buffer.undo(&mut elsewhere));
        assert_eq!(buffer.contents(), "ab", "typing elsewhere is a new step");
    }

    #[test]
    fn detects_crlf_and_preserves_it_for_new_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        fs::write(&path, b"one\r\ntwo\r\n").unwrap();

        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        assert_eq!(buffer.line_ending(), LineEnding::CrLf);

        let mut cursor = Cursor::new(0, 3);
        // Enter inserts "\n" plus indentation through insert_text.
        buffer.insert_text(&mut cursor, "\n");
        buffer.save().unwrap();

        let saved = fs::read(&path).unwrap();
        assert!(saved.windows(2).any(|window| window == b"\r\n"));
    }

    #[test]
    fn normalizes_pasted_line_endings_to_the_open_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        fs::write(&path, b"one\r\ntwo\r\n").unwrap();

        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        let mut cursor = Cursor::new(0, 3);
        buffer.insert_text(&mut cursor, "\nthree\nfour");
        buffer.save().unwrap();

        let saved = fs::read_to_string(path).unwrap();
        assert!(!saved.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn preserves_utf8_bom() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bom.txt");
        let mut input = UTF8_BOM.to_vec();
        input.extend_from_slice(b"hello\n");
        fs::write(&path, input).unwrap();

        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        assert!(buffer.has_bom());
        let mut cursor = Cursor::new(0, 5);
        buffer.insert_char(&mut cursor, '!');
        buffer.save().unwrap();

        assert!(fs::read(&path).unwrap().starts_with(UTF8_BOM));
    }

    #[test]
    fn moves_over_a_combined_grapheme_as_one_editor_column() {
        let mut buffer = Buffer {
            path: None,
            save_path: None,
            text: Rope::from_str("e\u{301}x"),
            line_ending: LineEnding::Lf,
            has_bom: false,
            revision: 0,
            saved_revision: 0,
            last_save_mode: SaveMode::Atomic,
            next_revision: 1,
            disk_snapshot: None,
            persisted: false,
            undo: Vec::new(),
            redo: Vec::new(),
            draft_id: 0,
            typing_run: None,
            tab_indent: false,
            indent_width: 4,
        };
        assert_eq!(buffer.grapheme_count(0), 2);

        let mut cursor = Cursor::new(0, 1);
        buffer.backspace(&mut cursor);
        assert_eq!(buffer.line_text(0), "x");
        assert_eq!(cursor.col, 0);
    }

    #[test]
    fn undo_and_redo_restore_dirty_state_correctly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("undo.txt");
        fs::write(&path, "hello").unwrap();
        let mut buffer = Buffer::open(Some(path)).unwrap();
        let mut cursor = Cursor::new(0, 5);

        buffer.insert_char(&mut cursor, '!');
        assert!(buffer.is_dirty());
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.line_text(0), "hello");
        assert!(!buffer.is_dirty());
        assert!(buffer.redo(&mut cursor));
        assert_eq!(buffer.line_text(0), "hello!");
        assert!(buffer.is_dirty());
    }

    #[test]
    fn maps_terminal_cells_back_to_grapheme_columns() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "a漢b");

        assert_eq!(buffer.grapheme_col_at_display_column(0, 0, 4), 0);
        assert_eq!(buffer.grapheme_col_at_display_column(0, 1, 4), 1);
        assert_eq!(buffer.grapheme_col_at_display_column(0, 2, 4), 1);
        assert_eq!(buffer.grapheme_col_at_display_column(0, 3, 4), 2);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_open_a_broken_symlink_as_a_new_file() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target = dir.path().join("missing.txt");
        let link = dir.path().join("broken.txt");
        symlink(&target, &link).unwrap();

        let error = Buffer::open(Some(link)).unwrap_err();
        assert!(error.to_string().contains("broken symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn saving_an_opened_symlink_updates_target_without_replacing_link() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, "hello").unwrap();
        symlink(&target, &link).unwrap();

        let mut buffer = Buffer::open(Some(link.clone())).unwrap();
        let mut cursor = Cursor::new(0, 5);
        buffer.insert_char(&mut cursor, '!');
        buffer.save().unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "hello!");
    }

    #[test]
    fn lsp_utf16_columns_round_trip_across_emoji_and_combining_text() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "a🙂e\u{301}漢");

        for grapheme_col in 0..=buffer.grapheme_count(0) {
            let utf16 = buffer.utf16_col_for_grapheme(0, grapheme_col);
            assert_eq!(
                buffer.grapheme_col_for_utf16(0, utf16),
                grapheme_col,
                "round trip failed at grapheme {grapheme_col}"
            );
        }

        assert_eq!(buffer.utf16_col_for_grapheme(0, 2), 3);
    }

    #[test]
    fn replace_ranges_supports_zero_width_insertions() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "ac");
        let count = buffer.replace_ranges(
            &[(Cursor::new(0, 1), Cursor::new(0, 1), "b".to_owned())],
            Cursor::new(0, 0),
        );
        assert_eq!(count, 1);
        assert_eq!(buffer.contents(), "abc");
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.contents(), "ac");
    }

    #[test]
    fn replace_ranges_is_one_undo_transaction_and_preserves_unicode_positions() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "alpha 🙂 alpha\nalpha");
        let before = buffer.contents();
        let post_cursor = Cursor::new(0, 0);

        let count = buffer.replace_ranges(
            &[
                (Cursor::new(0, 0), Cursor::new(0, 5), "A".to_owned()),
                (Cursor::new(0, 8), Cursor::new(0, 13), "B".to_owned()),
                (Cursor::new(1, 0), Cursor::new(1, 5), "C".to_owned()),
            ],
            post_cursor,
        );

        assert_eq!(count, 3);
        assert_eq!(buffer.contents(), "A 🙂 B\nC");

        let mut undo_cursor = post_cursor;
        assert!(buffer.undo(&mut undo_cursor));
        assert_eq!(buffer.contents(), before);
        assert_eq!(undo_cursor, post_cursor);
    }

    #[test]
    fn bracket_matching_is_bounded_for_large_files() {
        let near = format!("({})", "x".repeat(1_000));
        let mut buffer = Buffer::empty(None);
        buffer.insert_text(&mut Cursor::new(0, 0), &near);
        let end = Cursor::new(0, 1_001);
        assert_eq!(
            buffer.matching_bracket(Cursor::new(0, 0)),
            Some((Cursor::new(0, 0), end))
        );
        assert_eq!(buffer.matching_bracket(end), Some((end, Cursor::new(0, 0))));

        let far = format!("[\n{}]", "  1,\n".repeat(MAX_BRACKET_SCAN_CHARS / 5 + 1));
        let mut buffer = Buffer::empty(None);
        buffer.insert_text(&mut Cursor::new(0, 0), &far);
        assert_eq!(buffer.matching_bracket(Cursor::new(0, 0)), None);
    }

    #[test]
    fn goal15_matching_brackets_tracks_nested_pairs_from_either_side() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "(a(b)c)");
        assert_eq!(
            buffer.matching_bracket(Cursor::new(0, 0)),
            Some((Cursor::new(0, 0), Cursor::new(0, 6)))
        );
        assert_eq!(
            buffer.matching_bracket(Cursor::new(0, 2)),
            Some((Cursor::new(0, 2), Cursor::new(0, 4)))
        );
        assert_eq!(
            buffer.matching_bracket(Cursor::new(0, 7)),
            Some((Cursor::new(0, 6), Cursor::new(0, 0)))
        );
        assert_eq!(
            buffer.matching_bracket(Cursor::new(0, 1)),
            Some((Cursor::new(0, 0), Cursor::new(0, 6)))
        );
    }

    #[test]
    fn goal_editing_word_range_handles_unicode_graphemes() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "hello తెలుగు world");
        let range = buffer.word_range(0, 7).unwrap();
        assert_eq!(buffer.get_text_range(range.0, range.1), "తెలుగు");
    }

    #[test]
    fn goal_editing_comment_move_duplicate_are_undoable_transactions() {
        let mut buffer = Buffer::empty(Some(PathBuf::from("demo.rs")));
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "one\ntwo\nthree");
        let baseline = buffer.contents();

        assert!(buffer.toggle_comment_lines(0, 1));
        assert_eq!(buffer.line_text(0), "// one");
        assert_eq!(buffer.line_text(1), "// two");
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.contents(), baseline);

        assert!(buffer.move_lines_down(0, 0));
        assert_eq!(buffer.line_text(0), "two");
        assert_eq!(buffer.line_text(1), "one");
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.contents(), baseline);

        assert!(buffer.duplicate_lines(1, 1));
        assert_eq!(buffer.line_text(1), "two");
        assert_eq!(buffer.line_text(2), "two");
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.contents(), baseline);
    }

    #[test]
    fn detects_language_from_extension() {
        let rs_buffer = Buffer::empty(Some(PathBuf::from("main.rs")));
        assert_eq!(rs_buffer.language(), "Rust");
        let py_buffer = Buffer::empty(Some(PathBuf::from("script.py")));
        assert_eq!(py_buffer.language(), "Python");
        let untitled = Buffer::empty(None);
        assert_eq!(untitled.language(), "Plain Text");
    }

    #[test]
    fn gets_and_deletes_text_range() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "Hello World\nSecond line");

        let start = Cursor::new(0, 6);
        let end = Cursor::new(0, 11);
        assert_eq!(buffer.get_text_range(start, end), "World");

        buffer.delete_range(start, end);
        assert_eq!(buffer.line_text(0), "Hello ");
        assert_eq!(buffer.line_text(1), "Second line");
    }

    #[test]
    fn myn_e02_pasting_trailing_newline_keeps_cursor_at_col_zero() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "abc");
        cursor = Cursor::new(0, 0);

        buffer.insert_text(&mut cursor, "x\n");
        assert_eq!(cursor.row, 1);
        assert_eq!(cursor.col, 0);

        buffer.insert_char(&mut cursor, 'Y');
        assert_eq!(buffer.line_text(1), "Yabc");
    }

    #[test]
    fn myn_e08_open_rejects_directories_and_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let result = Buffer::open(Some(dir.path().to_path_buf()));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("is a directory"));

        #[cfg(unix)]
        {
            if std::path::Path::new("/dev/null").exists() {
                let dev_result = Buffer::open(Some(std::path::PathBuf::from("/dev/null")));
                assert!(dev_result.is_err());
                assert!(
                    dev_result
                        .unwrap_err()
                        .to_string()
                        .contains("not a regular file")
                );
            }
        }
    }

    #[test]
    fn myn_e07_undo_and_redo_restore_caret_position() {
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "hello");
        assert_eq!(cursor, Cursor::new(0, 5));

        cursor = Cursor::new(0, 2);
        buffer.insert_char(&mut cursor, 'X');
        assert_eq!(buffer.line_text(0), "heXllo");
        assert_eq!(cursor, Cursor::new(0, 3));

        // Move cursor elsewhere
        cursor = Cursor::new(0, 0);
        assert!(buffer.undo(&mut cursor));
        assert_eq!(buffer.line_text(0), "hello");
        assert_eq!(cursor, Cursor::new(0, 2));

        assert!(buffer.redo(&mut cursor));
        assert_eq!(buffer.line_text(0), "heXllo");
        assert_eq!(cursor, Cursor::new(0, 3));
    }

    #[test]
    fn save_as_persists_untitled_buffer_and_updates_identity() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("saved.txt");
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "hello save as");

        assert!(!buffer.is_persisted());
        buffer.save_as(target.clone()).unwrap();

        assert_eq!(buffer.path(), Some(&target));
        assert!(buffer.is_persisted());
        assert!(!buffer.is_dirty());
        assert_eq!(fs::read_to_string(target).unwrap(), "hello save as");
    }

    #[test]
    fn save_as_refuses_existing_destination_without_overwrite() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("existing.txt");
        fs::write(&target, "existing").unwrap();

        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "new content");

        let error = buffer.save_as(target.clone()).unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "existing");
        assert_eq!(buffer.path(), None);
        assert!(buffer.is_dirty());
        assert!(!buffer.is_persisted());
    }

    #[test]
    fn goal_safety_detects_same_size_external_content_change() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("same-size.txt");
        fs::write(&path, "AAAA").unwrap();

        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        let mut cursor = Cursor::new(0, 4);
        buffer.insert_char(&mut cursor, '!');

        fs::write(&path, "BBBB").unwrap();
        let error = buffer.save().unwrap_err();
        assert!(error.to_string().contains("conflict"));
        assert_eq!(fs::read_to_string(path).unwrap(), "BBBB");
        assert!(buffer.is_dirty());
    }

    #[test]
    fn goal_safety_detects_atomic_external_replace_even_with_same_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("replace.txt");
        fs::write(&path, "base").unwrap();

        let buffer = Buffer::open(Some(path.clone())).unwrap();
        let replacement = dir.path().join("replacement.txt");
        fs::write(&replacement, "base").unwrap();
        fs::rename(&replacement, &path).unwrap();

        assert!(buffer.external_conflict().unwrap().is_some());
    }

    #[test]
    fn goal_safety_detects_destination_created_after_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("new.txt");
        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, "mine");

        fs::write(&path, "theirs").unwrap();
        let error = buffer.save().unwrap_err();
        assert!(error.to_string().contains("conflict"));
        assert_eq!(fs::read_to_string(path).unwrap(), "theirs");
    }

    #[test]
    fn goal_safety_force_save_requires_explicit_call_and_refreshes_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("force.txt");
        fs::write(&path, "disk").unwrap();

        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        let mut cursor = Cursor::new(0, 4);
        buffer.insert_text(&mut cursor, "-mine");
        fs::write(&path, "external").unwrap();

        assert!(buffer.external_conflict().unwrap().is_some());
        buffer.force_save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "disk-mine");
        assert!(buffer.external_conflict().unwrap().is_none());
    }

    #[test]
    fn myn_e06_save_detects_external_file_conflict() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("conflict.txt");
        fs::write(&path, "initial").unwrap();

        let mut buffer = Buffer::open(Some(path.clone())).unwrap();
        let mut cursor = Cursor::new(0, 7);
        buffer.insert_text(&mut cursor, " edits");

        // External update
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&path, "external overwrite").unwrap();

        let save_result = buffer.save();
        assert!(save_result.is_err());
        assert!(save_result.unwrap_err().to_string().contains("conflict:"));
    }

    #[test]
    fn myn_e10_detects_and_preserves_cr_and_unicode_line_endings() {
        let text = "line1\rline2\rline3";
        assert_eq!(detect_line_ending(text), LineEnding::Cr);
        assert_eq!(strip_line_ending("line1\r".to_string()), "line1");
        assert_eq!(strip_line_ending("line1\u{0085}".to_string()), "line1");
        assert_eq!(strip_line_ending("line1\u{2028}".to_string()), "line1");
    }
}
