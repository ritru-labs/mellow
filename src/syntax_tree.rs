use std::fmt;

use anyhow::{Context, Result};
use tree_sitter::{
    InputEdit, Language, Node, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::syntax::TokenType;

const MAX_SYNTAX_DIAGNOSTICS: usize = 200;
const MAX_HIGHLIGHT_CAPTURES_PER_ROW: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxHighlightSpan {
    pub start_col: usize,
    pub end_col: usize,
    pub token_type: TokenType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxSeverity {
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxDiagnostic {
    pub row: usize,
    pub col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub severity: SyntaxSeverity,
    pub message: String,
}

pub struct SyntaxDocument {
    parser: Parser,
    tree: Tree,
    highlight_query: Option<Query>,
    language_name: String,
    source: String,
    /// Where each row of `source` starts, so rendering a row never scans
    /// the file from the top.
    lines: LineIndex,
    revision: u64,
    parse_generation: u64,
    incremental_reparse_count: u64,
    diagnostics: Vec<SyntaxDiagnostic>,
}

impl fmt::Debug for SyntaxDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntaxDocument")
            .field("language_name", &self.language_name)
            .field("semantic_highlighting", &self.highlight_query.is_some())
            .field("revision", &self.revision)
            .field("parse_generation", &self.parse_generation)
            .field("incremental_reparse_count", &self.incremental_reparse_count)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

impl SyntaxDocument {
    pub fn new(language_name: &str, source: &str, revision: u64) -> Result<Option<Self>> {
        let Some(language) = language_for(language_name) else {
            return Ok(None);
        };

        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .with_context(|| format!("failed to load Tree-sitter grammar for {language_name}"))?;
        let tree = parser
            .parse(source, None)
            .with_context(|| format!("Tree-sitter parse cancelled for {language_name}"))?;
        let highlight_query = highlight_query_for(language_name, &language);

        let lines = LineIndex::new(source);
        let diagnostics = collect_diagnostics(&tree, source, &lines);
        Ok(Some(Self {
            parser,
            tree,
            highlight_query,
            language_name: language_name.to_owned(),
            source: source.to_owned(),
            lines,
            revision,
            parse_generation: 1,
            incremental_reparse_count: 0,
            diagnostics,
        }))
    }

    pub fn language_name(&self) -> &str {
        &self.language_name
    }

    #[cfg(test)]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg(test)]
    pub const fn parse_generation(&self) -> u64 {
        self.parse_generation
    }

    #[cfg(test)]
    pub const fn incremental_reparse_count(&self) -> u64 {
        self.incremental_reparse_count
    }

    pub fn diagnostics(&self) -> &[SyntaxDiagnostic] {
        &self.diagnostics
    }

    #[cfg(test)]
    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    pub fn highlight_spans_for_row(&self, row: usize) -> Vec<SyntaxHighlightSpan> {
        let Some(query) = self.highlight_query.as_ref() else {
            return Vec::new();
        };
        let line_start = self.lines.start(row);
        let line_end = self
            .lines
            .start(row.saturating_add(1))
            .unwrap_or(self.source.len());
        let Some(line_start) = line_start else {
            return Vec::new();
        };

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(line_start..line_end.max(line_start));
        cursor.set_match_limit(MAX_HIGHLIGHT_CAPTURES_PER_ROW as u32);
        let names = query.capture_names();
        let mut captures = cursor.captures(query, self.tree.root_node(), self.source.as_bytes());
        let mut spans = Vec::new();

        while let Some((query_match, capture_index)) = captures.next() {
            if spans.len() >= MAX_HIGHLIGHT_CAPTURES_PER_ROW {
                break;
            }
            let Some(capture) = query_match.captures.get(*capture_index) else {
                continue;
            };
            let start = capture.node.start_position();
            let end = capture.node.end_position();
            if row < start.row || row > end.row {
                continue;
            }
            let Some(name) = names.get(capture.index as usize).copied() else {
                continue;
            };
            let Some(token_type) = token_type_for_capture(name) else {
                continue;
            };
            let start_col = if row == start.row {
                self.lines.grapheme_col(&self.source, row, start.column)
            } else {
                0
            };
            let end_col = if row == end.row {
                self.lines.grapheme_col(&self.source, row, end.column)
            } else {
                self.lines.line(&self.source, row).graphemes(true).count()
            };
            if start_col < end_col {
                spans.push(SyntaxHighlightSpan {
                    start_col,
                    end_col,
                    token_type,
                });
            }
        }

        spans.sort_by_key(|span| (span.start_col, span.end_col));
        spans
    }

    #[cfg(test)]
    pub fn semantic_highlighting_enabled(&self) -> bool {
        self.highlight_query.is_some()
    }

    pub fn sync(&mut self, language_name: &str, source: &str, revision: u64) -> Result<bool> {
        if language_name != self.language_name {
            let Some(language) = language_for(language_name) else {
                return Ok(false);
            };
            self.parser.set_language(&language).with_context(|| {
                format!("failed to load Tree-sitter grammar for {language_name}")
            })?;
            self.tree = self
                .parser
                .parse(source, None)
                .with_context(|| format!("Tree-sitter parse cancelled for {language_name}"))?;
            self.highlight_query = highlight_query_for(language_name, &language);
            self.language_name = language_name.to_owned();
            self.source = source.to_owned();
            self.lines = LineIndex::new(source);
            self.revision = revision;
            self.parse_generation = self.parse_generation.saturating_add(1);
            self.diagnostics = collect_diagnostics(&self.tree, source, &self.lines);
            return Ok(true);
        }

        if revision == self.revision && source == self.source {
            return Ok(false);
        }

        if source == self.source {
            self.revision = revision;
            return Ok(false);
        }

        let lines = LineIndex::new(source);
        let edit = compute_input_edit(&self.source, &self.lines, source, &lines);
        self.tree.edit(&edit);
        let new_tree = self
            .parser
            .parse(source, Some(&self.tree))
            .with_context(|| {
                format!("incremental Tree-sitter parse cancelled for {language_name}")
            })?;

        self.tree = new_tree;
        self.source = source.to_owned();
        self.lines = lines;
        self.revision = revision;
        self.parse_generation = self.parse_generation.saturating_add(1);
        self.incremental_reparse_count = self.incremental_reparse_count.saturating_add(1);
        self.diagnostics = collect_diagnostics(&self.tree, source, &self.lines);
        Ok(true)
    }
}

/// A named definition in the file: where it starts and ends, for the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSymbol {
    pub kind: &'static str,
    pub name: String,
    /// Zero-based rows of the definition's first and last line.
    pub start_row: usize,
    pub end_row: usize,
}

/// Definition node kinds per language, with the label shown for each.
fn symbol_kinds(language_name: &str) -> &'static [(&'static str, &'static str)] {
    match language_name {
        "Rust" => &[
            ("function_item", "fn"),
            ("struct_item", "struct"),
            ("enum_item", "enum"),
            ("trait_item", "trait"),
            ("impl_item", "impl"),
            ("mod_item", "mod"),
            ("type_item", "type"),
            ("const_item", "const"),
            ("static_item", "static"),
        ],
        "Python" => &[
            ("function_definition", "def"),
            ("class_definition", "class"),
        ],
        "Shell" => &[("function_definition", "fn")],
        _ => &[],
    }
}

impl SyntaxDocument {
    /// Named definitions in document order, parents before their children.
    pub fn symbols(&self) -> Vec<SyntaxSymbol> {
        let kinds = symbol_kinds(&self.language_name);
        let mut found = Vec::new();
        if kinds.is_empty() {
            return found;
        }
        collect_symbols(
            self.tree.root_node(),
            self.source.as_bytes(),
            kinds,
            &mut found,
        );
        found
    }
}

fn collect_symbols(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    kinds: &[(&'static str, &'static str)],
    found: &mut Vec<SyntaxSymbol>,
) {
    if let Some((_, kind)) = kinds
        .iter()
        .find(|(node_kind, _)| *node_kind == node.kind())
    {
        // An impl block is named by the type it implements, not by a `name` field.
        let name_node = if node.kind() == "impl_item" {
            node.child_by_field_name("type")
        } else {
            node.child_by_field_name("name")
        };
        if let Some(name) = name_node.and_then(|name| name.utf8_text(source).ok()) {
            found.push(SyntaxSymbol {
                kind,
                name: name.to_owned(),
                start_row: node.start_position().row,
                end_row: node.end_position().row,
            });
        }
    }
    for index in 0..node.child_count() {
        if let Some(child) = node.child(index) {
            collect_symbols(child, source, kinds, found);
        }
    }
}

#[cfg(test)]
pub fn supports_language(language_name: &str) -> bool {
    language_for(language_name).is_some()
}

fn language_for(language_name: &str) -> Option<Language> {
    match language_name {
        "Rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "Python" => Some(tree_sitter_python::LANGUAGE.into()),
        "JSON" => Some(tree_sitter_json::LANGUAGE.into()),
        "Shell" => Some(tree_sitter_bash::LANGUAGE.into()),
        "YAML" => Some(tree_sitter_yaml::LANGUAGE.into()),
        "Terraform" => Some(tree_sitter_hcl::LANGUAGE.into()),
        _ => None,
    }
}

const HCL_HIGHLIGHTS_QUERY: &str = r#"
(comment) @comment
(string_lit) @string
(quoted_template) @string
(heredoc_template) @string
(numeric_lit) @number
(bool_lit) @constant
(null_lit) @constant
(function_call (identifier) @function)
(attribute (identifier) @property)
(block (identifier) @type)
(template_interpolation_start) @punctuation
(template_interpolation_end) @punctuation
"#;

fn highlight_query_for(language_name: &str, language: &Language) -> Option<Query> {
    let source = match language_name {
        "Rust" => tree_sitter_rust::HIGHLIGHTS_QUERY,
        "Python" => tree_sitter_python::HIGHLIGHTS_QUERY,
        "JSON" => tree_sitter_json::HIGHLIGHTS_QUERY,
        "Shell" => tree_sitter_bash::HIGHLIGHT_QUERY,
        "YAML" => tree_sitter_yaml::HIGHLIGHTS_QUERY,
        "Terraform" => HCL_HIGHLIGHTS_QUERY,
        _ => return None,
    };
    Query::new(language, source).ok()
}

fn token_type_for_capture(name: &str) -> Option<TokenType> {
    let root = name.split('.').next().unwrap_or(name);
    Some(match root {
        "comment" => TokenType::Comment,
        "string" | "character" => TokenType::String,
        "number" | "float" => TokenType::Number,
        "function" | "method" | "constructor" => TokenType::Function,
        "type" | "constant" | "tag" | "label" => TokenType::ConstantOrType,
        "keyword" | "include" | "conditional" | "repeat" | "exception" => TokenType::Keyword,
        "operator" | "punctuation" => TokenType::Punctuation,
        "variable" | "property" | "attribute" | "parameter" | "embedded" | "escape" => {
            TokenType::Text
        }
        _ => return None,
    })
}

/// Byte offset of every row start in a source text. Built once per parse;
/// row lookups are then O(1) instead of a scan from the top of the file,
/// which made drawing rows near the end of a large file very slow.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self { starts }
    }

    /// Byte offset where `row` starts, if the source has that row.
    fn start(&self, row: usize) -> Option<usize> {
        self.starts.get(row).copied()
    }

    /// The text of `row` exactly as `source.lines().nth(row)` yields it:
    /// without its `\n` or `\r\n` ending, and empty past the last line.
    fn line<'a>(&self, source: &'a str, row: usize) -> &'a str {
        let Some(start) = self.start(row) else {
            return "";
        };
        match self.start(row + 1) {
            Some(next) => {
                let line = &source[start..next - 1];
                line.strip_suffix('\r').unwrap_or(line)
            }
            None => &source[start..],
        }
    }

    /// Tree-sitter row and byte column of a byte offset.
    fn point(&self, byte: usize) -> Point {
        let row = self.starts.partition_point(|start| *start <= byte) - 1;
        Point::new(row, byte - self.starts[row])
    }

    /// Grapheme column of a Tree-sitter byte column on `row`.
    fn grapheme_col(&self, source: &str, row: usize, byte_col: usize) -> usize {
        let line = self.line(source, row);
        let byte_col = byte_col.min(line.len());
        let mut boundary = byte_col;
        while boundary > 0 && !line.is_char_boundary(boundary) {
            boundary -= 1;
        }
        line[..boundary].graphemes(true).count()
    }
}

