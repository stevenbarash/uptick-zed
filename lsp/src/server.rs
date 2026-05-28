//! The LSP server itself — the `tower-lsp` `LanguageServer` impl that
//! glues parsers, providers, cache, and the editor together.
//!
//! # Concurrency model
//!
//! - One `Arc<DashMap>` per kind of state (docs, pushed fingerprints,
//!   pending tasks). Every LSP handler is `async` and non-blocking.
//! - Network fetches fan out in parallel across entries, capped by
//!   per-host semaphores in [`crate::providers`].
//! - A new `did_change` aborts the prior pending resolve for that URI,
//!   so we never spawn more than one resolver per buffer at a time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dashmap::DashMap;
use reqwest::Client;
use semver::Version;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::notification::Progress as ProgressNotification;
use tower_lsp::lsp_types::request::WorkDoneProgressCreate;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client as LspClient, LanguageServer};
use tracing::{debug, warn};

use crate::cache::VersionCache;
use crate::lockfiles::{self, LockfileSnapshot, Resolutions};
use crate::manifest::{ManifestKind, RawEntry};
use crate::parsers;
use crate::providers;
use crate::version;
use crate::vulnerabilities::{
    cache::{DetailCache, VulnCache},
    Vulnerability,
};

/// How long a fetched version stays usable before we re-query the registry.
/// A one-hour window balances freshness against politeness across all four
/// registries.
const CACHE_TTL: Duration = Duration::from_secs(3600);
/// Vulnerabilities are essentially immutable once published, so we
/// can cache severity scores much longer than version metadata.
const DETAIL_TTL: Duration = Duration::from_secs(24 * 3600);

/// Debounce window applied to `did_change`. Short enough that users see
/// updates within a pause in typing, long enough that holding a key down
/// doesn't fire a fetch every keystroke.
const DEBOUNCE: Duration = Duration::from_millis(250);

const SERVER_NAME: &str = "uptick-lsp";
const DIAGNOSTIC_SOURCE: &str = "uptick";
/// Server-defined command names. Declared once so the capability list,
/// the code-lens emitters, and the dispatcher can't drift.
const CMD_BUMP: &str = "uptick.bump";
const CMD_OPEN: &str = "uptick.open";
/// Below this many uncached packages, don't bother showing a progress
/// banner — the resolve burst will finish faster than the banner
/// itself renders and just flashes.
const PROGRESS_THRESHOLD: usize = 5;
/// Message shown via a line-0 banner diagnostic when an entire resolve
/// burst's registry calls all failed. Cleared on the next burst with
/// any success.
const NETWORK_BANNER: &str =
    "Uptick: no registry reachable — check network/proxy. Set UPTICK_LOG=debug for details.";

/// User-Agent string. crates.io *requires* a descriptive UA — the default
/// reqwest UA gets 403'd — and the other registries appreciate it too.
/// Built at compile time from the Cargo package version.
const USER_AGENT: &str = concat!(
    "uptick-lsp/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/stevenbarash/uptick-zed)"
);

/// A `RawEntry` augmented with the latest version we've resolved so far.
/// `latest: None` means the entry is still being fetched (or the fetch
/// failed — we retry lazily on next reparse).
#[derive(Debug, Clone)]
struct Annotated {
    entry: RawEntry,
    latest: Option<Version>,
    /// Vulnerabilities known to affect the parsed `entry.version_literal`.
    vulns: Vec<Vulnerability>,
}

/// Immutable snapshot of one document. We always replace the `Arc` wholesale
/// rather than mutate in place, so LSP handlers can cheaply clone the `Arc`
/// and operate on a stable view without holding any locks.
#[derive(Debug)]
struct DocState {
    kind: ManifestKind,
    entries: Vec<Annotated>,
    /// `true` when the most recent non-empty resolve burst saw every
    /// registry call fail — surfaced as a line-0 banner diagnostic so
    /// the user knows the silence isn't because Uptick is dead.
    network_failure: bool,
    /// Direct-dependency `name → installed Version` resolved from the
    /// sibling lockfile. Empty when no lockfile is present (or the
    /// kind has no lockfile support yet). The vulnerability scanner
    /// targets these installed versions in preference to the manifest
    /// literal so a `^1.0.0` pin whose lockfile shows `1.0.7` actually
    /// scans `1.0.7` against OSV.
    resolutions: Arc<Resolutions>,
}

/// The `tower-lsp` service state. One `Backend` exists for the lifetime of
/// the process, shared across all `async` handler invocations.
///
/// Every field is `Arc`-wrapped (either directly or via `DashMap`) so the
/// spawn-and-forget resolve tasks in `schedule_resolve` can hold their own
/// references independent of the handler that created them.
pub struct Backend {
    /// Client handle for pushing diagnostics and triggering inlay-hint /
    /// code-lens refreshes.
    client: LspClient,
    /// Shared HTTP client for registry calls. `reqwest::Client` is
    /// connection-pooled internally, so we want exactly one.
    http: Client,
    cache: Arc<VersionCache>,
    vuln_cache: Arc<VulnCache>,
    detail_cache: Arc<DetailCache>,
    /// Current parsed state of every open document. Keyed by URI.
    docs: Arc<DashMap<Url, Arc<DocState>>>,
    /// Last-pushed fingerprint per doc. Skips the refresh/diagnostics storm
    /// when a reparse produced no user-visible changes (common while typing
    /// inside whitespace or comments).
    pushed: Arc<DashMap<Url, u64>>,
    /// In-flight debounced resolve tasks, keyed by document. A new `did_change`
    /// aborts the prior task so we only do one network round-trip per burst.
    pending: Arc<DashMap<Url, JoinHandle<()>>>,
    /// Monotonic counter for `$/progress` tokens, so concurrent resolves
    /// for different documents don't collide on the same token id.
    progress_seq: Arc<AtomicU64>,
    /// Parsed lockfile snapshots keyed by absolute path, reused across
    /// resolve bursts when the lockfile's mtime hasn't advanced. A
    /// `cargo update` between manifest edits is picked up automatically
    /// because the next mtime comparison invalidates the entry.
    lockfiles: Arc<DashMap<PathBuf, Arc<LockfileSnapshot>>>,
}

