use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;

/// npm registry endpoint for the latest "release" tag.
/// We request the compact `"latest"` tag rather than the full metadata
/// document to keep responses small.
///
/// Scoped packages (`@scope/name`) need URL-encoding of the slash.
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let path = if let Some(rest) = name.strip_prefix('@') {
        format!("@{}", rest.replace('/', "%2F"))
    } else {
        name.to_string()
    };
    let url = format!("https://registry.npmjs.org/{path}/latest");

    let resp = client.get(&url).send().await.context("npm request")?;
    if !resp.status().is_success() {
        return Err(anyhow!("npm {name}: {}", resp.status()));
    }
    let body: Latest = resp.json().await.context("npm response")?;
    let latest = Version::parse(&body.version).ok();

    Ok(VersionInfo {
        latest_stable: latest.clone(),
        latest_any: latest,
        url: Some(format!("https://www.npmjs.com/package/{name}")),
    })
}

#[derive(Deserialize)]
struct Latest {
    version: String,
}
