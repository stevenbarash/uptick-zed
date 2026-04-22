pub mod cargo;
pub mod composer;
pub mod npm;
pub mod pub_dev;

use anyhow::Result;
use reqwest::Client;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

/// Dispatch to the right registry for a manifest kind.
pub async fn fetch(
    client: &Client,
    kind: ManifestKind,
    name: &str,
) -> Result<VersionInfo> {
    match kind {
        ManifestKind::Npm => npm::fetch(client, name).await,
        ManifestKind::Cargo => cargo::fetch(client, name).await,
        ManifestKind::Pub => pub_dev::fetch(client, name).await,
        ManifestKind::Composer => composer::fetch(client, name).await,
    }
}
