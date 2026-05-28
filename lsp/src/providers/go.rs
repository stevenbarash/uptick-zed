//! Go module proxy client.
//!
//! Uses the public `proxy.golang.org` endpoint described at
//! <https://go.dev/ref/mod#goproxy-protocol>: a single `GET /<module>/@latest`
//! returns JSON describing the highest-tagged version.
//!
//! ## Path encoding
//!
//! The Go module proxy requires uppercase letters in module paths to be
//! escaped with `!`: `golang.org/X/foo` becomes `golang.org/!x/foo`. We
//! apply that transformation here so providers handle the rare modules
//! whose paths aren't all-lowercase. Most real-world modules
//! (`github.com/...`, `golang.org/x/...`) are lowercase already.
//!
//! ## Known limitations
//!
//! - **Pseudo-versions** (`v0.0.0-20240101000000-abcdef`) — used by
//!   `go get <module>@<commit>` when no tag exists — fail semver parsing
//!   and surface as no `latest` hint. The OSV scan still runs against
//!   the pseudo-version string itself, so vulnerability detection works;
//!   only the "→ X.Y.Z" upgrade hint is suppressed.
//! - **`+incompatible` suffix** on modules without a `/vN` path parses
//!   fine (semver treats `+incompatible` as build metadata, which it
//!   ignores during comparison). No special handling needed.

use anyhow::Result;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let encoded = encode_module_path(name);
    let url = format!("https://proxy.golang.org/{encoded}/@latest");
    let body: GoProxyResp = super::get_json(client, ManifestKind::Go, name, &url).await?;

    // proxy.golang.org returns `vX.Y.Z` — strip the leading `v` for
    // semver parsing. Pseudo-versions (`v0.0.0-20240101000000-abcdef`)
    // and `+incompatible` suffixes will fail semver and surface as
    // `None`, which is the right outcome: there's no clean upgrade
    // target to render for those.
    let stripped = body.version.strip_prefix('v').unwrap_or(&body.version);
    let parsed = Version::parse(stripped).ok();

    Ok(VersionInfo {
        latest_stable: parsed.clone(),
        latest_any: parsed,
        url: Some(format!("https://pkg.go.dev/{name}")),
    })
}

#[derive(Deserialize)]
struct GoProxyResp {
    #[serde(rename = "Version")]
    version: String,
}

/// Escape uppercase letters in a Go module path as `!<lowercase>` per
/// the Go module proxy protocol. Lowercase paths round-trip unchanged.
fn encode_module_path(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('!');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_lowercases_uppercase_letters() {
        assert_eq!(
            encode_module_path("github.com/foo/bar"),
            "github.com/foo/bar"
        );
        assert_eq!(encode_module_path("golang.org/X/foo"), "golang.org/!x/foo");
        assert_eq!(
            encode_module_path("github.com/Microsoft/go-winio"),
            "github.com/!microsoft/go-winio"
        );
    }
}
