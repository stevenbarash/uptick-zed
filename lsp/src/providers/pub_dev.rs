use anyhow::Result;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// pub.dev's `latest.version` is the package's own "max stable" — we trust
/// it directly. We still walk `versions` to compute `latest_any` so a future
/// prerelease-opt-in mode has somewhere to land, but do it in one pass with
/// no intermediate allocation.
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let url = format!("https://pub.dev/api/packages/{name}");
    let body: Pkg = super::get_json(client, ManifestKind::Pub, name, &url).await?;

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
