//! Shared JSON(C) parser backing `package.json` and `composer.json`.
//!
//! Both npm and Composer manifests follow the same shape — named object
//! groups at the document root, each mapping package names to version
//! strings. This module implements that once; the per-ecosystem parsers
//! just pick which groups to look at.

use jsonc_parser::ast::{ObjectPropName, Value};
use jsonc_parser::common::Ranged;
use jsonc_parser::{parse_to_ast, CollectOptions, ParseOptions};

use crate::manifest::RawEntry;
use crate::position::LineIndex;

/// Extract `{ "name": "version", ... }` pairs from the named top-level
/// groups of a JSON(C) document. Both `package.json` and `composer.json`
/// use this shape, just with different group names.
///
/// `groups` is typically a `'static` slice of group names (e.g.
/// `["dependencies", "devDependencies"]`). We store these names directly
/// on the resulting `RawEntry` — no allocation — via the `'static` lifetime.
pub fn parse_deps(source: &str, groups: &[&'static str]) -> Vec<RawEntry> {
    let idx = LineIndex::new(source);

    // `jsonc_parser` is permissive by design: we turn on tolerances for
    // comments and trailing commas (common in `package.json` and
    // `composer.json` edits-in-progress). A parse failure returns an empty
    // result so the server leaves the buffer alone instead of flashing
    // diagnostics on every keystroke.
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

    // Both manifests put the groups we want at the root. Bail if the root
    // is something weird (array, string literal, missing).
    let root = match parse_result.value {
        Some(Value::Object(o)) => o,
        _ => return Vec::new(),
    };

    let mut entries = Vec::new();
    for group in groups {
        // Find the property whose name matches the group. Missing groups
        // are fine — every manifest omits some of them.
        let Some(prop) = root
            .properties
            .iter()
            .find(|p| prop_name_str(&p.name) == *group)
        else {
            continue;
        };
        // We only handle the `{ "name": "version-string", ... }` shape.
        // npm/composer also allow object values (e.g. for git/path specs),
        // but there's no single upstream version to resolve against them.
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
            // `saturating_sub(1)` handles degenerate edge cases like an
            // unterminated string literal that parsed as empty.
            let inner = (raw_span.start + 1)..(raw_span.end.saturating_sub(1));
            // Invariant: `end >= start`. Enforce it explicitly so the
            // downstream `LineIndex::range` call never sees an inverted span.
            let inner = inner.start..inner.end.max(inner.start);

            // Property name spans differ depending on how the source was
            // written: quoted strings and bare identifiers take different
            // AST shapes but represent the same thing. Either way we just
            // want the character range for hover hit-testing.
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

/// Flatten both kinds of object property name (quoted strings vs bare words)
/// into a single `&str` view for comparison / extraction.
fn prop_name_str<'a>(n: &'a ObjectPropName) -> &'a str {
    match n {
        ObjectPropName::String(s) => s.value.as_ref(),
        ObjectPropName::Word(w) => w.value,
    }
}
