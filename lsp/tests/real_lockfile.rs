//! Real-file smoke test for the Cargo lockfile parser. Parses this
//! workspace's own `Cargo.lock` and asserts the resolutions match the
//! reality on disk.
//!
//! Loose assertions: dependency names, not versions. We expect those
//! to keep advancing.

use std::path::PathBuf;

use uptick_lsp::lockfiles::cargo;

#[test]
fn parses_workspace_cargo_lock() {
    // `CARGO_MANIFEST_DIR` resolves to `<repo>/lsp`; the lockfile is at
    // the workspace root, one level up.
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "Cargo.lock"]
        .iter()
        .collect();
    let text = std::fs::read_to_string(&path).expect("read workspace Cargo.lock");
    let res = cargo::parse(&text).expect("parse Cargo.lock");

    // Sanity: a Rust workspace with reqwest + tokio + tower-lsp pulls
    // in well over fifty crates.
    assert!(
        res.len() > 50,
        "expected > 50 resolved packages, got {}",
        res.len()
    );

    // A handful of direct dependencies should be present by name.
    for crate_name in ["tokio", "semver", "dashmap", "tower-lsp"] {
        assert!(
            res.contains_key(crate_name),
            "expected {crate_name} in workspace lockfile resolutions; \
             got {} entries, sample: {:?}",
            res.len(),
            res.keys().take(5).collect::<Vec<_>>()
        );
    }

    eprintln!(
        "real_lockfile: parsed {} entries from {}",
        res.len(),
        path.display()
    );
}
