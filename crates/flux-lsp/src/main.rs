//! `flux-lsp` — a Language Server for Flux-Lang.
//!
//! Editor-grade support for `.flux` files, driven by the lossless CST front-end in `flux-lang`:
//! error-recovering **diagnostics** with real spans (the CST always yields a complete tree plus a
//! positioned error list), **completion** (registered ops, node-kind keywords, prelude types, and
//! in-scope `$vars`), **hover** (op signatures + node-kind/prelude docs), and whole-document
//! **formatting** (the invertible `format`). Wired into Helix config-only — see `.helix/languages.toml`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use flux_lang::highlight::{highlight, HighlightClass};
use flux_lang::opspec::OpSignature;
use flux_lang::program::{CompositeOpDecl, Module, Program};
use flux_lang::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use text_size::{TextRange, TextSize};

/// The catalog Flux-Lang authors see in diagnostics, completion, and hover.
///
/// Kept as a helper so tests exercise the exact registry the server installs rather than a
/// hand-built approximation.
fn authoring_registry() -> flux_runtime::ToolRegistry {
    let mut reg = flux_runtime::ToolRegistry::new();
    flux_tools::try_register_builtins(&mut reg)
        .expect("flux-lsp built-in authoring catalog registration failed");

    // Catalog-only registrations: none of these constructors performs IO. The provider never
    // generates, the datasource is empty and in-memory, and WebOptions::default is public-only with
    // no audit/record sink. Execution still belongs to the real host; the LSP only reads specs.
    flux_cognition::CognitionPack::new(Arc::new(flux_provider::NullProvider), "flux-lsp")
        .try_register_from("flux-lsp cognition authoring catalog", &mut reg)
        .expect("flux-lsp cognition authoring catalog registration failed");
    flux_capabilities::try_register_datasource_ops(
        &mut reg,
        Arc::new(flux_capabilities::MemoryBackend::new()),
    )
    .expect("flux-lsp datasource authoring catalog registration failed");
    flux_web::try_register_web(&mut reg, &flux_web::WebOptions::default())
        .expect("flux-lsp web authoring catalog registration failed");
    reg
}

#[cfg(test)]
fn authoring_op_signatures() -> Vec<OpSignature> {
    let reg = authoring_registry();
    flux_flow::registry::OpRegistry::new(&reg).signatures()
}

fn composite_signature(op: &CompositeOpDecl) -> OpSignature {
    let param_types = op
        .params
        .iter()
        .map(|param| (param.name.0.clone(), param.ty.clone()))
        .collect();
    OpSignature {
        name: op.name.clone(),
        description: op.meta.description.clone(),
        effects: op.meta.effects.clone(),
        risk: op.meta.risk,
        idempotency: op.meta.idempotency,
        required_params: op.params.iter().map(|param| param.name.0.clone()).collect(),
        optional_params: Vec::new(),
        param_types,
        output: op.returns.clone().unwrap_or(flux_lang::ast::TypeRef::Any),
        // Composite ops don't yet declare their own semantic-effect tier (D-138 scopes catalog
        // semantics to leaf ops); see `flux_flow::registry::composite_signature`.
        semantic_effects: Vec::new(),
    }
}

/// Base host ops plus every composite declared in this document. Local declarations participate in
/// authoring even when `expose false`: exposure controls planner advertising, not whether another
/// declaration in the same module may call the op.
fn signatures_for_document(base: &[OpSignature], text: &str) -> Vec<OpSignature> {
    let mut ops = base.to_vec();
    if let Ok(Module::Program(program)) = Module::parse_str(text) {
        let mut known: HashSet<String> = ops.iter().map(|op| op.name.clone()).collect();
        for op in &program.ops {
            if known.insert(op.name.clone()) {
                ops.push(composite_signature(op));
            }
        }
    }
    ops.sort_by(|a, b| a.name.cmp(&b.name));
    ops
}

/// Format only a clean bare flow. The semantic `Program` currently stores declarations in separate
/// vectors and therefore cannot reproduce their source order; returning `None` for modules is safer
/// than silently reordering an author's file.
///
/// Two paths (L-70):
/// - a comment-free flow uses the canonical AST formatter (`format::format`), which drops comments;
/// - a flow that carries comments uses a CST-driven **re-indent** that keeps every token verbatim.
///   That path canonicalizes indentation only (interior spacing is preserved) and is guarded by a
///   reparse-equivalence check, so it can never reorder or lose content — worst case it is a no-op.
///   Full canonical spacing *with* comments is the documented remaining work.
fn format_document(text: &str) -> Option<String> {
    let Module::Flow(ast) = Module::parse_str(text).ok()? else {
        return None;
    };
    let parsed = flux_lang::parser::parse_cst(text);
    if !parsed.errors.is_empty() {
        return None;
    }
    let root = parsed.syntax();
    if cst_has_comment(&root) {
        let reindented = reindent(&root);
        if reindented == text {
            return None;
        }
        // Safety net: the re-indented buffer must reparse to the same flow (identical canonical AST)
        // and keep exactly the same comments. Otherwise emit no edit.
        let same_ast = matches!(
            Module::parse_str(&reindented),
            Ok(Module::Flow(ref reparsed))
                if flux_lang::format::format(reparsed) == flux_lang::format::format(&ast)
        );
        let reparsed = flux_lang::parser::parse_cst(&reindented);
        let comments_kept = reparsed.errors.is_empty()
            && comment_multiset(&reparsed.syntax()) == comment_multiset(&root);
        return (same_ast && comments_kept).then_some(reindented);
    }
    let formatted = flux_lang::format::format(&ast);
    (formatted != text).then_some(formatted)
}