fn compute_input_edit(
    old: &str,
    old_lines: &LineIndex,
    new: &str,
    new_lines: &LineIndex,
) -> InputEdit {
    let old_bytes = old.as_bytes();
    let new_bytes = new.as_bytes();

    let mut prefix = 0usize;
    let prefix_limit = old_bytes.len().min(new_bytes.len());
    while prefix < prefix_limit && old_bytes[prefix] == new_bytes[prefix] {
        prefix += 1;
    }
    while prefix > 0 && (!old.is_char_boundary(prefix) || !new.is_char_boundary(prefix)) {
        prefix -= 1;
    }

    let mut suffix = 0usize;
    while suffix < old_bytes.len().saturating_sub(prefix)
        && suffix < new_bytes.len().saturating_sub(prefix)
        && old_bytes[old_bytes.len() - 1 - suffix] == new_bytes[new_bytes.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mut old_end = old_bytes.len().saturating_sub(suffix);
    let mut new_end = new_bytes.len().saturating_sub(suffix);
    while old_end < old.len() && !old.is_char_boundary(old_end) {
        old_end += 1;
    }
    while new_end < new.len() && !new.is_char_boundary(new_end) {
        new_end += 1;
    }

    InputEdit {
        start_byte: prefix,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: old_lines.point(prefix.min(old.len())),
        old_end_position: old_lines.point(old_end.min(old.len())),
        new_end_position: new_lines.point(new_end.min(new.len())),
    }
}

fn collect_diagnostics(tree: &Tree, source: &str, lines: &LineIndex) -> Vec<SyntaxDiagnostic> {
    let mut diagnostics = Vec::new();
    walk_errors(tree.root_node(), source, lines, &mut diagnostics);
    diagnostics
}

fn walk_errors(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    diagnostics: &mut Vec<SyntaxDiagnostic>,
) {
    // `has_error` covers the node and everything below it (missing nodes
    // included), so clean subtrees are skipped. Walking the whole tree after
    // every keystroke was most of the typing delay in large files.
    if diagnostics.len() >= MAX_SYNTAX_DIAGNOSTICS || !node.has_error() {
        return;
    }

    if node.is_error() || node.is_missing() {
        let start = node.start_position();
        let end = node.end_position();
        diagnostics.push(SyntaxDiagnostic {
            row: start.row,
            col: lines.grapheme_col(source, start.row, start.column),
            end_row: end.row,
            end_col: lines.grapheme_col(source, end.row, end.column),
            severity: SyntaxSeverity::Error,
            message: if node.is_missing() {
                format!("Missing syntax element: {}", node.kind())
            } else {
                format!("Syntax error near {}", node.kind())
            },
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_errors(child, source, lines, diagnostics);
        if diagnostics.len() >= MAX_SYNTAX_DIAGNOSTICS {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_lists_rust_definitions_with_their_rows() {
        let source =
            "struct Point;\n\nimpl Point {\n    fn new() -> Self { Point }\n}\n\nfn main() {}\n";
        let document = SyntaxDocument::new("Rust", source, 1).unwrap().unwrap();
        let outline: Vec<(&str, String, usize)> = document
            .symbols()
            .into_iter()
            .map(|symbol| (symbol.kind, symbol.name, symbol.start_row))
            .collect();
        assert_eq!(
            outline,
            vec![
                ("struct", "Point".to_owned(), 0),
                ("impl", "Point".to_owned(), 2),
                ("fn", "new".to_owned(), 3),
                ("fn", "main".to_owned(), 6),
            ]
        );
    }

    #[test]
    fn outline_lists_python_and_shell_definitions() {
        let python = SyntaxDocument::new(
            "Python",
            "class Box:\n    def open(self):\n        pass\n",
            1,
        )
        .unwrap()
        .unwrap();
        let names: Vec<String> = python
            .symbols()
            .into_iter()
            .map(|symbol| symbol.name)
            .collect();
        assert_eq!(names, ["Box", "open"]);

        let shell = SyntaxDocument::new("Shell", "greet() {\n  echo hi\n}\n", 1)
            .unwrap()
            .unwrap();
        let names: Vec<String> = shell
            .symbols()
            .into_iter()
            .map(|symbol| symbol.name)
            .collect();
        assert_eq!(names, ["greet"]);
    }

    #[test]
    fn languages_without_definitions_have_an_empty_outline() {
        let json = SyntaxDocument::new("JSON", "{\"a\": 1}\n", 1)
            .unwrap()
            .unwrap();
        assert!(json.symbols().is_empty());
    }

    #[test]
    fn line_index_points_match_counting_from_the_top() {
        for source in ["", "a", "ab\ncd\n", "\n\n", "x\r\ny\nz", "é\n🙂z\n"] {
            let index = LineIndex::new(source);
            for byte in (0..=source.len()).filter(|byte| source.is_char_boundary(*byte)) {
                let prefix = &source[..byte];
                let row = prefix.matches('\n').count();
                let column = prefix
                    .rsplit_once('\n')
                    .map_or(prefix.len(), |(_, tail)| tail.len());
                assert_eq!(
                    index.point(byte),
                    Point::new(row, column),
                    "{source:?} @ {byte}"
                );
            }
        }
    }

    #[test]
    fn line_index_matches_str_lines_exactly() {
        for source in [
            "",
            "one",
            "one\n",
            "one\ntwo",
            "a\r\nb\r\n",
            "a\r\nb\r",
            "\n\n\n",
            "lone\rcarriage\nreturn",
            "తెలుగు\n日本語 🙂\r\nxé\u{301}",
        ] {
            let index = LineIndex::new(source);
            let expected: Vec<&str> = source.lines().collect();
            for row in 0..expected.len() + 3 {
                assert_eq!(
                    index.line(source, row),
                    expected.get(row).copied().unwrap_or(""),
                    "{source:?} row {row}"
                );
            }
        }
    }

    /// Audit: every highlighted row rescanned the file from the top, so a
    /// 150,000-line JSON file took ~0.5 s per keypress near its end.
    #[test]
    fn highlighting_deep_rows_matches_highlighting_the_row_alone() {
        let row_text = "  {\"id\": 7, \"name\": \"é🙂\", \"ok\": true},";
        let mut source = String::from("[\n");
        for _ in 0..5_000 {
            source.push_str(row_text);
            source.push('\n');
        }
        source.push_str("  {\"id\": 7, \"name\": \"é🙂\", \"ok\": true}\n]\n");
        let deep = SyntaxDocument::new("JSON", &source, 1).unwrap().unwrap();
        let alone = SyntaxDocument::new("JSON", &format!("[\n{row_text}\n{{}}]\n"), 1)
            .unwrap()
            .unwrap();
        let expected = alone.highlight_spans_for_row(1);
        assert!(!expected.is_empty());
        assert_eq!(deep.highlight_spans_for_row(4_000), expected);
        assert_eq!(deep.highlight_spans_for_row(5_000), expected);
    }

    #[test]
    fn goal16_query_highlighting_classifies_rust_semantics() {
        let document = SyntaxDocument::new(
            "Rust",
            "fn main() { let value = \"hello\"; } // comment\n",
            1,
        )
        .unwrap()
        .unwrap();
        assert!(document.semantic_highlighting_enabled());
        let spans = document.highlight_spans_for_row(0);
        assert!(
            spans
                .iter()
                .any(|span| span.token_type == TokenType::Keyword)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.token_type == TokenType::Function)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.token_type == TokenType::String)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.token_type == TokenType::Comment)
        );
    }

    #[test]
    fn goal16_semantic_highlights_follow_incremental_reparse_and_unicode_columns() {
        let mut document = SyntaxDocument::new("Python", "print(\"తెలుగు\")\n", 1)
            .unwrap()
            .unwrap();
        let initial = document.highlight_spans_for_row(0);
        assert!(
            initial
                .iter()
                .any(|span| span.token_type == TokenType::Function)
        );
        assert!(
            initial
                .iter()
                .any(|span| span.token_type == TokenType::String)
        );

        document
            .sync("Python", "value = 42\nprint(\"తెలుగు🙂\")\n", 2)
            .unwrap();
        let row_zero = document.highlight_spans_for_row(0);
        let row_one = document.highlight_spans_for_row(1);
        assert!(
            row_zero
                .iter()
                .any(|span| span.token_type == TokenType::Number)
        );
        assert!(
            row_one
                .iter()
                .any(|span| span.token_type == TokenType::String)
        );
        assert_eq!(document.incremental_reparse_count(), 1);
    }

    #[test]
    fn release_readiness_terraform_uses_semantic_hcl_highlighting() {
        let source =
            "resource \"aws_s3_bucket\" \"x\" {\n  bucket = \"demo\"\n  force_destroy = true\n}\n";
        let document = SyntaxDocument::new("Terraform", source, 1)
            .unwrap()
            .unwrap();
        assert!(document.semantic_highlighting_enabled());
        assert!(!document.has_errors());

        let header = document.highlight_spans_for_row(0);
        assert!(
            header
                .iter()
                .any(|span| span.token_type == TokenType::ConstantOrType)
        );
        assert!(
            header
                .iter()
                .any(|span| span.token_type == TokenType::String)
        );

        let attribute = document.highlight_spans_for_row(1);
        assert!(
            attribute
                .iter()
                .any(|span| span.token_type == TokenType::Text)
        );
        assert!(
            attribute
                .iter()
                .any(|span| span.token_type == TokenType::String)
        );
    }

    #[test]
    fn supported_languages_cover_core_config_and_code_files() {
        for language in ["Rust", "Python", "JSON", "Shell", "YAML", "Terraform"] {
            assert!(
                supports_language(language),
                "{language} should be supported"
            );
        }
        assert!(!supports_language("Plain Text"));
    }

    /// Skipping error-free subtrees must not lose any error or missing
    /// node that a full walk of the tree would report.
    #[test]
    fn pruned_error_walk_finds_everything_a_full_walk_finds() {
        fn full_walk(node: Node<'_>, found: &mut Vec<(usize, usize, bool)>) {
            if node.is_error() || node.is_missing() {
                let start = node.start_position();
                found.push((start.row, start.column, node.is_missing()));
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                full_walk(child, found);
            }
        }

        for (language, source) in [
            ("Rust", "fn main() { let x = 1 }\nfn ok() {}\n"),
            (
                "Rust",
                "fn main( {\n    let y = [1, 2;\n}\nstruct S { a: u8 b: u8 }\n",
            ),
            ("JSON", "{\"a\": 1, \"b\": [1, 2,, 3], \"c\" 4}\n"),
            ("Python", "def f(:\n    return 1\nx = (1, 2\n"),
            ("YAML", "key: [1, 2\nother: {a: 1\n"),
        ] {
            let document = SyntaxDocument::new(language, source, 1).unwrap().unwrap();
            let mut expected = Vec::new();
            full_walk(document.tree.root_node(), &mut expected);
            assert!(
                !expected.is_empty(),
                "{language}: fixture should be invalid"
            );
            let lines = LineIndex::new(source);
            let reported: Vec<(usize, usize, bool)> = document
                .diagnostics()
                .iter()
                .map(|diagnostic| {
                    let column = lines
                        .line(source, diagnostic.row)
                        .graphemes(true)
                        .take(diagnostic.col)
                        .map(str::len)
                        .sum();
                    (
                        diagnostic.row,
                        column,
                        diagnostic.message.starts_with("Missing"),
                    )
                })
                .collect();
            assert_eq!(reported, expected, "{language}: {source:?}");
        }
    }

    #[test]
    fn rust_parser_reports_real_syntax_errors() {
        let valid = SyntaxDocument::new("Rust", "fn main() { let x = 1; }", 1)
            .unwrap()
            .unwrap();
        assert!(!valid.has_errors());

        let invalid = SyntaxDocument::new("Rust", "fn main( {", 1)
            .unwrap()
            .unwrap();
        assert!(invalid.has_errors());
        assert!(
            invalid
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("Syntax"))
        );
    }

    #[test]
    fn reparses_incrementally_after_source_change() {
        let mut document = SyntaxDocument::new("Rust", "fn main() {\n}\n", 1)
            .unwrap()
            .unwrap();
        assert_eq!(document.parse_generation(), 1);
        assert_eq!(document.incremental_reparse_count(), 0);

        let changed = "fn main() {\n    let value = 1;\n}\n";
        assert!(document.sync("Rust", changed, 2).unwrap());
        assert_eq!(document.revision(), 2);
        assert_eq!(document.parse_generation(), 2);
        assert_eq!(document.incremental_reparse_count(), 1);
        assert!(!document.has_errors());

        assert!(!document.sync("Rust", changed, 2).unwrap());
        assert_eq!(document.incremental_reparse_count(), 1);
    }

    #[test]
    fn unicode_edit_diff_keeps_tree_valid_and_positions_safe() {
        let mut document = SyntaxDocument::new("Python", "name = \"తెలుగు\"\nprint(name)\n", 1)
            .unwrap()
            .unwrap();

        document
            .sync("Python", "name = \"తెలుగు🙂\"\nprint(name)\n", 2)
            .unwrap();

        assert_eq!(document.incremental_reparse_count(), 1);
        assert!(!document.has_errors());
    }

    #[test]
    fn language_change_reconfigures_parser_without_incremental_claim() {
        let mut document = SyntaxDocument::new("Rust", "fn main() {}", 1)
            .unwrap()
            .unwrap();
        assert!(document.sync("Python", "print('hello')\n", 2).unwrap());
        assert_eq!(document.language_name(), "Python");
        assert_eq!(document.incremental_reparse_count(), 0);
        assert!(!document.has_errors());
    }
}
