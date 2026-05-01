# Uptick

[![CI](https://github.com/stevenbarash/uptick-zed/actions/workflows/ci.yml/badge.svg)](https://github.com/stevenbarash/uptick-zed/actions/workflows/ci.yml)

**Know your dependencies at a glance.** Uptick is a [Zed](https://zed.dev) extension that shows the latest version of every dependency in your manifest — and flags the ones with known vulnerabilities, by severity — without leaving your editor.

```toml
[dependencies]
serde   = "1.0.100"   → 1.0.228
tokio   = { version = "1.35" }   ✓ 1.35.1
```

```jsonc
{
  "dependencies": {
    "react":  "^18.2.0",                   → 18.3.1
    "lodash": "4.17.15"  // ⛔ GHSA-35jh-r3h4-6jhm: Command Injection in lodash
  }
}
```

`✓` already up to date. `→` newer version available. `⛔` known vulnerability — colored by severity.

---

## What you get

- **Latest versions, inline.** Every dependency is annotated with the current upstream version. Out-of-date ones get a one-keystroke `Bump to X.Y.Z` code action that preserves your `^`, `~`, `>=` operator.
- **Vulnerability scanning.** Each pinned version is checked against [osv.dev](https://osv.dev). Known-vulnerable versions surface as LSP diagnostics with the GHSA or CVE ID as the code.
- **Severity that means something.** Each advisory's CVSS base score is fetched separately and mapped to a real editor severity — Critical and High advisories appear red, not buried in a sea of yellow.

| CVSS base score | Severity in editor |
|---|---|
| 9.0 – 10.0 (Critical) | `Error` |
| 7.0 – 8.9 (High) | `Error` |
| 4.0 – 6.9 (Medium) | `Warning` |
| 0.1 – 3.9 (Low) | `Information` |
| Unknown | `Warning` |

`lodash@4.17.15` produces six diagnostics: three red (Command Injection, Prototype Pollution, Code Injection) and three yellow (ReDoS and prototype-pollution variants). At a glance you know what to fix first.

---

## Supported manifests

| Manifest | Registry | Vulnerability source |
|---|---|---|
| `package.json` | [npm](https://registry.npmjs.org) | osv.dev (npm ecosystem) |
| `Cargo.toml` | [crates.io](https://crates.io) | osv.dev (crates.io ecosystem) |
| `pubspec.yaml` | [pub.dev](https://pub.dev) | osv.dev (Pub ecosystem) |
| `composer.json` | [Packagist](https://packagist.org) | osv.dev (Packagist ecosystem) |

More ecosystems on the [roadmap](#roadmap).

---

## Install

Uptick ships as two pieces: the LSP server (`uptick-lsp`) and a thin Zed extension that launches it.

**Quickest path — one-liner installer**

The installer detects your OS and architecture, downloads the matching binary from the latest release, verifies its `.sha256`, drops it under `~/.local/bin`, and warns if that's not on your `PATH`. Pass `--clone` to also clone this repo so you can install the Zed dev extension from it.

```sh
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.sh | bash

# Same, plus clone the repo to ~/.local/share/uptick-zed
curl -fsSL https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.sh | bash -s -- --clone
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.ps1 | iex
```

After that, in Zed run `zed: install dev extension` and point it at the cloned repo (or `git clone` it manually).

**Add the Zed extension by hand**

```sh
git clone https://github.com/stevenbarash/uptick-zed
cd uptick-zed
# In Zed: run `zed: install dev extension`, point it at this folder.
```

On first use, the extension downloads a prebuilt `uptick-lsp` binary from the latest non-prerelease GitHub release, verifies its `.sha256` sidecar, extracts it into Zed's extension cache, and launches it from there. Open any supported manifest. Hints appear within a second of the first network round-trip.

**Manual LSP binary install**

If you want to use a release-candidate binary, run the LSP outside Zed, or skip both the installer and the extension downloader, download the archive for your platform from [Releases](https://github.com/stevenbarash/uptick-zed/releases):

| Platform | Asset |
|---|---|
| Apple Silicon Mac | `uptick-lsp-<version>-aarch64-apple-darwin.tar.gz` |
| Intel Mac | `uptick-lsp-<version>-x86_64-apple-darwin.tar.gz` |
| Linux x64 | `uptick-lsp-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Windows x64 | `uptick-lsp-<version>-x86_64-pc-windows-msvc.zip` |

Each archive has a matching `.sha256` file. Verify the download before installing:

```sh
archive=uptick-lsp-<version>-<target>.tar.gz
shasum -a 256 -c "$archive.sha256"
tar -xzf "$archive"
mkdir -p ~/.local/bin
install -m 0755 uptick-lsp ~/.local/bin/uptick-lsp
```

Use the asset version without the leading `v`; for example, tag `v0.3.1-rc2` publishes `uptick-lsp-0.3.1-rc2-x86_64-apple-darwin.tar.gz`.

On Windows, unzip the `.zip`, then put `uptick-lsp.exe` somewhere on your `PATH`.

If `uptick-lsp` is already on `PATH`, the Zed extension uses that binary and does not download another copy.

> Set `UPTICK_LOG=debug` to see parse and fetch logs on stderr.

---

## What's new

| Release | Highlight |
|---|---|
| [**v0.4.0**](https://github.com/stevenbarash/uptick-zed/releases/tag/v0.4.0) | Hover augmentation for vulnerabilities — severity badge, GHSA/CVE id, summary, CVSS vector, and an osv.dev link, rendered inline next to the registry link. |
| [**v0.3.0**](https://github.com/stevenbarash/uptick-zed/releases/tag/v0.3.0) | CVSS-aligned severity. Critical/High → `Error`, Medium → `Warning`, Low → `Information`. |
| [**v0.2.0**](https://github.com/stevenbarash/uptick-zed/releases/tag/v0.2.0) | Vulnerability scanning. Known-vulnerable pins surface as warnings with GHSA/CVE codes. |
| [**v0.1.0**](https://github.com/stevenbarash/uptick-zed/releases/tag/v0.1.0) | Inline version hints, hover tooltips, "Bump to X.Y.Z" code actions. |

---

## Roadmap

**Coming next**

- Maven (`pom.xml`), .NET (`*.csproj`, `Directory.Packages.props`), Go (`go.mod`), Python (`pyproject.toml`).
- Lockfile-aware vulnerability scanning (read `package-lock.json`, `Cargo.lock`).

**Someday**

- `--include-prereleases` opt-in. Persistent on-disk cache. Workspace command "Bump all outdated". Deprecation warnings. Additional vulnerability sources beyond OSV. Private registry / auth support.

---

## Under the hood

Zed's extension API doesn't (yet — [zed#49438](https://github.com/zed-industries/zed/issues/49438)) expose inline decorations directly. Uptick works around that by shipping a real LSP server and letting Zed render its diagnostics, hovers, and inlay hints. The same server works with Neovim, Helix, or any other LSP-aware editor.

Two crates live here:

- **`src/lib.rs`** — the WASM extension Zed loads. It does one thing: launches the LSP binary.
- **`lsp/`** — the standalone Rust server. Parses the manifest, queries each registry, scans osv.dev for vulnerabilities, and publishes results.

### Design choices worth knowing

- **Three caches.** `VersionCache` (1h TTL) for upstream version lookups, `VulnCache` (1h TTL) for OSV scan results, `DetailCache` (24h TTL) for per-advisory CVSS scores. Advisories are essentially immutable once published, so the longer Detail TTL holds. None persist across restarts.
- **Per-host concurrency.** A single shared `reqwest` client with per-registry semaphores: 16 for npm and pub.dev, 8 for Packagist and OSV, 1 for crates.io plus a strict 1 req/sec rate-limit gate.
- **Lenient version parsing for OSV.** Real npm manifests pin ranges (`^1.2`, `~1`, `1.x`, `>=1.0 <2.0`). `parse_for_scan` normalizes them to a concrete floor version so OSV actually returns matches — closing a coverage gap in upstream tools that send `1.2` to OSV and silently get nothing back.
- **Severity from CVSS, not heuristics.** Each advisory's CVSS_V3 vector is parsed via the [`cvss`](https://crates.io/crates/cvss) crate (RustSec org). When a vector isn't published, the GHSA `database_specific.severity` text bucket is used as fallback.
- **Quiet typing.** A 250 ms debounce coalesces keystrokes; a fingerprint dedup skips diagnostic publishes when nothing visible changed.

---

## Known limitations

- Pubspec entries with `git:`, `path:`, or `hosted:` specs are skipped — no single upstream version to compare.
- Private registries return 401/403; no credential support yet.
- Zed renders inlay hints at end-of-line, not above as a clickable lens. The bump UX is `cmd-.` (code actions), not a click.
- Vulnerability scans use the manifest literal, not the lockfile-resolved install version. Pinning `^1.0.0` of a package whose latest 1.x is vulnerable will not flag — `1.0.0` is what gets scanned.
- CVSS_V4 vectors aren't parsed yet. Records carrying only V4 fall through to the text-bucket fallback if present, otherwise no severity.

---

## Develop

```sh
cargo test  -p uptick-lsp                              # unit tests
cargo check -p uptick-lsp                              # fast typecheck
cargo build --target wasm32-wasip1 --release           # build the WASM extension
cargo install --path lsp                               # install the LSP binary
```

---

## Acknowledgements

Inspired by [VSCode VersionLens](https://gitlab.com/versionlens/vscode-versionlens) by Peter Flannery and contributors. Uptick is an independent Rust + Zed implementation and shares no source code with the original; it isn't affiliated with or endorsed by the upstream project.

## License

MIT. See [`LICENSE`](LICENSE).
