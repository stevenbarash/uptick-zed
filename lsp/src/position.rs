//! Byte-offset → LSP `Position` conversion.
//!
//! Parsers naturally return byte ranges into the original source string,
//! but LSP speaks in `(line, utf16_column)` coordinates. This module bridges
//! the two without re-scanning the file for every conversion.

use tower_lsp::lsp_types::{Position, Range};

/// Converts byte offsets in a UTF-8 source string into LSP [`Position`] values
/// (line + UTF-16 column, which is the default position encoding in LSP).
///
/// Construction is `O(n)` over the source — we make one pass to record the
/// byte offset of every line start. After that, every `position()` call is
/// `O(log n)` (binary search over the line table) plus a scan of the current
/// line's UTF-8 → UTF-16 conversion.
///
/// One `LineIndex` is built per parse and handed to every entry in that parse.
pub struct LineIndex<'a> {
    /// The raw document text. Borrowed, so we don't copy megabytes of source.
    source: &'a str,
    /// Byte offsets where each line begins. `line_starts[0]` is always 0.
    /// Populated once in `new()` and never modified afterwards.
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    /// Scan the source once, recording the byte offset of every `\n`.
    ///
    /// We operate on bytes rather than chars because line breaks are always
    /// single-byte in UTF-8 — no need to decode multi-byte sequences here.
    pub fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                // The next line starts *after* the newline, at `i + 1`.
                line_starts.push(i + 1);
            }
        }
        Self {
            source,
            line_starts,
        }
    }

    /// Translate a byte offset into an LSP `Position`.
    ///
    /// Bounds-checks the input so callers can pass in ranges derived from
    /// parsers that may have produced slightly-past-EOF spans on malformed
    /// input.
    pub fn position(&self, byte_offset: usize) -> Position {
        // Clamp to EOF so we never index out of bounds below.
        let byte_offset = byte_offset.min(self.source.len());

        // Find the line whose start is the greatest offset ≤ byte_offset.
        // `partition_point` is like `binary_search` but returns the insertion
        // point even when the exact value isn't present, which is what we
        // want here — the offset is usually *between* two line starts.
        let line = self
            .line_starts
            .partition_point(|&s| s <= byte_offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line];

        // Count UTF-16 code units from the start of the line up to our
        // offset. LSP defaults to UTF-16 columns, which disagrees with
        // Rust's native UTF-8 byte offsets for any non-ASCII text.
        // Non-BMP chars (e.g. emoji) count as 2 UTF-16 code units — `len_utf16`
        // handles that for us.
        let utf16_col: usize = self.source[line_start..byte_offset]
            .chars()
            .map(char::len_utf16)
            .sum();

        // Saturate on pathological > 4 GiB documents rather than silently
        // wrapping. `u32::try_from` fails only when the value exceeds
        // `u32::MAX`; we choose a deterministic cap over a panic.
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: u32::try_from(utf16_col).unwrap_or(u32::MAX),
        }
    }

    /// Convenience: translate a byte range into an LSP `Range`.
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
        // Two 3-char lines, each newline-terminated. Verifies basic
        // line-index arithmetic with no multi-byte characters involved.
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
        // This confirms we report *UTF-16* columns, not byte offsets.
        let src = "å=1";
        let idx = LineIndex::new(src);
        // byte offset 2 is right after "å"
        assert_eq!(idx.position(2), Position::new(0, 1));
    }
}