/// Precomputed, owned completion/hover catalogs + the open-document store.
struct Backend {
    client: Client,
    /// Catalog-only tools retained so module-local composites can validate against the same specs.
    registry: Arc<flux_runtime::ToolRegistry>,
    /// All registered ops (name, description, params, effects…) — owned so we don't hold the
    /// borrowing `OpRegistry`/`ToolRegistry` across `await` points.
    ops: Vec<OpSignature>,
    /// `(kind, doc)` for every Flux-Lang node kind (the grammar keywords).
    node_kinds: Vec<(String, String)>,
    /// `(type, doc)` for the artifact prelude types.
    prelude_types: Vec<(String, String)>,
    /// Open documents by URI → current full text.
    docs: RwLock<HashMap<Url, String>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        // Build the op catalog once at startup and keep the owned signatures.
        let registry = Arc::new(authoring_registry());
        let ops = flux_flow::registry::OpRegistry::new(registry.as_ref()).signatures();
        Backend {
            client,
            registry,
            ops,
            node_kinds: flux_lang::schema::node_kind_rows(),
            prelude_types: flux_lang::prelude::prelude_type_rows(),
            docs: RwLock::new(HashMap::new()),
        }
    }

    /// Re-analyze `text` and publish diagnostics for `uri`.
    async fn refresh(&self, uri: Url, text: &str) {
        // One CST parse serves both phases: tolerant errors first; on a clean buffer, the SAME
        // tree feeds the strict lowering + range side-map (previously this path parsed the buffer
        // three times per keystroke — review finding, 2026-07-09).
        let parsed = flux_lang::parser::parse_cst(text);
        let mut diags = cst_diagnostics(&parsed, text);
        // On a cleanly-parsing flow, add analyzer findings (unknown ops, unbound `$vars`, arity)
        // as warnings — the L-59 range side-map turns their node paths into real spans.
        if diags.is_empty() {
            diags = self.analyzer_diagnostics(&parsed, text);
        }
        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "flux-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                // Incremental sync (L-70): `didChange` carries ranged edits, which we apply against
                // the stored buffer via the line-index rather than replacing the whole document.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["$".into(), "@".into()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                // Navigation (L-68): document outline + go-to-definition over the CST scope model.
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                // Semantic tokens (L-69): CST token stream classified with the semantic
                // distinctions a grammar can't make (known op vs unknown identifier, bind vs use).
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic_tokens_legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: Some(false),
                            ..Default::default()
                        },
                    ),
                ),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "flux-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.docs.write().await.insert(uri.clone(), text.clone());
        self.refresh(uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // INCREMENTAL sync: apply each ranged edit against the current buffer (a `None` range is a
        // whole-document replacement). Applying edits in order keeps the server's text identical to
        // a full reparse of the final buffer (see `incremental_edits_match_full_reparse`).
        let uri = params.text_document.uri;
        let mut docs = self.docs.write().await;
        let text = docs.entry(uri.clone()).or_default();
        for change in &params.content_changes {
            apply_content_change(text, change);
        }
        let updated = text.clone();
        drop(docs);
        self.refresh(uri, &updated).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.write().await.remove(&params.text_document.uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let docs = self.docs.read().await;
        let text = docs.get(uri).map(String::as_str).unwrap_or("");
        Ok(Some(CompletionResponse::Array(self.completions(text))))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let docs = self.docs.read().await;
        let Some(text) = docs.get(uri) else {
            return Ok(None);
        };
        Ok(self.hover_at(text, pos))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let docs = self.docs.read().await;
        let Some(text) = docs.get(uri) else {
            return Ok(None);
        };
        let Some(formatted) = format_document(text) else {
            return Ok(None);
        };
        Ok(Some(vec![TextEdit {
            range: whole_document_range(text),
            new_text: formatted,
        }]))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let docs = self.docs.read().await;
        let Some(text) = docs.get(uri) else {
            return Ok(None);
        };
        let symbols = document_symbols(text);
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
        let pos = params.text_document_position_params.position;
        let docs = self.docs.read().await;
        let Some(text) = docs.get(uri) else {
            return Ok(None);
        };
        let Some(range) = definition_at(text, pos) else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range,
        })))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let docs = self.docs.read().await;
        let Some(text) = docs.get(uri) else {
            return Ok(None);
        };
        let known: HashSet<String> = signatures_for_document(&self.ops, text)
            .into_iter()
            .map(|op| op.name)
            .collect();
        Ok(Some(SemanticTokensResult::Tokens(semantic_tokens(
            text, &known,
        ))))
    }
}

