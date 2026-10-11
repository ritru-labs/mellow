//! Small local templates and numbered, linked tab stops. Offsets are Unicode
//! scalar indices, matching Rope edits; the UI converts them to grapheme cursors.
use crate::{buffer::Buffer, cursor::Cursor};
use anyhow::{Result, bail};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    pub stops: Vec<Vec<(usize, usize)>>,
}

pub fn templates() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Shell function",
            "${1:function_name}() {\n    ${2:command}\n}\n$0",
        ),
        (
            "Terraform resource",
            "resource \"${1:type}\" \"${2:name}\" {\n  ${3:argument} = ${4:value}\n}\n$0",
        ),
        ("YAML mapping", "${1:parent}:\n  ${2:key}: ${3:value}\n$0"),
        ("Linked variable", "${1:name} = ${2:value}\nprint($1)\n$0"),
    ]
}

enum Token {
    Text(String),
    Stop(u16, Option<String>),
}

pub fn parse(body: &str) -> Result<Snippet> {
    if body.len() > 64 * 1024 {
        bail!("snippet exceeds the 64 KiB limit");
    }
    let chars: Vec<char> = body.chars().collect();
    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut defaults = BTreeMap::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\\'
            && index + 1 < chars.len()
            && matches!(chars[index + 1], '$' | '\\' | '}')
        {
            literal.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if chars[index] != '$'
            || index + 1 == chars.len()
            || (!chars[index + 1].is_ascii_digit() && chars[index + 1] != '{')
        {
            literal.push(chars[index]);
            index += 1;
            continue;
        }
        if !literal.is_empty() {
            tokens.push(Token::Text(std::mem::take(&mut literal)));
        }
        index += 1;
        let braced = chars[index] == '{';
        if braced {
            index += 1;
        }
        let start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        if start == index {
            bail!("snippet supports numbered placeholders only");
        }
        let number: u16 = chars[start..index]
            .iter()
            .collect::<String>()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid snippet tab stop"))?;
        if number > 99 {
            bail!("snippet tab stops must be between 0 and 99");
        }
        let mut default = None;
        if braced {
            if chars.get(index) == Some(&':') {
                index += 1;
                let mut value = String::new();
                while index < chars.len() && chars[index] != '}' {
                    if chars[index] == '\\'
                        && index + 1 < chars.len()
                        && matches!(chars[index + 1], '$' | '\\' | '}')
                    {
                        value.push(chars[index + 1]);
                        index += 2;
                        continue;
                    }
                    if chars[index] == '$' && chars.get(index + 1) == Some(&'{') {
                        bail!("nested snippet placeholders are unsupported");
                    }
                    value.push(chars[index]);
                    index += 1;
                }
                default = Some(value);
            }
            if chars.get(index) != Some(&'}') {
                bail!("unclosed or unsupported snippet placeholder");
            }
            index += 1;
        }
        if number == 0 && default.as_ref().is_some_and(|value| !value.is_empty()) {
            bail!("final snippet stop must be empty");
        }
        if let Some(value) = default.as_ref() {
            if defaults
                .get(&number)
                .is_some_and(|previous| previous != value)
            {
                bail!("linked snippet defaults disagree");
            }
            defaults.insert(number, value.clone());
        }
        tokens.push(Token::Stop(number, default));
    }
    if !literal.is_empty() {
        tokens.push(Token::Text(literal));
    }
    let mut text = String::new();
    let mut offset = 0;
    let mut groups: BTreeMap<u16, Vec<(usize, usize)>> = BTreeMap::new();
    for token in tokens {
        match token {
            Token::Text(value) => {
                offset += value.chars().count();
                text.push_str(&value);
            }
            Token::Stop(number, _default) => {
                let value = defaults.get(&number).map_or("", String::as_str);
                let end = offset + value.chars().count();
                let group = groups.entry(number).or_default();
                if group.contains(&(offset, end)) {
                    bail!(
                        "coincident linked placeholders are unsupported; separate them with text"
                    );
                }
                group.push((offset, end));
                text.push_str(value);
                offset = end;
            }
        }
    }
    let final_stop = groups.remove(&0).unwrap_or_else(|| vec![(offset, offset)]);
    if final_stop.len() != 1 {
        bail!("snippet must have exactly one final stop");
    }
    let mut stops: Vec<_> = groups.into_values().collect();
    stops.push(final_stop);
    Ok(Snippet { text, stops })
}

