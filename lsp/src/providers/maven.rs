//! Maven Central client.
//!
//! Hits `https://repo1.maven.org/maven2/<g-as-path>/<artifact>/maven-metadata.xml`,
//! which is the authoritative source for the latest published version of
//! a `groupId:artifactId` coordinate. The metadata document carries a
//! `<release>` element (highest non-snapshot version) and a `<latest>`
//! (highest of any kind). We prefer `<release>`.
//!
//! ## Version coercion
//!
//! Maven version strings are not strict semver. Real-world shapes include
//! `5.3.0`, `5.3.0.RELEASE`, `1.0`, `1.0-SNAPSHOT`, `2.0.0.Final`. The
//! `semver` crate parses none of those except `5.3.0`. Rather than show
//! `latest=None` for the majority of Maven artifacts, we run versions
//! through `coerce_to_semver` which strips common Maven release suffixes
//! and pads short versions to three components. Anything still unparseable
//! after that falls through to no hint (same contract as every other
//! provider).

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use semver::Version;

use crate::cache::VersionInfo;
use crate::manifest::ManifestKind;

pub async fn fetch(client: &Client, name: &str) -> Result<VersionInfo> {
    let (group, artifact) = split_coordinate(name)?;
    let group_path = group.replace('.', "/");
    let url = format!("https://repo1.maven.org/maven2/{group_path}/{artifact}/maven-metadata.xml");

    let body = super::get_text(client, ManifestKind::Maven, name, &url).await?;
    let doc = roxmltree::Document::parse(&body).context("Maven metadata XML")?;
    let versioning = doc
        .root_element()
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "versioning")
        .ok_or_else(|| anyhow!("no <versioning> in Maven metadata for {name}"))?;

    // Prefer `<release>` (highest stable). Fall back to `<latest>` (any
    // kind, including snapshots) only when no stable release exists.
    let release = element_text(versioning, "release");
    let latest = element_text(versioning, "latest");

    let latest_stable = release.as_deref().and_then(coerce_to_semver);
    let latest_any = latest
        .as_deref()
        .and_then(coerce_to_semver)
        .or_else(|| latest_stable.clone());

    Ok(VersionInfo {
        latest_stable,
        latest_any,
        url: Some(format!(
            "https://central.sonatype.com/artifact/{group}/{artifact}"
        )),
    })
}

/// Best-effort Maven-version → `semver::Version` coercion.
///
/// Tries, in order:
///   1. Strict semver — handles the rare `1.2.3` literal.
///   2. Strip a leading `v` plus any of the common Maven release
///      suffixes (`.RELEASE`, `.Final`, `.GA`, case-insensitive).
///   3. Pad missing components so `1.0` → `1.0.0` and `1` → `1.0.0`.
///
/// Returns `None` for genuinely-unparseable strings (`1.0-SNAPSHOT`,
/// `2.0-M1`, `LATEST`). The caller treats `None` as "no hint" which
/// matches every other provider's behaviour on exotic versions.
fn coerce_to_semver(raw: &str) -> Option<Version> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(v) = Version::parse(raw) {
        return Some(v);
    }
    // Trim a leading `v` (some legacy artifacts).
    let stripped_v = raw.strip_prefix('v').unwrap_or(raw);
    // Drop a Maven release suffix if present. Maven Central lists these
    // verbatim, but they're meta-tags about the release lifecycle, not
    // part of the semantic version.
    let stripped = strip_release_suffix(stripped_v);

    // Try as-is after stripping.
    if let Ok(v) = Version::parse(stripped) {
        return Some(v);
    }
    // Pad to 3 numeric components: `1.2` → `1.2.0`, `1` → `1.0.0`.
    let padded = pad_to_three_components(stripped)?;
    Version::parse(&padded).ok()
}

/// Strip a trailing Maven release suffix (case-insensitive). Returns
/// the slice unchanged when none matches.
fn strip_release_suffix(s: &str) -> &str {
    for suffix in [".RELEASE", ".Final", ".GA"] {
        if let Some(stripped) = strip_suffix_case_insensitive(s, suffix) {
            return stripped;
        }
    }
    s
}

