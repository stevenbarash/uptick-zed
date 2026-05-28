//! # uptick-lsp
//!
//! A standalone LSP server that surfaces "latest available version" hints for
//! package manifests. It speaks LSP over stdio, so any editor that supports
//! LSP — Zed, Neovim, Helix, VS Code — can use it.
//!
//! ## Module layout
//!
//! - [`cache`] — thread-safe TTL cache keyed by (ecosystem, package name).
//! - [`lockfiles`] — sibling-lockfile parsers (`Cargo.lock`,
//!   `package-lock.json`, …) so vulnerability scans target the actually-
//!   installed version, not the literal in the manifest range.
//! - [`manifest`] — the `ManifestKind` enum that drives dispatch, plus the
//!   `RawEntry` struct each parser produces.
//! - [`parsers`] — format-specific parsers (one per ecosystem). Each returns
//!   a `Vec<RawEntry>` with source positions.
//! - [`position`] — `LineIndex`: byte-offset → LSP `Position` conversion.
//! - [`providers`] — registry HTTP clients (one per ecosystem).
//! - [`server`] — the `tower-lsp` `LanguageServer` implementation that ties
//!   everything together.
//! - [`version`] — small semver helpers for stripping range operators and
//!   checking whether a latest version satisfies a user range.
//! - [`vulnerabilities`] — OSV vulnerability scanner parallel to providers.
//!
//! All modules are declared `pub` so integration tests and external
//! consumers can reach into them directly without going through the full
//! LSP event loop.

pub mod cache;
pub mod lockfiles;
pub mod manifest;
pub mod onboarding;
pub mod parsers;
pub mod position;
pub mod providers;
pub mod server;
pub mod version;
pub mod vulnerabilities;
