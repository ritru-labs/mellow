use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    cursor: usize,
}

impl TextInput {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.grapheme_count();
    }

    pub fn insert_char(&mut self, ch: char) {
        let byte = self.byte_offset(self.cursor);
        self.text.insert(byte, ch);
        self.cursor += 1;
    }

    pub fn insert_text(&mut self, text: &str) {
        let single_line = text.replace(['\r', '\n', '\t'], " ");
        if single_line.is_empty() {
            return;
        }

        let byte = self.byte_offset(self.cursor);
        let added = single_line.graphemes(true).count();
        self.text.insert_str(byte, &single_line);
        self.cursor += added;
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        let start = self.byte_offset(self.cursor - 1);
        let end = self.byte_offset(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
        true
    }

    pub fn delete(&mut self) -> bool {
        let count = self.grapheme_count();
        if self.cursor >= count {
            return false;
        }

        let start = self.byte_offset(self.cursor);
        let end = self.byte_offset(self.cursor + 1);
        self.text.replace_range(start..end, "");
        true
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.grapheme_count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.grapheme_count();
    }

    pub fn visible_window(&self, max_columns: usize) -> (String, usize) {
        if max_columns == 0 {
            return (String::new(), 0);
        }

        let graphemes: Vec<&str> = self.text.graphemes(true).collect();
        let cursor = self.cursor.min(graphemes.len());
        let before_budget = max_columns.saturating_sub(1);

        let mut start = cursor;
        let mut cursor_width = 0usize;
        while start > 0 {
            let width = grapheme_width(graphemes[start - 1]);
            if cursor_width + width > before_budget {
                break;
            }
            cursor_width += width;
            start -= 1;
        }

        let mut rendered = String::new();
        let mut used = 0usize;
        for grapheme in graphemes.iter().skip(start) {
            let width = grapheme_width(grapheme);
            if used + width > max_columns {
                break;
            }
            rendered.push_str(grapheme);
            used += width;
        }

        (rendered, cursor_width.min(max_columns.saturating_sub(1)))
    }

    fn grapheme_count(&self) -> usize {
        self.text.graphemes(true).count()
    }

    fn byte_offset(&self, grapheme_index: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .nth(grapheme_index)
            .map(|(offset, _)| offset)
            .unwrap_or(self.text.len())
    }
}

fn grapheme_width(grapheme: &str) -> usize {
    UnicodeWidthStr::width(grapheme).max(1)
}

#[cfg(test)]
mod tests {
    use super::TextInput;

    #[test]
    fn edits_in_the_middle_by_grapheme() {
        let mut input = TextInput::default();
        input.set("ab😀d");
        input.move_left();
        input.insert_char('c');
        assert_eq!(input.as_str(), "ab😀cd");

        input.move_left();
        assert!(input.backspace());
        assert_eq!(input.as_str(), "abcd");
    }

    #[test]
    fn combining_grapheme_backspaces_as_one_unit() {
        let mut input = TextInput::default();
        input.set("e\u{301}x");
        input.move_left();
        assert!(input.backspace());
        assert_eq!(input.as_str(), "x");
    }

    #[test]
    fn pasted_text_stays_single_line() {
        let mut input = TextInput::default();
        input.insert_text("save\nall\tfiles");
        assert_eq!(input.as_str(), "save all files");
    }

    #[test]
    fn visible_window_keeps_caret_visible() {
        let mut input = TextInput::default();
        input.set("0123456789");
        let (view, cursor_x) = input.visible_window(5);
        assert_eq!(view, "6789");
        assert_eq!(cursor_x, 4);

        input.home();
        let (view, cursor_x) = input.visible_window(5);
        assert_eq!(view, "01234");
        assert_eq!(cursor_x, 0);
    }
}
