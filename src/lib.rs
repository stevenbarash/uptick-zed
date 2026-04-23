//! Root crate: the WASM component loaded by Zed itself.
//!
//! This crate is intentionally tiny. Zed's extension API doesn't expose
//! inline decorations (see https://github.com/zed-industries/zed/issues/49438),
//! so the real work — parsing manifests, fetching registry metadata, publishing
//! hints/diagnostics/hovers/code-actions — happens in the separate
//! `uptick-lsp` binary. All this module does is tell Zed how to launch that
//! binary when a supported language server is requested.
//!
//! The crate compiles to `wasm32-wasip1` and is loaded by Zed via
//! `register_extension!`. It never runs any network or parser code itself.

use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

/// Zero-sized marker type that implements `zed::Extension`.
///
/// Zed instantiates one of these per extension load via `new()`. We hold no
/// state — every LSP request starts a fresh `uptick-lsp` process which owns
/// its own cache, connections, etc.
struct UptickExtension;

impl zed::Extension for UptickExtension {
    fn new() -> Self {
        // Nothing to initialise — construction is effectively free.
        Self
    }

    /// Called by Zed the first time a supported manifest is opened. We return
    /// a `Command` describing how to spawn the language server; Zed handles
    /// the stdio plumbing and restart logic from there.
    fn language_server_command(
        &mut self,
        _id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        // Look up `uptick-lsp` on the worktree's PATH. This is the same PATH
        // the user sees in their shell, so if `cargo install --path lsp` put
        // the binary on `~/.cargo/bin` and that's on PATH, it will be found.
        //
        // We intentionally do *not* bundle the LSP binary into the WASM
        // component: the WASM sandbox can't make outbound HTTP calls, and
        // shipping platform-specific binaries inside a WASM file would mean
        // every user downloads every target. Binary distribution via GitHub
        // releases is tracked in the v0.2 roadmap.
        let path = worktree.which("uptick-lsp").ok_or_else(|| {
            // Surface a clear, actionable error inside Zed's UI if the user
            // installed the extension but not the binary.
            "uptick-lsp was not found on PATH. \
             Install it with `cargo install --path lsp` from the extension repo, \
             or `cargo install --git https://github.com/stevenbarash/uptick-zed uptick-lsp`."
                .to_string()
        })?;

        // No arguments, no environment overrides — the LSP is driven entirely
        // through stdin/stdout using the JSON-RPC protocol. Users who want
        // debug logs can set `UPTICK_LOG=debug` in their own shell environment.
        Ok(Command {
            command: path,
            args: vec![],
            env: vec![],
        })
    }
}

// Registers the extension with Zed's host. This macro expands into the
// `extern "C"` symbols Zed expects, wiring `UptickExtension::new()` to the
// component's constructor.
zed::register_extension!(UptickExtension);
