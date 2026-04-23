//! `Cargo.toml` parser, using `toml_edit`'s span-preserving `Document`.
//!
//! Cargo supports several shapes we need to flatten into a single stream of
//! `RawEntry` values:
//!
//! ```toml
//! [dependencies]
//! serde = "1.0"                              # short form
//! tokio = { version = "1.35", features = [] } # inline-table form
//!
//! [dependencies.tracing]                     # block-table form
//! version = "0.1"
//! default-features = false
//!
//! [target.'cfg(unix)'.dependencies]          # platform-gated
//! libc = "0.2"
//! ```
//!
//! We recurse one level into `[target.'…'.dependencies]` subtables but
//! deliberately don't walk arbitrary nesting — the shapes above cover the
//! vast majority of real crates, and anything more exotic rarely has a
//! plain version string to bump.

use std::ops::Range;

use toml_edit::{Document, Item, Value};

use crate::manifest::RawEntry;
use crate::parsers::trim_matching_quote;
use crate::position::LineIndex;

/// The three standard Cargo dependency-table names. `[target.*]` subtables
/// use these same names, which is why we iterate them twice.
const GROUPS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

pub fn parse(source: &str) -> Vec<RawEntry> {
    // `Document` preserves source spans; the mutable `DocumentMut` strips them.
    // We need spans for hover/inlay positioning, so this is the variant to use.
    let Ok(doc) = Document::parse(source) else {
        return Vec::new();
    };
    let idx = LineIndex::new(source);
    let mut out = Vec::new();

    // Top-level `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`.
    for group in GROUPS {
        if let Some(table) = doc.get(group).and_then(Item::as_table) {
            collect_table(&idx, source, table, group, &mut out);
        }
    }

    // `[target.'cfg(...)'.dependencies]` — one level of nesting, which covers
    // the common case without getting tangled in arbitrary subtables.
    if let Some(target) = doc.get("target").and_then(Item::as_table) {
        for (_cfg, cfg_item) in target {
            if let Some(cfg_tbl) = cfg_item.as_table() {
                for group in GROUPS {
                    if let Some(tbl) = cfg_tbl.get(group).and_then(Item::as_table) {
                        collect_table(&idx, source, tbl, group, &mut out);
                    }
                }
            }
        }
    }

    out
}

/// Walk one dependency table, matching each of the three shapes documented
/// at the top of the file and producing a `RawEntry` for each.
fn collect_table(
    idx: &LineIndex,
    source: &str,
    table: &toml_edit::Table,
    group: &'static str,
    out: &mut Vec<RawEntry>,
) {
    for (key, item) in table {
        // Figure out *where* the version string lives for this entry. We
        // return `None` for anything we can't handle cleanly (e.g. version
        // given via `workspace = true`, or a `path` dependency without a
        // version), which causes the entry to be silently skipped.
        let span = match item {
            // `serde = "1.0"`
            Item::Value(v @ Value::String(_)) => v.span(),
            // `serde = { version = "1.0", features = [...] }`
            Item::Value(Value::InlineTable(tbl)) => tbl
                .get("version")
                .filter(|v| matches!(v, Value::String(_)))
                .and_then(Value::span),
            // `[dependencies.serde]` block-table form.
            Item::Table(sub) => match sub.get("version") {
                Some(Item::Value(v @ Value::String(_))) => v.span(),
                _ => None,
            },
            _ => None,
        };
        let Some(span) = span else { continue };
        push_entry(out, idx, source, table, key, group, span);
    }
}

/// Build a `RawEntry` once we've located the version-string span.
fn push_entry(
    out: &mut Vec<RawEntry>,
    idx: &LineIndex,
    source: &str,
    table: &toml_edit::Table,
    key: &str,
    group: &'static str,
    span: Range<usize>,
) {
    // `Value::span()` on a string includes the surrounding quotes. Strip
    // them so `version_range` lines up exactly with the literal text, which
    // is what code-action edits will replace and where inlay hints anchor.
    let (inner, literal) = trim_matching_quote(source, span);
    out.push(RawEntry {
        name: key.to_string(),
        version_literal: literal,
        version_range: idx.range(inner),
        name_range: name_range(idx, table, key),
        group: Some(group),
    });
}

/// Locate the source range of a key's identifier (not its value). Used for
/// hover hit-testing so the user can mouse over the crate name as well as
/// the version literal.
fn name_range(idx: &LineIndex, table: &toml_edit::Table, key: &str) -> tower_lsp::lsp_types::Range {
    // `Table::key()` returns the key token with span + decor. Fall back to a
    // zero-width range at the document start if the span is missing — the
    // name_range is used only for hovers, so the degradation is harmless
    // (hover-on-name becomes a no-op).
    table
        .key(key)
        .and_then(toml_edit::Key::span)
        .map_or_else(|| idx.range(0..0), |s| idx.range(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn parses_inline_and_detailed_forms() {
        // One test that covers all three shapes plus a second dep group,
        // ensuring our walker reaches every nest the parser supports.
        let src = indoc! {r#"
            [dependencies]
            serde = "1.0"
            tokio = { version = "1.35", features = ["full"] }

            [dependencies.tracing]
            version = "0.1"
            default-features = false

            [dev-dependencies]
            pretty_assertions = "1.4"
        "#};
        let entries = parse(src);
        let names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"serde".to_string()));
        assert!(names.contains(&"tokio".to_string()));
        assert!(names.contains(&"tracing".to_string()));
        assert!(names.contains(&"pretty_assertions".to_string()));
        let serde = entries.iter().find(|e| e.name == "serde").unwrap();
        assert_eq!(serde.version_literal, "1.0");
    }
}
