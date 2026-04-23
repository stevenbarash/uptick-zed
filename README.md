# uptick-zed

A [Zed](https://zed.dev) extension that shows the latest available version of each dependency in package manifests, inline, with one-click bumps that preserve your semver operator.

Supports:

| Manifest          | Registry                  |
| ----------------- | ------------------------- |
| `package.json`    | [registry.npmjs.org](https://registry.npmjs.org) |
| `Cargo.toml`      | [crates.io](https://crates.io) |
| `pubspec.yaml`    | [pub.dev](https://pub.dev) |
| `composer.json`   | [Packagist](https://packagist.org) |

Maven, NuGet, Dub, and jspm are not implemented yet — PRs welcome.

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

- **Spans.** We rely on `toml_edit::ImDocument` for TOML, `jsonc_parser`'s AST for JSON/JSONC, and a hand-rolled line scanner for pubspec YAML. All three produce byte ranges that we convert to LSP UTF-16 positions via a small `LineIndex`.
- **Caching.** One in-memory `DashMap<(ecosystem, name), (VersionInfo, Instant)>` per server instance with a 1-hour TTL. The cache is never persisted, so a restart re-queries.
- **Rate limiting.** The server uses a single `reqwest` client with a 10-second timeout and a descriptive `User-Agent` (crates.io requires this). Bursts on `didOpen` are implicitly deduped because the cache key is stable.
- **Prereleases.** Each provider returns `latest_stable` and `latest_any`; today we always prefer stable. A `--include-prereleases` config flag is the natural next step.

## Acknowledgements

Inspired by the [VSCode VersionLens](https://gitlab.com/versionlens/vscode-versionlens) extension by Peter Flannery and contributors. Uptick is an independent Rust/Zed implementation and shares no source code with the original; it's not affiliated with or endorsed by the upstream project.

## License

MIT. See [`LICENSE`](LICENSE).
