//! Vulnerability scanning for open manifests.
//!
//! This module is parallel to `crate::providers`: providers answer "what is
//! the latest version of X?"; this module answers "is version V of X in
//! ecosystem E known-vulnerable?". Today the only source is OSV
//! (osv.dev); future sources (e.g. GitHub Advisory DB) would live here
//! alongside `osv.rs`.

pub mod cache;
pub mod osv;

use anyhow::Result;
use reqwest::Client;
use semver::Version;
use tokio::sync::Semaphore;

use crate::manifest::ManifestKind;

/// A single vulnerability entry as surfaced to the server.
///
/// Fields mirror OSV's `/v1/query` minimal response shape. `id` is the only
/// guaranteed field (everything else is optional upstream).
#[derive(Debug, Clone, PartialEq)]
pub struct Vulnerability {
    /// Unique identifier, e.g. `"GHSA-jf85-cpcp-j695"`.
    pub id: String,
    /// ISO 8601 timestamp of the last modification upstream.
    pub modified: String,
    /// One-line summary, when provided.
    pub summary: Option<String>,
    /// Longer description, when provided.
    pub details: Option<String>,
    /// CVSS base score 0.0–10.0 if any `severity[]` entry parsed, else
    /// `None`. `None` is rendered as `Warning` (the v0.2 default).
    pub score: Option<f32>,
}

/// Map a `ManifestKind` to its OSV ecosystem identifier.
///
/// Values come from OSV's published ecosystem list
/// (https://ossf.github.io/osv-schema/#affectedpackage-field) and match
/// vscode-versionlens's upstream mapping verbatim.
pub fn osv_ecosystem(kind: ManifestKind) -> &'static str {
    match kind {
        ManifestKind::Cargo => "crates.io",
        ManifestKind::Npm => "npm",
        ManifestKind::Composer => "Packagist",
        ManifestKind::Pub => "Pub",
    }
}

/// Parallelism cap for OSV requests. Conservative default — OSV publishes
/// no explicit rate limit. Raise if scans become noticeably serialised.
static OSV_SEM: Semaphore = Semaphore::const_new(8);

/// Query OSV for vulnerabilities affecting `(kind, name, version)`.
///
/// Thin wrapper around `osv::query` that (a) maps the `ManifestKind` to an
/// OSV ecosystem string and (b) bounds concurrency via the module-level
/// semaphore. Caching is the caller's responsibility.
pub async fn fetch_vulns(
    client: &Client,
    kind: ManifestKind,
    name: &str,
    version: &Version,
) -> Result<Vec<Vulnerability>> {
    let _permit = OSV_SEM.acquire().await.expect("OSV semaphore");
    let ecosystem = osv_ecosystem(kind);
    osv::query(client, ecosystem, name, &version.to_string()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecosystem_map_exhaustive() {
        assert_eq!(osv_ecosystem(ManifestKind::Cargo), "crates.io");
        assert_eq!(osv_ecosystem(ManifestKind::Npm), "npm");
        assert_eq!(osv_ecosystem(ManifestKind::Composer), "Packagist");
        assert_eq!(osv_ecosystem(ManifestKind::Pub), "Pub");
    }
}