impl Backend {
    pub fn new(client: LspClient) -> Self {
        // Single shared HTTP client. Pool connections, set the required UA,
        // and cap each request at 10 seconds so a hung registry doesn't
        // stall the server forever.
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            client,
            http,
            cache: Arc::new(VersionCache::new(CACHE_TTL)),
            vuln_cache: Arc::new(VulnCache::new(CACHE_TTL)),
            detail_cache: Arc::new(DetailCache::new(DETAIL_TTL)),
            docs: Arc::new(DashMap::new()),
            pushed: Arc::new(DashMap::new()),
            pending: Arc::new(DashMap::new()),
            progress_seq: Arc::new(AtomicU64::new(0)),
            lockfiles: Arc::new(DashMap::new()),
        }
    }

    async fn resolutions_for(&self, uri: &Url, kind: ManifestKind) -> Arc<Resolutions> {
        resolutions_for(&self.lockfiles, uri, kind).await
    }

    /// Parse text into entries and store. Returns the new state's `ManifestKind`
    /// if the document is one we handle, or `None` for unsupported files
    /// (in which case the caller should leave the buffer alone).
    ///
    /// Pre-populates `latest` from the cache where possible so we don't have
    /// to wait for the network round-trip before showing hints on already-
    /// fetched packages.
    async fn reparse(&self, uri: &Url, text: &str) -> Option<ManifestKind> {
        let kind = ManifestKind::from_url(uri)?;
        let entries = parsers::parse(kind, text)
            .into_iter()
            .map(|entry| {
                // Prefer a stable release, fall back to any (which currently
                // is only interesting once prerelease opt-in lands).
                let latest = self
                    .cache
                    .get(kind, &entry.name)
                    .and_then(|info| info.latest_stable.or(info.latest_any));
                let vulns = crate::version::parse_for_scan(&entry.version_literal)
                    .and_then(|v| self.vuln_cache.get(kind, &entry.name, &v))
                    .unwrap_or_default();
                Annotated {
                    entry,
                    latest,
                    vulns,
                }
            })
            .collect();
        // Preserve the prior banner flag across reparse — `resolve_and_push`
        // is the only place that should toggle it, so a mid-typing reparse
        // shouldn't flicker the banner off and back on.
        let network_failure = self
            .docs
            .get(uri)
            .map(|e| e.network_failure)
            .unwrap_or(false);
        // Pull lockfile resolutions eagerly so hover and the scan-target
        // builder both see the same snapshot. Cheap when cached; a
        // single FS read on first call per lockfile mtime.
        let resolutions = self.resolutions_for(uri, kind).await;
        self.docs.insert(
            uri.clone(),
            Arc::new(DocState {
                kind,
                entries,
                network_failure,
                resolutions,
            }),
        );
        Some(kind)
    }

    /// Schedule registry lookups for entries without a cached latest, after
    /// `delay` (0 for `did_open`, `DEBOUNCE` for `did_change`). A prior
    /// pending task for the same URI is aborted so bursts of keystrokes
    /// collapse into a single round-trip.
    fn schedule_resolve(&self, uri: Url, delay: Duration) {
        // Cancel any prior debounced resolve for this buffer. `abort()`
        // cancels the task at its next `.await` — any in-flight future is
        // dropped, not awaited to completion.
        if let Some((_, prev)) = self.pending.remove(&uri) {
            prev.abort();
        }

        // Clone everything the task will need. Each `Arc::clone` is cheap;
        // it's all pointer bumps. We do this here (rather than moving `self`
        // in) because the task outlives the handler.
        let http = self.http.clone();
        let caches = Caches {
            version: self.cache.clone(),
            vuln: self.vuln_cache.clone(),
            detail: self.detail_cache.clone(),
        };
        let docs = self.docs.clone();
        let pushed = self.pushed.clone();
        let pending = self.pending.clone();
        let client = self.client.clone();
        let progress_seq = self.progress_seq.clone();
        let lockfiles = self.lockfiles.clone();
        let uri_key = uri.clone();

        let handle = tokio::spawn(async move {
            if !delay.is_zero() {
                sleep(delay).await;
            }
            // Self-evict from the pending map before doing any real work,
            // so a concurrent `schedule_resolve` doesn't see a stale handle
            // and try to abort something that's already finished its sleep.
            pending.remove(&uri_key);
            resolve_and_push(
                &client,
                &http,
                &caches,
                &docs,
                &pushed,
                &progress_seq,
                &lockfiles,
                &uri_key,
            )
            .await;
        });
        self.pending.insert(uri, handle);
    }
}

/// Bundle of caches passed into the resolve task. Grouping them here
/// keeps `resolve_and_push`'s arity sane and lets `schedule_resolve`
/// clone each `Arc` once.
struct Caches {
    version: Arc<VersionCache>,
    vuln: Arc<VulnCache>,
    detail: Arc<DetailCache>,
}

/// Async I/O form callable from both sync entry points (via .await)
/// and the resolve task. Locates the lockfile, mtime-checks it, and
/// returns the cached parsed map when fresh; otherwise reparses and
/// caches. All FS reads go through `tokio::fs` so a 1 MB workspace
/// lockfile doesn't block the runtime worker.
///
/// Every "no signal" outcome — missing lockfile, unreadable file,
/// unsupported kind, parse failure — returns the same `EMPTY` Arc, so
/// the scanner always sees one shape regardless of filesystem state.
async fn resolutions_for(
    cache: &DashMap<PathBuf, Arc<LockfileSnapshot>>,
    uri: &Url,
    kind: ManifestKind,
) -> Arc<Resolutions> {
    let Some(path) = lockfiles::locate(uri, kind) else {
        return empty_resolutions();
    };
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        cache.remove(&path);
        return empty_resolutions();
    };
    let Ok(mtime) = meta.modified() else {
        return empty_resolutions();
    };
    if let Some(snap) = cache.get(&path) {
        if snap.mtime == mtime {
            return snap.resolutions.clone();
        }
    }
    match lockfiles::parse(kind, &path).await {
        Ok(map) => {
            let resolutions = Arc::new(map);
            cache.insert(
                path,
                Arc::new(LockfileSnapshot {
                    mtime,
                    resolutions: resolutions.clone(),
                }),
            );
            resolutions
        }
        Err(e) => {
            warn!(?path, "lockfile parse failed: {e:#}");
            empty_resolutions()
        }
    }
}

/// Shared sentinel `Arc<Resolutions>` for every "no signal" return.
/// Avoids the per-call allocation that `Arc::new(Resolutions::new())`
/// would do at the five early-exits in `resolutions_for`.
fn empty_resolutions() -> Arc<Resolutions> {
    static EMPTY: OnceLock<Arc<Resolutions>> = OnceLock::new();
    EMPTY.get_or_init(|| Arc::new(Resolutions::new())).clone()
}

