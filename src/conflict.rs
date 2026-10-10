//! Git merge-conflict blocks: finding one around a row and keeping one side.
//!
//! Pure text logic, so it can be tested without a buffer or a terminal.

/// Which side of a conflict block to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// The side above `=======` (the current branch).
    Ours,
    /// The side below `=======` (the incoming change).
    Theirs,
    /// Both sides, ours first.
    Both,
}

/// One conflict block, as zero-based row indices of its marker lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictBlock {
    /// The `<<<<<<<` line.
    pub start: usize,
    /// The `=======` line.
    pub middle: usize,
    /// The `>>>>>>>` line.
    pub end: usize,
}

/// The conflict block that contains `row`, if the row lies inside one.
///
/// A block is only recognised with all three markers in order, so a stray
/// `=======` (in a Markdown rule, say) does not count as a conflict.
pub fn block_at<S: AsRef<str>>(lines: &[S], row: usize) -> Option<ConflictBlock> {
    let start = (0..=row.min(lines.len().checked_sub(1)?))
        .rev()
        .find(|&index| lines[index].as_ref().starts_with("<<<<<<<"))?;
    let middle =
        (start + 1..lines.len()).find(|&index| lines[index].as_ref().starts_with("======="))?;
    let end =
        (middle + 1..lines.len()).find(|&index| lines[index].as_ref().starts_with(">>>>>>>"))?;
    // The cursor must be on a marker or inside the block, not after it.
    (row <= end).then_some(ConflictBlock { start, middle, end })
}

/// The lines that replace the whole block when `choice` is kept.
pub fn resolved_lines<S: AsRef<str>>(
    lines: &[S],
    block: ConflictBlock,
    choice: ConflictChoice,
) -> Vec<String> {
    let ours = || {
        lines[block.start + 1..block.middle]
            .iter()
            .map(|line| line.as_ref().to_owned())
    };
    let theirs = || {
        lines[block.middle + 1..block.end]
            .iter()
            .map(|line| line.as_ref().to_owned())
    };
    match choice {
        ConflictChoice::Ours => ours().collect(),
        ConflictChoice::Theirs => theirs().collect(),
        ConflictChoice::Both => ours().chain(theirs()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<&'static str> {
        vec![
            "before",
            "<<<<<<< HEAD",
            "ours one",
            "ours two",
            "=======",
            "theirs one",
            ">>>>>>> feature",
            "after",
        ]
    }

    #[test]
    fn finds_the_block_from_any_row_inside_it() {
        for row in 1..=6 {
            assert_eq!(
                block_at(&sample(), row),
                Some(ConflictBlock {
                    start: 1,
                    middle: 4,
                    end: 6
                }),
                "row {row}"
            );
        }
    }

    #[test]
    fn rows_outside_a_block_have_no_block() {
        assert_eq!(block_at(&sample(), 0), None);
        assert_eq!(block_at(&sample(), 7), None);
    }

    #[test]
    fn a_lone_separator_is_not_a_conflict() {
        let lines = ["intro", "=======", "section"];
        assert_eq!(block_at(&lines, 1), None);
    }

    #[test]
    fn keeps_each_side_or_both() {
        let lines = sample();
        let block = block_at(&lines, 2).unwrap();
        assert_eq!(
            resolved_lines(&lines, block, ConflictChoice::Ours),
            ["ours one", "ours two"]
        );
        assert_eq!(
            resolved_lines(&lines, block, ConflictChoice::Theirs),
            ["theirs one"]
        );
        assert_eq!(
            resolved_lines(&lines, block, ConflictChoice::Both),
            ["ours one", "ours two", "theirs one"]
        );
    }

    #[test]
    fn an_empty_side_resolves_to_no_lines() {
        let lines = ["<<<<<<< HEAD", "=======", "theirs", ">>>>>>> x"];
        let block = block_at(&lines, 0).unwrap();
        assert!(resolved_lines(&lines, block, ConflictChoice::Ours).is_empty());
    }
}