#[derive(Clone, Debug)]
pub struct Session {
    stops: Vec<Vec<(usize, usize)>>,
    active: usize,
    valid: bool,
    expected_revision: u64,
    edit_text: Option<String>,
}

impl Session {
    pub fn new(snippet: &Snippet, start: usize, revision: u64) -> Self {
        Self {
            stops: snippet
                .stops
                .iter()
                .map(|group| group.iter().map(|(a, b)| (a + start, b + start)).collect())
                .collect(),
            active: 0,
            valid: true,
            expected_revision: revision,
            edit_text: None,
        }
    }
    pub fn ranges(&self, buffer: &Buffer) -> Vec<(Cursor, Cursor)> {
        if self.expected_revision != buffer.revision() {
            return Vec::new();
        }
        self.current()
            .iter()
            .map(|(start, end)| {
                (
                    buffer.cursor_from_char_index(*start),
                    buffer.cursor_from_char_index(*end),
                )
            })
            .collect()
    }
    pub fn next(&mut self, buffer: &Buffer) -> bool {
        if self.expected_revision != buffer.revision() {
            self.valid = false;
            return false;
        }
        self.edit_text = None;
        self.advance()
    }
    pub fn replace(&mut self, buffer: &mut Buffer, cursor: &mut Cursor, text: &str) -> bool {
        if self.expected_revision != buffer.revision() || !self.valid || self.is_final() {
            self.valid = false;
            return false;
        }
        let mut staged = self.clone();
        let normalized = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', buffer.line_ending().as_str());
        let mut accumulated = self.edit_text.clone().unwrap_or_default();
        accumulated.push_str(&normalized);
        let Ok(edits) = staged.replace_current(&accumulated) else {
            self.valid = false;
            return false;
        };
        let mut replacements = Vec::new();
        for (start, end, replacement) in edits {
            let a = buffer.cursor_from_char_index(start);
            let b = buffer.cursor_from_char_index(end);
            // Do not split a grapheme when a template boundary fell inside it.
            if buffer.char_index(a.row, a.col) != start || buffer.char_index(b.row, b.col) != end {
                self.valid = false;
                return false;
            }
            replacements.push((a, b, replacement));
        }
        let cursors = buffer.replace_ranges_with_cursors(&replacements, *cursor);
        if let Some(first) = cursors.first() {
            *cursor = *first;
        }
        staged.expected_revision = buffer.revision();
        staged.edit_text = Some(accumulated);
        *self = staged;
        true
    }
    pub fn current(&self) -> &[(usize, usize)] {
        if self.valid {
            self.stops.get(self.active).map_or(&[], Vec::as_slice)
        } else {
            &[]
        }
    }
    pub fn is_final(&self) -> bool {
        self.valid && self.active + 1 == self.stops.len()
    }
    /// Advances through numbered groups, then the final caret. A further Tab
    /// ends the session and lets normal indentation resume.
    pub fn advance(&mut self) -> bool {
        if !self.valid || self.active + 1 >= self.stops.len() {
            self.valid = false;
            false
        } else {
            self.active += 1;
            true
        }
    }

    /// Returns replacements in descending order (safe to apply to the rope as
    /// one undo transaction) and remaps every linked group for the next Tab.
    pub fn replace_current(&mut self, text: &str) -> Result<Vec<(usize, usize, String)>> {
        if !self.valid || self.is_final() {
            bail!("snippet has no editable placeholder");
        }
        let mut ranges = self.current().to_vec();
        ranges.sort_by_key(|range| std::cmp::Reverse(range.0));
        let count = text.chars().count();
        for &(start, end) in &ranges {
            for (group_index, group) in self.stops.iter_mut().enumerate() {
                for range in group {
                    if group_index == self.active && *range == (start, end) {
                        *range = (start, start + count);
                    } else if range.0 >= end {
                        *range = (
                            shift(range.0, start, end, count),
                            shift(range.1, start, end, count),
                        );
                    } else if range.1 > start {
                        self.valid = false;
                        bail!("snippet placeholders overlap");
                    }
                }
            }
        }
        Ok(ranges
            .into_iter()
            .map(|(a, b)| (a, b, text.to_owned()))
            .collect())
    }
}

