//! crates.io client. Rate-limiting (≤1 req/sec per the crates.io crawler
//! policy) lives in [`crate::providers`]; this module just maps one crate
//! name to a single GET.

use anyhow::Result;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    // crates.io has a richer v1 API, but the top-level `/api/v1/crates/{name}`
    // document gives us both `max_version` (any) and `max_stable_version`
    // pre-computed, which is exactly what `VersionInfo` wants.
    let url = format!("https://crates.io/api/v1/crates/{name}");
    let body: CratesResp = super::get_json(client, ManifestKind::Cargo, name, &url).await?;

    // Parse both, tolerating failures (e.g. an exotic `max_version` string
    // that our semver crate disagrees with). Lost versions are rare and
    // degrade gracefully to "no hint".
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

/// The `crate` key is reserved in Rust, so we rename the field in the
/// Rust struct and tell serde about it. Everything else in the response
/// (owners, categories, keywords, …) is ignored by serde.
#[derive(Deserialize)]
struct CratesResp {
    #[serde(rename = "crate")]
    crate_: CrateMeta,
}

/// Only the two fields we care about; optional because a brand-new crate
/// that has no published releases at all legitimately has neither.
#[derive(Deserialize)]
struct CrateMeta {
    max_version: Option<String>,
    max_stable_version: Option<String>,
}
