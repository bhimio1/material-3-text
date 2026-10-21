//! Display columns: tabs expand to tab stops, and with word wrap a line is
//! split into rows of a fixed number of columns. Every display column is one
//! char of the expanded line, so rows can be sliced by char index.

/// Tab width and the wrap width in columns (`None`: no wrapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wrap {
    pub cols: Option<usize>,
    pub tab: usize,
}

/// Where each row of a line starts (display columns), wrapping after
/// spaces when there's one in the row, else mid-word.
pub fn row_starts(chars: impl Iterator<Item = char>, tab: usize, width: usize) -> Vec<usize> {
    let width = width.max(8);
    let mut starts = vec![0];
    let mut row_start = 0;
    let mut col = 0;
    let mut after_space: Option<usize> = None;
    for c in chars {
        if c == '\n' {
            break;
        }
        let w = if c == '\t' { tab_advance(col, tab) } else { 1 };
        while col + w > row_start + width && col > row_start {
            let brk = match after_space {
                Some(b) if b > row_start + width / 3 && b <= col => b,
                _ => col,
            };
            starts.push(brk);
            row_start = brk;
            after_space = None;
        }
        col += w;
        if c == ' ' || c == '\t' {
            after_space = Some(col);
        }
    }
    starts
}

impl Wrap {
    /// Row start columns of a line (`[0]` without wrapping).
    pub fn starts(&self, text: &str) -> Vec<usize> {
        match self.cols {
            None => vec![0],
            Some(w) => row_starts(text.chars(), self.tab, w),
        }
    }

    /// Visual rows of a line.
    pub fn rows(&self, text: &str) -> usize {
        self.starts(text).len()
    }

    /// (row, column within the row) of a display column.
    pub fn row_of(&self, text: &str, dcol: usize) -> (usize, usize) {
        let starts = self.starts(text);
        let row = starts.partition_point(|s| *s <= dcol).saturating_sub(1);
        (row, dcol - starts[row])
    }

    /// The display column at `col` within `row`, kept on that row.
    pub fn col_at(&self, text: &str, row: usize, col: usize) -> usize {
        let starts = self.starts(text);
        let row = row.min(starts.len() - 1);
        let end = match starts.get(row + 1) {
            Some(next) => next - 1,
            None => width(text, self.tab),
        };
        (starts[row] + col).min(end)
    }
}

/// The display range of `row`, given the row starts and the line width.
pub fn row_range(starts: &[usize], display_len: usize, row: usize) -> (usize, usize) {
    let a = starts.get(row).copied().unwrap_or(display_len).min(display_len);
    let b = starts.get(row + 1).copied().unwrap_or(display_len).min(display_len);
    (a, b)
}

fn tab_advance(col: usize, tab: usize) -> usize {
    let tab = tab.max(1);
    tab - col % tab
}

/// Display width of a line.
pub fn width(text: &str, tab: usize) -> usize {
    let mut col = 0;
    for c in text.chars() {
        col += if c == '\t' { tab_advance(col, tab) } else { 1 };
    }
    col
}

/// Display column of the char at `char_col`.
pub fn display_col(text: &str, char_col: usize, tab: usize) -> usize {
    let mut col = 0;
    for c in text.chars().take(char_col) {
        col += if c == '\t' { tab_advance(col, tab) } else { 1 };
    }
    col
}

/// The char index nearest to display column `target`.
pub fn char_at_col(text: &str, target: usize, tab: usize) -> usize {
    let mut col = 0;
    for (i, c) in text.chars().enumerate() {
        let w = if c == '\t' { tab_advance(col, tab) } else { 1 };
        if target < col + w {
            return if target - col > w / 2 { i + 1 } else { i };
        }
        col += w;
    }
    text.chars().count()
}

/// A line as drawn: tabs expanded (and whitespace made visible when asked),
/// with the display column of every char (plus one past the end).
pub struct Expanded {
    pub text: String,
    pub cols: Vec<usize>,
}

pub const SPACE_MARK: char = '·';
pub const TAB_MARK: char = '→';

pub fn expand(text: &str, tab: usize, show_whitespace: bool) -> Expanded {
    let mut out = String::with_capacity(text.len() + 8);
    let mut cols = Vec::with_capacity(text.len() + 1);
    let mut col = 0;
    for c in text.chars() {
        cols.push(col);
        if c == '\t' {
            let w = tab_advance(col, tab);
            out.push(if show_whitespace { TAB_MARK } else { ' ' });
            out.extend(std::iter::repeat_n(' ', w - 1));
            col += w;
        } else {
            out.push(if show_whitespace && c == ' ' { SPACE_MARK } else if c.is_control() { '\u{FFFD}' } else { c });
            col += 1;
        }
    }
    cols.push(col);
    Expanded { text: out, cols }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(width("a\tb", 4), 5);
        assert_eq!(display_col("a\tb", 2, 4), 4);
        assert_eq!(char_at_col("a\tb", 4, 4), 2);
        assert_eq!(char_at_col("a\tb", 3, 4), 2, "nearer the tab's end");
        assert_eq!(char_at_col("a\tb", 1, 4), 1);
        assert_eq!(char_at_col("ab", 10, 4), 2);
        let e = expand("\tx y", 4, true);
        assert_eq!(e.text, "→   x·y");
        assert_eq!(e.cols, vec![0, 4, 5, 6, 7]);
    }

    #[test]
    fn wrapping_rows() {
        let w = Wrap { cols: Some(8), tab: 4 };
        assert_eq!(w.rows(""), 1);
        assert_eq!(w.rows("12345678"), 1);
        assert_eq!(w.rows("123456789"), 2);
        assert_eq!(w.row_of("123456789", 8), (1, 0));
        assert_eq!(w.col_at("1234567890", 0, 20), 7);
        assert_eq!(w.col_at("1234567890", 1, 20), 10);
        assert_eq!(row_range(&w.starts("1234567890"), 10, 1), (8, 10));
        let none = Wrap { cols: None, tab: 4 };
        assert_eq!(none.rows("x".repeat(500).as_str()), 1);
    }

    #[test]
    fn wraps_after_spaces() {
        let w = Wrap { cols: Some(10), tab: 4 };
        // "hello big world": breaks after "big ", not inside "world".
        assert_eq!(w.starts("hello big world"), vec![0, 10]);
        assert_eq!(w.starts("hello wonderful world"), vec![0, 6, 16]);
        // No space far enough in: break mid-word.
        assert_eq!(w.starts("abcdefghijklmnopqrstuvwxy"), vec![0, 10, 20]);
        assert_eq!(w.row_of("hello wonderful world", 6), (1, 0));
        assert_eq!(w.col_at("hello wonderful world", 0, 9), 5, "stays before the next row");
    }
}
