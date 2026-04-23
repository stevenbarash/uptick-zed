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

// Per-host concurrency caps. npm and pub.dev are CDN-fronted and happy with
// plenty of parallelism; packagist is smaller but still cache-fronted.
// crates.io explicitly asks crawlers for ≤1 req/sec (see their crawler
// policy), so it gets a 1-wide semaphore plus a min-interval gate below.
static NPM_SEM: Semaphore = Semaphore::const_new(16);
static PUB_SEM: Semaphore = Semaphore::const_new(16);
static PACKAGIST_SEM: Semaphore = Semaphore::const_new(8);
static CRATES_IO_SEM: Semaphore = Semaphore::const_new(1);

const CRATES_IO_MIN_INTERVAL: Duration = Duration::from_millis(1100);
// `std::sync::Mutex`: the crates.io semaphore is 1-wide, so there is no real
// contention on this lock — we only need a safe way to read/write the last
// request timestamp. The guard is released before every `.await` below.
static CRATES_IO_LAST: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

fn semaphore_for(kind: ManifestKind) -> &'static Semaphore {
    match kind {
        ManifestKind::Npm => &NPM_SEM,
        ManifestKind::Pub => &PUB_SEM,
        ManifestKind::Composer => &PACKAGIST_SEM,
        ManifestKind::Cargo => &CRATES_IO_SEM,
    }
}

/// Shared registry-call helper. Applies per-host concurrency limits (and
/// crates.io's 1-req/sec rate limit), performs the GET, retries once on a
/// transient 5xx, and decodes JSON on success. `kind` and `name` are threaded
/// into every error message so failures point at the right package.
pub(crate) async fn get_json<T: DeserializeOwned>(
    client: &Client,
    kind: ManifestKind,
    name: &str,
    url: &str,
) -> Result<T> {
    let registry = kind.display();
    let _permit = semaphore_for(kind).acquire().await.expect("semaphore");

    if matches!(kind, ManifestKind::Cargo) {
        // Compute required wait while holding the lock; release it before sleeping.
        let wait = {
            let last = CRATES_IO_LAST.lock().expect("crates.io rate-limit mutex");
            last.and_then(|prev| CRATES_IO_MIN_INTERVAL.checked_sub(prev.elapsed()))
        };
        if let Some(w) = wait {
            sleep(w).await;
        }
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
        return Err(anyhow!("{registry} {name}: {status}"));
    }
}
