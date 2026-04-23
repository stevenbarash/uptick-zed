//! Manifest parsers — one per ecosystem.
//!
//! Every parser has the same signature:
//!
//! ```ignore
//! fn parse(source: &str) -> Vec<RawEntry>
//! ```
//!
//! and the same contract:
//!
//! - Parse errors are **not** fatal. Return the entries you did manage to
//!   extract. Users are typing — partial input is the common case.
//! - Produce `version_range`s that exclude surrounding quotes.
//! - Produce UTF-16 column positions (use `LineIndex::range`).
//!
//! `json_common` holds the shared JSON AST walker used by `package.json` and
//! `composer.json`; it's not called from outside the parsers module.

pub mod cargo_toml;
pub mod composer_json;
pub mod json_common;
pub mod package_json;
pub mod pubspec_yaml;

use std::ops::Range;

use crate::manifest::{ManifestKind, RawEntry};

/// Run the right parser for this manifest kind. Parse errors are not fatal:
/// each parser returns whatever entries it managed to extract (partial input
/// is common while the user is mid-edit).
pub fn parse(kind: ManifestKind, source: &str) -> Vec<RawEntry> {
    match kind {
        ManifestKind::Npm => package_json::parse(source),
        ManifestKind::Cargo => cargo_toml::parse(source),
        ManifestKind::Pub => pubspec_yaml::parse(source),
        ManifestKind::Composer => composer_json::parse(source),
    }
}

/// Strip a matching pair of `"` or `'` quotes from the ends of a span,
/// returning the inner byte range and the literal text. Non-quote bytes
/// are left untouched on each side independently.
///
/// Used by the TOML parser, which reports spans that *include* the string
/// delimiters; the JSON parser already hands us inner spans.
pub(crate) fn trim_matching_quote(source: &str, span: Range<usize>) -> (Range<usize>, String) {
    let bytes = source.as_bytes();
    let mut start = span.start;
    let mut end = span.end;
    // We check each side independently: if only one side has a quote (odd,
    // but possible on a malformed literal), we still trim the one we see.
    if matches!(bytes.get(start), Some(b'"' | b'\'')) {
        start += 1;
    }
    if end > start && matches!(bytes.get(end - 1), Some(b'"' | b'\'')) {
        end -= 1;
    }
    // `get(..)` saturates to an empty string if the computed range is
    // invalid (e.g. after trimming both quotes off an empty `""`).
    let literal = source.get(start..end).unwrap_or("").to_string();
    (start..end, literal)
}
