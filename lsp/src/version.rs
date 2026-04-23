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

/// Lenient parser used by the OSV vulnerability scanner.
///
/// Unlike `parse_literal`, this function accepts npm-style shorthand and
/// range literals by normalising them to a concrete floor version: `^1.2`
/// becomes `1.2.0`, `1.x` becomes `1.0.0`, `>=1.0 <2.0` becomes `1.0.0`.
/// Returns `None` for literals that don't have an identifiable numeric
/// component (`latest`, `file:…`, `github:…`, bare `*`/`x`, empty).
///
/// See spec: docs/superpowers/specs/2026-04-23-osv-vulnerability-scanner-design.md
pub fn parse_for_scan(raw: &str) -> Option<Version> {
    let stripped = strip_leading(raw);

    let narrow_end = stripped
        .bytes()
        .position(|b| matches!(b, b' ' | b'\t' | b'|' | b','))
        .unwrap_or(stripped.len());
    let narrowed = &stripped[..narrow_end];

    if narrowed.is_empty() {
        return None;
    }
    if narrowed.chars().all(|c| matches!(c, '*' | 'x' | 'X' | '.')) {
        return None;
    }

    let mut parts: Vec<String> = narrowed
        .split('.')
        .map(|p| match p {
            "*" | "x" | "X" => "0".to_string(),
            other => other.to_string(),
        })
        .collect();
    while parts.len() < 3 {
        parts.push("0".to_string());
    }

    let rebuilt = parts.join(".");
    Version::parse(&rebuilt).ok()
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

    #[test]
    fn scan_caret_full() {
        assert_eq!(
            parse_for_scan("^1.2.3"),
            Some(Version::parse("1.2.3").unwrap())
        );
    }

    #[test]
    fn scan_caret_missing_patch() {
        assert_eq!(
            parse_for_scan("^1.2"),
            Some(Version::parse("1.2.0").unwrap())
        );
    }

    #[test]
    fn scan_tilde_missing_minor_patch() {
        assert_eq!(parse_for_scan("~1"), Some(Version::parse("1.0.0").unwrap()));
    }

    #[test]
    fn scan_wildcard_patch() {
        assert_eq!(
            parse_for_scan("1.2.x"),
            Some(Version::parse("1.2.0").unwrap())
        );
        assert_eq!(
            parse_for_scan("1.2.*"),
            Some(Version::parse("1.2.0").unwrap())
        );
        assert_eq!(
            parse_for_scan("1.2.X"),
            Some(Version::parse("1.2.0").unwrap())
        );
    }

    #[test]
    fn scan_wildcard_minor() {
        assert_eq!(
            parse_for_scan("1.x"),
            Some(Version::parse("1.0.0").unwrap())
        );
    }

    #[test]
    fn scan_compound_range() {
        assert_eq!(
            parse_for_scan(">=1.0 <2.0"),
            Some(Version::parse("1.0.0").unwrap())
        );
    }

    #[test]
    fn scan_hyphen_range() {
        assert_eq!(
            parse_for_scan("1.2.3 - 2.3.4"),
            Some(Version::parse("1.2.3").unwrap())
        );
    }

    #[test]
    fn scan_or_range() {
        assert_eq!(
            parse_for_scan("1.2.3 || 2.0.0"),
            Some(Version::parse("1.2.3").unwrap())
        );
    }

    #[test]
    fn scan_prerelease_preserved() {
        assert_eq!(
            parse_for_scan("1.2.3-beta.1"),
            Some(Version::parse("1.2.3-beta.1").unwrap())
        );
    }

    #[test]
    fn scan_build_metadata_preserved() {
        assert_eq!(
            parse_for_scan("1.2.3+build.5"),
            Some(Version::parse("1.2.3+build.5").unwrap())
        );
    }

    #[test]
    fn scan_bare_wildcard_rejected() {
        assert_eq!(parse_for_scan("*"), None);
        assert_eq!(parse_for_scan("x"), None);
        assert_eq!(parse_for_scan("X"), None);
        assert_eq!(parse_for_scan(""), None);
    }

    #[test]
    fn scan_non_semver_rejected() {
        assert_eq!(parse_for_scan("latest"), None);
        assert_eq!(parse_for_scan("file:../foo"), None);
        assert_eq!(parse_for_scan("github:user/repo"), None);
    }

    #[test]
    fn scan_with_v_prefix() {
        assert_eq!(
            parse_for_scan("v1.2.3"),
            Some(Version::parse("1.2.3").unwrap())
        );
    }
}
