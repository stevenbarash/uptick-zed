# uptick-zed

[![CI](https://github.com/stevenbarash/versionlens-zed/actions/workflows/ci.yml/badge.svg)](https://github.com/stevenbarash/versionlens-zed/actions/workflows/ci.yml)

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

## Roadmap

This is **v0.1** — a minimum-viable port with the four largest ecosystems covered end-to-end. The items below are rough plans, not a schedule; contributions are welcome.

### v0.1 — shipped today

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

### v0.2 — next up

**More ecosystems**, widest ROI first:

- Maven (`pom.xml`) — XML parsing plus `<parent>` / BOM / property interpolation.
- .NET (`*.csproj`, `*.fsproj`, `Directory.Packages.props`) — MSBuild XML with imports and floating-version notation.
- Go (`go.mod`) — `proxy.golang.org`.
- Python (`pyproject.toml`, `requirements.txt`) — PyPI JSON API.
- NuGet as a stand-alone target if it isn't already covered by .NET.

**Distribution**

- Prebuilt `uptick-lsp` binaries attached to GitHub releases, auto-downloaded by the WASM extension on first run (the `zed-dependi` pattern). Removes the manual `cargo install` step.
- Publish to Zed's extension registry once stable.

### v0.3+ — nice to have

- `--include-prereleases` config flag. Each provider already returns `latest_stable` and `latest_any`; we just always prefer stable today.
- Persistent on-disk cache across LSP restarts (e.g. `~/.cache/uptick/`).
- Workspace command: "Bump all outdated".
- Deprecation warnings — surface npm's `deprecated` field from registry responses.
- Security advisories — integrate GHSA / [osv.dev](https://osv.dev) on affected deps.
- Private registry / auth support — read `.npmrc`, Cargo `[registries]`, per-project tokens.
- Per-ecosystem TTL tuning (npm moves fast, crates.io is slower).
- Workspace-aware Cargo support — one root `Cargo.toml` covering all workspace members' deps without needing each opened individually.
- Optional `uptick.toml` per-project config.

### Known limitations in v0.1

- Pubspec entries with `git:` / `path:` / `hosted:` specs are skipped — no single upstream version to compare against.
- Private registries return 401/403; no credential support yet.
- Zed's extension API doesn't expose inline decorations directly ([zed#49438](https://github.com/zed-industries/zed/issues/49438)); we use LSP inlay hints, which render at the end of the line rather than above it as a clickable lens. The "bump" UX is `cmd-.` (code actions), not a click.
- The LSP binary is installed manually via `cargo install --path lsp` until binary distribution lands in v0.2.

## How it's built

Zed's extension API doesn't (yet — see [zed#49438](https://github.com/zed-industries/zed/issues/49438)) expose inline decorations, but it renders everything a language server publishes. This repo therefore ships two pieces:

- **Root crate (`src/lib.rs`)** — a thin WASM extension (the file Zed loads). It implements `language_server_command()` and launches the LSP binary.
- **`lsp/`** — a standalone Rust LSP (`uptick-lsp`) that parses the manifest, hits the registry, caches results for an hour, and publishes inlay hints, diagnostics, code actions, and hovers.

That separation means `uptick-lsp` is reusable from any LSP-aware editor (Neovim, Helix, …), not just Zed.

## Installation

### 1. Build and install the LSP binary

```sh
cargo install --path lsp
# or, once the repo is public:
# cargo install --git https://github.com/stevenbarash/versionlens-zed uptick-lsp
```

Make sure `~/.cargo/bin` is on your `PATH`.

### 2. Install the Zed extension

While the extension isn't yet in the Zed registry, install it as a dev extension:

```sh
git clone https://github.com/stevenbarash/versionlens-zed
cd versionlens-zed
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
- **Caching.** One in-memory `DashMap<(ecosystem, name), (VersionInfo, Instant)>` per server instance with a 1-hour TTL. The cache is never persisted, so a restart re-queries.
- **Rate limiting.** The server uses a single `reqwest` client with a 10-second timeout and a descriptive `User-Agent` (crates.io requires this). Bursts on `didOpen` are implicitly deduped because the cache key is stable.
- **Prereleases.** Each provider returns `latest_stable` and `latest_any`; today we always prefer stable. A `--include-prereleases` config flag is the natural next step.

## License

MIT. See [`LICENSE`](LICENSE).

## Acknowledgements

This project is an independent Rust/Zed port **inspired by** the [VSCode VersionLens](https://gitlab.com/versionlens/vscode-versionlens) extension (ISC License, © Peter Flannery and Contributors). It shares **no source code** with the original — every parser, registry provider, and server module here is original work written from scratch.

This project is **not affiliated with or endorsed by** the upstream VersionLens project. See [`NOTICES.md`](NOTICES.md) for the upstream license text.