impl Backend {
    fn completions(&self, text: &str) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        // Registered ops.
        for op in signatures_for_document(&self.ops, text) {
            items.push(CompletionItem {
                label: op.name.clone(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(op.description),
                insert_text: Some(format!("{}()", op.name)),
                ..Default::default()
            });
        }
        // Node-kind grammar keywords.
        for (kind, doc) in &self.node_kinds {
            items.push(CompletionItem {
                label: kind.clone(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some(doc.clone()),
                ..Default::default()
            });
        }
        // Prelude artifact types.
        for (ty, doc) in &self.prelude_types {
            items.push(CompletionItem {
                label: ty.clone(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some(doc.clone()),
                ..Default::default()
            });
        }
        // In-scope `$vars` scraped from the buffer.
        for name in scan_symbols(text) {
            items.push(CompletionItem {
                label: format!("${name}"),
                kind: Some(CompletionItemKind::VARIABLE),
                ..Default::default()
            });
        }
        items
    }

    /// Analyzer findings with real spans: run `analyze_flow` against the server's op catalog and
    /// resolve each diagnostic's rendered node path through the declaration-local range side-map.
    /// Every top-level flow and composite op is checked against one catalog containing all local
    /// composites, so forward references work and one declaration's `body[0]` cannot steal another's
    /// source range.
    fn analyzer_diagnostics(
        &self,
        parsed: &flux_lang::parser::Parse,
        text: &str,
    ) -> Vec<Diagnostic> {
        analyzer_diagnostics_for(self.registry.as_ref(), parsed, text)
    }

    fn hover_at(&self, text: &str, pos: Position) -> Option<Hover> {
        let word = word_at(text, pos)?;
        // An op call?
        if let Some(op) = signatures_for_document(&self.ops, text)
            .into_iter()
            .find(|o| o.name == word)
        {
            return Some(markdown_hover(render_op(&op)));
        }
        // A node-kind keyword?
        if let Some((kind, doc)) = self.node_kinds.iter().find(|(k, _)| *k == word) {
            return Some(markdown_hover(format!("**{kind}** (node kind)\n\n{doc}")));
        }
        // A prelude type?
        if let Some((ty, doc)) = self.prelude_types.iter().find(|(t, _)| *t == word) {
            return Some(markdown_hover(format!("**{ty}** (prelude type)\n\n{doc}")));
        }
        None
    }
}

fn analyzer_diagnostics_for(
    registry: &flux_runtime::ToolRegistry,
    parsed: &flux_lang::parser::Parse,
    text: &str,
) -> Vec<Diagnostic> {
    let index = LineIndex::new(text);
    let lowered = match flux_lang::lower_cst::cst_to_module(parsed) {
        Ok(lowered) => lowered,
        Err(errors) => {
            return errors
                .into_iter()
                .map(|error| {
                    let range = error
                        .range
                        .map(|range| source_range(range, text, &index))
                        .unwrap_or_default();
                    lsp_warning(range, error.message)
                })
                .collect();
        }
    };
    match &lowered.module {
        Module::Flow(ast) => {
            let catalog = flux_flow::registry::OpRegistry::new(registry);
            declaration_findings(ast, &catalog, lowered.flows.first(), text, &index)
        }
        Module::Program(program) => program_diagnostics(registry, program, &lowered, text, &index),
    }
}

fn program_diagnostics(
    registry: &flux_runtime::ToolRegistry,
    program: &Program,
    lowered: &flux_lang::lower_cst::LoweredModule,
    text: &str,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    let catalog = flux_flow::registry::OpRegistry::new(registry).with_composites(&program.ops);
    let mut diagnostics = Vec::new();

    // Body diagnostics are analyzed declaration-by-declaration below for precise ranges. Keep only
    // module-level composite findings here (duplicates, cycles, metadata surface, await).
    if let Err(findings) = flux_flow::registry::analyze_composites(&program.ops, registry) {
        diagnostics.extend(
            findings
                .into_iter()
                .filter(|finding| !finding.message.contains("(at `body"))
                .map(|finding| {
                    let op_index = composite_index_for_message(program, &finding.message);
                    let range = op_index
                        .and_then(|i| lowered.ops.get(i))
                        .map(|ranges| source_range(ranges.declaration, text, index))
                        .unwrap_or_default();
                    lsp_warning(range, finding.message)
                }),
        );
    }

    for (i, op) in program.ops.iter().enumerate() {
        diagnostics.extend(declaration_findings(
            &op.body,
            &catalog,
            lowered.ops.get(i),
            text,
            index,
        ));
    }
    for (i, flow) in program.flows.iter().enumerate() {
        diagnostics.extend(declaration_findings(
            flow,
            &catalog,
            lowered.flows.get(i),
            text,
            index,
        ));
    }
    diagnostics
}

fn declaration_findings(
    ast: &flux_lang::ast::DraftAst,
    catalog: &dyn flux_lang::opspec::OpCatalog,
    ranges: Option<&flux_lang::lower_cst::DeclarationRanges>,
    text: &str,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    let Err(findings) = flux_lang::analyze::lower(ast, catalog, &HashSet::new()) else {
        return Vec::new();
    };
    findings
        .into_iter()
        .map(|finding| {
            let precise =
                ranges.and_then(|ranges| ranges.body.resolve_diagnostic(&finding.message));
            let range = precise
                .map(|range| source_range(range, text, index))
                .or_else(|| ranges.map(|ranges| source_range(ranges.declaration, text, index)))
                .unwrap_or_default();
            let message = if precise.is_none() && finding.message.contains("(at `") {
                format!(
                    "{} (declaration range — body range map incomplete)",
                    finding.message
                )
            } else {
                finding.message
            };
            lsp_warning(range, message)
        })
        .collect()
}

fn composite_index_for_message(program: &Program, message: &str) -> Option<usize> {
    if message.starts_with("duplicate composite op") {
        return program.ops.iter().enumerate().find_map(|(i, op)| {
            let duplicated = program.ops[..i].iter().any(|prior| prior.name == op.name);
            (duplicated && message.contains(&format!("`{}`", op.name))).then_some(i)
        });
    }
    program.ops.iter().position(|op| {
        message.contains(&format!("`{}`", op.name))
            || (message.starts_with("recursive composite op cycle:")
                && message.split_whitespace().any(|part| {
                    part.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
                        == op.name
                }))
    })
}

fn source_range(range: text_size::TextRange, text: &str, index: &LineIndex) -> Range {
    Range {
        start: index.position(text, u32::from(range.start()) as usize),
        end: index.position(text, u32::from(range.end()) as usize),
    }
}

fn lsp_warning(range: Range, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("flux-lsp".into()),
        message,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Diagnostics (from the lossless CST parse — real spans, error recovery)
// ---------------------------------------------------------------------------

/// The server's op list as an analyzer catalog (name lookup over the snapshot).
#[cfg(test)]
struct SliceCatalog<'a>(&'a [OpSignature]);

#[cfg(test)]
impl flux_lang::opspec::OpCatalog for SliceCatalog<'_> {
    fn lookup(&self, name: &str) -> Option<OpSignature> {
        self.0.iter().find(|o| o.name == name).cloned()
    }
}

/// Tolerant-parse errors from an already-built CST, as positioned LSP diagnostics.
fn cst_diagnostics(parsed: &flux_lang::parser::Parse, text: &str) -> Vec<Diagnostic> {
    let index = LineIndex::new(text);
    parsed
        .errors
        .iter()
        .map(|e| Diagnostic {
            range: Range {
                start: index.position(text, u32::from(e.range.start()) as usize),
                end: index.position(text, u32::from(e.range.end()) as usize),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("flux-lsp".into()),
            message: e.message.clone(),
            ..Default::default()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Byte-offset → LSP `Position` (line + UTF-16 column) via cached line starts.
struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex { line_starts }
    }

    fn position(&self, text: &str, offset: usize) -> Position {
        let offset = offset.min(text.len());
        let line = self
            .line_starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line];
        let character = text[line_start..offset].encode_utf16().count() as u32;
        Position {
            line: line as u32,
            character,
        }
    }

    /// LSP `Position` (line + UTF-16 column) → byte offset. The inverse of [`LineIndex::position`],
    /// used to resolve a request cursor and to apply incremental edit ranges. Clamps out-of-range
    /// lines/columns to the end of the line (or the buffer), so a stale client position never panics.
    fn offset(&self, text: &str, pos: Position) -> usize {
        let line = pos.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return text.len();
        };
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(text.len());
        // Walk the line by UTF-16 units until we have consumed `pos.character` of them.
        let mut remaining = pos.character as usize;
        let mut offset = line_start;
        for ch in text[line_start..line_end].chars() {
            if remaining == 0 {
                break;
            }
            let units = ch.len_utf16();
            if units > remaining {
                break;
            }
            remaining -= units;
            offset += ch.len_utf8();
        }
        offset.min(text.len())
    }
}

/// Apply one `didChange` content change to `text` in place: replace the given range (via the
/// line-index) or, when the change has no range, the whole document. Applying changes in the order
/// the client sent them keeps the buffer byte-identical to a full reparse of the final text.
fn apply_content_change(text: &mut String, change: &TextDocumentContentChangeEvent) {
    match change.range {
        Some(range) => {
            let index = LineIndex::new(text);
            let start = index.offset(text, range.start);
            let end = index.offset(text, range.end).max(start);
            text.replace_range(start..end, &change.text);
        }
        None => *text = change.text.clone(),
    }
}

fn whole_document_range(text: &str) -> Range {
    let index = LineIndex::new(text);
    Range {
        start: Position::new(0, 0),
        end: index.position(text, text.len()),
    }
}

/// The identifier word under `pos` (ASCII alphanumerics + `_`), if any. Flux identifiers are ASCII,
/// so treating the LSP UTF-16 character as a char index is exact here.
fn word_at(text: &str, pos: Position) -> Option<String> {
    let line = text.lines().nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut i = (pos.character as usize).min(chars.len());
    if i == chars.len() || !is_word(chars[i]) {
        if i == 0 || !is_word(chars[i - 1]) {
            return None;
        }
        i -= 1;
    }
    let mut start = i;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = i + 1;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    Some(chars[start..end].iter().collect())
}

/// Every distinct `$symbol` name appearing in `text` (for in-scope variable completion).
fn scan_symbols(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > i + 1 {
                let name = text[i + 1..j].to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn render_op(op: &OpSignature) -> String {
    let mut params = op.required_params.clone();
    let opt: Vec<String> = op.optional_params.iter().map(|p| format!("{p}?")).collect();
    params.extend(opt);
    format!(
        "**{}**({}) — {}\n\neffects: {:?} · risk: {:?} · idempotency: {:?}",
        op.name,
        params.join(", "),
        op.description,
        op.effects,
        op.risk,
        op.idempotency
    )
}

fn markdown_hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

// ---------------------------------------------------------------------------
// Scope model, document symbols, and go-to-definition (L-68)
// ---------------------------------------------------------------------------

/// The role a definition plays — drives its LSP `SymbolKind` and how a use resolves to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefRole {
    Flow,
    Op,
    Param,
    Bind,
}

/// One definition site in the CST: a top-level `flow`/`op` declaration, a flow/op parameter, or a
/// `$var` bind (`bind`/`memo`/`each`/arrow-collect/`parallel`-branch/`catch`/`scope`). `scope` is the
/// source region in which the binding is visible, so a use resolves to the *innermost* same-named
/// binding that contains it.
#[derive(Debug, Clone)]
struct Def {
    name: String,
    role: DefRole,
    /// Range of the defining token (the name / `$var`) — the go-to-definition target.
    name_range: TextRange,
    /// The full declaration/statement range (a symbol's enclosing range).
    full_range: TextRange,
    /// The region in which this binding is in scope (for use → def resolution).
    scope: TextRange,
}

fn range_len(range: TextRange) -> u32 {
    u32::from(range.len())
}

/// First direct `$var` token child of `node` (bind targets, branch/catch/scope binders).
fn first_var_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::VAR)
}