/// Do the actual network work for one URI and push updated diagnostics.
///
/// Runs outside of any `Backend` method so `schedule_resolve` can spawn it
/// as an independent task — `&self` borrows can't escape into `tokio::spawn`.
// Eight named params is more legible here than another bundle struct
// that adds zero clarity. Each one is referenced by name throughout
// the body; grouping them would just force a `ctx.` prefix everywhere.
#[allow(clippy::too_many_arguments)]
async fn resolve_and_push(
    client: &LspClient,
    http: &Client,
    caches: &Caches,
    docs: &DashMap<Url, Arc<DocState>>,
    pushed: &DashMap<Url, u64>,
    progress_seq: &AtomicU64,
    lockfiles: &DashMap<PathBuf, Arc<LockfileSnapshot>>,
    uri: &Url,
) {
    let cache = &caches.version;
    let vuln_cache = &caches.vuln;
    let detail_cache = &caches.detail;

    // Grab an immutable snapshot of the current parsed state. If the doc
    // was closed while we were waiting out the debounce, there's nothing
    // to do.
    let Some(state) = docs.get(uri).map(|e| Arc::clone(&*e)) else {
        return;
    };
    let kind = state.kind;
    // Refresh on every resolve burst — picks up `cargo update` /
    // `npm install` since the prior `did_change` without needing a
    // file watcher. The mtime gate inside `resolutions_for` keeps the
    // common case (unchanged lockfile) to a single `stat`.
    let resolutions = resolutions_for(lockfiles, uri, kind).await;

    // Deduplicate names — a package could appear in both `dependencies`
    // and `devDependencies`, but we only need to fetch its version once.
    let to_fetch: HashSet<String> = state
        .entries
        .iter()
        .filter(|a| a.latest.is_none())
        .map(|a| a.entry.name.clone())
        .collect();

    // Track registry-call outcomes so we can flip the per-doc banner
    // diagnostic on (all fail) or off (any success).
    let mut registry_attempted = 0usize;
    let mut registry_succeeded = 0usize;

    let progress_token = begin_progress(client, progress_seq, to_fetch.len()).await;

    if !to_fetch.is_empty() {
        use futures::StreamExt;
        registry_attempted = to_fetch.len();
        let total = registry_attempted;
        let mut done = 0usize;
        let mut futs = futures::stream::FuturesUnordered::new();
        for name in to_fetch {
            let http = http.clone();
            futs.push(async move { (name.clone(), providers::fetch(&http, kind, &name).await) });
        }
        while let Some((name, res)) = futs.next().await {
            match res {
                Ok(info) => {
                    cache.put(kind, name, info);
                    registry_succeeded += 1;
                }
                Err(e) => warn!(?name, "registry lookup failed: {e:#}"),
            }
            done += 1;
            // Per-package counter — drives the live "N/M packages"
            // string clients render under the progress title. Skipped
            // by `report_progress` when `begin_progress` was below the
            // banner threshold.
            report_progress(client, progress_token.as_ref(), done, total).await;
        }
    }

    // --- Phase 2: OSV vulnerability scans ---
    //
    // For every entry whose literal parses to a concrete version, consult
    // the vuln cache. Anything missing gets scanned in parallel. We never
    // overwrite a cache hit with a failed scan — an error is logged and
    // treated as "retry next keystroke".
    //
    // Choose the scan target once per entry, in order: the version
    // pinned by the sibling lockfile (when present), else the floor of
    // the manifest literal. The pair (entry index → scan version) is
    // reused for the scan-target sweep, the detail-id sweep, and stays
    // in scope until the alter() fold below where literals may have
    // shifted.
    let parsed: Vec<Option<Version>> = state
        .entries
        .iter()
        .map(|a| scan_version_for(&a.entry, &resolutions))
        .collect();

    // Size hints: every entry is a potential scan target at most, so
    // over-allocating once avoids growth reallocations during collection.
    let mut scan_targets: Vec<(String, Version)> = Vec::with_capacity(state.entries.len());
    let mut seen: HashSet<(String, Version)> = HashSet::with_capacity(state.entries.len());
    for (a, ver) in state.entries.iter().zip(&parsed) {
        if let Some(ver) = ver {
            if seen.insert((a.entry.name.clone(), ver.clone()))
                && vuln_cache.get(kind, &a.entry.name, ver).is_none()
            {
                scan_targets.push((a.entry.name.clone(), ver.clone()));
            }
        }
    }

    if !scan_targets.is_empty() {
        use futures::StreamExt;
        let mut futs = futures::stream::FuturesUnordered::new();
        for (name, ver) in scan_targets {
            let http = http.clone();
            futs.push(async move {
                let res = crate::vulnerabilities::fetch_vulns(&http, kind, &name, &ver).await;
                (name, ver, res)
            });
        }
        while let Some((name, ver, res)) = futs.next().await {
            match res {
                Ok(vulns) => vuln_cache.put(kind, name, ver, vulns),
                Err(e) => warn!(?name, version = %ver, "OSV scan failed: {e:#}"),
            }
        }
    }

    // Fetch severity scores for any cached advisory IDs not yet in DetailCache.
    let mut detail_ids: HashSet<String> = HashSet::new();
    for (a, ver) in state.entries.iter().zip(&parsed) {
        if let Some(ver) = ver {
            if let Some(vulns) = vuln_cache.get(kind, &a.entry.name, ver) {
                for v in vulns {
                    if detail_cache.get(&v.id).is_none() {
                        detail_ids.insert(v.id);
                    }
                }
            }
        }
    }

    if !detail_ids.is_empty() {
        let ids: Vec<String> = detail_ids.into_iter().collect();
        let details = crate::vulnerabilities::fetch_vuln_details(http, &ids).await;
        for (id, detail) in details {
            detail_cache.put(id, detail);
        }
    }

    // --- Fold both results back into DocState ---
    //
    // `alter` is a no-op if the doc has been closed in the meantime — a
    // `did_close` landing during the fetch must not resurrect the doc.
    docs.alter(uri, |_, existing| {
        let mut new_entries = existing.entries.clone();
        for a in &mut new_entries {
            if a.latest.is_none() {
                if let Some(info) = cache.get(kind, &a.entry.name) {
                    a.latest = info.latest_stable.or(info.latest_any);
                }
            }
            // Use the same scan version as the fetch-target sweep
            // above so the cache key lines up. Lockfile resolutions
            // take precedence over the literal floor.
            if let Some(ver) = scan_version_for(&a.entry, &resolutions) {
                if let Some(mut vulns) = vuln_cache.get(kind, &a.entry.name, &ver) {
                    // Sort so the fingerprint is deterministic regardless of
                    // scan-completion order.
                    vulns.sort_by(|x, y| x.id.cmp(&y.id));
                    for v in &mut vulns {
                        if let Some(detail) = detail_cache.get(&v.id) {
                            v.score = detail.score;
                            v.vector = detail.vector;
                        }
                    }
                    a.vulns = vulns;
                }
            }
        }
        // Update the banner only when the burst actually contacted a
        // registry — a no-op burst (everything already cached) shouldn't
        // claim "network OK" or stamp out a real outage flag.
        let network_failure = if registry_attempted > 0 {
            registry_succeeded == 0
        } else {
            existing.network_failure
        };
        Arc::new(DocState {
            kind: existing.kind,
            entries: new_entries,
            network_failure,
            resolutions: resolutions.clone(),
        })
    });

    end_progress(client, progress_token).await;
    push_updates_raw(client, docs, pushed, uri).await;
}

/// Ask the client to register a `$/progress` token and emit a `Begin`
/// notification covering the upcoming resolve burst. Returns the token
/// so the caller can pair an `End` with it; returns `None` when the
/// burst is too small to bother (or when the client refused the token).
async fn begin_progress(
    client: &LspClient,
    seq: &AtomicU64,
    pending: usize,
) -> Option<NumberOrString> {
    if pending < PROGRESS_THRESHOLD {
        return None;
    }
    let token = NumberOrString::String(format!(
        "uptick-resolve-{}",
        seq.fetch_add(1, Ordering::Relaxed)
    ));
    client
        .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: token.clone(),
        })
        .await
        .ok()?;
    client
        .send_notification::<ProgressNotification>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: format!("Uptick: resolving {pending} packages"),
                cancellable: Some(false),
                message: Some(format!("0/{pending}")),
                // Seed at 0 so clients that render a bar know to allocate
                // one — subsequent `Report`s update it.
                percentage: Some(0),
            })),
        })
        .await;
    Some(token)
}

