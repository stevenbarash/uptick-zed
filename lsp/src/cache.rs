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
    /// Highest version with an empty pre-release tag. This is what we display
    /// today — stable releases only.
    pub latest_stable: Option<Version>,
    /// Highest version overall, including pre-releases. Stashed for a future
    /// `--include-prereleases` opt-in.
    pub latest_any: Option<Version>,
    /// Canonical registry URL for the package (used for the hover "registry"
    /// link). `None` if we haven't computed one, but in practice every
    /// provider fills this in.
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
    /// Create an empty cache with the given freshness window.
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
        // `DashMap` keys need to own their components (the internal hash is
        // computed over `(&K,)` so we can't borrow the tuple here).
        let key = (kind, name.to_owned());
        let entry = self.entries.get(&key)?;

        // Lazy eviction: if the entry has gone stale, drop it and report a
        // miss. We must `drop(entry)` first because `remove` would deadlock
        // against the read guard we're still holding.
        if entry.at.elapsed() > self.ttl {
            drop(entry);
            self.entries.remove(&key);
            return None;
        }

        // Clone `VersionInfo` (cheap — a couple of `Option<Version>`s and a
        // `String`) so the caller doesn't hold a lock into the shard.
        Some(entry.info.clone())
    }

    /// Insert or overwrite a package's metadata. Resets the TTL timer.
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
