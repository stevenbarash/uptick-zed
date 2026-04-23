//! The LSP server itself — the `tower-lsp` `LanguageServer` impl that
//! glues parsers, providers, cache, and the editor together.
//!
//! # Lifecycle
//!
//! 1. `initialize` — advertise capabilities.
//! 2. `did_open` — parse the buffer, fetch any uncached registry data, then
//!    publish diagnostics + refresh hints.
//! 3. `did_change` — same as open, but with a 250 ms debounce so a burst of
//!    keystrokes collapses into one round-trip.
//! 4. `inlay_hint` — cheap: read from `DocState` and emit LSP hints.
//! 5. `hover` — cheap: read from `DocState`, render markdown.
//! 6. `code_action` — cheap: produce `Bump to X.Y.Z` edits for entries whose
//!    latest is ahead of their current literal.
//! 7. `did_close` — abort any pending resolve, drop the doc state, and clear
//!    diagnostics.
//!
//! # Concurrency model
//!
//! - One `Arc<DashMap>` per kind of state (docs, pushed fingerprints,
//!   pending tasks). Every LSP handler is `async` and non-blocking.
//! - Network fetches fan out in parallel across entries, capped by
//!   per-host semaphores in [`crate::providers`].
//! - A new `did_change` aborts the prior pending resolve for that URI,
//!   so we never spawn more than one resolver per buffer at a time.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use reqwest::Client;
use semver::Version;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client as LspClient, LanguageServer};
use tracing::{debug, warn};

use crate::cache::VersionCache;
use crate::manifest::{ManifestKind, RawEntry};
use crate::parsers;
use crate::providers;
use crate::version;
use crate::vulnerabilities::{cache::VulnCache, Vulnerability};

/// How long a fetched version stays usable before we re-query the registry.
/// A one-hour window balances freshness against politeness across all four
/// registries.
const CACHE_TTL: Duration = Duration::from_secs(3600);

/// Debounce window applied to `did_change`. Short enough that users see
/// updates within a pause in typing, long enough that holding a key down
/// doesn't fire a fetch every keystroke.
const DEBOUNCE: Duration = Duration::from_millis(250);

const SERVER_NAME: &str = "uptick-lsp";
const DIAGNOSTIC_SOURCE: &str = "uptick";

/// User-Agent string. crates.io *requires* a descriptive UA — the default
/// reqwest UA gets 403'd — and the other registries appreciate it too.
/// Built at compile time from the Cargo package version.
const USER_AGENT: &str = concat!(
    "uptick-lsp/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/stevenbarash/uptick-zed)"
);

/// A `RawEntry` augmented with the latest version we've resolved so far.
/// `latest: None` means the entry is still being fetched (or the fetch
/// failed — we retry lazily on next reparse).
#[derive(Debug, Clone)]
struct Annotated {
    entry: RawEntry,
    latest: Option<Version>,
    /// Vulnerabilities known to affect the parsed `entry.version_literal`.
    /// Empty means either "no scan yet" or "scan completed and clean"; the
    /// cache distinguishes these two cases.
    vulns: Vec<Vulnerability>,
}

/// Immutable snapshot of one document. We always replace the `Arc` wholesale
/// rather than mutate in place, so LSP handlers can cheaply clone the `Arc`
/// and operate on a stable view without holding any locks.
#[derive(Debug)]
struct DocState {
    kind: ManifestKind,
    entries: Vec<Annotated>,
}

/// The `tower-lsp` service state. One `Backend` exists for the lifetime of
/// the process, shared across all `async` handler invocations.
///
/// Every field is `Arc`-wrapped (either directly or via `DashMap`) so the
/// spawn-and-forget resolve tasks in `schedule_resolve` can hold their own
/// references independent of the handler that created them.
pub struct Backend {
    /// Client handle for pushing diagnostics and triggering inlay-hint /
    /// code-lens refreshes.
    client: LspClient,
    /// Shared HTTP client for registry calls. `reqwest::Client` is
    /// connection-pooled internally, so we want exactly one.
    http: Client,
    cache: Arc<VersionCache>,
    vuln_cache: Arc<VulnCache>,
    /// Current parsed state of every open document. Keyed by URI.
    docs: Arc<DashMap<Url, Arc<DocState>>>,
    /// Last-pushed fingerprint per doc. Skips the refresh/diagnostics storm
    /// when a reparse produced no user-visible changes (common while typing
    /// inside whitespace or comments).
    pushed: Arc<DashMap<Url, u64>>,
    /// In-flight debounced resolve tasks, keyed by document. A new `did_change`
    /// aborts the prior task so we only do one network round-trip per burst.
    pending: Arc<DashMap<Url, JoinHandle<()>>>,
}