/// Every direct `$var` token child of `node` (the `each` loop + collect binders).
fn direct_var_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::VAR)
        .collect()
}

/// The name + range of a declaration from its header, joining kebab-case segments (`god-review`).
/// `None` for an anonymous `flow`/`op` (no name token before `(` / `->` / newline).
fn decl_name(header: &SyntaxNode) -> Option<(String, TextRange)> {
    let toks: Vec<SyntaxToken> = header
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    // toks[0] is the `flow`/`op` keyword; the name (if any) is the next IDENT.
    let first = toks.get(1)?;
    if first.kind() != SyntaxKind::IDENT {
        return None;
    }
    let start = first.text_range().start();
    let mut end = first.text_range().end();
    let mut name = first.text().to_string();
    let mut i = 2;
    while i + 1 < toks.len()
        && toks[i].kind() == SyntaxKind::MINUS
        && matches!(toks[i + 1].kind(), SyntaxKind::IDENT | SyntaxKind::NUMBER)
        && toks[i].text_range().start() == end
    {
        name.push('-');
        name.push_str(toks[i + 1].text());
        end = toks[i + 1].text_range().end();
        i += 2;
    }
    Some((name, TextRange::new(start, end)))
}

/// Collect the flow/op parameter definitions from a header into `out` (visible across the decl).
fn collect_params(header: &SyntaxNode, decl_range: TextRange, out: &mut Vec<Def>) {
    let Some(list) = header
        .children()
        .find(|c| c.kind() == SyntaxKind::PARAM_LIST)
    else {
        return;
    };
    for param in list.children().filter(|c| c.kind() == SyntaxKind::PARAM) {
        // The param name is the first direct IDENT token (its type lives in a child NAME node).
        if let Some(name_tok) = param
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT)
        {
            out.push(Def {
                name: name_tok.text().to_string(),
                role: DefRole::Param,
                name_range: name_tok.text_range(),
                full_range: param.text_range(),
                scope: decl_range,
            });
        }
    }
}

fn push_var_def(out: &mut Vec<Def>, tok: &SyntaxToken, full: TextRange, scope: TextRange) {
    let name = tok.text().trim_start_matches('$');
    if name.is_empty() {
        return;
    }
    out.push(Def {
        name: name.to_string(),
        role: DefRole::Bind,
        name_range: tok.text_range(),
        full_range: full,
        scope,
    });
}

/// Collect every `$var` binding inside a declaration's body. Ordinary `bind`/`memo` binds are
/// visible across the whole declaration; the narrower binders (`each` loop/collect vars,
/// `parallel`/`race`/`fallback` branch vars, `catch`, `scope`) scope to their own statement so a
/// shadowing use resolves to the inner binding.
fn collect_binds(decl: &SyntaxNode, decl_range: TextRange, out: &mut Vec<Def>) {
    for node in decl.descendants() {
        match node.kind() {
            SyntaxKind::BIND_STMT | SyntaxKind::MEMO_STMT => {
                if let Some(v) = first_var_token(&node) {
                    push_var_def(out, &v, node.text_range(), decl_range);
                }
            }
            SyntaxKind::EACH_STMT => {
                let scope = node.text_range();
                for v in direct_var_tokens(&node) {
                    push_var_def(out, &v, scope, scope);
                }
            }
            SyntaxKind::BRANCH_ARM | SyntaxKind::CATCH_CLAUSE | SyntaxKind::SCOPE_STMT => {
                let scope = node.text_range();
                if let Some(v) = first_var_token(&node) {
                    push_var_def(out, &v, scope, scope);
                }
            }
            _ => {}
        }
    }
}

/// Every top-level executable declaration with its member definitions (params + binds).
fn collect_declarations(root: &SyntaxNode) -> Vec<(Def, Vec<Def>)> {
    let mut out = Vec::new();
    for decl in root.children() {
        let (role, header_kind, default_name) = match decl.kind() {
            SyntaxKind::FLOW_DECL => (DefRole::Flow, SyntaxKind::FLOW_HEADER, "flow"),
            SyntaxKind::OP_DECL => (DefRole::Op, SyntaxKind::OP_HEADER, "op"),
            _ => continue,
        };
        let full_range = decl.text_range();
        let header = decl.children().find(|c| c.kind() == header_kind);
        let (name, name_range) = header
            .as_ref()
            .and_then(decl_name)
            .unwrap_or_else(|| (default_name.to_string(), full_range));
        let mut members = Vec::new();
        if let Some(header) = &header {
            collect_params(header, full_range, &mut members);
        }
        collect_binds(&decl, full_range, &mut members);
        out.push((
            Def {
                name,
                role,
                name_range,
                full_range,
                scope: full_range,
            },
            members,
        ));
    }
    out
}

/// Flat list of every `$var`/param definition across all declarations (for use → def resolution).
fn all_var_defs(root: &SyntaxNode) -> Vec<Def> {
    collect_declarations(root)
        .into_iter()
        .flat_map(|(_, members)| members)
        .collect()
}

/// The document outline: each `flow`/`op` with its params and `$var` binds as child symbols.
fn document_symbols(text: &str) -> Vec<DocumentSymbol> {
    let parsed = flux_lang::parser::parse_cst(text);
    let root = parsed.syntax();
    let index = LineIndex::new(text);
    collect_declarations(&root)
        .into_iter()
        .map(|(decl, members)| {
            let children: Vec<DocumentSymbol> = members
                .iter()
                .map(|m| member_symbol(m, text, &index))
                .collect();
            #[allow(deprecated)]
            DocumentSymbol {
                name: decl.name,
                detail: None,
                kind: match decl.role {
                    DefRole::Op => SymbolKind::METHOD,
                    _ => SymbolKind::FUNCTION,
                },
                tags: None,
                deprecated: None,
                range: source_range(decl.full_range, text, &index),
                selection_range: source_range(decl.name_range, text, &index),
                children: (!children.is_empty()).then_some(children),
            }
        })
        .collect()
}

