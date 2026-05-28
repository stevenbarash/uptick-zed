# Uptick — agent notes

Zed extension that surfaces latest versions + OSV vulnerabilities for `package.json`, `Cargo.toml`, `pubspec.yaml`, `composer.json`. Two crates: a tiny WASM extension shim (`src/lib.rs`) that launches `uptick-lsp`, and the LSP server itself (`lsp/`).

## Before declaring a change done

```
cargo test  -p uptick-lsp --lib
cargo clippy -p uptick-lsp --all-targets -- -D warnings
cargo fmt --all
```

Pre-commit hook rejects unformatted code; the commit will be blocked before it lands. Don't `--no-verify`.

## Code map

Most edits touch one of these. Reach for the others rarely.

- `lsp/src/server.rs` — every LSP handler (`inlay_hint`, `hover`, `code_action`, `code_lens`, `document_link`, `execute_command`, sync), plus the resolve loop. Big file; navigate by handler name.
- `lsp/src/parsers/{cargo_toml,package_json,pubspec_yaml,composer_json}.rs` — one per manifest. All return `Vec<RawEntry>` with LSP-coordinate ranges.
- `lsp/src/providers/{cargo,npm,pub_dev,composer}.rs` — registry HTTP. Per-host semaphores live in `providers/mod.rs`; crates.io adds a 1 req/sec gate.
- `lsp/src/vulnerabilities/{osv,cache}.rs` — OSV scan + per-advisory CVSS detail fetch.
- `lsp/src/{version,manifest,cache,position}.rs` — semver helpers, `ManifestKind`, `VersionCache`, UTF-16 column math.
- `src/lib.rs` — WASM shim. Only edit when changing how the LSP is launched / downloaded.

History + design rationale lives in `CHANGELOG.md` and `docs/superpowers/specs/`.

## Targets

- LSP: native (`cargo build -p uptick-lsp`).
- Extension shim: `wasm32-wasip1` **only**. `wasip2`/`wasip3` produce component-model binaries Zed's extension host silently rejects. `zed_extension_api 0.7.0` is built against the wasip1 ABI.

## Dev loop

1. `cargo install --path lsp` — installs `uptick-lsp` into `~/.cargo/bin`. The extension prefers a `PATH` binary over the GitHub-release download.
2. In Zed: `zed: install dev extension` → repo root.
3. Iterate on LSP only: rerun step 1, then `editor: restart language server` in Zed.
4. Iterate on the WASM shim: `cargo build --target wasm32-wasip1 --release`, then `zed: rebuild dev extension`.

Logs: `UPTICK_LOG=debug` (standalone) or `zed: open language server logs` → Uptick.

## Invariants worth knowing

- **`server.rs::should_bump(literal, latest)`** is the single source of truth for "is this entry out-of-date?". Three surfaces (diagnostic, code action, code lens) all call it. They previously drifted — one site silently skipped a guard the others enforced. Don't reinline the predicate.
- **Three caches with distinct TTLs**: `VersionCache` 1 h, `VulnCache` 1 h, `DetailCache` 24 h (advisories are immutable once published). None persist across restarts.
- **Fingerprint dedup** in `push_updates_raw` skips redundant `publishDiagnostics` + refresh requests when reparse produced no user-visible change. If you add a new state field that should invalidate a redraw, hash it in `fingerprint`.
- **Command names** are `const CMD_BUMP` / `const CMD_OPEN` in `server.rs`. Capability list, lens emitters, and dispatcher all reference the consts.

## Release

Tag `v*` on `main` triggers `.github/workflows/release.yml`. Builds prebuilt `uptick-lsp` for 4 targets (aarch64 + x86_64 macOS, x86_64 Linux gnu, x86_64 Windows MSVC) plus `.sha256` sidecars, attaches to a GitHub release. The extension downloader in `src/lib.rs` constructs asset URLs deterministically — **do not rename release assets**.

Version bump touches four files: `Cargo.toml` (`workspace.package.version`), `extension.toml` (`version`), `CHANGELOG.md` (Unreleased → dated section + tag link at the bottom), `README.md` (What's new table top row). Then `cargo check -p uptick-lsp` to refresh `Cargo.lock`.

## Don't

- Push to `main` without explicit user approval — the auto-mode classifier blocks it.
- Switch the extension shim to `wasip2`/`wasip3`. Zed won't load the result.
- Add a new bump-emitting surface without routing through `should_bump`.
- Skip `cargo fmt`; the pre-commit hook blocks the commit.
