use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use reqwest::Client;
use semver::Version;
use serde_json::{Value, json};
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

const CACHE_TTL: Duration = Duration::from_secs(3600);
const DEBOUNCE: Duration = Duration::from_millis(250);
const SERVER_NAME: &str = "uptick-lsp";
const DIAGNOSTIC_SOURCE: &str = "uptick";
const USER_AGENT: &str = concat!(
    "uptick-lsp/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/stevenbarash/uptick-zed)"
);

#[derive(Debug, Clone)]
struct Annotated {
    entry: RawEntry,
    latest: Option<Version>,
}

/// Immutable snapshot of one document. We always replace the `Arc` wholesale
/// rather than mutate in place, so LSP handlers can cheaply clone the `Arc`
/// and operate on a stable view.
#[derive(Debug)]
struct DocState {
    kind: ManifestKind,
    entries: Vec<Annotated>,
}

pub struct Backend {
    client: LspClient,
    http: Client,
    cache: Arc<VersionCache>,
    docs: Arc<DashMap<Url, Arc<DocState>>>,
    /// Last-pushed fingerprint per doc. Skips the refresh/diagnostics storm
    /// when a reparse produced no user-visible changes (common while typing).
    pushed: Arc<DashMap<Url, u64>>,
    /// In-flight debounced resolve tasks, keyed by document. A new `did_change`
    /// aborts the prior task so we only do one network round-trip per burst.
    pending: Arc<DashMap<Url, JoinHandle<()>>>,
}

impl Backend {
    pub fn new(client: LspClient) -> Self {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            client,
            http,
            cache: Arc::new(VersionCache::new(CACHE_TTL)),
            docs: Arc::new(DashMap::new()),
            pushed: Arc::new(DashMap::new()),
            pending: Arc::new(DashMap::new()),
        }
    }

    /// Parse text into entries and store. Returns the new state's `ManifestKind`
    /// if the document is one we handle.
    fn reparse(&self, uri: &Url, text: &str) -> Option<ManifestKind> {
        let kind = ManifestKind::from_url(uri)?;
        let entries = parsers::parse(kind, text)
            .into_iter()
            .map(|entry| {
                let latest = self
                    .cache
                    .get(kind, &entry.name)
                    .and_then(|info| info.latest_stable.or(info.latest_any));
                Annotated { entry, latest }
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
        if let Some((_, prev)) = self.pending.remove(&uri) {
            prev.abort();
        }
        let http = self.http.clone();
        let cache = self.cache.clone();
        let docs = self.docs.clone();
        let pushed = self.pushed.clone();
        let pending = self.pending.clone();
        let client = self.client.clone();
        let uri_key = uri.clone();

        let handle = tokio::spawn(async move {
            if !delay.is_zero() {
                sleep(delay).await;
            }
            pending.remove(&uri_key);
            resolve_and_push(&client, &http, &cache, &docs, &pushed, &uri_key).await;
        });
        self.pending.insert(uri, handle);
    }
}

async fn resolve_and_push(
    client: &LspClient,
    http: &Client,
    cache: &Arc<VersionCache>,
    docs: &DashMap<Url, Arc<DocState>>,
    pushed: &DashMap<Url, u64>,
    uri: &Url,
) {
    let Some(state) = docs.get(uri).map(|e| Arc::clone(&*e)) else {
        return;
    };
    let kind = state.kind;

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

        // `alter` only fires if the entry is still present, so a
        // `did_close` landed during fetch doesn't resurrect the doc.
        docs.alter(uri, |_, existing| {
            let mut new_entries = existing.entries.clone();
            for a in &mut new_entries {
                if a.latest.is_none() {
                    if let Some(info) = cache.get(kind, &a.entry.name) {
                        a.latest = info.latest_stable.or(info.latest_any);
                    }
                }
            }
            Arc::new(DocState {
                kind: existing.kind,
                entries: new_entries,
            })
        });
    }

    push_updates_raw(client, docs, pushed, uri).await;
}

/// Compute a stable fingerprint for the visible state: entry names, their
/// current literals, and their resolved latest. Anything else (positions,
/// group labels) can change without a user-visible delta, so we omit it.
fn fingerprint(state: &DocState) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    state.kind.display().hash(&mut h);
    for a in &state.entries {
        a.entry.name.hash(&mut h);
        a.entry.version_literal.hash(&mut h);
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
    // Capability-gated refreshes; best-effort.
    let _ = client.inlay_hint_refresh().await;
    let _ = client.code_lens_refresh().await;
}

