//! Registry HTTP clients — one module per ecosystem.
//!
//! Each provider exposes a single `fetch(client, name) -> Result<VersionInfo>`
//! function. This module hosts:
//!
//! - The [`fetch`] dispatcher that routes by `ManifestKind`.
//! - The shared `get_json` helper every provider calls through, which handles
//!   per-host concurrency caps, crates.io's 1-req/sec rate limit, one retry
//!   on transient 5xx, and JSON decoding.
//!
//! Network policy lives here, not in the individual providers, so we can
//! change retry/rate-limit behaviour in one place.

pub mod cargo;
pub mod composer;
pub mod npm;
pub mod pub_dev;

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing::warn;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// Dispatch to the right registry for a manifest kind.
pub async fn fetch(client: &Client, kind: ManifestKind, name: &str) -> Result<VersionInfo> {
    match kind {
        ManifestKind::Npm => npm::fetch(client, name).await,
        ManifestKind::Cargo => cargo::fetch(client, name).await,
        ManifestKind::Pub => pub_dev::fetch(client, name).await,
        ManifestKind::Composer => composer::fetch(client, name).await,
    }
}

// ---------------------------------------------------------------------------
// Per-host concurrency caps.
//
// npm and pub.dev are CDN-fronted and happy with plenty of parallelism;
// packagist is smaller but still cache-fronted. crates.io explicitly asks
// crawlers for ≤1 req/sec (see their crawler policy), so it gets a 1-wide
// semaphore *plus* a min-interval gate below to enforce the rate limit even
// when bursts line up exactly on semaphore release.
// ---------------------------------------------------------------------------

/// Parallelism cap for npm registry requests.
static NPM_SEM: Semaphore = Semaphore::const_new(16);
/// Parallelism cap for pub.dev requests.
static PUB_SEM: Semaphore = Semaphore::const_new(16);
/// Parallelism cap for Packagist requests (a bit tighter — smaller infra).
static PACKAGIST_SEM: Semaphore = Semaphore::const_new(8);
/// Single-slot semaphore for crates.io. Combined with the min-interval
/// below, this enforces a strict ≤1 req/sec globally.
static CRATES_IO_SEM: Semaphore = Semaphore::const_new(1);

/// Minimum gap between crates.io requests. 1.1 s (not 1.0 s) leaves a safety
/// margin so timer jitter and tokio scheduling slop don't nudge us under the
/// 1-req/sec ceiling.
const CRATES_IO_MIN_INTERVAL: Duration = Duration::from_millis(1100);

/// Timestamp of the last crates.io request (or `None` if we haven't made one).
///
/// `std::sync::Mutex`: the crates.io semaphore is 1-wide, so there is no real
/// contention on this lock — we only need a safe way to read/write the last
/// request timestamp. The guard is released before every `.await` below,
/// which is the standard pattern for holding a sync mutex in async code.
static CRATES_IO_LAST: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// Pick the right semaphore for a manifest kind. Returns a `'static`
/// reference because all four semaphores are module-level statics.
fn semaphore_for(kind: ManifestKind) -> &'static Semaphore {
    match kind {
        ManifestKind::Npm => &NPM_SEM,
        ManifestKind::Pub => &PUB_SEM,
        ManifestKind::Composer => &PACKAGIST_SEM,
        ManifestKind::Cargo => &CRATES_IO_SEM,
    }
}

/// See module docs for network policy. `kind` and `name` are threaded
/// into every error message so failures point at the right package.
pub(crate) async fn get_json<T: DeserializeOwned>(
    client: &Client,
    kind: ManifestKind,
    name: &str,
    url: &str,
) -> Result<T> {
    let registry = kind.display();

    // Acquire the per-host semaphore. Released when `_permit` drops at the
    // end of this function, whether we return success or error.
    let _permit = semaphore_for(kind).acquire().await.expect("semaphore");

    // Extra rate-limit gate for crates.io. Computed inside a tight lock
    // scope so we don't hold the sync mutex across the `.await` below.
    if matches!(kind, ManifestKind::Cargo) {
        let wait = {
            let last = CRATES_IO_LAST.lock().expect("crates.io rate-limit mutex");
            // `checked_sub` returns `None` if enough time has already
            // passed, in which case we don't sleep at all.
            last.and_then(|prev| CRATES_IO_MIN_INTERVAL.checked_sub(prev.elapsed()))
        };
        if let Some(w) = wait {
            sleep(w).await;
        }
        // Record the intended request timestamp *before* we hit the wire.
        // This is slightly pessimistic (we might still fail to send) but
        // guarantees we never undershoot the rate limit under contention.
        *CRATES_IO_LAST.lock().expect("crates.io rate-limit mutex") = Some(Instant::now());
    }

    // One retry on transient 5xx. Registry CDNs occasionally load-shed
    // (we observed a 503 from Packagist during smoke testing); retrying
    // after a brief pause covers that case without being aggressive.
    let mut attempt = 0;
    loop {
        let resp = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("{registry} request for {name}"))?;
        let status = resp.status();
        if status.is_success() {
            // Happy path: parse body as JSON. This `await` can fail if the
            // server returned something malformed (very rare); the context
            // string still points at the right package.
            return resp
                .json()
                .await
                .with_context(|| format!("{registry} response for {name}"));
        }
        if status.is_server_error() && attempt == 0 {
            warn!(%registry, %name, %status, "transient registry error; retrying");
            attempt += 1;
            sleep(Duration::from_millis(500)).await;
            continue;
        }
        // Any non-success that isn't a retryable 5xx — or a 5xx on retry —
        // propagates up as an `anyhow::Error`. The server logs it at
        // `warn` level and leaves the hint empty for this package.
        return Err(anyhow!("{registry} {name}: {status}"));
    }
}
