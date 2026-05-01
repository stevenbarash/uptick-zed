# uptick-zed

[![CI](https://github.com/stevenbarash/uptick-zed/actions/workflows/ci.yml/badge.svg)](https://github.com/stevenbarash/uptick-zed/actions/workflows/ci.yml)

A [Zed](https://zed.dev) extension that shows the latest available version of each dependency in package manifests — inline "what's the newest release?" feedback for `package.json`, `Cargo.toml`, `pubspec.yaml`, and `composer.json`. Inspired by the [VSCode VersionLens](https://gitlab.com/versionlens/vscode-versionlens) extension (see [Acknowledgements](#acknowledgements)).

Supports:

| Manifest          | Registry                  |
| ----------------- | ------------------------- |
| `package.json`    | [registry.npmjs.org](https://registry.npmjs.org) |
| `Cargo.toml`      | [crates.io](https://crates.io) |
| `pubspec.yaml`    | [pub.dev](https://pub.dev) |
| `composer.json`   | [Packagist](https://packagist.org) |

See the [Roadmap](#roadmap) for what's not yet supported.

## What it looks like

Each dependency gets an inlay hint next to the version literal:

```jsonc
{
  "dependencies": {
    "react": "^18.2.0"   → 18.3.1
  }
}
```

```toml
[dependencies]
serde = "1.0.100"   → 1.0.228
tokio = { version = "1.35" }   ✓ 1.35.1
```

A `✓` means the latest release satisfies your range; a `→` means a newer version is available. Out-of-date entries also get an `Information`-level diagnostic, and a `Bump to X.Y.Z` code action that rewrites the literal while preserving the semver operator (`^`, `~`, `>=`, …).

Hover over any dependency name or version for a summary with a link to the registry page.

### Vulnerability scanning (v0.2)

Every parseable `(ecosystem, name, version)` is queried against [osv.dev](https://osv.dev). Known-vulnerable pins surface as `Warning`-level diagnostics in the Problems panel, with the GHSA/CVE ID as the diagnostic code:

```jsonc
{
  "dependencies": {
    "lodash": "4.17.15"   // ⚠ GHSA-jf85-cpcp-j695: Prototype Pollution in lodash
  }
}
```

A lenient version parser handles npm-style shorthand (`^1.2`, `~1`, `1.x`, `>=1.0 <2.0`, hyphen/OR ranges) where upstream tools silently skip them.

## Roadmap

Latest release is **v0.2** — adds OSV vulnerability scanning on top of v0.1's four-ecosystem version-suggestion baseline. The items below are rough plans, not a schedule; contributions are welcome.

### v0.2 — shipped

- **OSV vulnerability scanner.** `Warning` diagnostics for every dependency whose pinned version appears in [osv.dev](https://osv.dev). Source label `uptick`, code = the GHSA/CVE ID so editors can suppress individual advisories.
- **Lenient version parser** (`parse_for_scan`). Closes the npm coverage gap where upstream sends `1.2` to OSV for `^1.2` (no match — npm needs `x.y.z`); we pad to `1.2.0`. Handles `^1.2`, `~1`, `1.x`, `>=1.0 <2.0`, hyphen and OR ranges. Pure wildcards / non-semver literals (`*`, `latest`, `file:…`, `github:…`) skipped.
- **Two-phase resolve pipeline.** Version fetches run first, OSV scans run second, both fold into `DocState` and emit diagnostics in one publish.
- New TTL cache keyed on `(ManifestKind, name, Version)`; empty `Vec` distinguishes "scanned and clean" from cache miss.

### v0.1 — shipped

**Ecosystems**

- npm (`package.json`): `dependencies`, `devDependencies`, `peerDependencies`, `optionalDependencies`.
- Cargo (`Cargo.toml`): `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, inline-table and block-table detailed forms, one level of `[target.'cfg(…)'.dependencies]`.
- Pub (`pubspec.yaml`): `dependencies`, `dev_dependencies` (inline scalar entries).
- Composer (`composer.json`): `require`, `require-dev`, with PHP and `ext-*` meta-packages filtered out.

**LSP features**

- Inlay hints: `→ 1.4.0` when an update is available, `✓ 1.4.0` when the latest already satisfies the user's range.
- `Information`-level diagnostics on out-of-date entries.
- `Bump to X.Y.Z` code actions that preserve the semver operator (`^`, `~`, `>=`, …).
- Markdown hover with a link to the registry page.
- 1-hour in-memory TTL cache, lazy-evicted on read.
- Per-host concurrency limits and crates.io's 1-req/sec policy honored.
- Single retry with 500 ms backoff on transient 5xx.
- 250 ms debounce on `did_change`; in-flight tasks aborted on close or re-trigger.
- Fingerprint-based refresh skip — no diagnostic storms on keystrokes that don't change state.

### v0.3 — next up

**More ecosystems**, widest ROI first:

- Maven (`pom.xml`) — XML parsing plus `<parent>` / BOM / property interpolation.
- .NET (`*.csproj`, `*.fsproj`, `Directory.Packages.props`) — MSBuild XML with imports and floating-version notation.
- Go (`go.mod`) — `proxy.golang.org`.
- Python (`pyproject.toml`, `requirements.txt`) — PyPI JSON API.
- NuGet as a stand-alone target if it isn't already covered by .NET.

**Distribution**

- Prebuilt `uptick-lsp` binaries attached to GitHub releases, auto-downloaded by the WASM extension on first run (the `zed-dependi` pattern). Removes the manual `cargo install` step.
- Publish to Zed's extension registry once stable.

**Vulnerability UX (next iteration of v0.2)**

- Hover augmentation: surface vuln IDs and summaries inside the existing hover popup alongside the registry link.
- CVSS severity (requires a second OSV call per advisory or switching to the batch endpoint).
- Lockfile-aware scanning — read `package-lock.json` / `Cargo.lock` so the *resolved* install version is scanned rather than the manifest literal.

### v0.4+ — nice to have

- `--include-prereleases` config flag. Each provider already returns `latest_stable` and `latest_any`; we just always prefer stable today.
- Persistent on-disk cache across LSP restarts (e.g. `~/.cache/uptick/`).
- Workspace command: "Bump all outdated".
- Deprecation warnings — surface npm's `deprecated` field from registry responses.
- Additional vuln sources beyond OSV (GitHub Advisory Database direct, Snyk, …).
- Private registry / auth support — read `.npmrc`, Cargo `[registries]`, per-project tokens.
- Per-ecosystem TTL tuning (npm moves fast, crates.io is slower).
- Workspace-aware Cargo support — one root `Cargo.toml` covering all workspace members' deps without needing each opened individually.
- Optional `uptick.toml` per-project config.

### Known limitations in v0.2

- Pubspec entries with `git:` / `path:` / `hosted:` specs are skipped — no single upstream version to compare against.
- Private registries return 401/403; no credential support yet.
- Zed's extension API doesn't expose inline decorations directly ([zed#49438](https://github.com/zed-industries/zed/issues/49438)); we use LSP inlay hints, which render at the end of the line rather than above it as a clickable lens. The "bump" UX is `cmd-.` (code actions), not a click.
- The LSP binary is installed manually via `cargo install --path lsp` until binary distribution lands in v0.3.
- Vulnerability scans use the manifest literal, not the lockfile-resolved install version. Pinning `^1.0.0` of a package whose latest 1.x is vulnerable will not flag — we scan `1.0.0`.
- OSV diagnostics carry no severity (osv.dev's `/v1/query` endpoint returns IDs and summaries only). All vulns surface as `Warning`.

## How it's built

Zed's extension API doesn't (yet — see [zed#49438](https://github.com/zed-industries/zed/issues/49438)) expose inline decorations, but it renders everything a language server publishes. This repo therefore ships two pieces:

- **Root crate (`src/lib.rs`)** — a thin WASM extension (the file Zed loads). It implements `language_server_command()` and launches the LSP binary.
- **`lsp/`** — a standalone Rust LSP (`uptick-lsp`) that parses the manifest, hits the registry, scans osv.dev for vulnerabilities, caches both for an hour, and publishes inlay hints, diagnostics, code actions, and hovers. The `vulnerabilities/` module runs parallel to `providers/` — version lookups and vuln scans share the cache pattern but stay logically separate.

That separation means `uptick-lsp` is reusable from any LSP-aware editor (Neovim, Helix, …), not just Zed.

## Installation

### 1. Build and install the LSP binary

```sh
cargo install --path lsp
# or, once the repo is public:
# cargo install --git https://github.com/stevenbarash/uptick-zed uptick-lsp
```

Make sure `~/.cargo/bin` is on your `PATH`.

### 2. Install the Zed extension

While the extension isn't yet in the Zed registry, install it as a dev extension:

```sh
git clone https://github.com/stevenbarash/uptick-zed
cd uptick-zed
# In Zed: run the command `zed: install dev extension` and point it at this folder.
```

Open a supported manifest (`package.json`, `Cargo.toml`, `pubspec.yaml`, `composer.json`) and versions should appear within a second or two of the first network round-trip.

## Development

```sh
cargo test -p uptick-lsp                               # unit tests
cargo check -p uptick-lsp                              # fast typecheck for inner-loop iteration
cargo build --target wasm32-wasip1 --release           # build the WASM extension (root package)
cargo install --path lsp                               # release-build and install the LSP binary
```

Set `UPTICK_LOG=debug` to see parse/fetch logs on stderr.

## Design notes

- **Spans.** We rely on `toml_edit::Document` for TOML, `jsonc_parser`'s AST for JSON/JSONC, and a hand-rolled line scanner for pubspec YAML. All three produce byte ranges that we convert to LSP UTF-16 positions via a small `LineIndex`.
- **Caching.** Two in-memory `DashMap` caches per server instance with a 1-hour TTL: `VersionCache` keyed on `(ecosystem, name)` for upstream version lookups, and `VulnCache` keyed on `(ecosystem, name, Version)` for OSV scan results. Empty `Vec<Vulnerability>` distinguishes "scanned and clean" from cache miss. Neither cache persists across restarts.
- **Rate limiting.** The server uses a single `reqwest` client with a 10-second timeout and a descriptive `User-Agent` (crates.io requires this). Per-host semaphores cap concurrency: 16 for npm/pub.dev, 8 for Packagist and OSV, 1 for crates.io plus a min-interval gate.
- **Vulnerability parsing.** OSV's `/v1/query` needs concrete `x.y.z` versions but manifests pin ranges. `version::parse_for_scan` strips operators, narrows to the first alternative, replaces `x`/`*` wildcards with `0`, pads to three components, then parses. Bare wildcards / non-semver literals return `None` and skip the OSV call entirely.
- **Prereleases.** Each provider returns `latest_stable` and `latest_any`; today we always prefer stable. A `--include-prereleases` config flag is the natural next step.

## Acknowledgements

Inspired by the [VSCode VersionLens](https://gitlab.com/versionlens/vscode-versionlens) extension by Peter Flannery and contributors. Uptick is an independent Rust/Zed implementation and shares no source code with the original; it's not affiliated with or endorsed by the upstream project.

## License

MIT. See [`LICENSE`](LICENSE).