impl Backend {
    pub fn new(client: LspClient) -> Self {
        // Single shared HTTP client. Pool connections, set the required UA,
        // and cap each request at 10 seconds so a hung registry doesn't
        // stall the server forever.
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            client,
            http,
            cache: Arc::new(VersionCache::new(CACHE_TTL)),
            vuln_cache: Arc::new(VulnCache::new(CACHE_TTL)),
            docs: Arc::new(DashMap::new()),
            pushed: Arc::new(DashMap::new()),
            pending: Arc::new(DashMap::new()),
        }
    }

    /// Parse text into entries and store. Returns the new state's `ManifestKind`
    /// if the document is one we handle, or `None` for unsupported files
    /// (in which case the caller should leave the buffer alone).
    ///
    /// Pre-populates `latest` from the cache where possible so we don't have
    /// to wait for the network round-trip before showing hints on already-
    /// fetched packages.
    fn reparse(&self, uri: &Url, text: &str) -> Option<ManifestKind> {
        let kind = ManifestKind::from_url(uri)?;
        let entries = parsers::parse(kind, text)
            .into_iter()
            .map(|entry| {
                // Prefer a stable release, fall back to any (which currently
                // is only interesting once prerelease opt-in lands).
                let latest = self
                    .cache
                    .get(kind, &entry.name)
                    .and_then(|info| info.latest_stable.or(info.latest_any));
                // Look up cached vulns via the lenient parser, same shape
                // the scanner uses. A `None` parse means "not scannable";
                // an empty vec means "scan completed and clean".
                let vulns = crate::version::parse_for_scan(&entry.version_literal)
                    .and_then(|v| self.vuln_cache.get(kind, &entry.name, &v))
                    .unwrap_or_default();
                Annotated {
                    entry,
                    latest,
                    vulns,
                }
            })
            .collect();
        self.docs
            .insert(uri.clone(), Arc::new(DocState { kind, entries }));
        Some(kind)
    }

    /// Schedule registry lookups for entries without a cached latest, after
    /// `delay` (0 for `did_open`, `DEBOUNCE` for `did_change`). A prior
    /// pending task for the same URI is aborted so bursts of keystrokes
    /// collapse into a single round-trip.
    fn schedule_resolve(&self, uri: Url, delay: Duration) {
        // Cancel any prior debounced resolve for this buffer. `abort()`
        // cancels the task at its next `.await` — any in-flight future is
        // dropped, not awaited to completion.
        if let Some((_, prev)) = self.pending.remove(&uri) {
            prev.abort();
        }

        // Clone everything the task will need. Each `Arc::clone` is cheap;
        // it's all pointer bumps. We do this here (rather than moving `self`
        // in) because the task outlives the handler.
        let http = self.http.clone();
        let cache = self.cache.clone();
        let vuln_cache = self.vuln_cache.clone();
        let docs = self.docs.clone();
        let pushed = self.pushed.clone();
        let pending = self.pending.clone();
        let client = self.client.clone();
        let uri_key = uri.clone();

        let handle = tokio::spawn(async move {
            if !delay.is_zero() {
                sleep(delay).await;
            }
            // Self-evict from the pending map before doing any real work,
            // so a concurrent `schedule_resolve` doesn't see a stale handle
            // and try to abort something that's already finished its sleep.
            pending.remove(&uri_key);
            resolve_and_push(
                &client,
                &http,
                &cache,
                &vuln_cache,
                &docs,
                &pushed,
                &uri_key,
            )
            .await;
        });
        self.pending.insert(uri, handle);
    }
}

