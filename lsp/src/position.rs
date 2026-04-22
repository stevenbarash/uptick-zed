use tower_lsp::lsp_types::{Position, Range};

/// Converts byte offsets in a UTF-8 source string into LSP [`Position`] values
/// (line + UTF-16 column, which is the default position encoding in LSP).
pub struct LineIndex<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            source,
            line_starts,
        }
    }

    pub fn position(&self, byte_offset: usize) -> Position {
        let byte_offset = byte_offset.min(self.source.len());
        let line = self
            .line_starts
            .partition_point(|&s| s <= byte_offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line];
        let utf16_col: usize = self.source[line_start..byte_offset]
            .chars()
            .map(|c| c.len_utf16())
            .sum();
        Position {
            line: line as u32,
            character: utf16_col as u32,
        }
    }

    pub fn range(&self, span: std::ops::Range<usize>) -> Range {
        Range {
            start: self.position(span.start),
            end: self.position(span.end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_positions() {
        let src = "abc\ndef\n";
        let idx = LineIndex::new(src);
        assert_eq!(idx.position(0), Position::new(0, 0));
        assert_eq!(idx.position(3), Position::new(0, 3));
        assert_eq!(idx.position(4), Position::new(1, 0));
        assert_eq!(idx.position(6), Position::new(1, 2));
    }

    #[test]
    fn utf8_columns_are_utf16() {
        // "å" is 2 bytes in UTF-8, 1 unit in UTF-16.
        let src = "å=1";
        let idx = LineIndex::new(src);
        // byte offset 2 is right after "å"
        assert_eq!(idx.position(2), Position::new(0, 1));
    }
}