fn shift(offset: usize, start: usize, end: usize, inserted: usize) -> usize {
    offset.saturating_sub(end - start).saturating_add(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linked_defaults_expand_even_before_the_definition() {
        let snippet = parse("$1 = ${1:🦀name}; ${2:value}$0").unwrap();
        assert_eq!(snippet.text, "🦀name = 🦀name; value");
        assert_eq!(snippet.stops[0], vec![(0, 5), (8, 13)]);
    }
    #[test]
    fn linked_replacements_remap_unicode_stops_and_final_caret() {
        let snippet = parse("${1:x} / $1 / ${2:尾}$0").unwrap();
        let mut session = Session::new(&snippet, 3, 0);
        let replacements = session.replace_current("🦀ab").unwrap();
        assert_eq!(
            replacements,
            vec![(7, 8, "🦀ab".to_owned()), (3, 4, "🦀ab".to_owned())]
        );
        assert_eq!(session.current(), &[(3, 6), (9, 12)]);
        assert!(session.advance());
        assert_eq!(session.current(), &[(15, 16)]);
        assert!(session.advance());
        assert!(session.is_final());
        assert_eq!(session.current(), &[(16, 16)]);
        assert!(!session.advance());
        assert!(session.current().is_empty());
    }
    #[test]
    fn external_edits_invalidate_ranges_and_tab_traversal() {
        let snippet = parse("${1:name}$0").unwrap();
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::default();
        buffer.insert_text(&mut cursor, &snippet.text);
        let mut session = Session::new(&snippet, 0, buffer.revision());
        assert!(!session.ranges(&buffer).is_empty());
        buffer.insert_text(&mut Cursor::default(), "unrelated");
        assert!(session.ranges(&buffer).is_empty());
        assert!(!session.next(&buffer));
    }
    #[test]
    fn buffer_linked_edit_is_one_undoable_change_and_stale_sessions_stop() {
        let snippet = parse("${1:x} = $1; ${2:tail}$0").unwrap();
        let mut buffer = Buffer::empty(None);
        let mut cursor = Cursor::default();
        buffer.insert_text(&mut cursor, &snippet.text);
        let mut session = Session::new(&snippet, 0, buffer.revision());
        assert!(session.replace(&mut buffer, &mut cursor, "🦀"));
        assert_eq!(buffer.contents(), "🦀 = 🦀; tail");
        assert!(session.replace(&mut buffer, &mut cursor, "a"));
        assert_eq!(buffer.contents(), "🦀a = 🦀a; tail");
        buffer.undo(&mut cursor);
        assert_eq!(buffer.contents(), "🦀 = 🦀; tail");
        assert!(!session.replace(&mut buffer, &mut cursor, "stale"));
    }

    #[test]
    fn invalid_or_unsupported_syntax_is_actionable() {
        for body in [
            "${1:broken",
            "${word}",
            "${1:a} ${1:b}",
            "${0:no}",
            "$0 $0",
            "${1:${2:x}}",
            "$100",
        ] {
            assert!(parse(body).is_err(), "accepted {body}");
        }
    }
    #[test]
    fn escaped_shell_dollars_and_every_builtin_work() {
        assert_eq!(
            parse(r"echo \$HOME ${1:value}").unwrap().text,
            "echo $HOME value"
        );
        for (_, body) in templates() {
            assert!(parse(body).is_ok());
        }
    }
    #[test]
    fn linked_multiline_paste_preserves_crlf_and_cannot_split_graphemes() {
        let snippet = parse("${1:x} = $1\r\n${2:tail}$0").unwrap();
        // Loading detects the CRLF convention, unlike a new unnamed buffer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snippet.txt");
        std::fs::write(&path, &snippet.text).unwrap();
        let mut buffer = Buffer::open(Some(path)).unwrap();
        let mut cursor = Cursor::default();
        let mut session = Session::new(&snippet, 0, buffer.revision());
        assert!(session.replace(&mut buffer, &mut cursor, "🦀\n尾"));
        assert_eq!(buffer.contents(), "🦀\r\n尾 = 🦀\r\n尾\r\ntail");
        assert!(session.next(&buffer));
        let stop = session.ranges(&buffer)[0];
        assert_eq!(buffer.get_text_range(stop.0, stop.1), "tail");

        let snippet = parse("${1:e}\u{301}$0").unwrap();
        let mut buffer = Buffer::empty(None);
        buffer.insert_text(&mut Cursor::default(), &snippet.text);
        let mut session = Session::new(&snippet, 0, buffer.revision());
        assert!(!session.replace(&mut buffer, &mut cursor, "replacement"));
        assert_eq!(buffer.contents(), "e\u{301}");
    }
}