/// Do the actual network work for one URI and push updated diagnostics.
///
/// Runs outside of any `Backend` method so `schedule_resolve` can spawn it
/// as an independent task — `&self` borrows can't escape into `tokio::spawn`.
async fn resolve_and_push(
    client: &LspClient,
    http: &Client,
    cache: &Arc<VersionCache>,
    vuln_cache: &Arc<VulnCache>,
    docs: &DashMap<Url, Arc<DocState>>,
    pushed: &DashMap<Url, u64>,
    uri: &Url,
) {
    // Grab an immutable snapshot of the current parsed state. If the doc
    // was closed while we were waiting out the debounce, there's nothing
    // to do.
    let Some(state) = docs.get(uri).map(|e| Arc::clone(&*e)) else {
        return;
    };
    let kind = state.kind;

    // --- Phase 1: version fetches (existing behavior) ---
    //
    // Deduplicate names — a package could appear in both `dependencies`
    // and `devDependencies`, but we only need to fetch its version once.
    let to_fetch: HashSet<String> = state
        .entries
        .iter()
        .filter(|a| a.latest.is_none())
        .map(|a| a.entry.name.clone())
        .collect();

    if !to_fetch.is_empty() {
        use futures::StreamExt;
        let mut futs = futures::stream::FuturesUnordered::new();
        for name in to_fetch {
            let http = http.clone();
            futs.push(async move { (name.clone(), providers::fetch(&http, kind, &name).await) });
        }
        while let Some((name, res)) = futs.next().await {
            match res {
                Ok(info) => cache.put(kind, name, info),
                Err(e) => warn!(?name, "registry lookup failed: {e:#}"),
            }
        }
    }

    // --- Phase 2: OSV vulnerability scans ---
    //
    // For every entry whose literal parses to a concrete version, consult
    // the vuln cache. Anything missing gets scanned in parallel. We never
    // overwrite a cache hit with a failed scan — an error is logged and
    // treated as "retry next keystroke".
    //
    // Size hints: every entry is a potential scan target at most, so
    // over-allocating once avoids growth reallocations during collection.
    let mut scan_targets: Vec<(String, Version)> = Vec::with_capacity(state.entries.len());
    let mut seen: HashSet<(String, Version)> = HashSet::with_capacity(state.entries.len());
    for a in &state.entries {
        if let Some(ver) = crate::version::parse_for_scan(&a.entry.version_literal) {
            if seen.insert((a.entry.name.clone(), ver.clone()))
                && vuln_cache.get(kind, &a.entry.name, &ver).is_none()
            {
                scan_targets.push((a.entry.name.clone(), ver));
            }
        }
    }

    if !scan_targets.is_empty() {
        use futures::StreamExt;
        let mut futs = futures::stream::FuturesUnordered::new();
        for (name, ver) in scan_targets {
            let http = http.clone();
            futs.push(async move {
                let res = crate::vulnerabilities::fetch_vulns(&http, kind, &name, &ver).await;
                (name, ver, res)
            });
        }
        while let Some((name, ver, res)) = futs.next().await {
            match res {
                Ok(vulns) => vuln_cache.put(kind, name, ver, vulns),
                Err(e) => warn!(?name, version = %ver, "OSV scan failed: {e:#}"),
            }
        }
    }

    // --- Fold both results back into DocState ---
    //
    // `alter` is a no-op if the doc has been closed in the meantime — a
    // `did_close` landing during the fetch must not resurrect the doc.
    docs.alter(uri, |_, existing| {
        let mut new_entries = existing.entries.clone();
        for a in &mut new_entries {
            if a.latest.is_none() {
                if let Some(info) = cache.get(kind, &a.entry.name) {
                    a.latest = info.latest_stable.or(info.latest_any);
                }
            }
            if let Some(ver) = crate::version::parse_for_scan(&a.entry.version_literal) {
                if let Some(mut vulns) = vuln_cache.get(kind, &a.entry.name, &ver) {
                    // Sort so the fingerprint is deterministic regardless of
                    // scan-completion order.
                    vulns.sort_by(|x, y| x.id.cmp(&y.id));
                    a.vulns = vulns;
                }
            }
        }
        Arc::new(DocState {
            kind: existing.kind,
            entries: new_entries,
        })
    });

    push_updates_raw(client, docs, pushed, uri).await;
}