/// Emit a `$/progress` Report notification under `token`. Silently
/// skipped if `token` is `None` (sub-threshold burst). `done` is the
/// number of fetches completed; `total` is the burst size set at
/// `begin_progress`.
async fn report_progress(
    client: &LspClient,
    token: Option<&NumberOrString>,
    done: usize,
    total: usize,
) {
    let Some(token) = token else {
        return;
    };
    // `checked_div` here is purely to satisfy clippy::manual_checked_ops
    // — the surrounding burst will only ever invoke `report_progress`
    // with `total > 0` (we early-return otherwise), but the explicit
    // form keeps the lint quiet and the saturating fallback honest.
    let pct = (done * 100)
        .checked_div(total)
        .map(|p| p.min(100) as u32)
        .unwrap_or(100);
    client
        .send_notification::<ProgressNotification>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                WorkDoneProgressReport {
                    cancellable: Some(false),
                    message: Some(format!("{done}/{total}")),
                    percentage: Some(pct),
                },
            )),
        })
        .await;
}

/// Pair with `begin_progress` — only sends `End` if `begin` actually
/// registered a token.
async fn end_progress(client: &LspClient, token: Option<NumberOrString>) {
    let Some(token) = token else {
        return;
    };
    client
        .send_notification::<ProgressNotification>(ProgressParams {
            token,
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                message: None,
            })),
        })
        .await;
}

