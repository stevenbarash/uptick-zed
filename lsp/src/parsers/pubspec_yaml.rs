use crate::manifest::RawEntry;
use crate::position::LineIndex;

/// Minimal pubspec.yaml parser. Pubspec's structure is restricted enough
/// that a line scanner handles 99% of real files:
///
/// ```yaml
/// dependencies:
///   package_name: ^1.2.3
///   quoted: "2.0.0"
///   complex:           # skipped (nested mapping)
///     git: ...
/// dev_dependencies:
///   test: ^1.0.0
/// ```
///
/// Anything with a nested mapping value (git/path/hosted specs) is skipped
/// — there's no single "version" to resolve against pub.dev.
pub fn parse(source: &str) -> Vec<RawEntry> {
    let idx = LineIndex::new(source);
    let mut out = Vec::new();
    let mut in_group: Option<&'static str> = None;
    let mut child_indent: Option<usize> = None;

    let mut byte_offset = 0usize;
    for chunk in source.split_inclusive('\n') {
        let line_start = byte_offset;
        byte_offset += chunk.len();

        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        let leading = line.chars().take_while(|c| *c == ' ').count();
        let rest = &line[leading..];
        if rest.is_empty() || rest.starts_with('#') {
            continue;
        }

        // Top-level keys (column 0) reset the group state.
        if leading == 0 {
            let key = rest.split(':').next().unwrap_or("").trim();
            in_group = match key {
                "dependencies" => Some("dependencies"),
                "dev_dependencies" => Some("dev_dependencies"),
                _ => None,
            };
            child_indent = None;
            continue;
        }

        let Some(group) = in_group else { continue };
        let ci = *child_indent.get_or_insert(leading);
        // Deeper-than-children lines belong to a nested mapping; skip.
        if leading != ci {
            continue;
        }

        let Some(colon_rel) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon_rel].trim().to_string();
        if name.is_empty() {
            continue;
        }

        let after_colon = &rest[colon_rel + 1..];
        let ws = after_colon.chars().take_while(|c| c.is_whitespace()).count();
        let body = &after_colon[ws..];

        // Strip a trailing ` # comment` (YAML requires whitespace before `#`).
        let body_wo_comment = match body.find(" #") {
            Some(i) => &body[..i],
            None => body,
        };
        let body_trim = body_wo_comment.trim_end();
        if body_trim.is_empty() {
            // Nested mapping — skip this entry.
            continue;
        }

        // Handle quoted scalars — single or double, matching opener/closer.
        let is_quoted = |q: char| {
            body_trim.len() >= 2 && body_trim.starts_with(q) && body_trim.ends_with(q)
        };
        let (lit, quote_skip): (String, usize) = if is_quoted('"') || is_quoted('\'') {
            (body_trim[1..body_trim.len() - 1].to_string(), 1)
        } else {
            (body_trim.to_string(), 0)
        };

        let name_visible_end = leading + rest[..colon_rel].trim_end().len();
        let name_range = idx.range((line_start + leading)..(line_start + name_visible_end));

        let version_start = line_start + leading + colon_rel + 1 + ws + quote_skip;
        let version_end = version_start + lit.len();
        let version_range = idx.range(version_start..version_end);

        out.push(RawEntry {
            name,
            version_literal: lit,
            version_range,
            name_range,
            group: Some(group.to_string()),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn parses_simple_deps() {
        let src = indoc! {r#"
            name: demo
            dependencies:
              flutter:
                sdk: flutter
              path: ^1.8.0
              http: "1.2.0"
            dev_dependencies:
              test: ^1.24.0
        "#};
        let entries = parse(src);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["path", "http", "test"]);
        assert_eq!(entries[1].version_literal, "1.2.0");
    }

    #[test]
    fn stops_at_next_top_level_key() {
        let src = indoc! {r#"
            dependencies:
              a: 1.0.0
            flutter:
              uses-material-design: true
        "#};
        let entries = parse(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a");
    }
}
