//! pub.dev (Dart/Flutter) client.
//!
//! The `/api/packages/{name}` endpoint gives us a `latest` shortcut plus a
//! full list of `versions`. We trust `latest` for stable releases and walk
//! `versions` once to compute the overall max (including prereleases).

use anyhow::Result;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// pub.dev's `latest.version` is the package's own "max stable" — we trust
/// it directly. We compute `latest_any` in one pass over `versions` with no
/// intermediate allocation.
pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let url = format!("https://pub.dev/api/packages/{name}");
    let body: Pkg = super::get_json(client, ManifestKind::Pub, name, &url).await?;

    // `latest` is canonical; no need to walk `versions` ourselves for the
    // stable case.
    let latest_stable = Version::parse(&body.latest.version).ok();
    // Overall max across every version the package has ever published.
    // `filter_map + max` runs in one pass with no sorting — O(n) time,
    // O(1) extra memory.
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

/// Top-level shape of the pub.dev response.
#[derive(Deserialize)]
struct Pkg {
    latest: VersionObj,
    /// Defaults to empty so a malformed response without `versions` still
    /// produces a usable `VersionInfo` from just `latest`.
    #[serde(default)]
    versions: Vec<VersionObj>,
}

/// Both `latest` and each element of `versions` use this same shape; we just
/// need the `version` string.
#[derive(Deserialize)]
struct VersionObj {
    version: String,
}
