use std::ops::Range;

use toml_edit::{ImDocument, Item, Value};

use crate::manifest::RawEntry;
use crate::parsers::trim_matching_quote;
use crate::position::LineIndex;

const GROUPS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

pub fn parse(source: &str) -> Vec<RawEntry> {
    // `ImDocument` preserves source spans; `DocumentMut` strips them.
    let Ok(doc) = ImDocument::parse(source) else {
        return Vec::new();
    };
    let idx = LineIndex::new(source);
    let mut out = Vec::new();

    for group in GROUPS {
        if let Some(table) = doc.get(group).and_then(Item::as_table) {
            collect_table(&idx, source, table, group, &mut out);
        }
    }
    // `[target.'cfg(...)'.dependencies]` — one level of nesting, which covers
    // the common case without getting tangled in arbitrary subtables.
    if let Some(target) = doc.get("target").and_then(Item::as_table) {
        for (_cfg, cfg_item) in target.iter() {
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

fn collect_table(
    idx: &LineIndex,
    source: &str,
    table: &toml_edit::Table,
    group: &'static str,
    out: &mut Vec<RawEntry>,
) {
    for (key, item) in table.iter() {
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

fn push_entry(
    out: &mut Vec<RawEntry>,
    idx: &LineIndex,
    source: &str,
    table: &toml_edit::Table,
    key: &str,
    group: &'static str,
    span: Range<usize>,
) {
    // `Value::span()` on a string includes the surrounding quotes.
    let (inner, literal) = trim_matching_quote(source, span);
    out.push(RawEntry {
        name: key.to_string(),
        version_literal: literal,
        version_range: idx.range(inner),
        name_range: name_range(idx, table, key),
        group: Some(group),
    });
}

fn name_range(idx: &LineIndex, table: &toml_edit::Table, key: &str) -> tower_lsp::lsp_types::Range {
    // `Table::key()` returns the key token with span + decor. Fall back to a
    // zero-width range at the document start if the span is missing — the
    // name_range is used only for hovers, so the degradation is harmless.
    table
        .key(key)
        .and_then(|k| k.span())
        .map(|s| idx.range(s))
        .unwrap_or_else(|| idx.range(0..0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn parses_inline_and_detailed_forms() {
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