/// Compute a stable fingerprint for the visible state: entry names, their
/// current literals, and their resolved latest. Anything else (positions,
/// group labels) can change without a user-visible delta, so we omit it.
///
/// This is the key to quiet typing: if a keystroke only shuffles whitespace
/// around, the fingerprint is unchanged, and we skip the diagnostic push
/// and hint refresh entirely.
fn fingerprint(state: &DocState) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    state.kind.display().hash(&mut h);
    // Banner toggles must invalidate the fingerprint so the editor
    // actually receives the diagnostic flip.
    (state.network_failure as u8).hash(&mut h);
    // Lockfile churn (cargo update, npm install) shifts resolved
    // versions without touching the manifest text — hash a stable
    // view so the editor sees the resulting hover / scan delta.
    // `BTreeMap` collect makes iteration order deterministic across
    // platforms.
    let sorted: BTreeMap<&String, &Version> = state.resolutions.iter().collect();
    sorted.len().hash(&mut h);
    for (name, ver) in sorted {
        name.hash(&mut h);
        ver.major.hash(&mut h);
        ver.minor.hash(&mut h);
        ver.patch.hash(&mut h);
        ver.pre.as_str().hash(&mut h);
        ver.build.as_str().hash(&mut h);
    }
    for a in &state.entries {
        a.entry.name.hash(&mut h);
        a.entry.version_literal.hash(&mut h);
        // `Version` doesn't implement `Hash` directly, so decompose it.
        // Discriminant byte (0/1) distinguishes `None` from a `Some` whose
        // every field happens to hash to zero.
        match &a.latest {
            None => 0u8.hash(&mut h),
            Some(v) => {
                1u8.hash(&mut h);
                v.major.hash(&mut h);
                v.minor.hash(&mut h);
                v.patch.hash(&mut h);
                v.pre.as_str().hash(&mut h);
                v.build.as_str().hash(&mut h);
            }
        }
        // Vulns are sorted by id at fold time, so this hash is stable.
        a.vulns.len().hash(&mut h);
        for v in &a.vulns {
            v.id.hash(&mut h);
            // Hash a discriminant + bit pattern so a late-arriving
            // severity score invalidates the fingerprint and the
            // re-rendered diagnostic reaches the editor.
            match v.score {
                None => 0u8.hash(&mut h),
                Some(s) => {
                    1u8.hash(&mut h);
                    s.to_bits().hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

/// Emit the current state of `uri` to the editor — diagnostics, and a
/// best-effort kick to refresh inlay hints / code lenses.
///
/// Skips the push entirely if the fingerprint hasn't changed since the
/// last round, which is the usual case while typing inside strings that
/// already parsed cleanly.
async fn push_updates_raw(
    client: &LspClient,
    docs: &DashMap<Url, Arc<DocState>>,
    pushed: &DashMap<Url, u64>,
    uri: &Url,
) {
    let Some(state) = docs.get(uri).map(|e| Arc::clone(&*e)) else {
        return;
    };
    let fp = fingerprint(&state);
    if pushed.get(uri).map(|e| *e) == Some(fp) {
        return;
    }
    pushed.insert(uri.clone(), fp);

    let diags = build_diagnostics(&state);
    client.publish_diagnostics(uri.clone(), diags, None).await;

    // These refreshes require server capability registration on the client
    // side; the Err case just means the client ignored our request. We
    // still send them so editors that *do* support them pick up the update.
    let _ = client.inlay_hint_refresh().await;
    let _ = client.code_lens_refresh().await;
}

/// Single source of truth for CVSS bucket thresholds. Keeps the hover
/// label (`CRITICAL` / `HIGH` / …) and the LSP diagnostic severity
/// (`ERROR` / `WARNING` / …) from drifting apart.
#[derive(Clone, Copy)]
enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Unknown,
}

impl Severity {
    fn from_score(score: Option<f32>) -> Self {
        match score {
            Some(s) if s >= 9.0 => Self::Critical,
            Some(s) if s >= 7.0 => Self::High,
            Some(s) if s >= 4.0 => Self::Medium,
            Some(s) if s > 0.0 => Self::Low,
            _ => Self::Unknown,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::High => "HIGH",
            Self::Medium => "MEDIUM",
            Self::Low => "LOW",
            Self::Unknown => "UNKNOWN",
        }
    }

    /// `Unknown` falls through to `Warning` so advisories without a
    /// parseable score stay visible rather than disappearing as `Hint`.
    fn diagnostic(self) -> DiagnosticSeverity {
        match self {
            Self::Critical | Self::High => DiagnosticSeverity::ERROR,
            Self::Medium => DiagnosticSeverity::WARNING,
            Self::Low => DiagnosticSeverity::INFORMATION,
            Self::Unknown => DiagnosticSeverity::WARNING,
        }
    }
}

/// Replace literal backticks so a value can be safely embedded inside a
/// markdown code span. Used four times in `hover()` for name, literal,
/// summary, and CVSS vector.
fn escape_backticks(s: &str) -> String {
    s.replace('`', "'")
}

/// Produce diagnostics for every entry: `Information`-level for update
/// availability, and `Warning`-level for known vulnerabilities.
fn build_diagnostics(state: &DocState) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if state.network_failure {
        out.push(Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            severity: Some(DiagnosticSeverity::WARNING),
            source: Some(DIAGNOSTIC_SOURCE.into()),
            code: Some(NumberOrString::String("network-error".into())),
            message: NETWORK_BANNER.into(),
            ..Default::default()
        });
    }
    for a in &state.entries {
        let name = &a.entry.name;

        if let Some(latest) = &a.latest {
            if should_bump(&a.entry.version_literal, latest) {
                out.push(Diagnostic {
                    range: a.entry.version_range,
                    severity: Some(DiagnosticSeverity::INFORMATION),
                    source: Some(DIAGNOSTIC_SOURCE.into()),
                    code: Some(NumberOrString::String("update-available".into())),
                    message: format!("{name}: newer version {latest} is available"),
                    ..Default::default()
                });
            }
        }

        for v in &a.vulns {
            let message = v
                .summary
                .as_deref()
                .or(v.details.as_deref())
                .unwrap_or("(no description)");
            let id = &v.id;
            out.push(Diagnostic {
                range: a.entry.version_range,
                severity: Some(Severity::from_score(v.score).diagnostic()),
                source: Some(DIAGNOSTIC_SOURCE.into()),
                code: Some(NumberOrString::String(v.id.clone())),
                message: format!("{id}: {message}"),
                ..Default::default()
            });
        }
    }
    out
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    /// Called once at client connect. We tell the client which LSP features
    /// we support; it only sends us requests for capabilities we advertise.
    async fn initialize(&self, _p: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: SERVER_NAME.into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                // Explicit: `LineIndex` produces UTF-16 columns, matching
                // LSP's default. We state it anyway so clients don't
                // renegotiate to UTF-8 or UTF-32 mid-session.
                position_encoding: Some(PositionEncodingKind::UTF16),
                // FULL sync: clients send the entire buffer on each change.
                // Simpler than incremental sync and plenty fast for the
                // small manifests we target.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                inlay_hint_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        // We only emit `quickfix` actions (Bump to X.Y.Z).
                        // Declaring the kind lets Zed / VS Code filter the
                        // action list appropriately.
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: Some(false),
                        work_done_progress_options: WorkDoneProgressOptions::default(),
                    },
                )),
                // Clickable links on package names (→ registry page) and on
                // vulnerable version literals (→ osv.dev advisory). Zed 0.X
                // gated this behind `lsp_document_links` (default-on).
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                // Inline `↑ Bump to X.Y.Z` and `⛔ N advisories` above each
                // dep line. Resolution happens up-front in `code_lens` —
                // every lens already carries its command + arguments.
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                // Server-defined commands invoked by the lenses. `uptick.bump`
                // applies a text edit via `workspace/applyEdit`; `uptick.open`
                // asks the client to surface a URL via `window/showDocument`.
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![CMD_BUMP.into(), CMD_OPEN.into()],
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _p: InitializedParams) {
        debug!("{SERVER_NAME} ready");
        // Fire the once-per-user welcome toast off the critical path —
        // the LSP must come up even if the state-dir write fails or
        // the client takes its time responding.
        let client = self.client.clone();
        tokio::spawn(async move {
            crate::onboarding::maybe_send_welcome(client).await;
        });
    }

    async fn shutdown(&self) -> LspResult<()> {
        // No cleanup needed: tokio will drop spawned tasks when the runtime
        // exits, and the OS reclaims the TCP connections.
        Ok(())
    }

    /// Buffer opened: parse immediately and kick off an eager resolve.
    /// No debounce — the user explicitly asked to see this file.
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        if self
            .reparse(&uri, &params.text_document.text)
            .await
            .is_some()
        {
            self.schedule_resolve(uri, Duration::ZERO);
        }
    }

    /// Buffer edited: reparse and schedule a debounced resolve.
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // We negotiated FULL sync, so exactly one change arrives and it
        // carries the entire new document text.
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };
        if self.reparse(&uri, &change.text).await.is_some() {
            self.schedule_resolve(uri, DEBOUNCE);
        }
    }

    /// Buffer closed: tear down all state for it and clear diagnostics.
    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some((_, h)) = self.pending.remove(&uri) {
            h.abort();
        }
        self.docs.remove(&uri);
        self.pushed.remove(&uri);
        // Clearing diagnostics explicitly: some clients keep the last
        // published list forever otherwise, leaving ghost warnings.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    /// Produce inlay hints for every entry whose version range falls inside
    /// the client's requested window. The hint is placed flush with the end
    /// of the version literal with a single leading space for readability.
    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let registry = state.kind.display();
        let hints: Vec<InlayHint> = state
            .entries
            .iter()
            // Only emit hints the editor actually asked about — cuts payload
            // size on large manifests when the user is viewing a small slice.
            .filter(|a| {
                a.entry.version_range.start >= params.range.start
                    && a.entry.version_range.end <= params.range.end
            })
            .filter_map(|a| {
                let latest = a.latest.as_ref()?;
                let up_to_date = version::satisfies(&a.entry.version_literal, latest);
                // `✓` for "you're already compatible", `→` for "newer
                // available". Both hints include the latest version so the
                // user sees it without moving their cursor.
                let label = if up_to_date {
                    format!(" ✓ {latest}")
                } else {
                    format!(" → {latest}")
                };
                Some(InlayHint {
                    position: a.entry.version_range.end,
                    label: InlayHintLabel::String(label),
                    kind: None,
                    text_edits: None,
                    tooltip: Some(InlayHintTooltip::String(format!("latest on {registry}"))),
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                })
            })
            .collect();
        Ok(Some(hints))
    }

    /// Hover summary: name (grouped), current literal, resolved latest, and
    /// a link to the registry page. Triggered when the cursor is inside
    /// either the name or the version literal.
    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        // Pick the first entry whose name or version range contains the
        // cursor. Ranges from different entries never overlap, so "first"
        // is unambiguous.
        let Some(hit) = state
            .entries
            .iter()
            .find(|a| contains(&a.entry.version_range, pos) || contains(&a.entry.name_range, pos))
        else {
            return Ok(None);
        };

        // Backticks in user-supplied strings would break the surrounding
        // markdown code spans, so all such values go through `escape_backticks`.
        let mut md = String::new();
        let name = escape_backticks(&hit.entry.name);
        write!(md, "`{name}`").unwrap();
        if let Some(group) = hit.entry.group {
            write!(md, " _({group})_").unwrap();
        }
        md.push('\n');

        let literal = escape_backticks(&hit.entry.version_literal);
        write!(md, "\ncurrent: `{literal}`\n").unwrap();
        if let Some(latest) = &hit.latest {
            writeln!(md, "latest: `{latest}`").unwrap();
        } else {
            // Fetch still in flight (or failed and waiting for retry). Be
            // honest with the user rather than hiding the field.
            md.push_str("latest: _resolving…_\n");
        }
        // Surface the actual installed version + which lockfile it came
        // from. Helps the user understand why a vulnerability fires
        // against a literal like `^1.0.0` (because the lockfile pins
        // `1.0.7`, and that's what OSV was asked about).
        if let Some(resolved) = state.resolutions.get(&hit.entry.name) {
            let lockfile = lockfiles::filename(state.kind).unwrap_or("lockfile");
            writeln!(md, "installed: `{resolved}` _({lockfile})_").unwrap();
        }
        // Final optional section: link straight to the registry page.
        if let Some(url) = self
            .cache
            .get(state.kind, &hit.entry.name)
            .and_then(|info| info.url)
        {
            write!(md, "\n[registry]({url})").unwrap();
        }

        if !hit.vulns.is_empty() {
            md.push_str("\n\n---\n");
            for v in &hit.vulns {
                let label = Severity::from_score(v.score).label();
                write!(md, "\n**{label}** `{id}`", id = v.id).unwrap();
                if let Some(score) = v.score {
                    write!(md, " · score {score:.1}").unwrap();
                }
                md.push('\n');
                if let Some(summary) = v.summary.as_deref().filter(|s| !s.is_empty()) {
                    writeln!(md, "\n{}", escape_backticks(summary)).unwrap();
                }
                if let Some(vector) = v.vector.as_deref() {
                    writeln!(md, "\nCVSS: `{}`", escape_backticks(vector)).unwrap();
                }
                writeln!(
                    md,
                    "\n[osv.dev](https://osv.dev/vulnerability/{id})",
                    id = v.id
                )
                .unwrap();
            }
        }

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: Some(hit.entry.version_range),
        }))
    }

    /// Emit one `Bump to X.Y.Z` quickfix per out-of-date entry that overlaps
    /// the requested range. The selected range is typically the cursor
    /// position; `ranges_overlap` is generous so clicks on boundaries work.
    async fn code_action(&self, params: CodeActionParams) -> LspResult<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let mut out: CodeActionResponse = Vec::new();
        for a in &state.entries {
            if !ranges_overlap(&a.entry.version_range, &params.range) {
                continue;
            }
            let Some(latest) = &a.latest else { continue };
            if !should_bump(&a.entry.version_literal, latest) {
                continue;
            }
            let new_text = replacement(&a.entry.version_literal, latest);
            let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![TextEdit {
                    range: a.entry.version_range,
                    new_text,
                }],
            );
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Bump {} to {}", a.entry.name, latest),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                // `is_preferred` tells the editor to highlight this as the
                // default action.
                is_preferred: Some(true),
                disabled: None,
                data: None,
            }));
        }
        Ok(Some(out))
    }

    /// Emit code lenses above each dep line:
    ///   * `↑ Bump to X.Y.Z` for out-of-date entries — fires `uptick.bump`,
    ///     which produces the same text edit as the `Bump` quickfix.
    ///   * `⛔ N advisor{y|ies} — view on osv.dev` for vulnerable entries —
    ///     fires `uptick.open` with the first advisory's URL.
    ///
    /// Lenses are pre-resolved so the client never needs to round-trip
    /// `codeLens/resolve`.
    async fn code_lens(&self, params: CodeLensParams) -> LspResult<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let mut lenses: Vec<CodeLens> = Vec::new();
        for a in &state.entries {
            if let Some(latest) = &a.latest {
                if should_bump(&a.entry.version_literal, latest) {
                    let new_text = replacement(&a.entry.version_literal, latest);
                    lenses.push(CodeLens {
                        range: a.entry.version_range,
                        command: Some(Command {
                            title: format!("↑ Bump to {latest}"),
                            command: CMD_BUMP.into(),
                            arguments: Some(vec![json!({
                                "uri": uri,
                                "range": a.entry.version_range,
                                "new_text": new_text,
                            })]),
                        }),
                        data: None,
                    });
                }
            }
            // Surface the highest-severity advisory in the lens link;
            // alphabetical id order (the fingerprint-stable sort) is
            // arbitrary for severity, so don't just grab `.first()`.
            // Treat missing scores as zero so a scored advisory always
            // wins over an unscored one.
            if let Some(worst) = a.vulns.iter().max_by(|x, y| {
                let xs = x.score.unwrap_or(0.0);
                let ys = y.score.unwrap_or(0.0);
                xs.partial_cmp(&ys).unwrap_or(std::cmp::Ordering::Equal)
            }) {
                let n = a.vulns.len();
                let noun = if n == 1 { "advisory" } else { "advisories" };
                lenses.push(CodeLens {
                    range: a.entry.version_range,
                    command: Some(Command {
                        title: format!("⛔ {n} {noun} — view on osv.dev"),
                        command: CMD_OPEN.into(),
                        arguments: Some(vec![json!({
                            "url": format!("https://osv.dev/vulnerability/{}", worst.id),
                        })]),
                    }),
                    data: None,
                });
            }
        }
        Ok(Some(lenses))
    }

    /// Emit clickable links: one per entry pointing at the package's
    /// registry page (anchored on `name_range`), plus one per known
    /// vulnerability pointing at the corresponding osv.dev advisory
    /// (anchored on `version_range`). Hover already exposes the same
    /// URLs, but document links surface them without requiring a hover.
    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> LspResult<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let registry = state.kind.display();
        let mut links: Vec<DocumentLink> = Vec::new();
        for a in &state.entries {
            // Prefer the provider-supplied canonical URL (e.g. crates.io's
            // exact slug after redirects); fall back to a deterministic
            // template so the link works even before the first fetch
            // returns.
            let target = self
                .cache
                .get(state.kind, &a.entry.name)
                .and_then(|info| info.url)
                .and_then(|s| Url::parse(&s).ok())
                .or_else(|| registry_url(state.kind, &a.entry.name));
            if let Some(target) = target {
                links.push(DocumentLink {
                    range: a.entry.name_range,
                    target: Some(target),
                    tooltip: Some(format!("View {} on {registry}", a.entry.name)),
                    data: None,
                });
            }
            for v in &a.vulns {
                if let Ok(target) = Url::parse(&format!("https://osv.dev/vulnerability/{}", v.id)) {
                    links.push(DocumentLink {
                        range: a.entry.version_range,
                        target: Some(target),
                        tooltip: Some(format!("{}: view advisory on osv.dev", v.id)),
                        data: None,
                    });
                }
            }
        }
        Ok(Some(links))
    }

    /// Dispatch the two commands code lenses can fire:
    ///   * `uptick.bump` → apply the same text edit as the `Bump` quickfix.
    ///   * `uptick.open` → ask the client to surface a URL externally.
    /// Unknown commands return null so unsupported clients don't error.
    async fn execute_command(&self, p: ExecuteCommandParams) -> LspResult<Option<Value>> {
        let cmd = p.command.as_str();
        match cmd {
            CMD_BUMP => {
                let Some(arg) = p.arguments.into_iter().next() else {
                    warn!(%cmd, "missing arguments");
                    return Ok(None);
                };
                let args = match serde_json::from_value::<BumpArgs>(arg) {
                    Ok(a) => a,
                    Err(e) => {
                        warn!(%cmd, "bad arguments: {e:#}");
                        return Ok(None);
                    }
                };
                let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
                changes.insert(
                    args.uri,
                    vec![TextEdit {
                        range: args.range,
                        new_text: args.new_text,
                    }],
                );
                match self
                    .client
                    .apply_edit(WorkspaceEdit {
                        changes: Some(changes),
                        document_changes: None,
                        change_annotations: None,
                    })
                    .await
                {
                    Ok(resp) if !resp.applied => {
                        warn!(%cmd, reason = ?resp.failure_reason, "client rejected edit");
                    }
                    Err(e) => warn!(%cmd, "apply_edit failed: {e:#}"),
                    _ => {}
                }
                Ok(None)
            }
            CMD_OPEN => {
                let Some(arg) = p.arguments.into_iter().next() else {
                    warn!(%cmd, "missing arguments");
                    return Ok(None);
                };
                let args = match serde_json::from_value::<OpenArgs>(arg) {
                    Ok(a) => a,
                    Err(e) => {
                        warn!(%cmd, "bad arguments: {e:#}");
                        return Ok(None);
                    }
                };
                match self
                    .client
                    .show_document(ShowDocumentParams {
                        uri: args.url,
                        external: Some(true),
                        take_focus: Some(true),
                        selection: None,
                    })
                    .await
                {
                    Ok(false) => warn!(%cmd, "client refused show_document"),
                    Err(e) => warn!(%cmd, "show_document failed: {e:#}"),
                    _ => {}
                }
                Ok(None)
            }
            _ => Ok(Some(json!(null))),
        }
    }
}

