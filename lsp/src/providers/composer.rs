use std::collections::HashMap;

use anyhow::{Result, anyhow};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// Packagist's v2 metadata endpoint returns every tagged release ever. Walk
/// once, keeping running maxes for stable and any — avoids allocating a
/// `Vec<Version>` for packages with hundreds of tags (e.g. `symfony/console`).
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    if !name.contains('/') {
        return Err(anyhow!("composer package name must be vendor/package: {name}"));
    }
    let url = format!("https://repo.packagist.org/p2/{name}.json");
    let body: Root = super::get_json(client, ManifestKind::Composer, name, &url).await?;

    let versions = body
        .packages
        .get(name)
        .ok_or_else(|| anyhow!("packagist {name}: missing from response"))?;

    let mut latest_any: Option<Version> = None;
    let mut latest_stable: Option<Version> = None;
    for v in versions {
        let raw = v.version.strip_prefix('v').unwrap_or(&v.version);
        let Ok(parsed) = Version::parse(raw) else { continue };
        if latest_any.as_ref().is_none_or(|cur| &parsed > cur) {
            latest_any = Some(parsed.clone());
        }
        if parsed.pre.is_empty()
            && latest_stable.as_ref().is_none_or(|cur| &parsed > cur)
        {
            latest_stable = Some(parsed);
        }
    }

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
