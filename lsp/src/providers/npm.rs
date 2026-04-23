//! npm registry client.
//!
//! We hit the `/{pkg}/latest` endpoint, which returns the document pointed
//! at by the `latest` dist-tag — just the version and some metadata. This is
//! much cheaper than fetching the full package document (which includes
//! every tarball URL for every version ever published).

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
    // `@scope/name` → `@scope%2Fname` so the `/` doesn't get interpreted as
    // a path separator by the registry. Non-scoped names pass through as-is.
    let path = if let Some(rest) = name.strip_prefix('@') {
        format!("@{}", rest.replace('/', "%2F"))
    } else {
        name.to_string()
    };
    let url = format!("https://registry.npmjs.org/{path}/latest");
    let body: Latest = super::get_json(client, ManifestKind::Npm, name, &url).await?;
    // `/latest` returns only the current stable release, so `latest_stable`
    // and `latest_any` are the same value. A future prerelease-opt-in mode
    // would need to switch to the `/{pkg}` full doc.
    let latest = Version::parse(&body.version).ok();
    Ok(VersionInfo {
        latest_stable: latest.clone(),
        latest_any: latest,
        url: Some(format!("https://www.npmjs.com/package/{name}")),
    })
}

/// Minimal payload shape from `/{pkg}/latest`. We only need the version;
/// the real document has dozens of fields (maintainers, dist, etc.) that
/// `serde` happily ignores thanks to the default `deny_unknown_fields = false`.
#[derive(Deserialize)]
struct Latest {
    version: String,
}
