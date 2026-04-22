use semver::{Version, VersionReq};

/// Strip common leading range operators (`^`, `~`, `>=`, `=`, `v`) so we can
/// compare the literal against a canonical version. We're permissive — any
/// bytes we don't recognise are returned untouched.
pub fn strip_leading(raw: &str) -> &str {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    let mut skip = 0;
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
/// hint when this is `true`, but suppress the diagnostic.
pub fn satisfies(range: &str, latest: &Version) -> bool {
    if let Ok(req) = VersionReq::parse(range) {
        return req.matches(latest);
    }
    // Fall back to exact-match on stripped literal.
    if let Ok(exact) = Version::parse(strip_leading(range)) {
        return &exact == latest;
    }
    false
}

/// Extract the core `x.y.z` from a literal (ignoring any operator).
pub fn parse_literal(raw: &str) -> Option<Version> {
    Version::parse(strip_leading(raw)).ok()
}

/// Is this version a stable release (no pre-release suffix)?
pub fn is_stable(v: &Version) -> bool {
    v.pre.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_operators() {
        assert_eq!(strip_leading("^1.2.3"), "1.2.3");
        assert_eq!(strip_leading("~1.2.3"), "1.2.3");
        assert_eq!(strip_leading(">= 1.2.3"), "1.2.3");
        assert_eq!(strip_leading("v1.2.3"), "1.2.3");
        assert_eq!(strip_leading("1.2.3"), "1.2.3");
    }

    #[test]
    fn satisfies_caret() {
        let v = Version::parse("1.4.0").unwrap();
        assert!(satisfies("^1.2.3", &v));
        assert!(!satisfies("^2.0.0", &v));
    }
}
