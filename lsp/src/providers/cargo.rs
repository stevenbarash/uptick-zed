use anyhow::Result;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let url = format!("https://crates.io/api/v1/crates/{name}");
    let body: CratesResp = super::get_json(client, ManifestKind::Cargo, name, &url).await?;

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
