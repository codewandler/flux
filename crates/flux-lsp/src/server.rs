//! `server` — the `LanguageServer` implementation: capabilities, handlers, and the shared state
//! they read.
//!
//! Every handler follows the same shape: take the **cached** document (never re-parse), resolve the
//! cursor through the CST, answer. The one rule that governs the crate is stated in
//! [`crate::catalog`]: the LSP is a reader — no model, network, or credential IO, and the only disk
//! access is the read-only workspace composite scan, done once when the root is known and refreshed
//! on `didSave`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use flux_lang::opspec::OpSignature;
use flux_lang::program::CompositeOpDecl;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::{Error, Result};
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::catalog;
use crate::completion::completions;
use crate::document::DocumentStore;
use crate::hover::hover_at;
use crate::scope::{self, Symbol};
use crate::semantic;

/// Controls who selects the workspace root scanned for authored composite operations.
///
/// Editor processes use [`ClientProvided`](WorkspacePolicy::ClientProvided). Embedded transports
/// such as the documentation workbench must use [`Fixed`](WorkspacePolicy::Fixed), so an
/// untrusted `initialize.rootUri` cannot make the language server inspect arbitrary host paths.
#[derive(Clone, Debug, Default)]
pub enum WorkspacePolicy {
    #[default]
    ClientProvided,
    Fixed(Option<PathBuf>),
}

/// Precomputed, owned completion/hover catalogs + the open-document store.
pub struct Backend {
    client: Client,
    /// Catalog-only tools retained so composites can validate against the same specs.
    registry: Arc<flux_runtime::ToolRegistry>,
    /// All registered host ops — owned so we don't hold the borrowing `OpRegistry` across `await`.
    ops: Vec<OpSignature>,
    /// `(kind, doc)` for every Flux-Lang node kind (the grammar keywords).
    node_kinds: Vec<(String, String)>,
    /// `(type, doc)` for the artifact prelude types.
    prelude_types: Vec<(String, String)>,
    docs: DocumentStore,
    /// The workspace root, from `initialize` — the flow home the composite scan reads.
    root: RwLock<Option<PathBuf>>,
    /// Composites discovered in the workspace flow home (L-89), refreshed on `didSave`.
    workspace_ops: RwLock<Vec<CompositeOpDecl>>,
    /// The last semantic-token stream per document, for `full/delta`.
    tokens: RwLock<HashMap<Url, (String, Vec<SemanticToken>)>>,
    next_result_id: AtomicU64,
    workspace_policy: WorkspacePolicy,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self::with_workspace_policy(client, WorkspacePolicy::ClientProvided)
    }

    /// Construct a backend whose workspace root follows `workspace_policy`.
    pub fn with_workspace_policy(client: Client, workspace_policy: WorkspacePolicy) -> Self {
        // Build the op catalog once at startup and keep the owned signatures.
        let registry = Arc::new(catalog::authoring_registry());
        let ops = flux_flow::registry::OpRegistry::new(registry.as_ref()).signatures();
        Backend {
            client,
            registry,
            ops,
            node_kinds: flux_lang::schema::node_kind_rows(),
            prelude_types: flux_lang::prelude::prelude_type_rows(),
            docs: DocumentStore::default(),
            root: RwLock::new(None),
            workspace_ops: RwLock::new(Vec::new()),
            tokens: RwLock::new(HashMap::new()),
            next_result_id: AtomicU64::new(0),
            workspace_policy,
        }
    }

    /// Rescan the workspace flow home. Read-only, and cheap enough to redo on save; never per
    /// keystroke.
    async fn reload_workspace_ops(&self) {
        let root = self.root.read().await.clone();
        let discovered = match root {
            Some(root) => tokio::task::spawn_blocking(move || catalog::workspace_composites(&root))
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };
        *self.workspace_ops.write().await = discovered;
    }

    /// The full authoring catalog for a document: host ops + workspace composites + its own. Takes
    /// the cloned `Parse` rather than the `Document` so no read guard is held across an `await`.
    async fn signatures_for(&self, parse: &flux_lang::parser::Parse) -> Vec<OpSignature> {
        let workspace = self.workspace_ops.read().await;
        catalog::signatures_for(&self.ops, &workspace, &catalog::document_composites(parse))
    }

    /// Re-analyze the open document and publish its diagnostics.
    async fn refresh(&self, uri: Url) {
        let workspace = self.workspace_ops.read().await.clone();
        let Some(diags) = self
            .docs
            .with(&uri, |doc| {
                crate::diagnostics::diagnostics(
                    self.registry.as_ref(),
                    &workspace,
                    &doc.parse,
                    &doc.text,
                    &doc.index,
                )
            })
            .await
        else {
            return;
        };
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn semantic_data(
        &self,
        uri: &Url,
        lines: Option<std::ops::Range<u32>>,
    ) -> Option<Vec<SemanticToken>> {
        let parse = self.docs.with(uri, |doc| doc.parse.clone()).await?;
        let known: HashSet<String> = self
            .signatures_for(&parse)
            .await
            .into_iter()
            .map(|op| op.name)
            .collect();
        self.docs
            .with(uri, |doc| {
                semantic::semantic_tokens(&doc.root(), &doc.text, &doc.index, &known, lines)
            })
            .await
    }

    fn mint_result_id(&self) -> String {
        self.next_result_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string()
    }
}

