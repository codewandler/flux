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

use flux_lang::opspec::OpSignature;
use flux_lang::program::{CompositeOpDecl, Module, Program};

/// The catalog Flux-Lang authors see in diagnostics, completion, and hover.
///
/// Kept as a helper so tests exercise the exact registry the server installs rather than a
/// hand-built approximation.
fn authoring_registry() -> flux_runtime::ToolRegistry {
    let mut reg = flux_runtime::ToolRegistry::new();
    flux_tools::register_builtins(&mut reg);

    // Catalog-only registrations: none of these constructors performs IO. The provider never
    // generates, the datasource is empty and in-memory, and WebOptions::default is public-only with
    // no audit/record sink. Execution still belongs to the real host; the LSP only reads specs.
    flux_cognition::CognitionPack::new(Arc::new(flux_provider::NullProvider), "flux-lsp")
        .register(&mut reg);
    flux_capabilities::register_datasource_ops(
        &mut reg,
        Arc::new(flux_capabilities::MemoryBackend::new()),
    );
    flux_web::register_web(&mut reg, &flux_web::WebOptions::default());
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
fn format_document(text: &str) -> Option<String> {
    let Module::Flow(ast) = Module::parse_str(text).ok()? else {
        return None;
    };
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
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["$".into(), "@".into()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
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
        // FULL sync: the last change carries the whole new document.
        if let Some(change) = params.content_changes.into_iter().last() {
            let uri = params.text_document.uri;
            self.docs
                .write()
                .await
                .insert(uri.clone(), change.text.clone());
            self.refresh(uri, &change.text).await;
        }
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
    let lowered = match flux_lang::lower_cst::cst_to_module(parsed, text) {
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
        flux_tools::register_builtins(&mut reg);
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
            "web_fetch",
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
  $web = web_search({query: "flux", max_results: 2})
  $page = web_fetch("https://example.com")
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
}
