//! Root crate: the WASM component loaded by Zed itself.
//!
//! This crate is intentionally tiny. Zed's extension API doesn't expose
//! inline decorations (see <https://github.com/zed-industries/zed/issues/49438>),
//! so the real work — parsing manifests, fetching registry metadata, publishing
//! hints/diagnostics/hovers/code-actions — happens in the separate
//! `uptick-lsp` binary. All this module does is tell Zed how to launch that
//! binary when a supported language server is requested.
//!
//! The crate compiles to `wasm32-wasip1` and is loaded by Zed via
//! `register_extension!`. It never runs any network or parser code itself.
//!
//! On first run (or after a version change) it fetches the appropriate
//! prebuilt `uptick-lsp` from the GitHub release matching the extension's own
//! version, verifies the sha256 checksum of the downloaded archive, extracts
//! it, and caches it under the extension working directory.

use std::path::Path;

use zed_extension_api::{
    self as zed, Architecture, Command, DownloadedFileType, GithubReleaseOptions, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, Worktree,
};

/// Zero-sized marker type that implements `zed::Extension`.
struct UptickExtension;

/// Map the host platform to the target triple used in release asset names.
fn platform_triple(os: Os, arch: Architecture) -> Result<&'static str> {
    match (os, arch) {
        (Os::Mac, Architecture::Aarch64) => Ok("aarch64-apple-darwin"),
        (Os::Mac, Architecture::X8664) => Ok("x86_64-apple-darwin"),
        (Os::Linux, Architecture::X8664) => Ok("x86_64-unknown-linux-gnu"),
        (Os::Windows, Architecture::X8664) => Ok("x86_64-pc-windows-msvc"),
        _ => Err("uptick-lsp has no prebuilt binary for this platform. \
             Supported: aarch64-apple-darwin, x86_64-apple-darwin, \
             x86_64-unknown-linux-gnu, x86_64-pc-windows-msvc. \
             Build from source: `cargo install --path lsp`."
            .to_string()),
    }
}

/// SHA-256 digest of `bytes`, returned as a lowercase hex string.
///
/// Implemented inline because wasm32-wasip1 can't link native crypto and we
/// don't want a new dependency for a single call site.
fn sha256_hex(bytes: &[u8]) -> String {
    // Initial hash values: first 32 bits of fractional parts of sqrt(primes 2..19).
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    // Round constants: first 32 bits of fractional parts of cbrt(primes 2..311).
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    // Pad message: append 0x80, zero bytes, then 64-bit big-endian bit length.
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut msg = bytes.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] =
            [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    h.iter().map(|v| format!("{v:08x}")).collect()
}

impl zed::Extension for UptickExtension {
    fn new() -> Self {
        Self
    }

    /// Called by Zed the first time a supported manifest is opened.
    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        // Power-user override: if `uptick-lsp` is already on PATH (e.g. from
        // `cargo install --path lsp`), use it directly and skip the downloader.
        if let Some(path) = worktree.which("uptick-lsp") {
            return Ok(Command {
                command: path,
                args: vec![],
                env: vec![],
            });
        }

        // --- Download path ---

        let (os, arch) = zed::current_platform();
        let triple = platform_triple(os, arch)?;

        let is_windows = triple.contains("windows");
        let (archive_ext, file_type) = if is_windows {
            ("zip", DownloadedFileType::Zip)
        } else {
            ("tar.gz", DownloadedFileType::GzipTar)
        };

