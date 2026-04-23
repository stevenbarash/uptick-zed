//! Packagist (PHP/Composer) client.
//!
//! Packagist's v2 metadata endpoint (`/p2/{vendor/name}.json`) is a flat
//! array of every version ever published. Some long-lived packages
//! (`symfony/console`, `laravel/framework`) have *hundreds* of entries, so
//! we walk the array once and keep running maxes rather than collecting
//! and sorting.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// Packagist's v2 metadata endpoint returns every tagged release ever. Walk
/// once, keeping running maxes for stable and any — avoids allocating a
/// `Vec<Version>` for packages with hundreds of tags (e.g. `symfony/console`).
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    // Packagist packages are *always* `vendor/package`. A bare name would
    // 404; bail early with a clearer error.
    if !name.contains('/') {
        return Err(anyhow!(
            "composer package name must be vendor/package: {name}"
        ));
    }
    let url = format!("https://repo.packagist.org/p2/{name}.json");
    let body: Root = super::get_json(client, ManifestKind::Composer, name, &url).await?;

    // The response keys the `packages` map by the same `vendor/name`, so
    // we look it up there rather than assuming it's the only entry.
    let versions = body
        .packages
        .get(name)
        .ok_or_else(|| anyhow!("packagist {name}: missing from response"))?;

    let mut latest_any: Option<Version> = None;
    let mut latest_stable: Option<Version> = None;
    for v in versions {
        // Packagist tags commonly have a leading `v` (e.g. `v10.0.0`).
        // Strip it before parsing so semver accepts the string.
        let raw = v.version.strip_prefix('v').unwrap_or(&v.version);
        let Ok(parsed) = Version::parse(raw) else {
            continue;
        };
        // Running max for the "any release" case. `is_none_or` handles the
        // first iteration (where `latest_any` is still `None`).
        if latest_any.as_ref().is_none_or(|cur| &parsed > cur) {
            latest_any = Some(parsed.clone());
        }
        // Stable releases have no pre-release tag (`1.2.3` vs `1.2.3-alpha.1`).
        if parsed.pre.is_empty() && latest_stable.as_ref().is_none_or(|cur| &parsed > cur) {
            latest_stable = Some(parsed);
        }
    }

    Ok(VersionInfo {
        latest_stable,
        latest_any,
        url: Some(format!("https://packagist.org/packages/{name}")),
    })
}

/// Root of the `/p2/{name}.json` response. We only need the `packages` map;
/// the rest of the document (minimum-stability, etc.) is ignored.
#[derive(Deserialize)]
struct Root {
    packages: HashMap<String, Vec<PkgVersion>>,
}

/// One entry in the `versions` array. Packagist attaches a lot more
/// metadata (authors, autoload, dist) but we just need the version string.
#[derive(Deserialize)]
struct PkgVersion {
    version: String,
}
