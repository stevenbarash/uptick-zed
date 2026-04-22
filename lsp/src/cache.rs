use std::time::{Duration, Instant};

use dashmap::DashMap;
use semver::Version;

use crate::manifest::ManifestKind;

/// Result of a registry lookup for a package.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub latest_stable: Option<Version>,
    pub latest_any: Option<Version>,
    pub url: Option<String>,
}

#[derive(Clone)]
struct Entry {
    info: VersionInfo,
    at: Instant,
}

/// Thread-safe TTL cache keyed by (ecosystem, package name).
pub struct VersionCache {
    entries: DashMap<(ManifestKind, String), Entry>,
    ttl: Duration,
}

impl VersionCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: DashMap::new(),
            ttl,
        }
    }

    pub fn get(&self, kind: ManifestKind, name: &str) -> Option<VersionInfo> {
        let key = (kind, name.to_owned());
        let entry = self.entries.get(&key)?;
        if entry.at.elapsed() > self.ttl {
            drop(entry);
            self.entries.remove(&key);
            return None;
        }
        Some(entry.info.clone())
    }

    pub fn put(&self, kind: ManifestKind, name: String, info: VersionInfo) {
        self.entries.insert(
            (kind, name),
            Entry {
                info,
                at: Instant::now(),
            },
        );
    }
}
