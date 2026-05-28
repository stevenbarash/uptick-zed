//! Sibling-lockfile parsers.
//!
//! The manifest literal (`^1.0.0`, `~1.2`, `>=0.5`) is a range, not an
//! installed version. Vulnerability scans that target only the literal
//! floor (`1.0.0`) miss real exposures — a project pinned at `^1.0.0`
//! whose lockfile resolves `1.0.7`, and where `1.0.7` is in a known
//! advisory window, will silently slip through.
//!
//! This module locates and parses the sibling lockfile (`Cargo.lock`,
//! `package-lock.json`, …) so `server.rs` can hand OSV the version the
//! user actually has installed.
//!
//! ## Scope
//!
//! - Cargo (`Cargo.lock`) and npm (`package-lock.json`) covered today.
//! - Pub / Composer parsers will land in a follow-up; until then,
//!   `parse(...)` returns an empty map for those kinds and the scanner
//!   falls back to manifest floors unchanged.
//!
//! ## Caching
//!
//! Lockfiles are tiny and parsing is cheap, but we still mtime-check
//! and reuse the parsed map across resolve bursts — `Backend` stores
//! `LockfileSnapshot` per absolute path. A `cargo update` between
//! manifest edits is picked up automatically on the next resolve.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use semver::Version;
use tower_lsp::lsp_types::Url;

use crate::manifest::ManifestKind;

pub mod cargo;
pub mod npm;

/// Direct-dependency name → installed `Version` from the lockfile.
///
/// Only direct dependencies appear — transitives aren't in the manifest,
/// so they can't be hovered or annotated. Names use the manifest's exact
/// spelling (scoped npm packages keep the leading `@`, Composer keeps
/// `vendor/`, etc.).
pub type Resolutions = HashMap<String, Version>;

/// A parsed lockfile snapshot plus its mtime at parse time. `Backend`
/// caches these by absolute path (the key in the `DashMap`) and reuses
/// the parsed map until the mtime advances.
#[derive(Debug)]
pub struct LockfileSnapshot {
    pub mtime: SystemTime,
    pub resolutions: Arc<Resolutions>,
}

/// Walk upward from the manifest's parent directory to find the
/// matching lockfile. Workspaces (Cargo, npm) keep the lockfile at the
/// workspace root, not next to each member manifest, so a few levels
/// of walk-up is required.
///
/// Returns `None` when the URI isn't a local file, the kind has no
/// lockfile support yet, or no lockfile lives within `MAX_WALK_UP`
/// ancestors.
pub fn locate(manifest_uri: &Url, kind: ManifestKind) -> Option<PathBuf> {
    // Bound the walk so a manifest under `/Users/foo/very/deep/tree`
    // doesn't scan all the way to `/`. Eight ancestors is enough for any
    // workspace we've seen in the wild.
    const MAX_WALK_UP: usize = 8;
    let lockfile_name = filename(kind)?;
    let manifest_path = manifest_uri.to_file_path().ok()?;
    let mut dir = manifest_path.parent()?;
    for _ in 0..MAX_WALK_UP {
        let candidate = dir.join(lockfile_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

/// Read and parse the lockfile at `path`. Errors propagate; the caller
/// is expected to treat any failure as "no resolutions" and use the
/// manifest literal instead.
///
/// Async by way of `tokio::fs::read_to_string` so a multi-megabyte
/// `Cargo.lock` (large monorepo) doesn't block the runtime worker
/// while it's being slurped. The format-specific parsers stay sync
/// and operate on the `&str` so unit tests can drive them without an
/// async context.
pub async fn parse(kind: ManifestKind, path: &Path) -> Result<Resolutions> {
    let text = tokio::fs::read_to_string(path).await?;
    match kind {
        ManifestKind::Cargo => cargo::parse(&text),
        ManifestKind::Npm => npm::parse(&text),
        // Go's `go.mod` already carries concrete versions in its
        // `require` lines, so there's nothing extra to read from
        // `go.sum`. Maven has no broadly-deployed lockfile format.
        ManifestKind::Pub | ManifestKind::Composer | ManifestKind::Go | ManifestKind::Maven => {
            Ok(Resolutions::new())
        }
    }
}

/// Lockfile filename for a given manifest kind. Returns `None` for
/// kinds we haven't shipped a parser for yet. Single source of truth
/// — `locate()` uses it to find the file, `server::hover` uses it as
/// the display label next to the resolved version, and any future
/// `workspace/didChangeWatchedFiles` registration will share it too.
pub fn filename(kind: ManifestKind) -> Option<&'static str> {
    match kind {
        ManifestKind::Cargo => Some("Cargo.lock"),
        ManifestKind::Npm => Some("package-lock.json"),
        ManifestKind::Pub | ManifestKind::Composer | ManifestKind::Go | ManifestKind::Maven => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Construct a unique temp dir under `$TMPDIR`. Tests can layout
    /// arbitrary file trees underneath and clean up at the end.
    fn tmpdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("uptick-{label}-{}-{}", std::process::id(), nanos));
        fs::create_dir_all(&path).unwrap();
        // Canonicalize so test assertions see the same prefix as
        // `Url::to_file_path()` after macOS symlink resolution
        // (/tmp → /private/tmp).
        path.canonicalize().unwrap()
    }

    #[test]
    fn locate_finds_sibling_lockfile() {
        let dir = tmpdir("locate-sibling");
        let lockfile = dir.join("Cargo.lock");
        fs::write(&lockfile, "").unwrap();
        let manifest = dir.join("Cargo.toml");
        fs::write(&manifest, "").unwrap();
        let url = Url::from_file_path(&manifest).unwrap();
        assert_eq!(locate(&url, ManifestKind::Cargo), Some(lockfile));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn locate_walks_up_to_workspace_root() {
        // Layout: <tmp>/Cargo.lock + <tmp>/a/b/c/Cargo.toml.
        // Three levels up from the manifest is well within the
        // MAX_WALK_UP=8 bound.
        let root = tmpdir("locate-walkup");
        let lockfile = root.join("Cargo.lock");
        fs::write(&lockfile, "").unwrap();
        let crate_dir = root.join("a").join("b").join("c");
        fs::create_dir_all(&crate_dir).unwrap();
        let manifest = crate_dir.join("Cargo.toml");
        fs::write(&manifest, "").unwrap();
        let url = Url::from_file_path(&manifest).unwrap();
        assert_eq!(locate(&url, ManifestKind::Cargo), Some(lockfile));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn locate_returns_none_for_unsupported_kinds() {
        // Pub / Composer parsers haven't shipped yet; locate() must
        // signal that cleanly so callers fall through to manifest
        // floors.
        let dir = tmpdir("locate-unsupported");
        let manifest = dir.join("pubspec.yaml");
        fs::write(&manifest, "").unwrap();
        let url = Url::from_file_path(&manifest).unwrap();
        assert_eq!(locate(&url, ManifestKind::Pub), None);
        assert_eq!(locate(&url, ManifestKind::Composer), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn locate_returns_none_when_no_lockfile_exists() {
        let dir = tmpdir("locate-missing");
        let manifest = dir.join("Cargo.toml");
        fs::write(&manifest, "").unwrap();
        let url = Url::from_file_path(&manifest).unwrap();
        assert_eq!(locate(&url, ManifestKind::Cargo), None);
        fs::remove_dir_all(&dir).ok();
    }
}