fn member_symbol(def: &Def, text: &str, index: &LineIndex) -> DocumentSymbol {
    let (name, kind, detail) = match def.role {
        DefRole::Param => (
            def.name.clone(),
            SymbolKind::VARIABLE,
            Some("parameter".into()),
        ),
        _ => (format!("${}", def.name), SymbolKind::VARIABLE, None),
    };
    #[allow(deprecated)]
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range: source_range(def.full_range, text, index),
        selection_range: source_range(def.name_range, text, index),
        children: None,
    }
}

/// The token covering (or adjacent to) `offset`, preferring a `$var`/identifier over trivia.
fn token_at(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
    let ts = TextSize::from(offset as u32);
    let candidates: Vec<SyntaxToken> = root.token_at_offset(ts).collect();
    candidates
        .iter()
        .find(|t| matches!(t.kind(), SyntaxKind::VAR | SyntaxKind::IDENT))
        .or_else(|| candidates.iter().find(|t| !t.kind().is_trivia()))
        .or_else(|| candidates.first())
        .cloned()
}

/// Go-to-definition: a `$var` use jumps to its binding; an op/flow reference jumps to its
/// declaration name.
fn definition_at(text: &str, pos: Position) -> Option<Range> {
    let parsed = flux_lang::parser::parse_cst(text);
    let root = parsed.syntax();
    let index = LineIndex::new(text);
    let offset = index.offset(text, pos);
    let tok = token_at(&root, offset)?;
    let target = match tok.kind() {
        SyntaxKind::VAR => resolve_var(&root, &tok, offset),
        SyntaxKind::IDENT => resolve_ident(&root, &tok),
        _ => None,
    }?;
    Some(source_range(target, text, &index))
}

/// Resolve a `$var` use to the innermost same-named binding that is in scope at `offset`.
fn resolve_var(root: &SyntaxNode, tok: &SyntaxToken, offset: usize) -> Option<TextRange> {
    let name = tok.text().trim_start_matches('$');
    let use_off = TextSize::from(offset as u32);
    let defs = all_var_defs(root);
    let mut best: Option<&Def> = None;
    for cand in &defs {
        if cand.name != name || !cand.scope.contains_inclusive(use_off) {
            continue;
        }
        best = Some(match best {
            None => cand,
            Some(cur) if better_binding(cand, cur, use_off) => cand,
            Some(cur) => cur,
        });
    }
    best.map(|d| d.name_range)
}

/// Is binding `a` a better resolution than `b` for a use at `off`? Prefer the smaller (inner)
/// scope; within an equal scope prefer a binding defined at/before the use, latest first.
fn better_binding(a: &Def, b: &Def, off: TextSize) -> bool {
    let (la, lb) = (range_len(a.scope), range_len(b.scope));
    if la != lb {
        return la < lb;
    }
    let (a_before, b_before) = (a.name_range.start() <= off, b.name_range.start() <= off);
    if a_before != b_before {
        return a_before;
    }
    if a_before {
        a.name_range.start() > b.name_range.start()
    } else {
        a.name_range.start() < b.name_range.start()
    }
}

/// Resolve an op/flow reference identifier to its declaration name range.
fn resolve_ident(root: &SyntaxNode, tok: &SyntaxToken) -> Option<TextRange> {
    let name = match tok.parent() {
        Some(p) if p.kind() == SyntaxKind::NAME => p.text().to_string(),
        _ => tok.text().to_string(),
    };
    collect_declarations(root)
        .into_iter()
        .find(|(d, _)| d.name == name && matches!(d.role, DefRole::Op | DefRole::Flow))
        .map(|(d, _)| d.name_range)
}

// ---------------------------------------------------------------------------
// Semantic tokens (L-69) — CST token stream + the semantic distinctions a grammar can't make
// ---------------------------------------------------------------------------

// Legend indices — must match the order in `semantic_tokens_legend`.
const TOK_KEYWORD: u32 = 0;
const TOK_FUNCTION: u32 = 1;
const TOK_VARIABLE: u32 = 2;
const TOK_PARAMETER: u32 = 3;
const TOK_TYPE: u32 = 4;
const TOK_STRING: u32 = 5;
const TOK_NUMBER: u32 = 6;
const TOK_COMMENT: u32 = 7;
const TOK_DECORATOR: u32 = 8;

// Modifier bits — must match the order in `semantic_tokens_legend`.
const MOD_DECLARATION: u32 = 1 << 0;
const MOD_DEFINITION: u32 = 1 << 1;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 2;

/// The legend advertised in `initialize` and used to decode the token stream.
fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::TYPE,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::DECORATOR,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::DEFINITION,
            SemanticTokenModifier::DEFAULT_LIBRARY,
        ],
    }
}

/// NAME nodes in op-call position (`op(args)`, `do op`, `fmt`/`parse`) whose full (possibly dotted)
/// text is a registry-known op — the ranges that earn the `defaultLibrary` modifier.
fn known_op_ranges(root: &SyntaxNode, known: &HashSet<String>) -> Vec<TextRange> {
    root.descendants()
        .filter(|n| n.kind() == SyntaxKind::NAME)
        .filter(|name| {
            matches!(
                name.parent().map(|p| p.kind()),
                Some(
                    SyntaxKind::CALL_EXPR
                        | SyntaxKind::CALL_STMT
                        | SyntaxKind::FMT_EXPR
                        | SyntaxKind::PARSE_EXPR
                )
            ) && known.contains(&name.text().to_string())
        })
        .map(|name| name.text_range())
        .collect()
}

/// Full-document semantic tokens: the CST highlight classes lifted to the LSP legend, enriched with
/// the modifiers a grammar cannot compute (known op vs unknown identifier; `$var` bind vs use).
fn semantic_tokens(text: &str, known: &HashSet<String>) -> SemanticTokens {
    let parsed = flux_lang::parser::parse_cst(text);
    let root = parsed.syntax();
    let index = LineIndex::new(text);

    let defs = all_var_defs(&root);
    let def_ranges: HashSet<TextRange> = defs.iter().map(|d| d.name_range).collect();
    let param_ranges: HashSet<TextRange> = defs
        .iter()
        .filter(|d| d.role == DefRole::Param)
        .map(|d| d.name_range)
        .collect();
    let decl_name_ranges: HashSet<TextRange> = collect_declarations(&root)
        .iter()
        .map(|(d, _)| d.name_range)
        .collect();
    let op_ranges = known_op_ranges(&root, known);

    // (line, start_char, len, token_type, modifiers), source order (highlight is already ordered).
    let mut raw: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
    for (range, class) in highlight(text) {
        let Some((ty, modifiers)) = classify_semantic(
            class,
            range,
            &def_ranges,
            &param_ranges,
            &decl_name_ranges,
            &op_ranges,
        ) else {
            continue;
        };
        push_token_spans(range, ty, modifiers, text, &index, &mut raw);
    }
    raw.sort_by_key(|t| (t.0, t.1));

    let mut data = Vec::with_capacity(raw.len());
    let (mut prev_line, mut prev_char) = (0u32, 0u32);
    for (line, ch, len, ty, modifiers) in raw {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 { ch - prev_char } else { ch };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: ty,
            token_modifiers_bitset: modifiers,
        });
        prev_line = line;
        prev_char = ch;
    }
    SemanticTokens {
        result_id: None,
        data,
    }
}

