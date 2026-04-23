//! In-memory TTL cache for registry lookups.
//!
//! Each (ecosystem, package) pair is queried at most once per TTL window.
//! Entries are lazily evicted on read: we don't run a background scavenger,
//! because the cache is bounded by the number of distinct packages the user
//! has open in manifests (hundreds, not millions).

use std::time::{Duration, Instant};

use dashmap::DashMap;
use semver::Version;

use crate::manifest::ManifestKind;

/// Result of a registry lookup for a single package.
///
/// Both `latest_stable` and `latest_any` are `Option` because some registries
/// (a brand-new crate with only prereleases, say) won't have one of them, and
/// because our semver parser rejects some exotic version strings.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    /// Highest version with an empty pre-release tag.
    pub latest_stable: Option<Version>,
    /// Highest version overall, including pre-releases.
    pub latest_any: Option<Version>,
    /// Canonical registry URL for the package (used for the hover "registry"
    /// link).
    pub url: Option<String>,
}

/// Value stored in the DashMap. We keep the `Instant` of insertion so reads
/// can check freshness without consulting anything else.
#[derive(Clone)]
struct Entry {
    info: VersionInfo,
    at: Instant,
}

/// Thread-safe TTL cache keyed by (ecosystem, package name).
///
/// `DashMap` gives us sharded concurrent access — many provider tasks can
/// hit the cache in parallel without contention. The cache outlives any
/// single request: it's wrapped in an `Arc` inside `Backend` and shared
/// across every in-flight resolve task.
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

    /// Look up a package. Returns `None` if the entry is missing *or* expired;
    /// callers can't tell the difference because they don't need to — either
    /// way the answer is "fetch it".
    pub fn get(&self, kind: ManifestKind, name: &str) -> Option<VersionInfo> {
        // The key type is `(ManifestKind, String)`, and `Borrow` doesn't let
        // us look up via `(ManifestKind, &str)` without a wrapper — so we
        // allocate the owned tuple once per lookup.
        let key = (kind, name.to_owned());
        let entry = self.entries.get(&key)?;

        // Lazy eviction: if the entry has gone stale, drop it and report a
        // miss. We must drop the read guard before calling `remove`, which
        // needs the shard's write lock and would otherwise block on us.
        if entry.at.elapsed() > self.ttl {
            drop(entry);
            self.entries.remove(&key);
            return None;
        }

        // Clone `VersionInfo` (cheap — a couple of `Option<Version>`s and a
        // `String`) so the caller doesn't hold a lock into the shard.
        Some(entry.info.clone())
    }

    /// Resets the TTL timer on the entry.
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
