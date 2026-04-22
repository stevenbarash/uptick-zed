use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;

/// crates.io API. Requires a descriptive User-Agent — that's set globally
/// on the shared [`reqwest::Client`] in the server.
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let url = format!("https://crates.io/api/v1/crates/{name}");
    let resp = client.get(&url).send().await.context("crates.io request")?;
    if !resp.status().is_success() {
        return Err(anyhow!("crates.io {name}: {}", resp.status()));
    }
    let body: CratesResp = resp.json().await.context("crates.io response")?;

    let latest_stable = body
        .crate_
        .max_stable_version
        .as_deref()
        .and_then(|s| Version::parse(s).ok());
    let latest_any = body
        .crate_
        .max_version
        .as_deref()
        .and_then(|s| Version::parse(s).ok());

    Ok(VersionInfo {
        latest_stable,
        latest_any,
        url: Some(format!("https://crates.io/crates/{name}")),
    })
}

#[derive(Deserialize)]
struct CratesResp {
    #[serde(rename = "crate")]
    crate_: CrateMeta,
}

#[derive(Deserialize)]
struct CrateMeta {
    max_version: Option<String>,
    max_stable_version: Option<String>,
}