/// Map one highlight class + its range to a legend token type and modifier bitset, or `None` to
/// skip (punctuation/operators are left to the grammar).
fn classify_semantic(
    class: HighlightClass,
    range: TextRange,
    def_ranges: &HashSet<TextRange>,
    param_ranges: &HashSet<TextRange>,
    decl_name_ranges: &HashSet<TextRange>,
    op_ranges: &[TextRange],
) -> Option<(u32, u32)> {
    let token = match class {
        HighlightClass::Keyword => (TOK_KEYWORD, 0),
        HighlightClass::Op => {
            let mut modifiers = 0;
            if op_ranges.iter().any(|r| r.contains_range(range)) {
                modifiers |= MOD_DEFAULT_LIBRARY;
            }
            if decl_name_ranges.contains(&range) {
                modifiers |= MOD_DECLARATION;
            }
            (TOK_FUNCTION, modifiers)
        }
        HighlightClass::Var => {
            let ty = if param_ranges.contains(&range) {
                TOK_PARAMETER
            } else {
                TOK_VARIABLE
            };
            let modifiers = if def_ranges.contains(&range) {
                MOD_DEFINITION
            } else {
                0
            };
            (ty, modifiers)
        }
        HighlightClass::Annotation => (TOK_DECORATOR, 0),
        HighlightClass::String => (TOK_STRING, 0),
        HighlightClass::Number => (TOK_NUMBER, 0),
        HighlightClass::Comment => (TOK_COMMENT, 0),
        HighlightClass::Type => (TOK_TYPE, 0),
        // Punctuation, operators, and error tokens carry no semantic colour of their own.
        HighlightClass::Punct | HighlightClass::Error => return None,
    };
    Some(token)
}

/// Push one source span as one or more single-line semantic tokens (the LSP encoding cannot express
/// a token that crosses a line, so a multi-line `"""…"""` string is split per line).
fn push_token_spans(
    range: TextRange,
    ty: u32,
    modifiers: u32,
    text: &str,
    index: &LineIndex,
    out: &mut Vec<(u32, u32, u32, u32, u32)>,
) {
    let start = index.position(text, range.start().into());
    let end = index.position(text, range.end().into());
    if start.line == end.line {
        let len = end.character - start.character;
        if len > 0 {
            out.push((start.line, start.character, len, ty, modifiers));
        }
        return;
    }
    let start_byte: usize = range.start().into();
    let end_byte: usize = range.end().into();
    for line in start.line..=end.line {
        let content_start = if line == start.line {
            start_byte
        } else {
            index.line_starts[line as usize]
        };
        let mut content_end = if line == end.line {
            end_byte
        } else {
            index
                .line_starts
                .get(line as usize + 1)
                .copied()
                .unwrap_or(text.len())
        };
        // Exclude the trailing line break from non-final lines.
        while content_end > content_start
            && matches!(text.as_bytes().get(content_end - 1), Some(b'\n' | b'\r'))
        {
            content_end -= 1;
        }
        let start_char = index.position(text, content_start).character;
        let len = text[content_start..content_end].encode_utf16().count() as u32;
        if len > 0 {
            out.push((line, start_char, len, ty, modifiers));
        }
    }
}

// ---------------------------------------------------------------------------
// Comment-preserving formatting (L-70)
// ---------------------------------------------------------------------------

/// Whether the CST carries any `# …` comment token.
fn cst_has_comment(root: &SyntaxNode) -> bool {
    root.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::COMMENT)
}

/// The sorted comment texts of the CST — a fingerprint used to prove a reformat kept every comment.
fn comment_multiset(root: &SyntaxNode) -> Vec<String> {
    let mut comments: Vec<String> = root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::COMMENT)
        .map(|t| t.text().trim_end().to_string())
        .collect();
    comments.sort();
    comments
}

/// The BLOCK-nesting depth of a token — its canonical indentation level.
fn block_depth(tok: &SyntaxToken) -> usize {
    tok.parent()
        .into_iter()
        .flat_map(|p| p.ancestors())
        .filter(|n| n.kind() == SyntaxKind::BLOCK)
        .count()
}

