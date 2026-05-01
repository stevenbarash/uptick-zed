//! TTL cache for OSV query results. Same policy as `crate::cache::VersionCache`
//! but keyed on `(ManifestKind, package, version)`.

use std::time::{Duration, Instant};

use dashmap::DashMap;
use semver::Version;

use crate::manifest::ManifestKind;
use crate::vulnerabilities::Vulnerability;

/// Thread-safe TTL cache of OSV query results.
///
/// Entries are lazily evicted on read (same policy as `VersionCache`). An
/// empty `Vec<Vulnerability>` is a valid cached answer and means "we asked
/// OSV and the package version is clean"; it is distinguishable from a
/// cache miss, which returns `None`.
pub struct VulnCache {
    entries: DashMap<(ManifestKind, String, Version), Entry>,
    ttl: Duration,
}

#[derive(Clone)]
struct Entry {
    vulns: Vec<Vulnerability>,
    at: Instant,
}

impl VulnCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: DashMap::new(),
            ttl,
        }
    }

    /// Look up `(kind, name, version)`. Returns `None` for both "never
    /// scanned" and "stale"; callers can't tell the difference and don't
    /// need to (either way they'll queue a new scan).
    pub fn get(
        &self,
        kind: ManifestKind,
        name: &str,
        version: &Version,
    ) -> Option<Vec<Vulnerability>> {
        let key = (kind, name.to_owned(), version.clone());
        let entry = self.entries.get(&key)?;
        if entry.at.elapsed() > self.ttl {
            drop(entry);
            self.entries.remove(&key);
            return None;
        }
        Some(entry.vulns.clone())
    }

    /// Insert or refresh a cache entry. Resets the TTL timer.
    pub fn put(
        &self,
        kind: ManifestKind,
        name: String,
        version: Version,
        vulns: Vec<Vulnerability>,
    ) {
        self.entries.insert(
            (kind, name, version),
            Entry {
                vulns,
                at: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use semver::Version;

    use super::*;
    use crate::manifest::ManifestKind;
    use crate::vulnerabilities::Vulnerability;

    fn v(id: &str) -> Vulnerability {
        Vulnerability {
            id: id.to_string(),
            modified: "2024-01-01T00:00:00Z".to_string(),
            summary: None,
            details: None,
            score: None,
        }
    }

    #[test]
    fn put_then_get_roundtrip() {
        let c = VulnCache::new(Duration::from_secs(60));
        let ver = Version::parse("1.2.3").unwrap();
        c.put(
            ManifestKind::Npm,
            "lodash".to_string(),
            ver.clone(),
            vec![v("GHSA-x")],
        );
        let got = c.get(ManifestKind::Npm, "lodash", &ver).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "GHSA-x");
    }

    #[test]
    fn empty_vec_is_cached() {
        let c = VulnCache::new(Duration::from_secs(60));
        let ver = Version::parse("1.0.0").unwrap();
        c.put(
            ManifestKind::Cargo,
            "serde".to_string(),
            ver.clone(),
            vec![],
        );
        assert_eq!(c.get(ManifestKind::Cargo, "serde", &ver), Some(vec![]));
    }

    #[test]
    fn ttl_expires_on_read() {
        let c = VulnCache::new(Duration::from_millis(50));
        let ver = Version::parse("1.0.0").unwrap();
        c.put(
            ManifestKind::Npm,
            "foo".to_string(),
            ver.clone(),
            vec![v("X")],
        );
        sleep(Duration::from_millis(80));
        assert_eq!(c.get(ManifestKind::Npm, "foo", &ver), None);
    }

    #[test]
    fn different_kinds_do_not_collide() {
        let c = VulnCache::new(Duration::from_secs(60));
        let ver = Version::parse("1.0.0").unwrap();
        c.put(
            ManifestKind::Npm,
            "foo".to_string(),
            ver.clone(),
            vec![v("A")],
        );
        c.put(
            ManifestKind::Cargo,
            "foo".to_string(),
            ver.clone(),
            vec![v("B")],
        );
        assert_eq!(c.get(ManifestKind::Npm, "foo", &ver).unwrap()[0].id, "A");
        assert_eq!(c.get(ManifestKind::Cargo, "foo", &ver).unwrap()[0].id, "B");
    }

    #[test]
    fn different_versions_do_not_collide() {
        let c = VulnCache::new(Duration::from_secs(60));
        let v1 = Version::parse("1.0.0").unwrap();
        let v2 = Version::parse("1.0.1").unwrap();
        c.put(
            ManifestKind::Npm,
            "foo".to_string(),
            v1.clone(),
            vec![v("A")],
        );
        c.put(ManifestKind::Npm, "foo".to_string(), v2.clone(), vec![]);
        assert_eq!(c.get(ManifestKind::Npm, "foo", &v1).unwrap()[0].id, "A");
        assert_eq!(c.get(ManifestKind::Npm, "foo", &v2), Some(vec![]));
    }
}
