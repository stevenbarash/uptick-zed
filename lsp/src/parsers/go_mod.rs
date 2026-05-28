//! `go.mod` parser. Extracts module-path + version-literal pairs from
//! every `require` statement, in both single-line and block form.
//!
//! `go.mod` grammar relevant to us:
//!
//! ```text
//! module github.com/owner/repo
//! go 1.21
//!
//! require github.com/foo/bar v1.2.3                       // single-line
//!
//! require (
//!     github.com/baz/qux v0.5.0
//!     golang.org/x/text  v0.14.0 // indirect
//! )
//!
//! replace github.com/old => github.com/new v1.0.0         // skipped
//! exclude github.com/something v0.0.1                     // skipped
//! ```
//!
//! Skips:
//!   - `module` / `go` / `replace` / `exclude` directives.
//!   - Lines whose only `// indirect` marker means the user didn't ask
//!     for that module directly — they can't bump it from `go.mod`
//!     without first removing the transitive that pulled it in. Hovering
//!     it would be misleading.
//!   - Comments and blank lines.
//!
//! Position math is byte-based against the original source: callers get
//! LSP `Range` values for the version literal (no surrounding whitespace)
//! and the module name (no quotes — `go.mod` doesn't quote these).

use crate::manifest::RawEntry;
use crate::position::LineIndex;

pub fn parse(source: &str) -> Vec<RawEntry> {
    let idx = LineIndex::new(source);
    let mut out = Vec::new();
    let mut in_require_block = false;
    let mut line_start: usize = 0;

    for line in source.split_inclusive('\n') {
        // Snapshot the offset; `line_start` advances at end-of-loop
        // regardless of which branch we take below.
        let line_offset = line_start;
        line_start += line.len();

        // `line` ends with '\n'; strip it for matching but keep the
        // original length for offset bookkeeping.
        let trimmed_nl = line.trim_end_matches('\n').trim_end_matches('\r');
        let stripped = trimmed_nl.trim_start();
        let lead_ws = trimmed_nl.len() - stripped.len();

        if !in_require_block {
            if stripped.starts_with("require (") {
                in_require_block = true;
                continue;
            }
            // Single-line `require <module> <version>[ // comment]`.
            if let Some(rest) = stripped.strip_prefix("require ") {
                let rest_offset = line_offset + lead_ws + "require ".len();
                if let Some(entry) = parse_require_line(rest, rest_offset, &idx) {
                    out.push(entry);
                }
            }
            // Anything else at top level (module, go, replace, exclude
            // blocks, comments, blank lines) is silently ignored.
            continue;
        }

        // Inside a `require (...)` block. A `)` closes it; everything
        // else is either a require line or noise.
        if stripped.starts_with(')') {
            in_require_block = false;
            continue;
        }
        if stripped.is_empty() || stripped.starts_with("//") {
            continue;
        }
        let rest_offset = line_offset + lead_ws;
        if let Some(entry) = parse_require_line(stripped, rest_offset, &idx) {
            out.push(entry);
        }
    }

    out
}

/// Parse one `<module> <version>[ // comment]` payload. `payload_start`
/// is the byte offset of `payload[0]` in the original source — we need
/// it to translate sub-ranges back into source ranges for LSP.
///
/// Returns `None` for lines marked `// indirect`, since indirect
/// requirements aren't user-controllable from this file.
fn parse_require_line(payload: &str, payload_start: usize, idx: &LineIndex) -> Option<RawEntry> {
    // Strip trailing comment, but remember whether it was `// indirect`.
    let (content, was_indirect) = match payload.find("//") {
        Some(i) => {
            let comment = payload[i + 2..].trim();
            (payload[..i].trim_end(), comment == "indirect")
        }
        None => (payload, false),
    };
    if was_indirect {
        return None;
    }

    // Split on the first whitespace: <module> <rest>.
    let module_end_in_content = content.find(|c: char| c.is_whitespace())?;
    let module = &content[..module_end_in_content];
    let after_module = &content[module_end_in_content..];
    // Find where the version literal actually begins (skip the inter-
    // token whitespace) and where it ends (rest of `content`, since
    // we've already stripped the trailing comment).
    let ver_lead_ws = after_module.len() - after_module.trim_start().len();
    let version_start_in_content = module_end_in_content + ver_lead_ws;
    let version = &content[version_start_in_content..].trim_end();
    if version.is_empty() {
        return None;
    }

    let name_start = payload_start;
    let name_end = payload_start + module.len();
    let ver_start = payload_start + version_start_in_content;
    let ver_end = ver_start + version.len();

    Some(RawEntry {
        name: module.to_string(),
        version_literal: version.to_string(),
        version_range: idx.range(ver_start..ver_end),
        name_range: idx.range(name_start..name_end),
        group: Some("require"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn parses_single_line_require() {
        let src = indoc! {r#"
            module example.com/foo
            go 1.21

            require github.com/foo/bar v1.2.3
        "#};
        let entries = parse(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "github.com/foo/bar");
        assert_eq!(entries[0].version_literal, "v1.2.3");
    }

    #[test]
    fn parses_block_require() {
        let src = indoc! {r#"
            module example.com/foo

            require (
                github.com/baz/qux v0.5.0
                golang.org/x/text v0.14.0
            )
        "#};
        let entries = parse(src);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "github.com/baz/qux");
        assert_eq!(entries[0].version_literal, "v0.5.0");
        assert_eq!(entries[1].name, "golang.org/x/text");
        assert_eq!(entries[1].version_literal, "v0.14.0");
    }

    #[test]
    fn skips_indirect_entries() {
        // Indirect deps are pulled in by transitive imports; the user
        // can't bump them directly here, so they don't get a hint.
        let src = indoc! {r#"
            require (
                github.com/direct/dep v1.0.0
                github.com/transitive/dep v2.0.0 // indirect
            )
        "#};
        let entries = parse(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "github.com/direct/dep");
    }

    #[test]
    fn skips_replace_and_exclude_blocks() {
        // Block parsing only enters on `require (` — `replace (` /
        // `exclude (` open blocks of a different shape that don't
        // produce bumpable entries.
        let src = indoc! {r#"
            require github.com/keep/this v1.0.0

            replace github.com/old => github.com/new v2.0.0

            exclude github.com/bad v3.0.0
        "#};
        let entries = parse(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "github.com/keep/this");
    }

    #[test]
    fn ignores_comments_and_blank_lines_in_block() {
        let src = indoc! {r#"
            require (
                // a comment about the next dep
                github.com/foo v1.0.0

                github.com/bar v2.0.0
            )
        "#};
        let entries = parse(src);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn empty_source_returns_empty_vec() {
        assert!(parse("").is_empty());
    }
}
