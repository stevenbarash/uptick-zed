# Changelog

All notable changes to **Uptick** are documented in this file. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.4.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.4.0
[0.3.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.3.0
[0.2.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.2.0
[0.1.0]: https://github.com/stevenbarash/uptick-zed/releases/tag/v0.1.0
