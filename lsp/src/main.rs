//! Binary entry point for `uptick-lsp`.
//!
//! Responsibilities:
//!   1. Initialise structured logging to stderr (controlled by `UPTICK_LOG`).
//!   2. Wire stdin/stdout into a `tower-lsp` service backed by `server::Backend`.
//!   3. Run the event loop until the client disconnects.
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

    // Raw byte streams the LSP framing codec will sit on top of.
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // `LspService::build` takes a closure that receives the client handle and
    // returns our server state. That lets `Backend::new` stash the client
    // for later `publish_diagnostics` / `inlay_hint_refresh` calls.
    let (service, socket) = LspService::build(uptick_lsp::server::Backend::new).finish();

    // Blocks until the client sends `exit` (or the pipe closes). No explicit
    // shutdown handling is needed — the runtime drops the service on return.
    Server::new(stdin, stdout, socket).serve(service).await;
}
