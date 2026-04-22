pub mod cargo;
pub mod composer;
pub mod npm;
pub mod pub_dev;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::de::DeserializeOwned;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// Dispatch to the right registry for a manifest kind.
pub async fn fetch(
    client: &Client,
    kind: ManifestKind,
    name: &str,
) -> Result<VersionInfo> {
    match kind {
        ManifestKind::Npm => npm::fetch(client, name).await,
        ManifestKind::Cargo => cargo::fetch(client, name).await,
        ManifestKind::Pub => pub_dev::fetch(client, name).await,
        ManifestKind::Composer => composer::fetch(client, name).await,
    }
}

/// Shared registry-call helper: GET `url`, require a 2xx, decode JSON.
/// `registry` and `name` are threaded into every error message so failures
/// point at the right package without each provider re-typing the label.
pub(crate) async fn get_json<T: DeserializeOwned>(
    client: &Client,
    registry: &'static str,
    name: &str,
    url: &str,
) -> Result<T> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("{registry} request for {name}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("{registry} {name}: {status}"));
    }
    resp.json()
        .await
        .with_context(|| format!("{registry} response for {name}"))
}
