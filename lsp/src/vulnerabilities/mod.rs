//! Vulnerability scanning for open manifests.
//!
//! This module is parallel to `crate::providers`: providers answer "what is
//! the latest version of X?"; this module answers "is version V of X in
//! ecosystem E known-vulnerable?". Today the only source is OSV
//! (osv.dev); future sources (e.g. GitHub Advisory DB) would live here
//! alongside `osv.rs`.

pub mod cache;
pub mod osv;

use crate::manifest::ManifestKind;

/// A single vulnerability entry as surfaced to the server.
///
/// Fields mirror OSV's `/v1/query` minimal response shape. `id` is the only
/// guaranteed field (everything else is optional upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vulnerability {
    /// Unique identifier, e.g. `"GHSA-jf85-cpcp-j695"`.
    pub id: String,
    /// ISO 8601 timestamp of the last modification upstream.
    pub modified: String,
    /// One-line summary, when provided.
    pub summary: Option<String>,
    /// Longer description, when provided.
    pub details: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecosystem_map_exhaustive() {
        // Exhaustive by construction (match on Copy enum); these assertions
        // catch accidental string typos during refactors.
        assert_eq!(osv_ecosystem(ManifestKind::Cargo), "crates.io");
        assert_eq!(osv_ecosystem(ManifestKind::Npm), "npm");
        assert_eq!(osv_ecosystem(ManifestKind::Composer), "Packagist");
        assert_eq!(osv_ecosystem(ManifestKind::Pub), "Pub");
    }
}