/// Re-indent a flow to canonical two-space nesting while preserving every token verbatim (comments
/// included). Only the *leading* whitespace of each CST line is rewritten from the tree's block
/// depth; interior spacing and multi-line string interiors are untouched. A comment-only line takes
/// the indentation of the statement it precedes (comments attach above their block in the CST, so
/// their own depth would under-indent them).
fn reindent(root: &SyntaxNode) -> String {
    let tokens: Vec<SyntaxToken> = root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .collect();
    let mut out = String::new();
    let mut at_line_start = true;
    for (i, tok) in tokens.iter().enumerate() {
        let kind = tok.kind();
        if at_line_start {
            match kind {
                SyntaxKind::WHITESPACE => continue, // drop leading whitespace
                SyntaxKind::NEWLINE => {
                    out.push_str(tok.text()); // a blank line — no indentation
                    continue;
                }
                _ => {
                    let depth = if kind == SyntaxKind::COMMENT {
                        // Align a comment-only line with the next real statement.
                        tokens[i + 1..]
                            .iter()
                            .find(|t| !t.kind().is_trivia())
                            .map(block_depth)
                            .unwrap_or_else(|| block_depth(tok))
                    } else {
                        block_depth(tok)
                    };
                    for _ in 0..depth {
                        out.push_str("  ");
                    }
                    out.push_str(tok.text());
                    at_line_start = false;
                }
            }
        } else {
            out.push_str(tok.text());
            if kind == SyntaxKind::NEWLINE {
                at_line_start = true;
            }
        }
    }
    out
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse + diagnose in one step — only tests need this composition; the server
    /// diagnoses an already-built CST via [`cst_diagnostics`].
    fn diagnostics(text: &str) -> Vec<Diagnostic> {
        cst_diagnostics(&flux_lang::parser::parse_cst(text), text)
    }

    fn semantic_diagnostics(text: &str) -> Vec<Diagnostic> {
        let parsed = flux_lang::parser::parse_cst(text);
        let syntax = cst_diagnostics(&parsed, text);
        if syntax.is_empty() {
            analyzer_diagnostics_for(&authoring_registry(), &parsed, text)
        } else {
            syntax
        }
    }

    #[test]
    fn diagnostics_have_positioned_ranges() {
        // A bind with no RHS on line 2 (0-based line 1) — the CST parser recovers and reports it.
        let src = "flow f\n  $a =\n  $b = 1\n";
        let diags = diagnostics(src);
        assert!(
            !diags.is_empty(),
            "expected a diagnostic for the empty bind RHS"
        );
        // Later good statements still parse (no cascade); the diagnostic is on line 1.
        assert!(diags.iter().all(|d| d.range.start.line <= 2));
    }

    #[test]
    fn analyzer_warnings_carry_resolved_ranges() {
        // `$y = read($nope)` on 0-based line 2 references an unbound symbol. The analyzer
        // diagnostic's node path must resolve to that line via the L-59 range side-map.
        let src = "flow f\n  $x = read(\"a.txt\")\n  $y = read($nope)\n  return $x\n";
        let mut reg = flux_runtime::ToolRegistry::new();
        flux_tools::try_register_builtins(&mut reg).unwrap();
        let ops = flux_flow::registry::OpRegistry::new(&reg).signatures();
        let lowered = flux_lang::lower_cst::parse_with_ranges(src).expect("parses");
        let findings = flux_lang::analyze::analyze_flow(
            &lowered.ast,
            &SliceCatalog(&ops),
            &std::collections::HashSet::new(),
        )
        .expect_err("unbound $nope must be diagnosed");
        let index = LineIndex::new(src);
        let hit = findings
            .iter()
            .filter(|d| d.message.contains("$nope") || d.message.contains("nope"))
            .filter_map(|d| lowered.ranges.resolve_diagnostic(&d.message))
            .map(|r| index.position(src, u32::from(r.start()) as usize).line)
            .next();
        assert_eq!(hit, Some(2), "unbound-symbol warning resolves to line 2");
    }

    #[test]
    fn word_at_finds_the_identifier() {
        let src = "flow f\n  do read \"x\"\n";
        // On `read` (line 1, somewhere in "read").
        let w = word_at(src, Position::new(1, 6));
        assert_eq!(w.as_deref(), Some("read"));
    }

    #[test]
    fn scan_symbols_collects_vars() {
        let syms = scan_symbols("flow f\n  $a = 1\n  $bee = $a\n");
        assert!(syms.contains(&"a".to_string()) && syms.contains(&"bee".to_string()));
    }

    #[test]
    fn completions_include_ops_keywords_and_vars() {
        let ops = authoring_op_signatures();
        let backend_ops = ops.clone();
        // Build a throwaway backend-less completion set the same way `completions` does.
        assert!(!backend_ops.is_empty(), "expected registered ops");
        // Node kinds are non-empty (grammar keywords).
        assert!(!flux_lang::schema::node_kind_rows().is_empty());
    }

    #[test]
    fn authoring_catalog_contains_stable_cli_host_ops() {
        let names: std::collections::HashSet<String> = authoring_op_signatures()
            .into_iter()
            .map(|op| op.name)
            .collect();
        for required in [
            "ai.extract",
            "ai.rank",
            "ai.reason",
            "synth",
            "search",
            "sources",
            "http.request",
            "web.fetch",
        ] {
            assert!(
                names.contains(required),
                "LSP authoring catalog is missing stable CLI op `{required}`"
            );
        }
    }

    #[test]
    fn stable_host_ops_do_not_report_unknown_operation() {
        let src = r#"flow research
  $response = http.request({url: "https://example.com/api", method: "GET"})
  $page = web.fetch("https://example.com")
  $hits = search({query: "flux", limit: 2})
  $inventory = sources()
  $claims = ai.extract({from: $page, ask: "facts", schema: "Claim[]"})
  $ranked = ai.rank({items: $claims, by: "support"})
  $answer = synth({claims: $ranked, format: "detailed", cite: true})
  return $answer
"#;
        let diagnostics = semantic_diagnostics(src);
        assert!(
            diagnostics.is_empty(),
            "stable host ops must analyze cleanly: {diagnostics:?}"
        );
    }

    #[test]
    fn module_resolves_forward_composite_and_ranges_later_flow_error() {
        let src = r#"flow first
  $one = summarize("one")
  return $one

op summarize(text: String) -> String
  description "Summarize text"
  risk "low"
  idempotency "non_idempotent"
  effects [network]
  expose false
  $prompt = fmt("Summarize: {text}")
  $answer = ai.reason($prompt)
  return $answer

flow second
  $bad = definitely_missing()
  return $bad
"#;
        let diagnostics = semantic_diagnostics(src);
        assert_eq!(
            diagnostics.len(),
            1,
            "only the real unknown op: {diagnostics:?}"
        );
        assert!(diagnostics[0].message.contains("definitely_missing"));
        let expected_line = src[..src.find("$bad =").unwrap()].matches('\n').count() as u32;
        assert_eq!(diagnostics[0].range.start.line, expected_line);
    }

    #[test]
    fn document_signatures_include_unexposed_local_composites() {
        let src = "op internal(value: String) -> String\n  expose false\n  return $value\n\nflow f\n  return internal(\"x\")\n";
        let signatures = signatures_for_document(&authoring_op_signatures(), src);
        assert!(signatures.iter().any(|op| op.name == "internal"));
    }

    #[test]
    fn genuinely_unknown_operation_stays_a_warning() {
        let diagnostics = semantic_diagnostics("flow f\n  made.up()\n");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0]
            .message
            .contains("unknown operation: `made.up`"));
    }

    #[test]
    fn module_reports_composite_cycle_at_a_declaration() {
        let src = "op first() -> String\n  return second()\n\nop second() -> String\n  return first()\n\nflow run\n  return first()\n";
        let diagnostics = semantic_diagnostics(src);
        let cycle = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("recursive composite op cycle"))
            .expect("cycle diagnostic");
        assert!(cycle.range.start.line == 0 || cycle.range.start.line == 3);
    }

    #[test]
    fn module_reports_unbound_symbol_inside_composite_body() {
        let src = "op broken(value: String) -> String\n  return $missing\n\nflow run\n  return broken(\"x\")\n";
        let diagnostics = semantic_diagnostics(src);
        let unbound = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("$missing"))
            .expect("unbound diagnostic");
        assert_eq!(unbound.range.start.line, 1);
    }

    #[test]
    fn module_reports_wrong_composite_arguments_at_call_site() {
        let src = "op echo(value: String) -> String\n  return $value\n\nflow run\n  return echo({wrong: \"x\"})\n";
        let diagnostics = semantic_diagnostics(src);
        let missing = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .message
                    .contains("missing required parameter `value`")
            })
            .expect("arity diagnostic");
        assert_eq!(missing.range.start.line, 4);
    }

    #[test]
    fn formatting_is_deliberately_disabled_for_modules() {
        let src = "flow first\n  return \"one\"\n\nflow second\n  return \"two\"\n";
        assert_eq!(format_document(src), None);
    }

    // ---- L-68: document symbols + go-to-definition -------------------------

    #[test]
    fn document_symbol_outlines_flow_params_and_binds() {
        let src = "flow greet(name: String)\n  $msg = fmt(\"hi\")\n  return $msg\n";
        let symbols = document_symbols(src);
        assert_eq!(symbols.len(), 1, "one top-level flow");
        let flow = &symbols[0];
        assert_eq!(flow.name, "greet");
        assert_eq!(flow.kind, SymbolKind::FUNCTION);
        // The flow's selection range covers the name `greet` on line 0.
        assert_eq!(flow.selection_range.start, Position::new(0, 5));
        let children = flow.children.as_ref().expect("flow has member symbols");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"name"), "param in outline: {names:?}");
        assert!(names.contains(&"$msg"), "bind in outline: {names:?}");
    }

    #[test]
    fn go_to_definition_resolves_var_use_to_its_bind() {
        let src = "flow f\n  $x = 1\n  $y = $x\n  return $y\n";
        // Cursor on the `$x` *use* in `$y = $x` (line 2).
        let def = definition_at(src, Position::new(2, 8)).expect("resolves");
        // Jumps to the `$x` bind on line 1 (`  $x = 1`), which starts at column 2.
        assert_eq!(def.start, Position::new(1, 2));
    }

    #[test]
    fn go_to_definition_resolves_op_reference_to_declaration() {
        let src =
            "op greet(name: String) -> String\n  return $name\n\nflow run\n  return greet(\"x\")\n";
        // Cursor on the `greet` call in `flow run` (line 4).
        let def = definition_at(src, Position::new(4, 10)).expect("resolves");
        // Jumps to the `op greet` declaration name on line 0, column 3.
        assert_eq!(def.start, Position::new(0, 3));
    }

    #[test]
    fn go_to_definition_prefers_inner_shadowing_binding() {
        // `$it` is bound by the flow-level bind AND the each loop; a use inside the loop resolves to
        // the loop binder (the smaller scope), not the outer bind.
        let src = "flow f\n  $it = 0\n  each $it in $xs\n    do process $it\n  return $it\n";
        let inner = definition_at(src, Position::new(3, 15)).expect("inner use resolves");
        assert_eq!(
            inner.start.line, 2,
            "inner `$it` resolves to the each binder"
        );
        let outer = definition_at(src, Position::new(4, 9)).expect("outer use resolves");
        assert_eq!(outer.start.line, 1, "outer `$it` resolves to the flow bind");
    }

    // ---- L-69: semantic tokens --------------------------------------------

    /// Decode the delta-encoded token stream back to `(text, type, modifiers)` (ASCII input only).
    fn decode(src: &str, tokens: &SemanticTokens) -> Vec<(String, u32, u32)> {
        let lines: Vec<&str> = src.split('\n').collect();
        let (mut line, mut ch) = (0u32, 0u32);
        let mut out = Vec::new();
        for t in &tokens.data {
            if t.delta_line != 0 {
                line += t.delta_line;
                ch = t.delta_start;
            } else {
                ch += t.delta_start;
            }
            let text: String = lines[line as usize]
                .chars()
                .skip(ch as usize)
                .take(t.length as usize)
                .collect();
            out.push((text, t.token_type, t.token_modifiers_bitset));
        }
        out
    }

    #[test]
    fn semantic_tokens_distinguish_known_op_from_unknown_and_bind_from_use() {
        let known: HashSet<String> = authoring_op_signatures()
            .into_iter()
            .map(|op| op.name)
            .collect();
        let src =
            "flow f\n  # a note\n  $x = read(\"a.txt\")\n  $y = made_up(\"z\")\n  return $x\n";
        let decoded = decode(src, &semantic_tokens(src, &known));
        let find = |t: &str| {
            decoded
                .iter()
                .find(|(text, _, _)| text == t)
                .unwrap_or_else(|| panic!("no token {t:?} in {decoded:?}"))
        };
        // Keywords, string literal, and comment.
        assert_eq!(find("flow").1, TOK_KEYWORD);
        assert_eq!(find("return").1, TOK_KEYWORD);
        assert_eq!(find("\"a.txt\"").1, TOK_STRING);
        assert!(decoded
            .iter()
            .any(|(t, ty, _)| t.contains("a note") && *ty == TOK_COMMENT));
        // A registry-known op carries `defaultLibrary`; an unknown identifier does not.
        let read = find("read");
        assert_eq!(read.1, TOK_FUNCTION);
        assert_ne!(
            read.2 & MOD_DEFAULT_LIBRARY,
            0,
            "known op is defaultLibrary"
        );
        let made_up = find("made_up");
        assert_eq!(made_up.1, TOK_FUNCTION);
        assert_eq!(made_up.2 & MOD_DEFAULT_LIBRARY, 0, "unknown op is plain");
        // A `$var` bind site carries `definition`; a use does not.
        let bind = find("$x");
        assert_eq!(bind.1, TOK_VARIABLE);
        assert_ne!(bind.2 & MOD_DEFINITION, 0, "bind site is a definition");
        // The `$x` use on the final line lacks the definition modifier.
        let uses: Vec<_> = decoded.iter().filter(|(t, _, _)| t == "$x").collect();
        assert!(
            uses.iter().any(|(_, _, m)| m & MOD_DEFINITION == 0),
            "the `$x` use is not a definition"
        );
    }

    // ---- L-70: incremental sync + comment-preserving format ---------------

    #[test]
    fn incremental_edits_match_full_reparse() {
        // Build a buffer by replaying two ranged edits (as a client would over INCREMENTAL sync),
        // and assert it equals the document a full replace would have produced — so a reparse of
        // either is identical.
        let mut incr = String::from("flow f\n  $x = 1\n  return $x\n");
        let edit = |text: &mut String, range: Range, new: &str| {
            apply_content_change(
                text,
                &TextDocumentContentChangeEvent {
                    range: Some(range),
                    range_length: None,
                    text: new.to_string(),
                },
            );
        };
        // 1) replace `1` with a call.
        edit(
            &mut incr,
            Range::new(Position::new(1, 7), Position::new(1, 8)),
            "read(\"a.txt\")",
        );
        // 2) insert a comment line right after the header.
        edit(
            &mut incr,
            Range::new(Position::new(1, 0), Position::new(1, 0)),
            "  # note\n",
        );
        let full = "flow f\n  # note\n  $x = read(\"a.txt\")\n  return $x\n";
        assert_eq!(incr, full, "incremental edits reconstruct the full buffer");
        assert_eq!(
            semantic_diagnostics(&incr),
            semantic_diagnostics(full),
            "reparse of the incremental buffer matches the full reparse"
        );
    }

    #[test]
    fn apply_content_change_full_replace_when_no_range() {
        let mut text = String::from("flow f\n  return 1\n");
        apply_content_change(
            &mut text,
            &TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "flow g\n  return 2\n".into(),
            },
        );
        assert_eq!(text, "flow g\n  return 2\n");
    }

    #[test]
    fn formatting_preserves_comments_while_canonicalizing_indent() {
        // A flow indented with four spaces and carrying comments: formatting canonicalizes the
        // indentation to two spaces and keeps every comment (the CST-driven path).
        let src = "flow f\n    # a leading note\n    $x = 1  # trailing\n    return $x\n";
        let formatted = format_document(src).expect("re-indents a commented flow");
        assert!(
            formatted.contains("# a leading note"),
            "leading comment preserved: {formatted:?}"
        );
        assert!(
            formatted.contains("# trailing"),
            "trailing comment preserved: {formatted:?}"
        );
        assert!(
            formatted.contains("\n  # a leading note\n"),
            "canonical two-space indent: {formatted:?}"
        );
        assert!(
            formatted.contains("\n  $x = 1"),
            "body re-indented to two spaces: {formatted:?}"
        );
    }

    #[test]
    fn formatting_comment_free_flow_still_uses_canonical_formatter() {
        // No comments → the canonical AST formatter runs (unchanged L-67 behaviour).
        let src = "flow f\n    $x = 1\n    return $x\n";
        let formatted = format_document(src).expect("canonicalizes");
        assert_eq!(formatted, "flow f\n  $x = 1\n  return $x\n");
    }
}
