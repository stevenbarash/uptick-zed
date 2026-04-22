use tower_lsp::lsp_types::{Range, Url};

/// The different dependency manifest formats we understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManifestKind {
    Npm,
    Cargo,
    Pub,
    Composer,
}

impl ManifestKind {
    /// Classify a document by its URL. Returns `None` for anything we don't
    /// know how to parse — the server will then leave the buffer alone.
    pub fn from_url(url: &Url) -> Option<Self> {
        let name = url.path_segments()?.next_back()?.to_ascii_lowercase();
        match name.as_str() {
            "package.json" => Some(Self::Npm),
            "cargo.toml" => Some(Self::Cargo),
            "pubspec.yaml" | "pubspec.yml" => Some(Self::Pub),
            "composer.json" => Some(Self::Composer),
            _ => None,
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Cargo => "crates.io",
            Self::Pub => "pub.dev",
            Self::Composer => "Packagist",
        }
    }
}

/// A single `name = version` entry discovered by a parser, with the
/// source range of the version literal (used to place inlay hints and
/// code-action edits).
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub name: String,
    pub version_literal: String,
    pub version_range: Range,
    pub name_range: Range,
    /// Grouping label, e.g. "dependencies" / "devDependencies". Always a
    /// parser-side string literal, so it can be borrowed `'static`.
    pub group: Option<&'static str>,
}
