#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

impl Cursor {
    pub const fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}