fn build_diagnostics(state: &DocState) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for a in &state.entries {
        let Some(latest) = &a.latest else { continue };
        if version::satisfies(&a.entry.version_literal, latest) {
            continue;
        }
        // Pinned prereleases or locally-built tip can be `>= latest`; leave alone.
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
    async fn initialize(&self, _p: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: SERVER_NAME.into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                // Explicit: `LineIndex` produces UTF-16 columns.
                position_encoding: Some(PositionEncodingKind::UTF16),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                inlay_hint_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
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
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        if self.reparse(&uri, &params.text_document.text).is_some() {
            self.schedule_resolve(uri, Duration::ZERO);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // We negotiated FULL sync, so exactly one change arrives.
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };
        if self.reparse(&uri, &change.text).is_some() {
            self.schedule_resolve(uri, DEBOUNCE);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some((_, h)) = self.pending.remove(&uri) {
            h.abort();
        }
        self.docs.remove(&uri);
        self.pushed.remove(&uri);
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let registry = state.kind.display();
        let hints: Vec<InlayHint> = state
            .entries
            .iter()
            .filter(|a| {
                a.entry.version_range.start >= params.range.start
                    && a.entry.version_range.end <= params.range.end
            })
            .filter_map(|a| {
                let latest = a.latest.as_ref()?;
                let up_to_date = version::satisfies(&a.entry.version_literal, latest);
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

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return Ok(None);
        };
        let Some(hit) = state.entries.iter().find(|a| {
            contains(&a.entry.version_range, pos) || contains(&a.entry.name_range, pos)
        }) else {
            return Ok(None);
        };

        // Render the name inside a code span so backticks, brackets, or
        // parens in an exotic package name can't be interpreted as markdown
        // link/formatting syntax. `write!` into `String` is infallible.
        let mut md = String::new();
        let name = hit.entry.name.replace('`', "'");
        write!(md, "`{name}`").unwrap();
        if let Some(group) = hit.entry.group {
            write!(md, " _({group})_").unwrap();
        }
        md.push('\n');
        let literal = hit.entry.version_literal.replace('`', "'");
        write!(md, "\ncurrent: `{literal}`\n").unwrap();
        if let Some(latest) = &hit.latest {
            writeln!(md, "latest: `{latest}`").unwrap();
        } else {
            md.push_str("latest: _resolving…_\n");
        }
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

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> LspResult<Option<CodeActionResponse>> {
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
                is_preferred: Some(true),
                disabled: None,
                data: None,
            }));
        }
        Ok(Some(out))
    }

    async fn execute_command(&self, _p: ExecuteCommandParams) -> LspResult<Option<Value>> {
        Ok(Some(json!(null)))
    }
}

fn contains(range: &Range, pos: Position) -> bool {
    (range.start.line < pos.line
        || (range.start.line == pos.line && range.start.character <= pos.character))
        && (range.end.line > pos.line
            || (range.end.line == pos.line && range.end.character >= pos.character))
}

fn ranges_overlap(a: &Range, b: &Range) -> bool {
    let a_after_b = (a.start.line > b.end.line)
        || (a.start.line == b.end.line && a.start.character > b.end.character);
    let b_after_a = (b.start.line > a.end.line)
        || (b.start.line == a.end.line && b.start.character > a.end.character);
    !(a_after_b || b_after_a)
}

/// Preserve the user's range operator when bumping, so we don't turn a
/// semver range (`^1.2.3`) into an exact pin (`1.5.0`).
fn replacement(current: &str, latest: &Version) -> String {
    let trimmed = current.trim_start();
    let leading_ws = &current[..current.len() - trimmed.len()];
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
        let latest = Version::parse("1.5.0").unwrap();
        assert_eq!(replacement("^1.2.3", &latest), "^1.5.0");
        assert_eq!(replacement("~1.2.3", &latest), "~1.5.0");
        assert_eq!(replacement("1.2.3", &latest), "1.5.0");
        assert_eq!(replacement(">= 1.2.3", &latest), ">= 1.5.0");
    }

    fn r(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: Position::new(sl, sc),
            end: Position::new(el, ec),
        }
    }

    #[test]
    fn contains_handles_same_line() {
        let range = r(2, 4, 2, 10);
        assert!(contains(&range, Position::new(2, 4)));
        assert!(contains(&range, Position::new(2, 7)));
        assert!(contains(&range, Position::new(2, 10)));
        assert!(!contains(&range, Position::new(2, 3)));
        assert!(!contains(&range, Position::new(2, 11)));
    }

    #[test]
    fn contains_spans_multiple_lines() {
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
