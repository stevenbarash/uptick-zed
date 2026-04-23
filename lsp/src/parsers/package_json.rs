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
        // `json_common` opts into the permissive flags that make this work.
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
        let src = r#"{ "dependencies": { "weird": { "version": "1.0.0" } } }"#;
        let entries = parse(src);
        assert!(entries.is_empty());
    }
}
