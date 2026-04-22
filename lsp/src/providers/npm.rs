use anyhow::Result;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// npm registry's `/{pkg}/latest` endpoint returns the "release" dist-tag as
/// a compact doc — cheaper than the full metadata payload. Scoped packages
/// need the slash URL-encoded, which is the only non-trivial bit here.
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let path = if let Some(rest) = name.strip_prefix('@') {
        format!("@{}", rest.replace('/', "%2F"))
    } else {
        name.to_string()
    };
    let url = format!("https://registry.npmjs.org/{path}/latest");
    let body: Latest = super::get_json(client, ManifestKind::Npm, name, &url).await?;
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