fn strip_suffix_case_insensitive<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    if s.len() < suffix.len() {
        return None;
    }
    let (head, tail) = s.split_at(s.len() - suffix.len());
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

/// Pad a numeric-dot version to three components by appending `.0` as
/// needed. Returns `None` if any component isn't a pure non-negative
/// integer — we'd rather decline than coerce, say, `1.0-SNAPSHOT`
/// into something nonsensical.
fn pad_to_three_components(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 3 {
        return None;
    }
    for p in &parts {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let mut owned: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    while owned.len() < 3 {
        owned.push("0".into());
    }
    Some(owned.join("."))
}

/// Split `groupId:artifactId` into its two halves. Maven coordinates
/// never contain `:` in either field, so a single split is always safe.
fn split_coordinate(name: &str) -> Result<(&str, &str)> {
    let (group, artifact) = name
        .split_once(':')
        .ok_or_else(|| anyhow!("expected `groupId:artifactId` Maven coordinate, got `{name}`"))?;
    if group.is_empty() || artifact.is_empty() {
        return Err(anyhow!(
            "Maven coordinate has empty groupId or artifactId: `{name}`"
        ));
    }
    Ok((group, artifact))
}

/// Find a direct child element by local name and return its trimmed
/// text content.
fn element_text(node: roxmltree::Node, name: &str) -> Option<String> {
    let child = node
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == name)?;
    let text = child.children().find(|c| c.is_text())?.text()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_coordinate_happy_path() {
        assert_eq!(
            split_coordinate("org.springframework:spring-core").unwrap(),
            ("org.springframework", "spring-core")
        );
    }

    #[test]
    fn split_coordinate_rejects_missing_separator() {
        assert!(split_coordinate("just-an-artifact").is_err());
        assert!(split_coordinate(":artifact").is_err());
        assert!(split_coordinate("group:").is_err());
    }

    #[test]
    fn coerce_passes_strict_semver() {
        assert_eq!(
            coerce_to_semver("5.3.0").unwrap(),
            Version::parse("5.3.0").unwrap()
        );
    }

    #[test]
    fn coerce_strips_maven_release_suffixes() {
        // The whole point of the coercion — without this Spring's
        // `.RELEASE`-suffixed versions wouldn't surface a latest hint.
        assert_eq!(
            coerce_to_semver("5.3.0.RELEASE").unwrap(),
            Version::parse("5.3.0").unwrap()
        );
        assert_eq!(
            coerce_to_semver("2.0.0.Final").unwrap(),
            Version::parse("2.0.0").unwrap()
        );
        assert_eq!(
            coerce_to_semver("3.0.0.GA").unwrap(),
            Version::parse("3.0.0").unwrap()
        );
        // Case-insensitive.
        assert_eq!(
            coerce_to_semver("5.3.0.release").unwrap(),
            Version::parse("5.3.0").unwrap()
        );
    }

    #[test]
    fn coerce_pads_short_versions() {
        // `1.0` and `1` are common Maven shapes; both should pad to
        // `1.0.0` so they parse and order correctly against newer
        // releases.
        assert_eq!(
            coerce_to_semver("1.0").unwrap(),
            Version::parse("1.0.0").unwrap()
        );
        assert_eq!(
            coerce_to_semver("1").unwrap(),
            Version::parse("1.0.0").unwrap()
        );
    }

    #[test]
    fn coerce_rejects_snapshots_and_milestones() {
        // Snapshots aren't stable releases; refusing to coerce them
        // matches the `<release>` element's exclusion of snapshots.
        // Milestone tags carry alpha noise we shouldn't silently strip.
        assert_eq!(coerce_to_semver("1.0-SNAPSHOT"), None);
        assert_eq!(coerce_to_semver("2.0-M1"), None);
        assert_eq!(coerce_to_semver("LATEST"), None);
        assert_eq!(coerce_to_semver(""), None);
    }
}
