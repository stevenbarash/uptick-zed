//! Binary entry point for `uptick-lsp`.
//!
//! Why stderr for logs? LSP uses stdout for JSON-RPC traffic with the editor;
//! writing tracing output there would corrupt the protocol. stderr is ignored
//! by the LSP client but captured by most editors (Zed surfaces it in the
//! "language server logs" panel).

use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Set up tracing. `UPTICK_LOG` follows the usual `env_logger`/`tracing`
    // directive syntax (e.g. `info`, `uptick_lsp=debug`, `warn,hyper=info`).
    // We default to `info` if unset or malformed — enough to see registry
    // lookup warnings without being noisy.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("UPTICK_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // `Backend::new` receives the client handle so it can later push
    // diagnostics and request inlay-hint / code-lens refreshes.
    let (service, socket) = LspService::build(uptick_lsp::server::Backend::new).finish();

    // Blocks until the client sends `exit` (or the pipe closes). No explicit
    // shutdown handling is needed — the runtime drops the service on return.
    Server::new(stdin, stdout, socket).serve(service).await;
}
