//! Small semver helpers.
//!
//! Manifests rarely contain bare `x.y.z` versions — they're almost always
//! wrapped in a range operator (`^1.2.3`, `~1.2`, `>= 0.5`, `v1.0`). These
//! helpers deal with that wrapping uniformly across ecosystems so the server
//! doesn't need to know which format it's looking at.

use semver::{Version, VersionReq};

/// Strip common leading range operators (`^`, `~`, `>=`, `=`, `v`) so we can
/// compare the literal against a canonical version. We're permissive — any
/// bytes we don't recognise are returned untouched.
///
/// Examples:
///   `"^1.2.3"`   → `"1.2.3"`
///   `">= 1.2"`   → `"1.2"`
///   `"v0.10.0"`  → `"0.10.0"`
///   `"git:…"`    → `"git:…"` (left alone — we'll fail semver parsing later)
pub fn strip_leading(raw: &str) -> &str {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    let mut skip = 0;
    // Walk byte-by-byte skipping anything that looks like a range operator,
    // whitespace, or a `v` prefix. Stop at the first "real" version byte.
    // ASCII-only set, so byte comparison is safe.
    while skip < bytes.len() {
        match bytes[skip] {
            b'^' | b'~' | b'=' | b'>' | b'<' | b'v' | b'V' | b' ' | b'\t' => skip += 1,
            _ => break,
        }
    }
    &trimmed[skip..]
}

/// `true` if `latest` satisfies the user's range — meaning they're already
/// conceptually up-to-date for the constraint they wrote. We still show the
/// hint when this is `true` (the user wants to see the actual current
/// version), but suppress the "update available" diagnostic.
///
/// Two-step matching:
///   1. Try as a full semver VersionReq (handles `^1.2.3`, `>= 1.0, < 2.0`,
///      etc.). This covers the vast majority of real-world entries.
///   2. Fall back to stripping operators and comparing exactly — useful for
///      pubspec's `^1.2.3` that semver happens to parse cleanly too, but
///      also for `v1.2.3`-style Composer tags that `VersionReq` can't.
pub fn satisfies(range: &str, latest: &Version) -> bool {
    if let Ok(req) = VersionReq::parse(range) {
        return req.matches(latest);
    }
    if let Ok(exact) = Version::parse(strip_leading(range)) {
        return &exact == latest;
    }
    false
}

/// Extract the core `x.y.z` from a literal (ignoring any operator).
/// Returns `None` for non-semver literals (git specs, branch names, …).
pub fn parse_literal(raw: &str) -> Option<Version> {
    Version::parse(strip_leading(raw)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_operators() {
        // Every supported operator shape should leave a clean semver string.
        assert_eq!(strip_leading("^1.2.3"), "1.2.3");
        assert_eq!(strip_leading("~1.2.3"), "1.2.3");
        assert_eq!(strip_leading(">= 1.2.3"), "1.2.3");
        assert_eq!(strip_leading("v1.2.3"), "1.2.3");
        // Already-clean input is left alone.
        assert_eq!(strip_leading("1.2.3"), "1.2.3");
    }

    #[test]
    fn satisfies_caret() {
        // `^1.2.3` accepts any 1.x.y ≥ 1.2.3 but nothing in 2.x.
        let v = Version::parse("1.4.0").unwrap();
        assert!(satisfies("^1.2.3", &v));
        assert!(!satisfies("^2.0.0", &v));
    }
}
