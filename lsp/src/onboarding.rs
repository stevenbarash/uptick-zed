//! First-run welcome flow. Fires once per user (state flag in an OS-
//! appropriate config dir) on `initialized`. Surface is a single
//! `window/showMessageRequest` with two actions: open the README, or
//! dismiss. Failures (missing $HOME, fs errors, client refusal) all
//! silently fall through — we never want a UI nicety to crash the LSP.

use std::path::PathBuf;
use std::time::Duration;

use tokio::fs;
use tower_lsp::lsp_types::{
    request::{ShowDocument, ShowMessageRequest},
    MessageActionItem, MessageType, ShowDocumentParams, ShowMessageRequestParams,
};
use tower_lsp::Client as LspClient;
use tracing::debug;
use url::Url;

const WELCOME_FLAG_FILE: &str = "welcomed-v1";
const README_URL: &str = "https://github.com/stevenbarash/uptick-zed#first-run";
const WELCOME_MESSAGE: &str = "Uptick is active. Tip: set `\"code_lens\": \"on\"` in Zed settings to see one-click `↑ Bump to X.Y.Z` lenses above each dependency.";
/// Cap on how long we wait for the user to click an action. Without this
/// a client that swallows the request (or a user who walks away) would
/// keep the spawned task alive — and worse, the welcome flag never
/// persists, so the next session re-toasts.
const WELCOME_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// Returns the OS-appropriate state directory for uptick, or `None`
/// when we can't determine one (e.g. `$HOME` unset on a stripped CI
/// runner). Callers must treat `None` as "skip the welcome".
fn state_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        let mut p = PathBuf::from(home);
        p.push("Library/Application Support/uptick");
        Some(p)
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA"))?;
        let mut p = PathBuf::from(base);
        p.push("uptick");
        Some(p)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(base) = std::env::var_os("XDG_STATE_HOME") {
            let mut p = PathBuf::from(base);
            p.push("uptick");
            return Some(p);
        }
        let home = std::env::var_os("HOME")?;
        let mut p = PathBuf::from(home);
        p.push(".local/state/uptick");
        Some(p)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        None
    }
}

fn flag_path() -> Option<PathBuf> {
    Some(state_dir()?.join(WELCOME_FLAG_FILE))
}

/// Returns `true` if the flag exists or we couldn't determine where it
/// would live — in either case, we must not surface the toast.
async fn already_welcomed() -> bool {
    let Some(p) = flag_path() else {
        return true;
    };
    fs::try_exists(&p).await.unwrap_or(true)
}

async fn mark_welcomed() {
    let Some(p) = flag_path() else {
        return;
    };
    let Some(dir) = p.parent() else {
        return;
    };
    if fs::create_dir_all(dir).await.is_err() {
        return;
    }
    let _ = fs::write(&p, b"1").await;
}

/// Fire-and-forget. Sends the welcome toast if the user hasn't been
/// welcomed yet, persists the flag regardless of what they click, and
/// honours an "Open README" choice via `window/showDocument`. Spawned
/// from `initialized` so server startup isn't gated on the round trip.
pub async fn maybe_send_welcome(client: LspClient) {
    if already_welcomed().await {
        return;
    }
    let open = MessageActionItem {
        title: "Open README".to_string(),
        properties: std::collections::HashMap::new(),
    };
    let dismiss = MessageActionItem {
        title: "Dismiss".to_string(),
        properties: std::collections::HashMap::new(),
    };
    // Persist the flag *before* dispatching the request. Tradeoff:
    // at-most-once delivery (the user may miss the toast if they quit
    // mid-request) instead of at-least-once-until-clicked (the toast
    // re-fires every cold start until they actually click something).
    // The toast is purely informational, so the cost of a missed
    // delivery is lower than the cost of nagging.
    mark_welcomed().await;
    // Bounded wait so the spawned task can't outlive the session if
    // the client swallows or never answers the request. The outer
    // `Result` is the timeout outcome; the inner is the LSP request
    // outcome.
    let resp = tokio::time::timeout(
        WELCOME_RESPONSE_TIMEOUT,
        client.send_request::<ShowMessageRequest>(ShowMessageRequestParams {
            typ: MessageType::INFO,
            message: WELCOME_MESSAGE.to_string(),
            actions: Some(vec![open.clone(), dismiss]),
        }),
    )
    .await;
    if let Ok(Ok(Some(picked))) = resp {
        if picked.title == open.title {
            if let Ok(uri) = Url::parse(README_URL) {
                let _ = client
                    .send_request::<ShowDocument>(ShowDocumentParams {
                        uri,
                        external: Some(true),
                        take_focus: Some(true),
                        selection: None,
                    })
                    .await;
            }
        }
    }
    debug!("welcome flow done");
}
