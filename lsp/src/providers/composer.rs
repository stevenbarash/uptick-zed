use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::version::is_stable;

/// Packagist's v2 metadata endpoint. Returns a list of package versions;
/// we pick the highest stable (and highest absolute).
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    if !name.contains('/') {
        return Err(anyhow!("composer package name must be vendor/package: {name}"));
    }
    let url = format!("https://repo.packagist.org/p2/{name}.json");
    let resp = client.get(&url).send().await.context("packagist request")?;
    if !resp.status().is_success() {
        return Err(anyhow!("packagist {name}: {}", resp.status()));
    }
    let body: Root = resp.json().await.context("packagist response")?;

    let versions = body
        .packages
        .get(name)
        .ok_or_else(|| anyhow!("packagist {name}: missing from response"))?;

    let parsed: Vec<Version> = versions
        .iter()
        .filter_map(|v| {
            let s = v.version.strip_prefix('v').unwrap_or(&v.version);
            Version::parse(s).ok()
        })
        .collect();

    let latest_any = parsed.iter().max().cloned();
    let latest_stable = parsed.iter().filter(|v| is_stable(v)).max().cloned();

    Ok(VersionInfo {
        latest_stable,
        latest_any,
        url: Some(format!("https://packagist.org/packages/{name}")),
    })
}

#[derive(Deserialize)]
struct Root {
    packages: HashMap<String, Vec<PkgVersion>>,
}

#[derive(Deserialize)]
struct PkgVersion {
    version: String,
}
