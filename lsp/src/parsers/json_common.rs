use jsonc_parser::ast::{ObjectPropName, Value};
use jsonc_parser::common::Ranged;
use jsonc_parser::{CollectOptions, ParseOptions, parse_to_ast};

use crate::manifest::RawEntry;
use crate::position::LineIndex;

/// Extract `{ "name": "version", ... }` pairs from the named top-level
/// groups of a JSON(C) document. Both `package.json` and `composer.json`
/// use this shape, just with different group names.
pub fn parse_deps(source: &str, groups: &[&'static str]) -> Vec<RawEntry> {
    let idx = LineIndex::new(source);
    let parse_result = match parse_to_ast(
        source,
        &CollectOptions::default(),
        &ParseOptions {
            allow_comments: true,
            allow_loose_object_property_names: true,
            allow_trailing_commas: true,
            ..Default::default()
        },
    ) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let root = match parse_result.value {
        Some(Value::Object(o)) => o,
        _ => return Vec::new(),
    };

    let mut entries = Vec::new();
    for group in groups {
        let Some(prop) = root
            .properties
            .iter()
            .find(|p| prop_name_str(&p.name) == *group)
        else {
            continue;
        };
        let Value::Object(deps) = &prop.value else {
            continue;
        };
        for p in &deps.properties {
            let Value::StringLit(ver) = &p.value else {
                continue;
            };
            // The string's `range()` span includes the quotes. Shrink to
            // the inner content so our inlay hints land flush with the
            // closing quote.
            let raw_span = ver.range();
            let inner = (raw_span.start + 1)..(raw_span.end.saturating_sub(1));
            let inner = inner.start..inner.end.max(inner.start);

            let name_span = match &p.name {
                ObjectPropName::String(s) => s.range(),
                ObjectPropName::Word(w) => w.range(),
            };

            entries.push(RawEntry {
                name: prop_name_str(&p.name).to_string(),
                version_literal: ver.value.to_string(),
                version_range: idx.range(inner),
                name_range: idx.range(name_span.start..name_span.end),
                group: Some(*group),
            });
        }
    }
    entries
}

fn prop_name_str<'a>(n: &'a ObjectPropName) -> &'a str {
    match n {
        ObjectPropName::String(s) => s.value.as_ref(),
        ObjectPropName::Word(w) => w.value,
    }
}
