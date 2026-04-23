//! `pubspec.yaml` (Flutter/Dart) parser.
//!
//! Pubspec is *nominally* YAML, but the subset used for dependencies is so
//! restricted that pulling in a full YAML parser would be overkill. A hand-
//! rolled line scanner handles real-world files fine and — crucially — gives
//! us byte-accurate source spans that YAML libraries tend to lose.
//!
//! The scanner operates in a small state machine:
//!
//! ```text
//! (top level)
//!   → see `dependencies:` / `dev_dependencies:` → enter group
//!   → any other top-level key                   → leave group
//! (inside group)
//!   → `name: value`  (on the group's child-indent level) → emit entry
//!   → deeper indent                                      → nested spec; skip
//!   → different sibling indent (weird)                   → skip
//! ```

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

    // Which group we're currently inside (if any), and at what indent its
    // children live. Reset on any top-level (column-0) key.
    let mut in_group: Option<&'static str> = None;
    let mut child_indent: Option<usize> = None;

    // We iterate by line but keep a rolling byte offset so we can convert
    // line-local positions back to document-wide spans.
    let mut byte_offset = 0usize;
    for chunk in source.split_inclusive('\n') {
        let line_start = byte_offset;
        byte_offset += chunk.len();

        // Strip trailing newline(s). We support both `\n` and `\r\n`.
        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        // YAML indents with spaces only — tabs are illegal. Counting chars
        // (not bytes) is safe because leading spaces are single-byte ASCII.
        let leading = line.chars().take_while(|c| *c == ' ').count();
        let rest = &line[leading..];
        // Blank lines and full-line comments don't change any state.
        if rest.is_empty() || rest.starts_with('#') {
            continue;
        }

        // Top-level keys (column 0) reset the group state. Pubspec has no
        // concept of re-opening a group partway through; any new top-level
        // key ends whatever we were in.
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

        // Not a top-level key, and we're not tracking a dependency group:
        // nothing to do here.
        let Some(group) = in_group else { continue };
        // Pin the expected child indent to whatever we see on the first
        // child line. Subsequent siblings must match.
        let ci = *child_indent.get_or_insert(leading);
        // Deeper-than-children lines belong to a nested mapping (git: ...,
        // path: ..., hosted: ...). Skip.
        if leading != ci {
            continue;
        }

        // Split `name: body` at the first colon. Keys can't contain colons
        // in pubspec's subset, so this is safe.
        let Some(colon_rel) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon_rel].trim().to_string();
        if name.is_empty() {
            continue;
        }

        // Skip any whitespace between the colon and the body. Pubspec files
        // always have a single space after the colon, but we tolerate extra
        // whitespace (or none) without assuming it.
        let after_colon = &rest[colon_rel + 1..];
        let ws = after_colon
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        let body = &after_colon[ws..];

        // Strip a trailing ` # comment` (YAML requires whitespace before `#`
        // for it to start an inline comment; `#` inside a version string
        // would be unusual but isn't a comment without the leading space).
        let body_wo_comment = match body.find(" #") {
            Some(i) => &body[..i],
            None => body,
        };
        let body_trim = body_wo_comment.trim_end();
        if body_trim.is_empty() {
            // Empty body means the value is a nested mapping starting on
            // the next line. We skip these entries — no single version to
            // report for them.
            continue;
        }

        // Handle quoted scalars — single or double, matching opener/closer.
        // `is_quoted` checks for at least two chars so an empty string or a
        // single quote doesn't falsely match.
        let is_quoted =
            |q: char| body_trim.len() >= 2 && body_trim.starts_with(q) && body_trim.ends_with(q);
        let (lit, quote_skip): (String, usize) = if is_quoted('"') || is_quoted('\'') {
            // Strip the quotes for the literal content; remember the 1-byte
            // offset so our `version_range` lines up with the inner text.
            (body_trim[1..body_trim.len() - 1].to_string(), 1)
        } else {
            (body_trim.to_string(), 0)
        };

        // Name range = from the first non-whitespace character on the line
        // up to the last non-whitespace character of the name (exclusive of
        // any trailing space between the name and the colon).
        let name_visible_end = leading + rest[..colon_rel].trim_end().len();
        let name_range = idx.range((line_start + leading)..(line_start + name_visible_end));

        // Version range = the inner literal (quotes excluded).
        // Offset math:
        //   line_start
        //   + leading            (indent spaces)
        //   + colon_rel + 1      (up to and past the colon)
        //   + ws                 (inter-colon/value whitespace)
        //   + quote_skip         (past the opening quote, if any)
        let version_start = line_start + leading + colon_rel + 1 + ws + quote_skip;
        let version_end = version_start + lit.len();
        let version_range = idx.range(version_start..version_end);

        out.push(RawEntry {
            name,
            version_literal: lit,
            version_range,
            name_range,
            group: Some(group),
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
        // Covers: nested mapping (skipped), bare version, quoted version,
        // and transition to dev_dependencies.
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
        // A top-level `flutter:` key must end the `dependencies:` group
        // even though its children are indented the same way.
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