        zed::set_language_server_installation_status(
            id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        // Resolve the latest release; require_assets=true so we fail fast if
        // CI hasn't attached binaries yet.
        let release = zed::latest_github_release(
            "stevenbarash/uptick-zed",
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let version = &release.version;

        // Asset names follow the CI contract: tag `vX.Y.Z` → version `X.Y.Z`.
        let archive_name = format!("uptick-lsp-{version}-{triple}.{archive_ext}");
        let sha256_name = format!("{archive_name}.sha256");

        // Cache layout (all paths relative to extension working dir = CWD):
        //   uptick-lsp-<version>/uptick-lsp[.exe]   — the extracted binary
        //   uptick-lsp-<version>/.installed          — marker; content = sha256 hex
        let bin_name = if is_windows {
            "uptick-lsp.exe"
        } else {
            "uptick-lsp"
        };
        let cache_dir = format!("uptick-lsp-{version}");
        let bin_path = format!("{cache_dir}/{bin_name}");
        let marker_path = format!("{cache_dir}/.installed");
        let sha256_tmp = format!("{cache_dir}/.sha256.tmp");
        let archive_tmp = format!("{cache_dir}/.archive.tmp");

        // Ensure cache dir exists so temp files can be written into it.
        // The extension working dir (CWD) is writable via std::fs in wasip1.
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("Failed to create cache dir: {e}"))?;

        // --- Resolve expected sha256 ---
        // Download the tiny sidecar (~80 bytes) unconditionally to get the
        // current expected hex. We compare it against the marker to decide
        // whether to re-download.
        let sha256_asset = release
            .assets
            .iter()
            .find(|a| a.name == sha256_name)
            .ok_or_else(|| {
                format!(
                    "Release {version} is missing asset '{sha256_name}'. \
                 Check that CI ran for this tag."
                )
            })?;

        zed::download_file(
            &sha256_asset.download_url,
            &sha256_tmp,
            DownloadedFileType::Uncompressed,
        )
        .map_err(|e| format!("Failed to download sha256 sidecar: {e}"))?;

        // The sidecar is sha256sum format: `<hex>  <filename>\n`
        let sidecar_bytes = std::fs::read(&sha256_tmp)
            .map_err(|e| format!("Failed to read sha256 sidecar: {e}"))?;
        let expected_hex = std::str::from_utf8(&sidecar_bytes)
            .unwrap_or("")
            .split_whitespace()
            .next()
            .ok_or("sha256 sidecar is empty or malformed")?
            .to_string();

        // Check marker: if it contains the same hex and the binary is present,
        // we can skip the (potentially large) archive download.
        let cached = Path::new(&marker_path).exists()
            && Path::new(&bin_path).exists()
            && std::fs::read_to_string(&marker_path)
                .map(|s| s.trim() == expected_hex)
                .unwrap_or(false);

        if !cached {
            zed::set_language_server_installation_status(
                id,
                &LanguageServerInstallationStatus::Downloading,
            );

            let archive_asset = release
                .assets
                .iter()
                .find(|a| a.name == archive_name)
                .ok_or_else(|| format!("Release {version} is missing asset '{archive_name}'."))?;

            // Download the archive as raw bytes first so we can verify its
            // sha256 before extraction. The sidecar checksums the archive
            // (not the extracted binary), matching standard CI practice.
            zed::download_file(
                &archive_asset.download_url,
                &archive_tmp,
                DownloadedFileType::Uncompressed,
            )
            .map_err(|e| {
                zed::set_language_server_installation_status(
                    id,
                    &LanguageServerInstallationStatus::Failed(format!(
                        "Archive download failed: {e}"
                    )),
                );
                format!("Archive download failed: {e}")
            })?;

            // Verify checksum against the sidecar we just downloaded.
            let archive_bytes = std::fs::read(&archive_tmp)
                .map_err(|e| format!("Failed to read downloaded archive: {e}"))?;
            let actual_hex = sha256_hex(&archive_bytes);
            if actual_hex != expected_hex {
                let msg = format!(
                    "SHA-256 mismatch for {archive_name}: \
                     expected {expected_hex}, got {actual_hex}"
                );
                zed::set_language_server_installation_status(
                    id,
                    &LanguageServerInstallationStatus::Failed(msg.clone()),
                );
                return Err(msg);
            }

            // Checksum verified — now re-download with the correct format so
            // Zed's host extracts the archive into the cache dir.
            //
            // Note: this means we download the archive twice. The alternative
            // (extracting manually in WASM) would require bundling a tar/zip
            // decoder, which adds far more code than a second network round-trip.
            zed::download_file(&archive_asset.download_url, &cache_dir, file_type).map_err(
                |e| {
                    zed::set_language_server_installation_status(
                        id,
                        &LanguageServerInstallationStatus::Failed(format!(
                            "Archive extraction failed: {e}"
                        )),
                    );
                    format!("Archive extraction failed: {e}")
                },
            )?;

            // Make the binary executable on Unix.
            if !is_windows {
                zed::make_file_executable(&bin_path)
                    .map_err(|e| format!("make_file_executable failed: {e}"))?;
            }

            // Write the marker so subsequent runs skip re-downloading.
            std::fs::write(&marker_path, &expected_hex)
                .map_err(|e| format!("Failed to write install marker: {e}"))?;

            // Drop the verification temp files now that extraction succeeded;
            // the archive copy is large (tens of MB) and otherwise lingers.
            let _ = std::fs::remove_file(&archive_tmp);
            let _ = std::fs::remove_file(&sha256_tmp);

            // Purge cache dirs for older versions to avoid disk bloat.
            if let Ok(entries) = std::fs::read_dir(".") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("uptick-lsp-") && name != cache_dir {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
        }

        // None = installation status cleared; Zed treats this as "ready".
        zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::None);

        Ok(Command {
            command: bin_path,
            args: vec![],
            env: vec![],
        })
    }
}

// Registers the extension with Zed's host. This macro expands into the
// `extern "C"` symbols Zed expects, wiring `UptickExtension::new()` to the
// component's constructor.
zed::register_extension!(UptickExtension);