#[derive(serde::Deserialize)]
struct BumpArgs {
    uri: Url,
    range: Range,
    new_text: String,
}

#[derive(serde::Deserialize)]
struct OpenArgs {
    url: Url,
}

/// Returns `true` if `pos` lies inside `range`. Both ends are inclusive,
/// which matches how LSP clients typically hit-test hover positions.
fn contains(range: &Range, pos: Position) -> bool {
    (range.start.line < pos.line
        || (range.start.line == pos.line && range.start.character <= pos.character))
        && (range.end.line > pos.line
            || (range.end.line == pos.line && range.end.character >= pos.character))
}

/// Returns `true` if ranges `a` and `b` share at least one position
/// (including single-point touches at endpoints).
///
/// Implemented as "not disjoint": either `a` starts strictly after `b` ends,
/// or `b` starts strictly after `a` ends. Anything else is overlap.
fn ranges_overlap(a: &Range, b: &Range) -> bool {
    let a_after_b = (a.start.line > b.end.line)
        || (a.start.line == b.end.line && a.start.character > b.end.character);
    let b_after_a = (b.start.line > a.end.line)
        || (b.start.line == a.end.line && b.start.character > a.end.character);
    !(a_after_b || b_after_a)
}

/// Single source of truth for "which version should OSV be asked
/// about for this entry?". Lockfile resolutions win — that's the
/// version actually installed — and we fall back to the manifest
/// literal's floor when the lockfile is absent or doesn't pin this
/// package. Used by the scan-target builder, the detail-id sweep, and
/// the alter() fold, so all three see the same answer.
fn scan_version_for(entry: &RawEntry, resolutions: &Resolutions) -> Option<Version> {
    resolutions
        .get(&entry.name)
        .cloned()
        .or_else(|| version::parse_for_scan(&entry.version_literal))
}

