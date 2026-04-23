//! `package.json` (npm) parser.
//!
//! All the heavy lifting lives in `json_common`; this file just names the
//! four dependency groups npm understands and hands them off.

use crate::manifest::RawEntry;
use crate::parsers::json_common;

/// The four standard npm dependency groups. We treat them all equivalently —
/// each entry's `group` field preserves which one it came from for display
/// purposes (shown in hovers), but they all resolve against the same
/// npm registry.
const GROUPS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

pub fn parse(source: &str) -> Vec<RawEntry> {
    json_common::parse_deps(source, GROUPS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn reads_dependencies_and_dev_dependencies() {
        // Covers the happy path: both `dependencies` and `devDependencies`
        // are walked, and the literal value survives round-trip.
        let src = indoc! {r#"
            {
              "name": "demo",
              "dependencies": {
                "react": "^18.2.0",
                "left-pad": "1.3.0"
              },
              "devDependencies": {
                "typescript": "~5.4.0"
              }
            }
        "#};
        let entries = parse(src);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["react", "left-pad", "typescript"]);
        assert_eq!(entries[0].version_literal, "^18.2.0");
    }

    #[test]
    fn tolerates_trailing_commas_and_comments() {
        // Real editors let users write JSONC; our parser must not choke on
        // it. This is why `json_common` opts into all the permissive flags.
        let src = r#"{
  // comment
  "dependencies": {
    "a": "1.0.0",
  },
}"#;
        let entries = parse(src);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn ignores_non_string_version_values() {
        // Some packages embed objects where strings belong (e.g. monorepo
        // tools). We skip those rather than guess — there's no single
        // upstream version to resolve against.
        let src = r#"{ "dependencies": { "weird": { "version": "1.0.0" } } }"#;
        let entries = parse(src);
        assert!(entries.is_empty());
    }
}
