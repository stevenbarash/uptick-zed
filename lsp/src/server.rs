use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use reqwest::Client;
use semver::Version;
use serde_json::{Value, json};
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client as LspClient, LanguageServer};
use tracing::{debug, warn};

use crate::cache::VersionCache;
use crate::manifest::{ManifestKind, RawEntry};
use crate::parsers;
use crate::providers;
use crate::version;

const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const SERVER_NAME: &str = "versionlens-lsp";
const DIAGNOSTIC_SOURCE: &str = "versionlens";
const USER_AGENT: &str = concat!(
    "versionlens-lsp/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/stevenbarash/versionlens-zed)"
);

#[derive(Debug, Clone)]
pub struct Annotated {
    pub entry: RawEntry,
    pub latest: Option<Version>,
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
        }
    }

    /// Parse text into entries and store. Returns the new state's `ManifestKind`
    /// if the document is one we handle.
    fn reparse(&self, uri: &Url, text: String) -> Option<ManifestKind> {
        let kind = ManifestKind::from_url(uri)?;
        let entries = parsers::parse(kind, &text)
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

    /// Spawn lookups for any entries without a cached latest. Results are
    /// merged back via a fresh `Arc<DocState>` and then pushed to the editor
    /// — but only if the doc's fingerprint actually changed.
    fn resolve_missing(&self, uri: Url) {
        let Some(state) = self.docs.get(&uri).map(|e| Arc::clone(&*e)) else {
            return;
        };
        let kind = state.kind;

        let to_fetch: Vec<String> = state
            .entries
            .iter()
            .filter(|a| a.latest.is_none())
            .map(|a| a.entry.name.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if to_fetch.is_empty() {
            self.push_updates(uri);
            return;
        }

        let http = self.http.clone();
        let cache = self.cache.clone();
        let docs = self.docs.clone();
        let pushed = self.pushed.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
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

            if let Some(existing) = docs.get(&uri).map(|e| Arc::clone(&*e)) {
                let mut new_entries = existing.entries.clone();
                for a in new_entries.iter_mut() {
                    if a.latest.is_none() {
                        if let Some(info) = cache.get(kind, &a.entry.name) {
                            a.latest = info.latest_stable.or(info.latest_any);
                        }
                    }
                }
                let new_state = Arc::new(DocState {
                    kind: existing.kind,
                    entries: new_entries,
                });
                docs.insert(uri.clone(), new_state);
            }
            push_updates_raw(&client, &docs, &pushed, &uri).await;
        });
    }

    fn push_updates(&self, uri: Url) {
        let docs = self.docs.clone();
        let pushed = self.pushed.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            push_updates_raw(&client, &docs, &pushed, &uri).await;
        });
    }
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
        a.latest.as_ref().map(|v| v.to_string()).hash(&mut h);
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
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                inlay_hint_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
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
        if self.reparse(&uri, params.text_document.text).is_some() {
            self.resolve_missing(uri);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // We negotiated FULL sync, so exactly one change arrives.
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };
        if self.reparse(&uri, change.text).is_some() {
            self.resolve_missing(uri);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
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
                    format!(" ✓ {}", latest)
                } else {
                    format!(" → {}", latest)
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

        let mut md = format!("**{}**", hit.entry.name);
        if let Some(group) = hit.entry.group {
            md.push_str(&format!(" _({group})_"));
        }
        md.push('\n');
        md.push_str(&format!("\ncurrent: `{}`\n", hit.entry.version_literal));
        if let Some(latest) = &hit.latest {
            md.push_str(&format!("latest: `{latest}`\n"));
        } else {
            md.push_str("latest: _resolving…_\n");
        }
        if let Some(url) = self
            .cache
            .get(state.kind, &hit.entry.name)
            .and_then(|info| info.url)
        {
            md.push_str(&format!("\n[registry]({url})"));
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
}
