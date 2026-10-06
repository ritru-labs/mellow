use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{buffer::Buffer, cursor::Cursor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualRow {
    pub source_row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub start_display_col: usize,
    pub display_width: usize,
}

impl VisualRow {
    pub const fn is_continuation(self) -> bool {
        self.start_col > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualPosition {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone)]
pub struct VisualLayout {
    rows: Vec<VisualRow>,
    tab_width: usize,
}

impl VisualLayout {
    pub fn new(buffer: &Buffer, width: usize, tab_width: usize, wrap: bool) -> Self {
        let width = width.max(1);
        let tab_width = tab_width.max(1);
        let mut rows = Vec::new();

        for source_row in 0..buffer.line_count() {
            let line = buffer.line_text(source_row);
            let graphemes: Vec<&str> = line.graphemes(true).collect();
            let prefix = display_prefix(&graphemes, tab_width);

            if !wrap {
                rows.push(VisualRow {
                    source_row,
                    start_col: 0,
                    end_col: graphemes.len(),
                    start_display_col: 0,
                    display_width: *prefix.last().unwrap_or(&0),
                });
                continue;
            }

            if graphemes.is_empty() {
                rows.push(VisualRow {
                    source_row,
                    start_col: 0,
                    end_col: 0,
                    start_display_col: 0,
                    display_width: 0,
                });
                continue;
            }

            let mut start = 0usize;
            while start < graphemes.len() {
                let mut hard_end = start;
                while hard_end < graphemes.len() {
                    let next = hard_end + 1;
                    let segment_width = prefix[next].saturating_sub(prefix[start]);
                    if segment_width > width && hard_end > start {
                        break;
                    }
                    hard_end = next;
                    if segment_width >= width {
                        break;
                    }
                }

                if hard_end == start {
                    hard_end = (start + 1).min(graphemes.len());
                }

                let end = if hard_end < graphemes.len() {
                    preferred_word_break(&graphemes, start, hard_end).unwrap_or(hard_end)
                } else {
                    hard_end
                };

                let end = end.max(start + 1).min(graphemes.len());
                rows.push(VisualRow {
                    source_row,
                    start_col: start,
                    end_col: end,
                    start_display_col: prefix[start],
                    display_width: prefix[end].saturating_sub(prefix[start]),
                });
                start = end;
            }
        }

        if rows.is_empty() {
            rows.push(VisualRow {
                source_row: 0,
                start_col: 0,
                end_col: 0,
                start_display_col: 0,
                display_width: 0,
            });
        }

        Self { rows, tab_width }
    }

    /// A layout for one empty row, for callers that only consult the
    /// layout when word wrap is on.
    pub fn empty(tab_width: usize) -> Self {
        Self {
            rows: vec![VisualRow {
                source_row: 0,
                start_col: 0,
                end_col: 0,
                start_display_col: 0,
                display_width: 0,
            }],
            tab_width: tab_width.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, index: usize) -> Option<VisualRow> {
        self.rows.get(index).copied()
    }

    pub fn visual_position_for_cursor(&self, buffer: &Buffer, cursor: Cursor) -> VisualPosition {
        let source_row = cursor.row.min(buffer.line_count().saturating_sub(1));
        let source_col = cursor.col.min(buffer.grapheme_count(source_row));

        let mut last_match = None;
        for (visual_row, row) in self.rows.iter().copied().enumerate() {
            if row.source_row != source_row {
                continue;
            }
            last_match = Some((visual_row, row));

            if source_col < row.end_col {
                let absolute = buffer.display_width_before(source_row, source_col, self.tab_width);
                return VisualPosition {
                    row: visual_row,
                    column: absolute.saturating_sub(row.start_display_col),
                };
            }

            if source_col == row.start_col && row.start_col > 0 {
                return VisualPosition {
                    row: visual_row,
                    column: 0,
                };
            }

            if source_col == row.end_col {
                let next_is_same_line = self.rows.get(visual_row + 1).is_some_and(|next| {
                    next.source_row == source_row && next.start_col == source_col
                });
                if next_is_same_line && source_col < buffer.grapheme_count(source_row) {
                    continue;
                }

                return VisualPosition {
                    row: visual_row,
                    column: row.display_width,
                };
            }
        }

        let (row_index, row) = last_match.unwrap_or((0, self.rows[0]));
        VisualPosition {
            row: row_index,
            column: row.display_width,
        }
    }

    pub fn cursor_for_visual_position(
        &self,
        buffer: &Buffer,
        visual_row: usize,
        column: usize,
    ) -> Cursor {
        let row_index = visual_row.min(self.rows.len().saturating_sub(1));
        let row = self.rows[row_index];
        let line = buffer.line_text(row.source_row);
        let graphemes: Vec<&str> = line.graphemes(true).collect();

        if row.start_col >= row.end_col || graphemes.is_empty() {
            return Cursor::new(row.source_row, row.start_col);
        }

        let target = row.start_display_col.saturating_add(column);
        let mut absolute = row.start_display_col;

        for (offset, grapheme) in graphemes[row.start_col..row.end_col].iter().enumerate() {
            let width = grapheme_width(grapheme, absolute, self.tab_width);
            if target < absolute.saturating_add(width) {
                return Cursor::new(row.source_row, row.start_col + offset);
            }
            absolute = absolute.saturating_add(width);
        }

        Cursor::new(row.source_row, row.end_col)
    }

    pub fn clamp_scroll(&self, scroll: usize, viewport_rows: usize) -> usize {
        let viewport_rows = viewport_rows.max(1);
        let max_scroll = self.rows.len().saturating_sub(viewport_rows);
        scroll.min(max_scroll)
    }
}

fn display_prefix(graphemes: &[&str], tab_width: usize) -> Vec<usize> {
    let mut prefix = Vec::with_capacity(graphemes.len() + 1);
    let mut display = 0usize;
    prefix.push(display);
    for grapheme in graphemes {
        display = display.saturating_add(grapheme_width(grapheme, display, tab_width));
        prefix.push(display);
    }
    prefix
}

fn grapheme_width(grapheme: &str, absolute_col: usize, tab_width: usize) -> usize {
    if grapheme == "\t" {
        tab_width - (absolute_col % tab_width)
    } else {
        UnicodeWidthStr::width(grapheme).max(1)
    }
}

fn preferred_word_break(graphemes: &[&str], start: usize, hard_end: usize) -> Option<usize> {
    (start + 1..=hard_end)
        .rev()
        .find(|&end| graphemes[end - 1].chars().all(char::is_whitespace))
        .filter(|&end| end > start)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn buffer_with(text: &str) -> Buffer {
        let mut buffer = Buffer::empty(Some(PathBuf::from("visual.txt")));
        let mut cursor = Cursor::new(0, 0);
        buffer.insert_text(&mut cursor, text);
        buffer
    }

    #[test]
    fn wrap_prefers_word_boundary_without_losing_source_columns() {
        let buffer = buffer_with("alpha beta gamma");
        let layout = VisualLayout::new(&buffer, 7, 4, true);
        let rows = &layout.rows;

        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].start_col, rows[0].end_col), (0, 6));
        assert_eq!((rows[1].start_col, rows[1].end_col), (6, 11));
        assert_eq!((rows[2].start_col, rows[2].end_col), (11, 16));

        let reconstructed: String = rows
            .iter()
            .map(|row| {
                let start = Cursor::new(row.source_row, row.start_col);
                let end = Cursor::new(row.source_row, row.end_col);
                buffer.get_text_range(start, end)
            })
            .collect();
        assert_eq!(reconstructed, "alpha beta gamma");
    }

    #[test]
    fn wide_unicode_and_tabs_never_stall_or_drop_graphemes() {
        let buffer = buffer_with("a\t漢字🙂తెలుగు xyz");
        let layout = VisualLayout::new(&buffer, 5, 4, true);

        assert!(layout.len() > 1);
        for row in &layout.rows {
            assert!(row.end_col >= row.start_col);
            assert!(row.end_col > row.start_col || buffer.grapheme_count(row.source_row) == 0);
        }

        let last = layout.rows.last().copied().unwrap();
        assert_eq!(last.end_col, buffer.grapheme_count(0));
    }

    #[test]
    fn source_visual_round_trip_is_stable_at_every_grapheme_boundary() {
        let buffer = buffer_with("ab 漢🙂 cd");
        let layout = VisualLayout::new(&buffer, 4, 4, true);

        for col in 0..=buffer.grapheme_count(0) {
            let source = Cursor::new(0, col);
            let visual = layout.visual_position_for_cursor(&buffer, source);
            let round_trip = layout.cursor_for_visual_position(&buffer, visual.row, visual.column);
            assert_eq!(round_trip, source, "round trip failed for source col {col}");
        }
    }

    #[test]
    fn resize_reflows_visual_rows_without_changing_source_cursor() {
        let buffer = buffer_with("one two three four five");
        let cursor = Cursor::new(0, 13);
        let narrow = VisualLayout::new(&buffer, 6, 4, true);
        let wide = VisualLayout::new(&buffer, 20, 4, true);

        let narrow_pos = narrow.visual_position_for_cursor(&buffer, cursor);
        let wide_pos = wide.visual_position_for_cursor(&buffer, cursor);

        assert!(narrow.len() > wide.len());
        assert_ne!(narrow_pos.row, wide_pos.row);
        assert_eq!(
            narrow.cursor_for_visual_position(&buffer, narrow_pos.row, narrow_pos.column),
            cursor
        );
        assert_eq!(
            wide.cursor_for_visual_position(&buffer, wide_pos.row, wide_pos.column),
            cursor
        );
    }

    #[test]
    fn no_wrap_layout_preserves_one_visual_row_per_source_line() {
        let buffer = buffer_with("very long logical line\nsecond");
        let layout = VisualLayout::new(&buffer, 5, 4, false);

        assert_eq!(layout.len(), 2);
        assert_eq!(layout.row(0).unwrap().source_row, 0);
        assert_eq!(layout.row(1).unwrap().source_row, 1);
    }
}
