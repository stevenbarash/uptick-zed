pub mod cargo_toml;
pub mod composer_json;
pub mod json_common;
pub mod package_json;
pub mod pubspec_yaml;

use crate::manifest::{ManifestKind, RawEntry};

/// Run the right parser for this manifest kind. Parse errors are not fatal:
/// each parser returns whatever entries it managed to extract (partial input
/// is common while the user is mid-edit).
pub fn parse(kind: ManifestKind, source: &str) -> Vec<RawEntry> {
    match kind {
        ManifestKind::Npm => package_json::parse(source),
        ManifestKind::Cargo => cargo_toml::parse(source),
        ManifestKind::Pub => pubspec_yaml::parse(source),
        ManifestKind::Composer => composer_json::parse(source),
    }
}
