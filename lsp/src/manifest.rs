//! Shared types used across parsers, providers, and the server.
//!
//! This module is deliberately dependency-free — it only imports basic LSP
//! types — so everything downstream (parsers, providers, cache) can depend on
//! `ManifestKind` and `RawEntry` without pulling in a bigger dependency graph.

use tower_lsp::lsp_types::{Range, Url};

/// The different dependency manifest formats we understand.
///
/// This enum drives dispatch throughout the system: parsers, providers, the
/// cache key, and the "registry" label in hover/inlay tooltips all switch
/// on `ManifestKind`. Adding a new ecosystem means adding a variant here and
/// wiring the new module into [`crate::parsers`] and [`crate::providers`].
///
/// `Copy` + `Hash` + `Eq` lets us use the enum directly as a cache key and
/// pass it by value everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManifestKind {
    /// `package.json` / npm (registry.npmjs.org).
    Npm,
    /// `Cargo.toml` / crates.io.
    Cargo,
    /// `pubspec.yaml` / pub.dev.
    Pub,
    /// `composer.json` / Packagist.
    Composer,
    /// `go.mod` / proxy.golang.org. Names include the full module path
    /// (`github.com/foo/bar`), versions are `v`-prefixed.
    Go,
    /// `pom.xml` / Maven Central. Names are the `groupId:artifactId`
    /// coordinate; versions are free-form strings (Maven doesn't enforce
    /// semver, e.g. `5.3.0.RELEASE`).
    Maven,
}

impl ManifestKind {
    /// Classify a document by its URL. Returns `None` for anything we don't
    /// know how to parse — the server will then leave the buffer alone.
    ///
    /// We look only at the terminal path segment (case-insensitive) so we
    /// don't get fooled by directories named `Cargo.toml` or by URI schemes.
    /// Paths like `file:///workspace/Cargo.toml` or `untitled:Cargo.toml`
    /// both work.
    pub fn from_url(url: &Url) -> Option<Self> {
        let name = url.path_segments()?.next_back()?.to_ascii_lowercase();
        match name.as_str() {
            "package.json" => Some(Self::Npm),
            "cargo.toml" => Some(Self::Cargo),
            // Pub accepts either extension; treat them the same.
            "pubspec.yaml" | "pubspec.yml" => Some(Self::Pub),
            "composer.json" => Some(Self::Composer),
            "go.mod" => Some(Self::Go),
            "pom.xml" => Some(Self::Maven),
            _ => None,
        }
    }

    /// Human-readable name of the upstream registry. Used in hover tooltips
    /// ("latest on crates.io") and error messages. Static strings so we don't
    /// allocate every time we format a hint.
    pub fn display(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Cargo => "crates.io",
            Self::Pub => "pub.dev",
            Self::Composer => "Packagist",
            Self::Go => "Go modules",
            Self::Maven => "Maven Central",
        }
    }
}

/// A single `name = version` entry discovered by a parser.
///
/// This is the common currency between parsing and the server: every parser
/// produces a `Vec<RawEntry>`, and the server knows nothing about the
/// original file format afterwards. Ranges are already converted to LSP
/// coordinates, so the server can hand them back verbatim in inlay hints,
/// diagnostics, and workspace edits.
#[derive(Debug, Clone)]
pub struct RawEntry {
    /// Package name exactly as it appears in the manifest (including any
    /// scope prefix for npm, vendor prefix for composer, etc.).
    pub name: String,
    /// Version literal with operators intact (e.g. `"^1.2.3"`, `">= 0.5"`).
    /// The `version` module knows how to strip operators for comparison.
    pub version_literal: String,
    /// Source range of the version literal *without* surrounding quotes.
    /// Used both for placing inlay hints (at `version_range.end`) and as the
    /// target of `Bump to X.Y.Z` code-action text edits.
    pub version_range: Range,
    /// Source range of the package name. Drives hover hit-testing so the
    /// user can mouse over either the name or the version.
    pub name_range: Range,
    /// Grouping label, e.g. "dependencies" / "devDependencies". Always a
    /// parser-side string literal, so it can be borrowed `'static` — no
    /// allocation and no lifetimes to thread through.
    pub group: Option<&'static str>,
}
