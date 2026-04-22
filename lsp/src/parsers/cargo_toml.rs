use toml_edit::{ImDocument, Item, Value};

use crate::manifest::RawEntry;
use crate::position::LineIndex;

const GROUPS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

pub fn parse(source: &str) -> Vec<RawEntry> {
    // `ImDocument` preserves source spans on every value; `DocumentMut` drops
    // them. We only ever read from the document, so immutable is correct.
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
    // Also support `[target.'cfg(...)'.dependencies]` — only one level deep,
    // which covers the common case without getting tangled in TOML subtables.
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
    group: &str,
    out: &mut Vec<RawEntry>,
) {
    for (key, item) in table.iter() {
        match item {
            // `serde = "1.0"`
            Item::Value(v @ Value::String(_)) => {
                let Some(span) = v.span() else { continue };
                let (inner, literal) = trim_quotes(source, span);
                out.push(RawEntry {
                    name: key.to_string(),
                    version_literal: literal,
                    version_range: idx.range(inner),
                    name_range: name_range(idx, source, table, key),
                    group: Some(group.to_string()),
                });
            }
            // `serde = { version = "1.0", features = [...] }`
            Item::Value(Value::InlineTable(tbl)) => {
                if let Some(v) = tbl.get("version") {
                    if matches!(v, Value::String(_)) {
                        if let Some(span) = v.span() {
                            let (inner, literal) = trim_quotes(source, span);
                            out.push(RawEntry {
                                name: key.to_string(),
                                version_literal: literal,
                                version_range: idx.range(inner),
                                name_range: name_range(idx, source, table, key),
                                group: Some(group.to_string()),
                            });
                        }
                    }
                }
            }
            Item::Table(sub) => {
                // `[dependencies.serde]` block-table style.
                if let Some(Item::Value(v @ Value::String(_))) = sub.get("version") {
                    if let Some(span) = v.span() {
                        let (inner, literal) = trim_quotes(source, span);
                        out.push(RawEntry {
                            name: key.to_string(),
                            version_literal: literal,
                            version_range: idx.range(inner),
                            name_range: name_range(idx, source, table, key),
                            group: Some(group.to_string()),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

fn trim_quotes(source: &str, span: std::ops::Range<usize>) -> (std::ops::Range<usize>, String) {
    // `Formatted::span()` includes the surrounding quotes. Strip a single
    // quote on each side if present; leave everything else alone.
    let bytes = source.as_bytes();
    let mut start = span.start;
    let mut end = span.end;
    if let Some(&b) = bytes.get(start) {
        if b == b'"' || b == b'\'' {
            start += 1;
        }
    }
    if end > start {
        if let Some(&b) = bytes.get(end - 1) {
            if b == b'"' || b == b'\'' {
                end -= 1;
            }
        }
    }
    let literal = source.get(start..end).unwrap_or("").to_string();
    (start..end, literal)
}

fn name_range(
    idx: &LineIndex,
    _source: &str,
    table: &toml_edit::Table,
    key: &str,
) -> tower_lsp::lsp_types::Range {
    // `Table::key()` gives us back the decor+span of the key token.
    if let Some(k) = table.key(key) {
        if let Some(span) = k.span() {
            return idx.range(span);
        }
    }
    // Fallback: a zero-width range at the start of the file. The name_range
    // is used for hovers only, so this degradation is tolerable.
    idx.range(0..0)
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
