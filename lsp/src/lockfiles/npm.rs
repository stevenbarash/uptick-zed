//! `package-lock.json` (v2/v3) parser. Returns the installed version of
//! every top-level dependency.
//!
//! npm lockfile v2/v3 keys the `packages` map by node_modules path:
//!
//! ```jsonc
//! {
//!   "lockfileVersion": 3,
//!   "packages": {
//!     "": { ... },                                  // workspace root
//!     "node_modules/react": { "version": "18.3.1" }, // top-level
//!     "node_modules/react/node_modules/foo": { ... } // transitive (skipped)
//!   }
//! }
//! ```
//!
//! Manifests only reference direct deps, so transitives (any key
//! containing `/node_modules/` after the prefix) are skipped. Scoped
//! names (`@types/node`) include a `/` inside the name segment and are
//! kept.

use std::collections::HashMap;

use anyhow::Result;
use semver::Version;
use serde::Deserialize;

use super::Resolutions;

#[derive(Deserialize)]
struct Lockfile {
    #[serde(default)]
    packages: HashMap<String, PackageEntry>,
}

#[derive(Deserialize)]
struct PackageEntry {
    version: Option<String>,
}

/// Parse `package-lock.json` v2/v3 text into a `name → installed Version`
/// map. v1 lockfiles (`dependencies` tree) aren't supported — npm 7+
/// has emitted v2+ for years; if we see one in the wild we'll add a
/// parser.
pub fn parse(text: &str) -> Result<Resolutions> {
    let lock: Lockfile = serde_json::from_str(text)?;
    let mut out = Resolutions::new();
    for (key, entry) in lock.packages {
        let Some(name) = key.strip_prefix("node_modules/") else {
            continue;
        };
        // Transitives nest a second `node_modules/` segment. Top-level
        // entries — including scoped `node_modules/@scope/pkg` — never
        // contain that substring.
        if name.contains("/node_modules/") {
            continue;
        }
        let Some(version_str) = entry.version else {
            continue;
        };
        let Ok(version) = Version::parse(&version_str) else {
            continue;
        };
        out.insert(name.to_string(), version);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn extracts_top_level_packages() {
        let lock = indoc! {r#"
            {
              "lockfileVersion": 3,
              "packages": {
                "": { "name": "root", "version": "0.0.0" },
                "node_modules/react": { "version": "18.3.1" },
                "node_modules/lodash": { "version": "4.17.15" }
              }
            }
        "#};
        let res = parse(lock).unwrap();
        assert_eq!(res.get("react"), Some(&Version::parse("18.3.1").unwrap()));
        assert_eq!(res.get("lodash"), Some(&Version::parse("4.17.15").unwrap()));
        assert_eq!(res.len(), 2);
    }

    #[test]
    fn keeps_scoped_packages_skips_transitive_nesting() {
        let lock = indoc! {r#"
            {
              "lockfileVersion": 3,
              "packages": {
                "node_modules/@types/node": { "version": "20.10.0" },
                "node_modules/react": { "version": "18.3.1" },
                "node_modules/react/node_modules/scheduler": { "version": "0.23.0" }
              }
            }
        "#};
        let res = parse(lock).unwrap();
        assert_eq!(
            res.get("@types/node"),
            Some(&Version::parse("20.10.0").unwrap())
        );
        assert!(res.contains_key("react"));
        // Transitive nested under `react/node_modules/` must not surface;
        // the manifest never names it.
        assert!(!res.contains_key("scheduler"));
    }

    #[test]
    fn skips_entries_without_version() {
        let lock = indoc! {r#"
            {
              "lockfileVersion": 3,
              "packages": {
                "node_modules/no-version": {}
              }
            }
        "#};
        let res = parse(lock).unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn missing_packages_block_is_ok() {
        let lock = r#"{ "lockfileVersion": 3 }"#;
        let res = parse(lock).unwrap();
        assert!(res.is_empty());
    }
}