/// Single source of truth for "is this entry out-of-date and worth a
/// `Bump` action?". Used by `build_diagnostics`, `code_action`, and
/// `code_lens` so the three surfaces always agree.
///
/// Skip when (a) the user's range already accepts `latest`, or (b) the
/// literal parses to a version that is already >= `latest` (avoids
/// offering a downgrade if the cache lags behind a manual pin).
fn should_bump(literal: &str, latest: &Version) -> bool {
    !version::satisfies(literal, latest)
        && version::parse_literal(literal).is_none_or(|cur| &cur < latest)
}

/// Deterministic registry-page URL for a given (kind, name). Used by
/// `document_link` before the provider has had a chance to populate
/// `VersionCache::url`; provider-supplied URLs (post-fetch) take precedence
/// when present. Names are written verbatim — `@scope/foo` and
/// `vendor/pkg` are valid path segments on all four registries.
fn registry_url(kind: ManifestKind, name: &str) -> Option<Url> {
    let raw = match kind {
        ManifestKind::Npm => format!("https://www.npmjs.com/package/{name}"),
        ManifestKind::Cargo => format!("https://crates.io/crates/{name}"),
        ManifestKind::Pub => format!("https://pub.dev/packages/{name}"),
        ManifestKind::Composer => format!("https://packagist.org/packages/{name}"),
        // pkg.go.dev accepts the full module path verbatim.
        ManifestKind::Go => format!("https://pkg.go.dev/{name}"),
        // Maven Central's web UI takes `groupId/artifactId` separated
        // by a slash; our `name` is already the `groupId:artifactId`
        // coordinate, so swap the `:` for `/`.
        ManifestKind::Maven => format!(
            "https://central.sonatype.com/artifact/{}",
            name.replacen(':', "/", 1)
        ),
    };
    Url::parse(&raw).ok()
}

