use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;

/// pub.dev's public API. The `latest.version` field follows the package's
/// own "max stable" selection. We also walk `versions` for an absolute max
/// (including prereleases) so `--include-prereleases` mode works.
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let url = format!("https://pub.dev/api/packages/{name}");
    let resp = client.get(&url).send().await.context("pub.dev request")?;
    if !resp.status().is_success() {
        return Err(anyhow!("pub.dev {name}: {}", resp.status()));
    }
    let body: Pkg = resp.json().await.context("pub.dev response")?;

    let latest_stable = Version::parse(&body.latest.version).ok();
    let latest_any = body
        .versions
        .iter()
        .filter_map(|v| Version::parse(&v.version).ok())
        .max();

    Ok(VersionInfo {
        latest_stable,
        latest_any,
        url: Some(format!("https://pub.dev/packages/{name}")),
    })
}

#[derive(Deserialize)]
struct Pkg {
    latest: VersionObj,
    #[serde(default)]
    versions: Vec<VersionObj>,
}

#[derive(Deserialize)]
struct VersionObj {
    version: String,
}