/// The workspace root an `initialize` request names, preferring the first workspace folder.
fn root_from(params: &InitializeParams) -> Option<PathBuf> {
    #[allow(deprecated)]
    params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .map(|folder| folder.uri.clone())
        .or_else(|| params.root_uri.clone())
        .and_then(|uri| uri.to_file_path().ok())
}

/// The capabilities the server advertises. Every entry here has a handler below — the pairing is
/// what `tests/protocol.rs` checks, so a capability cannot be advertised into the void (L-91).
pub fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        // Incremental sync (L-70): `didChange` carries ranged edits, which we apply against the
        // stored buffer and the cached tree (L-90). `save` is opted into so the workspace composite
        // scan can refresh (L-89).
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec!["$".into(), "@".into()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        // Navigation (L-68) + editing (L-87) over the CST scope model.
        document_symbol_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        // Semantic tokens (L-69) with range + delta (L-90).
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: semantic::legend(),
                full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                range: Some(true),
                ..Default::default()
            },
        )),
        ..Default::default()
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        *self.root.write().await = match &self.workspace_policy {
            WorkspacePolicy::ClientProvided => root_from(&params),
            WorkspacePolicy::Fixed(root) => root.clone(),
        };
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "flux-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: capabilities(),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.reload_workspace_ops().await;
        self.client
            .log_message(MessageType::INFO, "flux-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.open(uri.clone(), params.text_document.text).await;
        self.refresh(uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.change(&uri, &params.content_changes).await;
        self.refresh(uri).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        // A saved buffer may have added or changed a composite in the flow home.
        self.reload_workspace_ops().await;
        self.refresh(params.text_document.uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.close(&uri).await;
        self.tokens.write().await.remove(&uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some(parse) = self.docs.with(uri, |doc| doc.parse.clone()).await else {
            return Ok(None);
        };
        let ops = self.signatures_for(&parse).await;
        let items = self
            .docs
            .with(uri, |doc| {
                completions(
                    &doc.root(),
                    &doc.text,
                    doc.offset(position),
                    &ops,
                    &self.node_kinds,
                    &self.prelude_types,
                )
            })
            .await;
        Ok(items.map(CompletionResponse::Array))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some(parse) = self.docs.with(uri, |doc| doc.parse.clone()).await else {
            return Ok(None);
        };
        let ops = self.signatures_for(&parse).await;
        Ok(self
            .docs
            .with(uri, |doc| {
                hover_at(
                    &doc.root(),
                    &doc.text,
                    &doc.index,
                    doc.offset(position),
                    &ops,
                    &self.node_kinds,
                    &self.prelude_types,
                )
            })
            .await
            .flatten())
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        Ok(self
            .docs
            .with(&params.text_document.uri, |doc| {
                crate::format::format_document(&doc.parse, &doc.text, &doc.index)
            })
            .await
            .flatten())
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        Ok(self
            .docs
            .with(&params.text_document.uri, |doc| {
                crate::format::format_selection(&doc.parse, &doc.text, &doc.index, params.range)
            })
            .await
            .flatten())
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let symbols = self
            .docs
            .with(&params.text_document.uri, |doc| {
                crate::symbols::document_symbols(&doc.root(), &doc.text, &doc.index)
            })
            .await
            .unwrap_or_default();
        if symbols.is_empty() {
            return Ok(None);
        }
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let range = self
            .docs
            .with(uri, |doc| {
                scope::definition_at(&doc.root(), doc.offset(position))
                    .map(|r| crate::convert::source_range(r, &doc.text, &doc.index))
            })
            .await
            .flatten();
        Ok(range.map(|range| {
            GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range,
            })
        }))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let ranges = self
            .docs
            .with(uri, |doc| {
                let root = doc.root();
                let symbol = scope::symbol_at(&root, doc.offset(position))?;
                let declaration = symbol.def().name_range;
                Some(
                    scope::references(&root, &symbol)
                        .into_iter()
                        .filter(|r| include_declaration || *r != declaration)
                        .map(|r| crate::convert::source_range(r, &doc.text, &doc.index))
                        .collect::<Vec<_>>(),
                )
            })
            .await
            .flatten();
        Ok(ranges.map(|ranges| {
            ranges
                .into_iter()
                .map(|range| Location {
                    uri: uri.clone(),
                    range,
                })
                .collect()
        }))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let range = self
            .docs
            .with(&params.text_document.uri, |doc| {
                scope::symbol_at(&doc.root(), doc.offset(params.position)).map(|symbol| {
                    crate::convert::source_range(symbol.token_range(), &doc.text, &doc.index)
                })
            })
            .await
            .flatten();
        // A position that is not a renameable symbol gets an honest "no", not a bogus range.
        Ok(range.map(PrepareRenameResponse::Range))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        let edits = self
            .docs
            .with(&uri, |doc| {
                let root = doc.root();
                let symbol: Symbol = scope::symbol_at(&root, doc.offset(position))
                    .ok_or_else(|| Error::invalid_params("not a renameable symbol"))?;
                if !scope::valid_new_name(&symbol, &new_name) {
                    return Err(Error::invalid_params(format!(
                        "`{new_name}` is not a legal Flux-Lang name"
                    )));
                }
                let replacement = scope::replacement_for(&symbol, &new_name);
                Ok(scope::references(&root, &symbol)
                    .into_iter()
                    .map(|range| TextEdit {
                        range: crate::convert::source_range(range, &doc.text, &doc.index),
                        new_text: replacement.clone(),
                    })
                    .collect::<Vec<_>>())
            })
            .await;
        let Some(edits) = edits else {
            return Ok(None);
        };
        let edits = edits?;
        // Single-document scope for now; the shape is already the multi-document one, so the
        // cross-file rename that the L-89 workspace index unlocks is an additive change.
        Ok(Some(WorkspaceEdit {
            changes: Some(HashMap::from([(uri, edits)])),
            ..Default::default()
        }))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(data) = self.semantic_data(&uri, None).await else {
            return Ok(None);
        };
        let result_id = self.mint_result_id();
        self.tokens
            .write()
            .await
            .insert(uri, (result_id.clone(), data.clone()));
        Ok(Some(SemanticTokensResult::Tokens(semantic::tokens(
            result_id, data,
        ))))
    }

    async fn semantic_tokens_full_delta(
        &self,
        params: SemanticTokensDeltaParams,
    ) -> Result<Option<SemanticTokensFullDeltaResult>> {
        let uri = params.text_document.uri;
        let Some(data) = self.semantic_data(&uri, None).await else {
            return Ok(None);
        };
        let previous = self.tokens.read().await.get(&uri).cloned();
        let result_id = self.mint_result_id();
        self.tokens
            .write()
            .await
            .insert(uri, (result_id.clone(), data.clone()));
        match previous {
            // The client's `previous_result_id` must be the stream we still hold, or we cannot
            // describe the difference and must send the whole thing.
            Some((id, before)) if id == params.previous_result_id => Ok(Some(
                SemanticTokensFullDeltaResult::TokensDelta(SemanticTokensDelta {
                    result_id: Some(result_id),
                    edits: semantic::delta(&before, &data),
                }),
            )),
            _ => Ok(Some(SemanticTokensFullDeltaResult::Tokens(
                semantic::tokens(result_id, data),
            ))),
        }
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        let lines = semantic::line_span(params.range);
        let Some(data) = self
            .semantic_data(&params.text_document.uri, Some(lines))
            .await
        else {
            return Ok(None);
        };
        Ok(Some(SemanticTokensRangeResult::Tokens(semantic::tokens(
            self.mint_result_id(),
            data,
        ))))
    }
}
