# Changelog

All notable changes to **Uptick** are documented in this file. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.1] - 2026-05-28

### Added
- `$/progress` reports during the initial resolve burst. Bursts of
  `PROGRESS_THRESHOLD` (5) or more uncached packages publish a
  `window/workDoneProgress` "Uptick: resolving N packages…" indicator
  so first-open on large manifests no longer looks like the server
  did nothing for a second.
- Line-0 banner diagnostic ("Uptick: no registry reachable — check
  network/proxy") published when an entire registry-fetch burst fails
  end-to-end. Cleared automatically on the next burst that succeeds.
  Prevents the "no inlay hints, no error, no idea why" experience for
  users behind a corporate proxy / offline / DNS-broken.
- `DocState.network_failure` field plus `fingerprint` integration so the
  banner flips invalidate the per-doc dedup and actually reach the
  editor.

### Changed
- `install.sh` PATH-not-set advisory now detects the user's `$SHELL`
  and prints a single copy-pasteable line (zsh → `~/.zshrc`; bash →
  `~/.bash_profile` on macOS / `~/.bashrc` on Linux; fish →
  `fish_add_path`).
- `install.sh` ends with a "Next steps" block that includes a one-line
  smoke test (`echo … > /tmp/uptick-smoke.json && zed …`) so new users
  can immediately verify the install works end-to-end.

## [0.5.0] - 2026-05-28

### Added
- LSP `textDocument/documentLink` provider. Clickable links are now
  surfaced inline on every dep without requiring a hover: package names
  link to the registry page (npm, crates.io, pub.dev, Packagist), and
  vulnerable version literals link to the corresponding `osv.dev`
  advisory. Cached canonical URLs (when available) take precedence over
  the deterministic template, so first-paint still produces working
  links before any registry round-trip lands.
- LSP `textDocument/codeLens` provider with two server-defined commands:
  `uptick.bump` (applies the same edit as the `Bump` quickfix via
  `workspace/applyEdit`) and `uptick.open` (asks the client to surface
  a URL via `window/showDocument`). Each outdated entry now shows a
  `↑ Bump to X.Y.Z` lens above its line; vulnerable entries show
  `⛔ N advisor{y|ies} — view on osv.dev` linking to the
  highest-severity advisory.

### Changed
- Extracted `should_bump(literal, latest)` so the diagnostic, code
  action, and code lens surfaces share one definition of "out-of-date".
  The code action previously skipped only on `satisfies(...)`; it now
  also respects the `parse_literal >= latest` downgrade guard the other
  surfaces already had, so a manual pin newer than the cached `latest`
  no longer offers a no-op (or downward) bump.
- `execute_command` now logs every failure path (missing args,
  malformed args, client-rejected `applyEdit`, refused/failed
  `showDocument`) instead of silently dropping the result.

## [0.4.0] - 2026-05-01

### Added
- Hover augmentation for vulnerable dependencies. Hover on a known-vulnerable
  literal now renders a per-advisory block with severity badge
  (`CRITICAL` / `HIGH` / `MEDIUM` / `LOW` / `UNKNOWN`), GHSA/CVE id, CVSS base
  score, summary, full CVSS vector, and a link to the corresponding
  `osv.dev/vulnerability/<id>` page.
- `VulnDetail` is plumbed through `osv::query_detail`, `fetch_vuln_details`,
  and `DetailCache` so the CVSS vector is cached alongside the score and
  available to hover without an extra round-trip.

### Changed
- `DetailCache` value type is now `VulnDetail { score, vector }` instead of
  `Option<f32>`. A cache hit returns `Some(VulnDetail)` with possibly-`None`
  inner fields; a miss is `None`.
- `Vulnerability` gains a `vector: Option<String>` field populated from the
  matched `severity[]` entry in the OSV detail response.

## [0.3.0] - 2026-04-29

### Added
- Per-advisory CVSS base score fetch via OSV's `/v1/vulns/{id}` endpoint.
- `DetailCache` (24 h TTL) for cached CVSS scores, separate from the 1 h
  `VulnCache` keyed on `(kind, name, version)`.
- CVSS-aligned diagnostic severity: 9.0–10.0 / 7.0–8.9 → `Error`, 4.0–6.9 →
  `Warning`, 0.1–3.9 → `Information`. Unknown severity stays `Warning`.

## [0.2.0] - 2026-04-23

### Added
- Vulnerability scanning against [osv.dev](https://osv.dev) for npm,
  crates.io, Pub, and Packagist ecosystems.
- LSP diagnostics surfaced with the GHSA / CVE id as `Diagnostic.code`.
- `parse_for_scan` lenient version normaliser so range literals like `^1.2`,
  `~1`, or `1.x` are queried as a concrete floor version.

## [0.1.0] - 2026-04-12

### Added
- Initial release: inline version hints, hover tooltips, and a
  `Bump to X.Y.Z` code action that preserves the user's range operator.
- Manifest support for `package.json`, `Cargo.toml`, `pubspec.yaml`, and
  `composer.json`.

[0.5.1]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.5.1
[0.5.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.5.0
[0.4.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.4.0
[0.3.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.3.0
[0.2.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.2.0
[0.1.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.1.0