/// Compute a stable fingerprint for the visible state: entry names, their
/// current literals, and their resolved latest. Anything else (positions,
/// group labels) can change without a user-visible delta, so we omit it.
///
/// This is the key to quiet typing: if a keystroke only shuffles whitespace
/// around, the fingerprint is unchanged, and we skip the diagnostic push
/// and hint refresh entirely.
fn fingerprint(state: &DocState) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    state.kind.display().hash(&mut h);
    for a in &state.entries {
        a.entry.name.hash(&mut h);
        a.entry.version_literal.hash(&mut h);
        // `Version` doesn't implement `Hash` directly, so decompose it.
        // Discriminant byte (0/1) distinguishes `None` from a `Some` whose
        // every field happens to hash to zero.
        match &a.latest {
            None => 0u8.hash(&mut h),
            Some(v) => {
                1u8.hash(&mut h);
                v.major.hash(&mut h);
                v.minor.hash(&mut h);
                v.patch.hash(&mut h);
                v.pre.as_str().hash(&mut h);
                v.build.as_str().hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Emit the current state of `uri` to the editor — diagnostics, and a
/// best-effort kick to refresh inlay hints / code lenses.
///
/// Skips the push entirely if the fingerprint hasn't changed since the
/// last round, which is the usual case while typing inside strings that
/// already parsed cleanly.
async fn push_updates_raw(
    client: &LspClient,
    docs: &DashMap<Url, Arc<DocState>>,
    pushed: &DashMap<Url, u64>,
    uri: &Url,
) {
    let Some(state) = docs.get(uri).map(|e| Arc::clone(&*e)) else {
        return;
    };
    let fp = fingerprint(&state);
    if pushed.get(uri).map(|e| *e) == Some(fp) {
        return;
    }
    pushed.insert(uri.clone(), fp);

    let diags = build_diagnostics(&state);
    client.publish_diagnostics(uri.clone(), diags, None).await;

    // These refreshes require server capability registration on the client
    // side; the Err case just means the client ignored our request. We
    // still send them so editors that *do* support them pick up the update.
    let _ = client.inlay_hint_refresh().await;
    let _ = client.code_lens_refresh().await;
}

/// Produce `Information`-level diagnostics for every entry whose latest is
/// newer than — and doesn't satisfy — the user's current range.
fn build_diagnostics(state: &DocState) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for a in &state.entries {
        let Some(latest) = &a.latest else { continue };
        // If the user's range already accepts `latest`, there's nothing
        // actionable to report.
        if version::satisfies(&a.entry.version_literal, latest) {
            continue;
        }
        // Pinned prereleases or locally-built tip can be `>= latest`; don't
        // nag the user about "updates" that would actually downgrade.
        if let Some(cur) = version::parse_literal(&a.entry.version_literal) {
            if &cur >= latest {
                continue;
            }
        }
        out.push(Diagnostic {
            range: a.entry.version_range,
            severity: Some(DiagnosticSeverity::INFORMATION),
            source: Some(DIAGNOSTIC_SOURCE.into()),
            code: Some(NumberOrString::String("update-available".into())),
            message: format!("{}: newer version {} is available", a.entry.name, latest),
            ..Default::default()
        });
    }
    out
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    /// Called once at client connect. We tell the client which LSP features
    /// we support; it only sends us requests for capabilities we advertise.
    async fn initialize(&self, _p: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: SERVER_NAME.into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                // Explicit: `LineIndex` produces UTF-16 columns, matching
                // LSP's default. We state it anyway so clients don't
                // renegotiate to UTF-8 or UTF-32 mid-session.
                position_encoding: Some(PositionEncodingKind::UTF16),
                // FULL sync: clients send the entire buffer on each change.
                // Simpler than incremental sync and plenty fast for the
                // small manifests we target.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                inlay_hint_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        // We only emit `quickfix` actions (Bump to X.Y.Z).
                        // Declaring the kind lets Zed / VS Code filter the
                        // action list appropriately.
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: Some(false),
                        work_done_progress_options: WorkDoneProgressOptions::default(),
                    },
                )),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _p: InitializedParams) {
        debug!("{SERVER_NAME} ready");
    }

    async fn shutdown(&self) -> LspResult<()> {
        // No cleanup needed: tokio will drop spawned tasks when the runtime
        // exits, and the OS reclaims the TCP connections.
        Ok(())
    }

    /// Buffer opened: parse immediately and kick off an eager resolve.
    /// No debounce — the user explicitly asked to see this file.
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        if self.reparse(&uri, &params.text_document.text).is_some() {
            self.schedule_resolve(uri, Duration::ZERO);
        }
    }

    /// Buffer edited: reparse and schedule a debounced resolve.
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // We negotiated FULL sync, so exactly one change arrives and it
        // carries the entire new document text.
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };
        if self.reparse(&uri, &change.text).is_some() {
            self.schedule_resolve(uri, DEBOUNCE);
        }
    }

    /// Buffer closed: tear down all state for it and clear diagnostics.
    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some((_, h)) = self.pending.remove(&uri) {
            h.abort();
        }
        self.docs.remove(&uri);
        self.pushed.remove(&uri);
        // Clearing diagnostics explicitly: some clients keep the last
        // published list forever otherwise, leaving ghost warnings.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    /// Produce inlay hints for every entry whose version range falls inside
    /// the client's requested window. The hint is placed flush with the end
    /// of the version literal with a single leading space for readability.
    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let registry = state.kind.display();
        let hints: Vec<InlayHint> = state
            .entries
            .iter()
            // Only emit hints the editor actually asked about — cuts payload
            // size on large manifests when the user is viewing a small slice.
            .filter(|a| {
                a.entry.version_range.start >= params.range.start
                    && a.entry.version_range.end <= params.range.end
            })
            .filter_map(|a| {
                let latest = a.latest.as_ref()?;
                let up_to_date = version::satisfies(&a.entry.version_literal, latest);
                // `✓` for "you're already compatible", `→` for "newer
                // available". Both hints include the latest version so the
                // user sees it without moving their cursor.
                let label = if up_to_date {
                    format!(" ✓ {latest}")
                } else {
                    format!(" → {latest}")
                };
                Some(InlayHint {
                    position: a.entry.version_range.end,
                    label: InlayHintLabel::String(label),
                    kind: None,
                    text_edits: None,
                    tooltip: Some(InlayHintTooltip::String(format!("latest on {registry}"))),
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                })
            })
            .collect();
        Ok(Some(hints))
    }

    /// Hover summary: name (grouped), current literal, resolved latest, and
    /// a link to the registry page. Triggered when the cursor is inside
    /// either the name or the version literal.
    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        // Pick the first entry whose name or version range contains the
        // cursor. Ranges from different entries never overlap, so "first"
        // is unambiguous.
        let Some(hit) = state
            .entries
            .iter()
            .find(|a| contains(&a.entry.version_range, pos) || contains(&a.entry.name_range, pos))
        else {
            return Ok(None);
        };

        // Render the name inside a code span so backticks, brackets, or
        // parens in an exotic package name can't be interpreted as markdown
        // link/formatting syntax. `write!` into `String` is infallible.
        let mut md = String::new();
        // Replace raw backticks in the name with apostrophes so our
        // surrounding code span stays unambiguous.
        let name = hit.entry.name.replace('`', "'");
        write!(md, "`{name}`").unwrap();
        if let Some(group) = hit.entry.group {
            write!(md, " _({group})_").unwrap();
        }
        md.push('\n');

        // Current literal block. Again: escape backticks to keep markdown sane.
        let literal = hit.entry.version_literal.replace('`', "'");
        write!(md, "\ncurrent: `{literal}`\n").unwrap();
        if let Some(latest) = &hit.latest {
            writeln!(md, "latest: `{latest}`").unwrap();
        } else {
            // Fetch still in flight (or failed and waiting for retry). Be
            // honest with the user rather than hiding the field.
            md.push_str("latest: _resolving…_\n");
        }
        // Final optional section: link straight to the registry page.
        if let Some(url) = self
            .cache
            .get(state.kind, &hit.entry.name)
            .and_then(|info| info.url)
        {
            write!(md, "\n[registry]({url})").unwrap();
        }

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range: Some(hit.entry.version_range),
        }))
    }

    /// Emit one `Bump to X.Y.Z` quickfix per out-of-date entry that overlaps
    /// the requested range. The selected range is typically the cursor
    /// position; `ranges_overlap` is generous so clicks on boundaries work.
    async fn code_action(&self, params: CodeActionParams) -> LspResult<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let mut out: CodeActionResponse = Vec::new();
        for a in &state.entries {
            if !ranges_overlap(&a.entry.version_range, &params.range) {
                continue;
            }
            let Some(latest) = &a.latest else { continue };
            // Don't offer a bump if the user's range already accepts the
            // latest — the action would be a no-op.
            if version::satisfies(&a.entry.version_literal, latest) {
                continue;
            }
            let new_text = replacement(&a.entry.version_literal, latest);
            let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![TextEdit {
                    range: a.entry.version_range,
                    new_text,
                }],
            );
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Bump {} to {}", a.entry.name, latest),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                // `is_preferred` tells the editor to highlight this as the
                // default action — handy for `cmd-.` → Enter shortcuts.
                is_preferred: Some(true),
                disabled: None,
                data: None,
            }));
        }
        Ok(Some(out))
    }

    /// We advertise no custom commands, but some clients call
    /// `workspace/executeCommand` speculatively on startup anyway. Return
    /// null to keep them happy.
    async fn execute_command(&self, _p: ExecuteCommandParams) -> LspResult<Option<Value>> {
        Ok(Some(json!(null)))
    }
}

