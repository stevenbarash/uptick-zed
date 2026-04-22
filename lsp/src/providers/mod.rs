pub mod cargo;
pub mod composer;
pub mod npm;
pub mod pub_dev;

use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use reqwest::Client;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, Semaphore};
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
static CRATES_IO_LAST: Lazy<Mutex<Option<Instant>>> = Lazy::new(|| Mutex::new(None));

fn semaphore_for(registry: &str) -> &'static Semaphore {
    match registry {
        "npm" => &NPM_SEM,
        "pub.dev" => &PUB_SEM,
        "packagist" => &PACKAGIST_SEM,
        "crates.io" => &CRATES_IO_SEM,
        _ => &NPM_SEM, // reasonable default; new registries should add a line
    }
}

/// Shared registry-call helper. Applies per-host concurrency limits (and
/// crates.io's 1-req/sec rate limit), performs the GET, retries once on a
/// transient 5xx, and decodes JSON on success. `registry` and `name` are
/// threaded into every error message so failures point at the right package.
pub(crate) async fn get_json<T: DeserializeOwned>(
    client: &Client,
    registry: &'static str,
    name: &str,
    url: &str,
) -> Result<T> {
    let _permit = semaphore_for(registry).acquire().await.expect("semaphore");

    if registry == "crates.io" {
        let mut last = CRATES_IO_LAST.lock().await;
        if let Some(prev) = *last {
            let since = prev.elapsed();
            if since < CRATES_IO_MIN_INTERVAL {
                sleep(CRATES_IO_MIN_INTERVAL - since).await;
            }
        }
        *last = Some(Instant::now());
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