/// Preserve the user's range operator when bumping, so we don't turn a
/// semver range (`^1.2.3`) into an exact pin (`1.5.0`).
///
/// Strategy: split the literal into
///   `leading-whitespace` + `range-operator-chars` + `the-version`
/// then reassemble with the new version replacing just the third piece.
fn replacement(current: &str, latest: &Version) -> String {
    let trimmed = current.trim_start();
    // Preserve any leading whitespace exactly — some formats allow `" 1.0"`
    // and we don't want to mess with indentation.
    let leading_ws = &current[..current.len() - trimmed.len()];
    // Grab the operator chars up to (but not including) the first
    // alphanumeric of the actual version. Same character class as
    // `strip_leading`, kept in sync manually.
    let op: String = trimmed
        .chars()
        .take_while(|c| matches!(c, '^' | '~' | '=' | '>' | '<' | 'v' | 'V' | ' '))
        .collect();
    format!("{leading_ws}{op}{latest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_preserves_operator() {
        // The user's operator (`^`, `~`, `>=`, or none) must survive the bump.
        let latest = Version::parse("1.5.0").unwrap();
        assert_eq!(replacement("^1.2.3", &latest), "^1.5.0");
        assert_eq!(replacement("~1.2.3", &latest), "~1.5.0");
        assert_eq!(replacement("1.2.3", &latest), "1.5.0");
        assert_eq!(replacement(">= 1.2.3", &latest), ">= 1.5.0");
    }

    /// Tiny helper — build a `Range` from four integers, inline in tests.
    fn r(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: Position::new(sl, sc),
            end: Position::new(el, ec),
        }
    }

    #[test]
    fn contains_handles_same_line() {
        // Both endpoints are inclusive; anything outside by one column is out.
        let range = r(2, 4, 2, 10);
        assert!(contains(&range, Position::new(2, 4)));
        assert!(contains(&range, Position::new(2, 7)));
        assert!(contains(&range, Position::new(2, 10)));
        assert!(!contains(&range, Position::new(2, 3)));
        assert!(!contains(&range, Position::new(2, 11)));
    }

    #[test]
    fn contains_spans_multiple_lines() {
        // Middle lines are always in-range regardless of column.
        let range = r(2, 4, 5, 2);
        assert!(contains(&range, Position::new(3, 0)));
        assert!(contains(&range, Position::new(4, 999)));
        assert!(!contains(&range, Position::new(1, 99)));
        assert!(!contains(&range, Position::new(5, 3)));
    }

    #[test]
    fn network_banner_emitted_only_when_flagged() {
        let empty_resolutions = Arc::new(Resolutions::new());
        // Empty state, no banner.
        let clean = DocState {
            kind: ManifestKind::Cargo,
            entries: vec![],
            network_failure: false,
            resolutions: empty_resolutions.clone(),
        };
        assert!(build_diagnostics(&clean).is_empty());

        // Same state with the banner flag → exactly one Warning-level
        // diagnostic at line 0 carrying the well-known code.
        let flagged = DocState {
            kind: ManifestKind::Cargo,
            entries: vec![],
            network_failure: true,
            resolutions: empty_resolutions,
        };
        let diags = build_diagnostics(&flagged);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("network-error".into()))
        );
        assert_eq!(diags[0].range.start, Position::new(0, 0));
    }

    #[tokio::test]
    async fn resolutions_for_caches_by_mtime() {
        // Build a tiny `<tmp>/Cargo.toml + Cargo.lock` pair on disk and
        // confirm that two back-to-back calls return the *same* Arc
        // (pointer-eq) — the mtime cache must skip the second parse.
        use std::fs;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "uptick-resolutions-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();

        let lockfile = dir.join("Cargo.lock");
        fs::write(
            &lockfile,
            indoc::indoc! {r#"
                [[package]]
                name = "serde"
                version = "1.0.0"
                source = "registry+https://github.com/rust-lang/crates.io-index"
            "#},
        )
        .unwrap();
        let manifest = dir.join("Cargo.toml");
        fs::write(&manifest, "").unwrap();
        let url = Url::from_file_path(&manifest).unwrap();
        let cache = DashMap::new();

        let r1 = resolutions_for(&cache, &url, ManifestKind::Cargo).await;
        assert!(
            r1.contains_key("serde"),
            "first call must parse the lockfile"
        );
        assert_eq!(cache.len(), 1, "first call must populate the cache");

        let r2 = resolutions_for(&cache, &url, ManifestKind::Cargo).await;
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "second call with unchanged mtime must return the cached Arc"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn resolutions_for_returns_empty_when_no_lockfile() {
        // No lockfile sibling → empty resolutions, no cache pollution.
        use std::fs;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "uptick-no-lockfile-{}-{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();

        let manifest = dir.join("Cargo.toml");
        fs::write(&manifest, "").unwrap();
        let url = Url::from_file_path(&manifest).unwrap();
        let cache = DashMap::new();

        let res = resolutions_for(&cache, &url, ManifestKind::Cargo).await;
        assert!(res.is_empty());
        assert_eq!(cache.len(), 0, "no lockfile means nothing to cache");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_version_for_prefers_lockfile() {
        let entry = RawEntry {
            name: "lodash".into(),
            version_literal: "^1.0.0".into(),
            version_range: Range::default(),
            name_range: Range::default(),
            group: None,
        };
        // No lockfile → manifest floor (1.0.0).
        let empty = Resolutions::new();
        assert_eq!(
            scan_version_for(&entry, &empty),
            Some(Version::parse("1.0.0").unwrap())
        );

        // Lockfile pins 1.0.7 → that wins even though the literal is ^1.0.0.
        let mut with_lockfile = Resolutions::new();
        with_lockfile.insert("lodash".into(), Version::parse("1.0.7").unwrap());
        assert_eq!(
            scan_version_for(&entry, &with_lockfile),
            Some(Version::parse("1.0.7").unwrap())
        );

        // Lockfile pins a different package → entry falls through to
        // its own manifest floor.
        let mut wrong_package = Resolutions::new();
        wrong_package.insert("something-else".into(), Version::parse("9.9.9").unwrap());
        assert_eq!(
            scan_version_for(&entry, &wrong_package),
            Some(Version::parse("1.0.0").unwrap())
        );
    }

    #[test]
    fn should_bump_predicate() {
        let latest = Version::parse("1.5.0").unwrap();
        // Exact pin below latest → bump.
        assert!(should_bump("=1.2.3", &latest));
        // Caret range already accepts latest → no bump.
        assert!(!should_bump("^1.0.0", &latest));
        // Bare `1.2.3` parses as `^1.2.3` (semver's implicit caret), which
        // accepts 1.5.0 — no bump.
        assert!(!should_bump("1.2.3", &latest));
        // Exact pin newer than `latest` → no bump (no downgrade when the
        // registry cache lags behind a manual override).
        assert!(!should_bump("=2.0.0", &latest));
        // Unparseable literal: `parse_literal` returns None, so the
        // `is_none_or` arm fires and we offer the bump.
        assert!(should_bump("not-a-version", &latest));
    }

    #[test]
    fn registry_url_per_kind() {
        // Plain names hit the canonical path on each registry.
        assert_eq!(
            registry_url(ManifestKind::Npm, "react").unwrap().as_str(),
            "https://www.npmjs.com/package/react"
        );
        assert_eq!(
            registry_url(ManifestKind::Cargo, "serde").unwrap().as_str(),
            "https://crates.io/crates/serde"
        );
        assert_eq!(
            registry_url(ManifestKind::Pub, "provider")
                .unwrap()
                .as_str(),
            "https://pub.dev/packages/provider"
        );
        assert_eq!(
            registry_url(ManifestKind::Composer, "symfony/console")
                .unwrap()
                .as_str(),
            "https://packagist.org/packages/symfony/console"
        );
    }

    #[test]
    fn registry_url_keeps_scope_prefix() {
        // npm scoped names contain `@` and `/`; both are valid in a path
        // segment and the registry's package page resolves them as-is.
        let url = registry_url(ManifestKind::Npm, "@types/node").unwrap();
        assert_eq!(url.as_str(), "https://www.npmjs.com/package/@types/node");
    }

    #[test]
    fn ranges_overlap_cases() {
        let a = r(1, 0, 1, 10);
        assert!(ranges_overlap(&a, &r(1, 0, 1, 10)));
        assert!(ranges_overlap(&a, &r(1, 5, 1, 15)));
        assert!(ranges_overlap(&a, &r(0, 5, 2, 3)));
        // Touching at a single point (end of A == start of B): treat as overlap.
        // Code actions on a cursor placed exactly at a boundary should still fire.
        assert!(ranges_overlap(&a, &r(1, 10, 1, 20)));
        // Disjoint same-line.
        assert!(!ranges_overlap(&a, &r(1, 11, 1, 20)));
        // Disjoint different-line.
        assert!(!ranges_overlap(&a, &r(3, 0, 3, 5)));
    }
}