/// Returns `true` if `pos` lies inside `range`. Both ends are inclusive,
/// which matches how LSP clients typically hit-test hover positions.
///
/// The logic: `pos` is after-or-at the start AND before-or-at the end,
/// handling line/column lexicographically.
fn contains(range: &Range, pos: Position) -> bool {
    (range.start.line < pos.line
        || (range.start.line == pos.line && range.start.character <= pos.character))
        && (range.end.line > pos.line
            || (range.end.line == pos.line && range.end.character >= pos.character))
}

/// Returns `true` if ranges `a` and `b` share at least one position
/// (including single-point touches at endpoints).
///
/// Implemented as "not disjoint": either `a` starts strictly after `b` ends,
/// or `b` starts strictly after `a` ends. Anything else is overlap.
fn ranges_overlap(a: &Range, b: &Range) -> bool {
    let a_after_b = (a.start.line > b.end.line)
        || (a.start.line == b.end.line && a.start.character > b.end.character);
    let b_after_a = (b.start.line > a.end.line)
        || (b.start.line == a.end.line && b.start.character > a.end.character);
    !(a_after_b || b_after_a)
}

/// Preserve the user's range operator when bumping, so we don't turn a
/// semver range (`^1.2.3`) into an exact pin (`1.5.0`).
///
/// Strategy: split the literal into
///   `leading-whitespace` + `range-operator-chars` + `the-version`
/// then reassemble with the new version replacing just the third piece.
fn replacement(current: &str, latest: &Version) -> String {
    let trimmed = current.trim_start();
    // Preserve any leading whitespace exactly — some formats allow `" 1.0"`
    // and we don't want to mess with indentation.
    let leading_ws = &current[..current.len() - trimmed.len()];
    // Grab the operator chars up to (but not including) the first
    // alphanumeric of the actual version. Same character class as
    // `strip_leading`, kept in sync manually.
    let op: String = trimmed
        .chars()
        .take_while(|c| matches!(c, '^' | '~' | '=' | '>' | '<' | 'v' | 'V' | ' '))
        .collect();
    format!("{leading_ws}{op}{latest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_preserves_operator() {
        // The user's operator (`^`, `~`, `>=`, or none) must survive the bump.
        let latest = Version::parse("1.5.0").unwrap();
        assert_eq!(replacement("^1.2.3", &latest), "^1.5.0");
        assert_eq!(replacement("~1.2.3", &latest), "~1.5.0");
        assert_eq!(replacement("1.2.3", &latest), "1.5.0");
        assert_eq!(replacement(">= 1.2.3", &latest), ">= 1.5.0");
    }

    /// Tiny helper — build a `Range` from four integers, inline in tests.
    fn r(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: Position::new(sl, sc),
            end: Position::new(el, ec),
        }
    }

    #[test]
    fn contains_handles_same_line() {
        // Both endpoints are inclusive; anything outside by one column is out.
        let range = r(2, 4, 2, 10);
        assert!(contains(&range, Position::new(2, 4)));
        assert!(contains(&range, Position::new(2, 7)));
        assert!(contains(&range, Position::new(2, 10)));
        assert!(!contains(&range, Position::new(2, 3)));
        assert!(!contains(&range, Position::new(2, 11)));
    }

    #[test]
    fn contains_spans_multiple_lines() {
        // Middle lines are always in-range regardless of column.
        let range = r(2, 4, 5, 2);
        assert!(contains(&range, Position::new(3, 0)));
        assert!(contains(&range, Position::new(4, 999)));
        assert!(!contains(&range, Position::new(1, 99)));
        assert!(!contains(&range, Position::new(5, 3)));
    }

    #[test]
    fn ranges_overlap_cases() {
        let a = r(1, 0, 1, 10);
        assert!(ranges_overlap(&a, &r(1, 0, 1, 10)));
        assert!(ranges_overlap(&a, &r(1, 5, 1, 15)));
        assert!(ranges_overlap(&a, &r(0, 5, 2, 3)));
        // Touching at a single point (end of A == start of B): treat as overlap.
        // Code actions on a cursor placed exactly at a boundary should still fire.
        assert!(ranges_overlap(&a, &r(1, 10, 1, 20)));
        // Disjoint same-line.
        assert!(!ranges_overlap(&a, &r(1, 11, 1, 20)));
        // Disjoint different-line.
        assert!(!ranges_overlap(&a, &r(3, 0, 3, 5)));
    }
}
