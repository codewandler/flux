//! Executable contract for the public website's hand-maintained mirrors.
//!
//! The node/prelude tables and customer changelog have their own generated-block test in
//! `flux-lang`. This suite covers the remaining cross-crate surfaces that are easy to let drift:
//! CLI command names, registered operations, config examples, plugin-pack membership, SDK package
//! names/lifecycle surfaces, and complete Flux-Lang snippets.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use flux_cognition::{CognitionPack, ConsultTool, DEFAULT_CONSULT_MAX_CALLS};
use flux_core::{Chunk, StopReason};
use flux_provider::{ChunkStream, NullProvider, Provider, Request};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = repo_path(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn normalized_prose(source: &str) -> String {
    source
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn files_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|x| x.to_str()) == Some(extension) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    files_with_extension(root, "md")
}

fn fenced_blocks(markdown: &str, language: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, usize, Vec<String>)> = None;

    for line in markdown.lines() {
        if let Some((indent, ticks, body)) = &mut open {
            let (line_indent, rest) = split_markdown_indent(line);
            let close_ticks = rest.bytes().take_while(|byte| *byte == b'`').count();
            if line_indent == *indent
                && close_ticks >= *ticks
                && rest[close_ticks..].trim().is_empty()
            {
                blocks.push(body.join("\n"));
                open = None;
                continue;
            }

            body.push(strip_markdown_indent(line, *indent).to_string());
            continue;
        }

        let (indent, rest) = split_markdown_indent(line);
        if indent > 3 {
            continue;
        }
        let ticks = rest.bytes().take_while(|byte| *byte == b'`').count();
        if ticks >= 3 && rest[ticks..].trim() == language {
            open = Some((indent, ticks, Vec::new()));
        }
    }

    assert!(open.is_none(), "closed markdown `{language}` code fence");
    blocks
}

fn split_markdown_indent(line: &str) -> (usize, &str) {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent, &line[indent..])
}

fn strip_markdown_indent(line: &str, width: usize) -> &str {
    let present = line
        .bytes()
        .take(width)
        .take_while(|byte| *byte == b' ')
        .count();
    &line[present..]
}

#[test]
fn fenced_block_scanner_handles_commonmark_indentation() {
    let markdown = r#"1. Nested example:

   ```flux
   flow nested
     return "ok"
   ```

Outside the list.

```flux
flow root
  return "ok"
```
"#;

    assert_eq!(
        fenced_blocks(markdown, "flux"),
        ["flow nested\n  return \"ok\"", "flow root\n  return \"ok\"",]
    );
}

fn significant_flux_tokens(source: &str) -> Vec<String> {
    use flux_lang::syntax::SyntaxKind;

    let parsed = flux_lang::parser::parse_cst(source);
    assert!(
        parsed.errors.is_empty(),
        "tokenize strictly parsed Flux source: {:?}",
        parsed.errors
    );
    parsed
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            !matches!(
                token.kind(),
                SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMENT
                    | SyntaxKind::NEWLINE
                    | SyntaxKind::INDENT
                    | SyntaxKind::DEDENT
            )
        })
        .filter(|token| !token.text().is_empty())
        .map(|token| token.text().to_string())
        .collect()
}

fn is_complete_flux_module(source: &str) -> bool {
    const DECLARATIONS: &[&str] = &[
        "permissions",
        "agent_loop ",
        "agent ",
        "channel ",
        "datasource ",
        "flow ",
        "journey ",
        "op ",
        "trigger ",
    ];

    source.lines().any(|line| {
        line.len() == line.trim_start().len()
            && DECLARATIONS
                .iter()
                .any(|declaration| line.starts_with(declaration))
    })
}

fn as_parseable_flux_module(source: &str) -> String {
    let source = format!("{}\n", source.trim_end());
    if is_complete_flux_module(&source) {
        return source;
    }

    let mut wrapped = String::from("flow __website_fragment\n");
    let mut in_multiline_string = false;
    for line in source.lines() {
        // The wrapper is test scaffolding, not published source. Keep blank lines genuinely blank
        // and preserve triple-quoted content byte-for-byte instead of injecting indentation that
        // changes the literal's value. Otherwise correctly formatted fragments fail because of
        // whitespace invented by this helper rather than by the documentation.
        if !line.is_empty() && !in_multiline_string {
            wrapped.push_str("  ");
        }
        wrapped.push_str(line);
        wrapped.push('\n');
        if line.matches("\"\"\"").count() % 2 == 1 {
            in_multiline_string = !in_multiline_string;
        }
    }
    wrapped
}

fn javascript_template_constant(source: &str, name: &str) -> String {
    let marker = format!("const {name} = `");
    source
        .split_once(&marker)
        .unwrap_or_else(|| panic!("JavaScript source declares `{name}` as a template constant"))
        .1
        .split_once("`;")
        .unwrap_or_else(|| panic!("JavaScript template constant `{name}` is terminated"))
        .0
        .to_string()
}

fn test_dir(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!("flux-{label}-{}-{nonce}", std::process::id()))
}

/// A throwaway event store, so `WakeupTool` can be registered for a name-only catalog check.
/// The store is never written to — only the registered spec matters here.
fn wakeup_events_for_contract() -> Arc<flux_events::EventStore> {
    let dir = test_dir("website-wakeup");
    fs::create_dir_all(&dir).expect("create contract store dir");
    Arc::new(flux_events::EventStore::open(dir.join("events.db")).expect("open contract store"))
}

/// A worker runtime for a name-only catalog check. `ExternalRuntime` over an empty table cannot
/// start anything, which is the point: this contract reads `Tool::spec`, it never executes an op.
fn fleet_runtime_for_contract() -> Arc<dyn flux_runtime::AgentRuntime> {
    Arc::new(flux_orchestrate::ExternalRuntime::new(
        std::collections::HashMap::new(),
    ))
}

/// A provider that records the cognition prompt and returns one deterministic answer. The tutorial
/// flow is authored, so this provider is called exactly once by `ai.reason`.
struct PromptCapture {
    prompts: Arc<Mutex<Vec<String>>>,
}

struct TutorialSearch {
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl Tool for TutorialSearch {
    fn spec(&self) -> flux_spec::ToolSpec {
        flux_spec::ToolSpec::read_only(
            "search",
            "search the Northstar handbook",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "source": {"type": "string"}
                },
                "required": ["query"]
            }),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        self.calls.lock().unwrap().push(params);
        Ok(ToolResult::ok(
            "Offline edits synchronize automatically when a device reconnects. Support is available Monday through Friday, 09:00–17:00 Central European Time.",
        ))
    }
}

#[async_trait]
impl Provider for PromptCapture {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, req: Request) -> flux_core::Result<ChunkStream> {
        let prompt = req
            .messages
            .last()
            .map(|message| message.text())
            .unwrap_or_default();
        self.prompts.lock().unwrap().push(prompt);
        Ok(Box::pin(futures::stream::iter([
            Ok(Chunk::TextDelta(
                "A deleted workspace can be recovered for 30 days.".into(),
            )),
            Ok(Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            }),
        ])))
    }
}

#[test]
fn cli_reference_covers_every_public_subcommand() {
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .arg("--help")
        .output()
        .expect("run flux --help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    let commands = help
        .split_once("Commands:\n")
        .expect("Commands section")
        .1
        .split("\n\n")
        .next()
        .expect("Commands body");
    // Both surfaces that enumerate the CLI. `website/docs/agent/cli.md` is the public reference (a
    // row per command); `docs/usage.md` is the in-repo surface map. Only the first was guarded, and
    // that is the whole reason `usage.md` drifted five subcommands behind while the site did not
    // (C-204). The check asserts *mention*, not option completeness, which suits both shapes.
    // The two files spell commands differently: the reference uses inline code (`` `flux run` ``),
    // the surface map uses bare lines inside annotated shell blocks (`flux run   # …`). Match each
    // on its own convention — the assertion is "is it mentioned", not "is it formatted like this".
    let surfaces = [
        (
            "website/docs/agent/cli.md",
            read("website/docs/agent/cli.md"),
            "`flux ",
        ),
        ("docs/usage.md", read("docs/usage.md"), "flux "),
    ];
    let names: Vec<&str> = commands
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .collect();
    assert!(
        names.len() >= 20,
        "expected to parse the subcommand list from --help, found {names:?}"
    );
    for (rel, docs, prefix) in &surfaces {
        let missing: Vec<&&str> = names
            .iter()
            .filter(|name| !docs.contains(&format!("{prefix}{name}")))
            .collect();
        assert!(
            missing.is_empty(),
            "{rel} omits {} shipped subcommand(s): {missing:?}",
            missing.len()
        );
    }
}

/// The TUI page must document the keys the TUI actually binds.
///
/// `HELP_KEYS` in `crates/flux-tui/src/lib.rs` is the table the in-app F1 overlay renders, so it is
/// the one list that cannot drift from the bindings without the overlay lying too. Tying the public
/// page to it means a rebind shows up here rather than in a user's bug report. Selectable themes
/// come from `Theme::names()` for the same reason — `reference/config.md` had listed three of six.
#[test]
fn tui_page_documents_the_bound_keys_and_themes() {
    let tui_src = read("crates/flux-tui/src/lib.rs");
    let syntax = syn::parse_file(&tui_src).expect("parse flux-tui source");
    let help_table = syntax
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Const(item) if item.ident == "HELP_KEYS" => Some(item.expr.as_ref()),
            _ => None,
        })
        .expect("HELP_KEYS table");

    fn peel(expr: &syn::Expr) -> &syn::Expr {
        match expr {
            syn::Expr::Group(group) => peel(&group.expr),
            syn::Expr::Paren(paren) => peel(&paren.expr),
            syn::Expr::Reference(reference) => peel(&reference.expr),
            _ => expr,
        }
    }

    let entries = match peel(help_table) {
        syn::Expr::Array(array) => &array.elems,
        _ => panic!("HELP_KEYS must be an array reference"),
    };

    // The chord spellings out of the overlay table, minus the prose glosses. Each entry may list
    // alternatives ("Ctrl-J / Alt-↵ / Shift-↵"); requiring the first is enough to prove the
    // binding is on the page, without pinning the page to the overlay's exact typography.
    let chords: Vec<String> = entries
        .iter()
        .map(|entry| match peel(entry) {
            syn::Expr::Tuple(tuple) => tuple.elems.first().expect("HELP_KEYS tuple chord"),
            _ => panic!("HELP_KEYS entry must be a tuple"),
        })
        .map(|chord| match peel(chord) {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(literal),
                ..
            }) => literal.value(),
            _ => panic!("HELP_KEYS chord must be a string literal"),
        })
        .filter_map(|literal| {
            let first = literal.split('/').next()?.trim();
            (!first.is_empty()).then(|| first.to_string())
        })
        .collect();
    assert!(
        chords.len() >= 10,
        "expected to recover the F1 overlay's chords, found {chords:?}"
    );

    // The overlay is width-constrained and uses glyphs; prose spells the same key out. Normalise
    // the one that differs so the page is not forced into the overlay's typography.
    let spell = |s: &str| s.replace('↵', "Enter");
    let page = spell(&read("website/docs/agent/tui.md"));
    let missing: Vec<&String> = chords
        .iter()
        .filter(|c| !page.contains(&spell(c)))
        .collect();
    assert!(
        missing.is_empty(),
        "website/docs/agent/tui.md omits {} key binding(s) the TUI's own F1 overlay lists: \
         {missing:?}",
        missing.len()
    );

    // Every selectable theme must be named where a reader looks for it.
    let theme_src = read("crates/flux-tui/src/theme.rs");
    let names_body = theme_src
        .split_once("pub fn names() -> &'static [&'static str] {")
        .expect("Theme::names")
        .1
        .split_once('}')
        .expect("terminated Theme::names")
        .0;
    let themes: Vec<&str> = names_body
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| !s.trim().is_empty())
        .collect();
    assert!(themes.len() >= 3, "expected the theme list, got {themes:?}");
    let config = read("website/docs/reference/config.md");
    for theme in themes {
        assert!(
            page.contains(theme),
            "website/docs/agent/tui.md omits the `{theme}` theme"
        );
        assert!(
            config.contains(theme),
            "website/docs/reference/config.md omits the `{theme}` theme"
        );
    }
}

/// Every route the server actually mounts must appear in the public HTTP reference.
///
/// The site documented three of twelve for a long time — `agent/a2a.md` covered the A2A routes and
/// nothing covered the session REST subtree, its SSE stream, the webhook, or either usage endpoint.
/// Axum's `Router` cannot be enumerated at runtime, so the route set is read out of the source, the
/// same way `cli_reference_covers_every_public_subcommand` reads `--help`.
#[test]
fn http_api_reference_covers_every_served_route() {
    let src = read("crates/flux-server/src/lib.rs");
    // Production mounts only: the file's `#[cfg(test)]` module builds throwaway routers of its own.
    let production = src
        .split("#[cfg(test)]")
        .next()
        .expect("source ahead of the test module");

    let mut routes: Vec<&str> = Vec::new();
    for (idx, _) in production.match_indices(".route(") {
        // The path literal is not always on the same line — rustfmt wraps longer `.route(` calls,
        // so skip whitespace rather than assuming `.route("`.
        let after = production[idx + ".route(".len()..].trim_start();
        let Some(literal) = after.strip_prefix('"') else {
            continue;
        };
        let path = literal.split('"').next().expect("terminated route literal");
        routes.push(path);
    }
    routes.sort_unstable();
    routes.dedup();
    assert!(
        routes.len() >= 12,
        "expected to recover the full mounted route set, found {}: {routes:?}",
        routes.len()
    );

    let docs = read("website/docs/agent/http-api.md");
    for path in routes {
        assert!(
            docs.contains(path),
            "website/docs/agent/http-api.md omits the served route `{path}`"
        );
    }
}

/// The channel inventory is a public mirror of the production dispatcher, not a hand-maintained
/// count. Reading the accepted string literals from `build_channels` means adding an adapter or an
/// alias makes the docs gate fail in the same change, including host-built kinds such as `a2a` and
/// the host-served `cli` kind whose match arms deliberately do not construct a background task.
#[test]
fn public_channel_inventory_covers_every_registered_kind() {
    let adapters = read("crates/flux-channels/src/adapters/mod.rs");
    let dispatcher = adapters
        .split_once("match d.kind.as_str() {")
        .expect("channel-kind dispatcher")
        .1
        .split_once("other => anyhow::bail!")
        .expect("unknown-kind arm")
        .0;

    let mut kinds = Vec::new();
    for arm in dispatcher.lines().filter_map(|line| line.split_once("=>")) {
        let patterns = arm.0;
        let mut rest = patterns;
        while let Some(start) = rest.find('"') {
            rest = &rest[start + 1..];
            let end = rest.find('"').expect("terminated channel-kind literal");
            kinds.push(rest[..end].to_string());
            rest = &rest[end + 1..];
        }
    }
    kinds.sort();
    kinds.dedup();
    assert!(
        kinds.len() >= 9,
        "expected every production channel kind and alias, recovered {kinds:?}"
    );

    let inventory = read("website/docs/channels/inventory.md");
    let table = inventory
        .split_once("## At a glance")
        .expect("channel inventory summary")
        .1
        .split_once("Every kind's `settings`")
        .expect("end of channel inventory table")
        .0;
    let missing: Vec<&String> = kinds
        .iter()
        .filter(|kind| !table.contains(&format!("`{kind}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "website/docs/channels/inventory.md omits {} production channel kind(s) or aliases from \
         its at-a-glance table: {missing:?}",
        missing.len()
    );
}

#[test]
fn operations_reference_covers_the_registered_public_catalog() {
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    flux_web::register_web(&mut registry, &flux_web::WebOptions::default());
    CognitionPack::new(Arc::new(NullProvider), "mock").register(&mut registry);
    // A-96: `consult` is registered independently of `CognitionPack` (a different provider per
    // call, not one fixed at pack construction); the factory is never invoked by this contract
    // check, only its registered name/spec matter here.
    ConsultTool::new(
        Arc::new(|_spec: &str| Err(flux_core::Error::Other("unused in this test".into()))),
        None,
        "mock",
        DEFAULT_CONSULT_MAX_CALLS,
    )
    .try_register(&mut registry)
    .unwrap();
    // The four packs above are NOT the catalog a real session assembles — `execution.rs` also
    // registers these, and their absence here is why `ai_segment`, the eval family and
    // `schedule_wakeup` sat undocumented while this test stayed green.
    flux_tools::try_register_reflect(&mut registry).unwrap();
    flux_tools::try_register_flows(&mut registry).unwrap();
    flux_eval::try_register_eval_ops(&mut registry).unwrap();
    // `schedule_wakeup` is config-gated (`[wakeup] enabled`) rather than absent, so it is
    // registered unconditionally here: gated-off is still public surface a reader must be able to
    // look up — that is precisely how it stayed undocumented.
    flux_flow::wakeup::WakeupTool::new(
        wakeup_events_for_contract(),
        flux_flow::wakeup::DEFAULT_MAX_HORIZON_SECS,
        flux_flow::wakeup::DEFAULT_MAX_PENDING_PER_SESSION,
    )
    .try_register(&mut registry)
    .unwrap();
    let docs = read("website/docs/language/ops.md");
    let mut names = registry.names();
    // The `fleet.*` ops (A-116), which `execution.rs`'s `try_register_fleet` puts in the production
    // catalog (A-131). They were constructed *nowhere* before that story, which is exactly why this
    // contract stayed green while they went undocumented — the same failure mode the comment above
    // records for `ai_segment` and the eval family. Their names are read off `Tool::spec` rather
    // than listed as literals below, so renaming one here alone cannot silence this check.
    names.extend(
        [
            Arc::new(flux_orchestrate::FleetDispatchTool::new(
                flux_system::net::PrivateNetAllow::None,
                None,
            )) as Arc<dyn Tool>,
            Arc::new(flux_orchestrate::FleetStatusTool::new(
                flux_system::net::PrivateNetAllow::None,
                None,
            )),
            Arc::new(flux_orchestrate::FleetCancelTool::new(
                flux_system::net::PrivateNetAllow::None,
                None,
            )),
            // C-243's worker-lifecycle half, added for exactly the reason recorded above: they are
            // in the production catalog via the same `try_register_fleet`, so leaving them out here
            // would let them go undocumented while this contract stayed green — the third instance
            // of that failure mode. The runtime is the `ExternalRuntime` (no process is started);
            // only the registered names and specs matter to this check.
            Arc::new(flux_orchestrate::FleetStartTool::new(
                fleet_runtime_for_contract(),
            )),
            Arc::new(flux_orchestrate::FleetWorkerStatusTool::new(
                fleet_runtime_for_contract(),
            )),
            Arc::new(flux_orchestrate::FleetStopTool::new(
                fleet_runtime_for_contract(),
            )),
        ]
        .into_iter()
        .map(|tool| tool.spec().name),
    );
    // C-223: the `pane.*` ops are registered by `try_register_surface_ops` — surfaced by the
    // presence of a `SurfaceSink` at assembly time, so they are absent from the packs above by
    // design. Named here as literals for the same reason `op.register` is: an op a real session
    // dispatches must be documented, whatever registers it.
    names.extend(flux_tools::PANE_OPS.into_iter().map(str::to_string));
    names.push(flux_tools::USER_ASK_OP.to_string());
    names.extend(
        [
            "ask",
            "emit",
            "endpoint.discover",
            "endpoint.import",
            "endpoint.info",
            "endpoint.list",
            "endpoint.select",
            "flow_list",
            "flow_run",
            "op.register",
            "send",
            "spawn",
        ]
        .into_iter()
        .map(str::to_string),
    );
    names.sort();
    names.dedup();
    // Report every omission at once: fixing a catalog gap one panic at a time is what let three
    // whole families accumulate.
    let missing: Vec<&String> = names
        .iter()
        .filter(|name| !docs.contains(&format!("`{name}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "website/docs/language/ops.md omits {} registered operation(s): {missing:?}",
        missing.len()
    );
}

/// Environment variables that are read by shipped code but are deliberately NOT public config.
///
/// Anything read from `crates/*/src` and absent from this list must appear in the reference —
/// adding a user-facing variable without documenting it should fail the gate, and adding an
/// internal one should require saying so here. The `FLUX_TEST_` prefix is excluded wholesale by
/// naming convention; everything else is named, so an undocumented public variable cannot hide
/// behind a broad substring rule.
const NON_PUBLIC_ENV: &[&str] = &[
    // Test doubles and fixtures reached from non-test code paths.
    "FLUX_CASSETTE",
    "FLUX_CASSETTE_MAX_BYTES",
    "FLUX_GOLDEN",
    "FLUX_MOCK_BASH",
    "FLUX_MOCK_ERROR",
    "FLUX_MOCK_HANG",
    "FLUX_MOCK_RESPONSE",
    "FLUX_MOCK_TOOL",
    "FLUX_MOCK_TOOL_INPUT",
    "FLUX_LIVE_BROWSER_SMOKE",
    "FLUX_LIVE_SANDBOX_SMOKE",
    "FLUX_WEB_DEFINITELY_UNSET",
    "FLUX_WEB_STOLEN_TOKEN",
    "FLUX_WEB_STOLEN_QUERY_TOKEN",
    "FLUX_WEB_TEST_TOKEN",
    "FLUX_WEB_TEST_QUERY_KEY",
    "FLUX_WEB_TEST_ECHO_TOKEN",
    "FLUX_WEB_TEST_NUMERIC_TOKEN",
    // CI-only deterministic corpus size for the adversarial assurance tests; not shipped behavior.
    "FLUX_ADVERSARIAL_CASES",
    // A `format!` prefix, not a variable: the D-116 endpoint e2e mints a per-process credential
    // key (`FLUX_D116_PGPASS_<pid>`) to prove a credential *location* is never part of the URL.
    "FLUX_D116_PGPASS_",
    // Re-exec markers the C-256/C-257 proxy-isolation tests set on their own child test process.
    // Proxy variables are process-global, so the assertion runs in an isolated re-exec of the test
    // binary; the marker exists only inside those `#[cfg(test)]` modules.
    "FLUX_A2A_PROXY_REGRESSION_CHILD",
    "FLUX_PLUGIN_PROXY_REGRESSION_CHILD",
    "FLUX_WEB_PROXY_REGRESSION_CHILD",
    // Markers flux sets for its own child processes — observable, but not knobs a user sets.
    "FLUX_BG_MARKER",
    "FLUX_C67_CWD_CHILD",
    "FLUX_EVAL_MARKER",
    // C-243: the fleet-worker generation a `ProcessRuntime` child is granted. Set by flux on its own
    // workers and read back by their runtimes to bound nesting; `build_command` clears the child's
    // environment first, so it is not something an operator (or a model) can hand in. Raising it by
    // hand only ever shrinks the budget, so it is not a knob worth documenting as one.
    "FLUX_FLEET_DEPTH",
    "FLUX_SANDBOXED",
    "FLUX_SECRET",
    "FLUX_SYSTEM_ENV_TRUTHY_PROBE",
    // Internal development toggles with no supported behaviour contract.
    "FLUX_OP_CACHE",
    "FLUX_RESPONSES_CACHE",
    "FLUX_SURFACE_ALL",
];

/// Every `FLUX_*` variable shipped code reads is either documented or explicitly classified.
///
/// The reference listed 13 of them while the tree read far more — including the whole
/// `FLUX_EMBEDDINGS_*` trio, which gates datasource embeddings, and `FLUX_ALLOW_ALL`, which widens
/// the safety envelope. Both classes are exactly the kind a user needs to find.
#[test]
fn config_reference_documents_every_public_env_var() {
    let mut sources = Vec::new();
    for crate_dir in fs::read_dir(repo_path("crates")).expect("read crates/") {
        let src = crate_dir.expect("crate entry").path().join("src");
        if src.is_dir() {
            for file in files_with_extension(&src, "rs") {
                sources.push(fs::read_to_string(&file).expect("read source"));
            }
        }
    }
    let joined = sources.concat();

    let mut vars: Vec<String> = Vec::new();
    for (idx, _) in joined.match_indices("\"FLUX_") {
        let rest = &joined[idx + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        if name.len() > "FLUX_".len() {
            vars.push(name);
        }
    }
    vars.sort();
    vars.dedup();
    assert!(
        vars.len() > 40,
        "expected to recover the env-var surface, found {}",
        vars.len()
    );

    let config = read("website/docs/reference/config.md");
    let providers = read("website/docs/agent/providers.md");
    let storage = read("website/docs/reference/storage.md");
    let undocumented: Vec<&String> = vars
        .iter()
        .filter(|v| !v.starts_with("FLUX_TEST_"))
        .filter(|v| !NON_PUBLIC_ENV.contains(&v.as_str()))
        .filter(|v| {
            !config.contains(v.as_str())
                && !providers.contains(v.as_str())
                && !storage.contains(v.as_str())
        })
        .collect();
    assert!(
        undocumented.is_empty(),
        "these FLUX_* variables are read by shipped code but documented nowhere on the site: \
         {undocumented:?}\nDocument them in website/docs/reference/config.md, or add them to \
         NON_PUBLIC_ENV with the reason."
    );
}

/// Every public `[section]` of the config schema is named in the reference.
#[test]
fn config_reference_documents_every_public_section() {
    let schema = read("crates/flux-config/src/lib.rs");
    let docs = read("website/docs/reference/config.md");
    // The serde field name of each table on `Config` is what a user actually writes.
    for section in [
        "agent",
        "consult",
        "limits",
        "private_net",
        "sandbox",
        "server",
        "skills",
        "tools",
        "wakeup",
        "workspace",
    ] {
        assert!(
            schema.contains(&format!("pub {section}:")),
            "`[{section}]` is no longer a field on the config schema — update this list"
        );
        assert!(
            docs.contains(&format!("[{section}]")),
            "website/docs/reference/config.md never mentions the `[{section}]` table"
        );
    }
}

#[test]
fn public_config_examples_deserialize_and_have_effect() {
    for rel in [
        "website/docs/reference/config.md",
        "website/docs/troubleshooting.md",
        "website/docs/plugins/using-plugins.md",
        "website/docs/plugins/gitlab.md",
    ] {
        let markdown = read(rel);
        for (index, block) in fenced_blocks(&markdown, "toml").into_iter().enumerate() {
            let cfg: flux_config::Config = toml::from_str(&block)
                .unwrap_or_else(|e| panic!("{rel} TOML block {}: {e}", index + 1));
            if block.contains("[private_net]") {
                assert!(
                    !cfg.web_private_hosts().is_empty(),
                    "{rel} TOML block {} declares [private_net] but grants no web scope",
                    index + 1
                );
            }
            if block.contains("[private_net.plugins]") {
                let plugin = block
                    .lines()
                    .skip_while(|line| line.trim() != "[private_net.plugins]")
                    .skip(1)
                    .find_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
                    .expect("plugin grant entry");
                assert!(
                    !cfg.plugin_private_hosts(plugin).is_empty(),
                    "{rel} TOML block {} has an ineffective plugin grant",
                    index + 1
                );
            }
        }
    }
}

/// Covers `website/blog` as well as `website/docs`: a blog post is published Flux source on the same
/// domain, and a tutorial whose example does not parse is worse than no tutorial. Adding the blog to
/// the same scan is what stops it becoming a second, unchecked corpus.
#[test]
fn complete_flux_fences_parse_and_legacy_syntax_stays_out() {
    let roots = ["website/docs", "website/blog"];
    let declarations = [
        "permissions",
        "agent_loop ",
        "agent ",
        "channel ",
        "datasource ",
        "flow ",
        "journey ",
        "op ",
        "trigger ",
    ];
    let mut checked = 0;
    let paths: Vec<_> = roots
        .iter()
        .flat_map(|root| markdown_files(&repo_path(root)))
        .collect();
    for path in paths {
        let markdown = fs::read_to_string(&path).expect("read website doc");
        for (index, block) in fenced_blocks(&markdown, "flux").into_iter().enumerate() {
            assert!(
                !block
                    .lines()
                    .any(|line| line.trim_start().starts_with("let ")),
                "{} Flux block {} uses the retired `let` syntax",
                path.display(),
                index + 1
            );
            let complete = block.lines().any(|line| {
                line.len() == line.trim_start().len()
                    && declarations.iter().any(|decl| line.starts_with(decl))
            });
            if complete {
                flux_lang::parse::parse_program(&block)
                    .unwrap_or_else(|e| panic!("{} Flux block {}: {e}", path.display(), index + 1));
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 25,
        "expected a representative Flux example corpus"
    );
}

/// Public Flux examples are source, including the short body fragments that omit only their flow
/// header. Parse every `flux` fence across the complete public documentation corpus with the real
/// module parser, wrap fragments in a throwaway flow, and require both formatter contracts: the
/// lossless CST formatter must have no whitespace edit, and a bare flow's significant tokens must
/// already match the semantic formatter's canonical projection. Shell, Rust, JSON, and other fences
/// never enter this scan.
#[test]
fn public_flux_examples_are_canonical_formatter_fixed_points() {
    let docs_root = repo_path("website/docs");
    let mut checked = 0;
    for path in markdown_files(&docs_root) {
        let markdown = fs::read_to_string(&path).expect("read website doc");
        for (index, block) in fenced_blocks(&markdown, "flux").into_iter().enumerate() {
            let source = as_parseable_flux_module(&block);
            let module = flux_lang::parse::parse_program(&source).unwrap_or_else(|e| {
                panic!(
                    "{} Flux block {} is neither a valid module nor a valid flow-body fragment: \
                     {e}",
                    path.display(),
                    index + 1
                )
            });

            if let Some(formatted) = flux_lang::format_cst::format_source(&source) {
                panic!(
                    "{} Flux block {} is not a CST-formatter fixed point. Canonical source:\n{}",
                    path.display(),
                    index + 1,
                    formatted
                );
            }

            match module {
                flux_lang::program::Module::Flow(ast) => {
                    let canonical = flux_lang::format::format(&ast);
                    assert_eq!(
                        significant_flux_tokens(&source),
                        significant_flux_tokens(&canonical),
                        "{} Flux block {} uses an accepted compatibility spelling instead of the \
                         semantic formatter's canonical syntax. Canonical source:\n{}",
                        path.display(),
                        index + 1,
                        canonical
                    );
                }
                flux_lang::program::Module::Program(_) => {
                    // The semantic formatter intentionally formats one DraftAst rather than a
                    // declaration module (which would reorder authored declarations). Its two most
                    // pervasive compatibility spellings remain unambiguous at the lossless-token
                    // layer, so keep them out of the few multi-declaration examples as well.
                    let parsed = flux_lang::parser::parse_cst(&source);
                    let tokens: Vec<_> = parsed
                        .syntax()
                        .descendants_with_tokens()
                        .filter_map(|element| element.into_token())
                        .filter(|token| {
                            !matches!(
                                token.kind(),
                                flux_lang::syntax::SyntaxKind::WHITESPACE
                                    | flux_lang::syntax::SyntaxKind::COMMENT
                                    | flux_lang::syntax::SyntaxKind::NEWLINE
                                    | flux_lang::syntax::SyntaxKind::INDENT
                                    | flux_lang::syntax::SyntaxKind::DEDENT
                            )
                        })
                        .collect();
                    assert!(
                        tokens
                            .iter()
                            .all(|token| token.kind() != flux_lang::syntax::SyntaxKind::VAR),
                        "{} Flux block {} uses legacy `$` symbol spelling",
                        path.display(),
                        index + 1
                    );
                    assert!(
                        !tokens.windows(2).any(|pair| {
                            pair[0].kind() == flux_lang::syntax::SyntaxKind::L_PAREN
                                && pair[1].kind() == flux_lang::syntax::SyntaxKind::L_BRACE
                        }),
                        "{} Flux block {} wraps named arguments in legacy object braces",
                        path.display(),
                        index + 1
                    );
                }
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 120,
        "expected the full public website example corpus, checked {checked} fences"
    );
}

/// The homepage hero is public Flux source too, but it lives in JSX rather than a Markdown fence.
/// Exercise it against the live built-in/cognition catalog so parser-only compatibility spellings
/// and analyzer-invalid argument shapes cannot become the first example readers see.
#[test]
fn homepage_flux_example_is_canonical_and_analyzes_against_the_live_catalog() {
    let homepage = read("website/src/pages/index.js");
    let source = javascript_template_constant(&homepage, "HERO_FLOW");
    let module = flux_lang::parse::parse_program(&source)
        .unwrap_or_else(|e| panic!("homepage HERO_FLOW must parse: {e}"));
    let flux_lang::program::Module::Flow(ast) = module else {
        panic!("homepage HERO_FLOW must be one flow");
    };

    assert_eq!(
        flux_lang::format_cst::format_source(&source),
        None,
        "homepage HERO_FLOW must be a CST-formatter fixed point"
    );
    let canonical = flux_lang::format::format(&ast);
    assert_eq!(
        significant_flux_tokens(&source),
        significant_flux_tokens(&canonical),
        "homepage HERO_FLOW uses a compatibility spelling; canonical source:\n{canonical}"
    );

    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    CognitionPack::new(Arc::new(NullProvider), "mock").register(&mut registry);
    let catalog = flux_flow::registry::OpRegistry::new(&registry);
    flux_lang::analyze::analyze_flow(&ast, &catalog, &std::collections::HashSet::new())
        .unwrap_or_else(|diagnostics| {
            panic!("homepage HERO_FLOW must analyze against the live catalog: {diagnostics:?}")
        });

    let concepts = read("website/docs/concepts.md");
    assert!(
        concepts.contains("symbols such as `src` or `tests`")
            && !concepts.contains("symbols such as `$src` or `$tests`"),
        "the Concepts symbol examples must use the formatter's canonical bare spelling"
    );
}

/// The endpoint walkthrough is duplicated deliberately between the operator overview and the SQL
/// plugin guide. Parse and analyze both copies with the published operation shapes: a formatter
/// fixed point alone does not reject a positional value mixed with named arguments.
#[test]
fn public_endpoint_examples_analyze_as_named_multi_argument_calls() {
    use flux_lang::opspec::{OpCatalog, OpSignature};

    struct EndpointCatalog(Vec<OpSignature>);
    impl OpCatalog for EndpointCatalog {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            self.0
                .iter()
                .find(|signature| signature.name == name)
                .cloned()
        }
    }

    let signature = |name: &str, schema: serde_json::Value| {
        OpSignature::from_spec(&flux_spec::ToolSpec::read_only(
            name,
            "website contract",
            schema,
        ))
    };
    let catalog = EndpointCatalog(vec![
        signature(
            "endpoint.select",
            serde_json::json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        signature(
            "sql.query",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "endpoint": {"type": "object"},
                    "endpoint_ref": {"type": "string"},
                    "driver": {"type": "string"},
                    "database": {"type": "string"},
                    "timeout": {"type": "number"},
                    "query": {"type": "string"},
                    "max_rows": {"type": "integer"}
                },
                "required": ["query"]
            }),
        ),
    ]);

    for rel in [
        "website/docs/agent/endpoints.md",
        "website/docs/plugins/sql.md",
    ] {
        let markdown = read(rel);
        let block = fenced_blocks(&markdown, "flux")
            .into_iter()
            .find(|block| block.contains("flow inspect-database"))
            .unwrap_or_else(|| panic!("{rel} must contain the inspect-database flow"));
        let module = flux_lang::parse::parse_program(&block)
            .unwrap_or_else(|e| panic!("{rel} inspect-database flow must parse: {e}"));
        let flux_lang::program::Module::Flow(ast) = module else {
            panic!("{rel} inspect-database example must be one flow");
        };
        flux_lang::analyze::analyze_flow(&ast, &catalog, &std::collections::HashSet::new())
            .unwrap_or_else(|diagnostics| {
                panic!("{rel} inspect-database flow must analyze: {diagnostics:?}")
            });
    }
}

/// The syntax page must document the `"""` string form, and the form it documents must be real.
///
/// This page previously asserted the opposite — "Strings are single-line; embed newlines with `\n`
/// escapes" — while the lexer had supported triple-quoted verbatim strings since L-39
/// (`crates/flux-lang/src/lexer.rs`, `crates/flux-lang/docs/syntax.md`). An omission costs a reader
/// a search; a false statement costs them the belief that the workaround is necessary.
#[test]
fn syntax_page_documents_multiline_strings_and_the_examples_parse() {
    let syntax = read("website/docs/language/flows-and-syntax.md");
    assert!(
        syntax.contains("## Multi-line strings") || syntax.contains("### Multi-line strings"),
        "website/docs/language/flows-and-syntax.md must document the `\"\"\"` multi-line string \
         form — it is the recommended spelling for prompts and embedded payloads"
    );
    assert!(
        !syntax.contains("Strings are single-line;"),
        "the retired claim that Flux-Lang strings are single-line is back on the syntax page; \
         triple-quoted strings ship (crates/flux-lang/src/lexer.rs `scan_string`)"
    );

    // Documenting the form is not enough — at least one complete example in the public corpus
    // must actually use it and parse, so the docs cannot drift away from the lexer.
    let docs_root = repo_path("website/docs");
    let mut demonstrated = 0;
    for path in markdown_files(&docs_root) {
        let markdown = fs::read_to_string(&path).expect("read website doc");
        for (index, block) in fenced_blocks(&markdown, "flux").into_iter().enumerate() {
            if !block.contains("\"\"\"") {
                continue;
            }
            let complete = block
                .lines()
                .any(|line| line.len() == line.trim_start().len() && line.starts_with("flow "));
            if complete {
                flux_lang::parse::parse_program(&block).unwrap_or_else(|e| {
                    panic!(
                        "{} Flux block {}: multi-line-string example does not parse: {e}",
                        path.display(),
                        index + 1
                    )
                });
                demonstrated += 1;
            }
        }
    }
    assert!(
        demonstrated >= 1,
        "no complete Flux example on the site demonstrates a `\"\"\"` string"
    );
}

/// First-reader surfaces must all describe the shipped authored outer loop, rather than reviving
/// the removed per-turn model-to-Flux compiler story in one hero, metadata string, or generated LLM
/// index. `op.register` is the deliberate narrow seam: one composite operation's source may be
/// proposed, then the host analyzes, scopes, and gates its persistence like any other effect.
#[test]
fn public_runtime_story_matches_the_authored_loop_contract() {
    let core_surfaces = [
        "website/src/pages/index.js",
        "website/docusaurus.config.js",
        "website/plugins/llms-txt/index.js",
        "website/docs/intro.md",
        "website/docs/concepts.md",
        "website/docs/infrastructure.md",
        "website/docs/agent/agent-loop.md",
        "website/docs/tutorial/first-agent.md",
    ];
    let retired_claims = [
        "model compiles",
        "llm compiles",
        "compiler front-end",
        "typed flux-lang plan",
        "returns either prose or a typed flux-lang plan",
        "planners emit this shape",
        "asking a model to compile it again",
    ];

    for rel in core_surfaces {
        let docs = read(rel);
        // JS strings are often wrapped across adjacent literals; compare their prose words rather
        // than making source layout part of the public architecture contract.
        let lower = normalized_prose(&docs);
        let present: Vec<&&str> = retired_claims
            .iter()
            .filter(|claim| lower.contains(**claim))
            .collect();
        assert!(
            present.is_empty(),
            "{rel} revives the retired per-turn model-generated Flux plan story: {present:?}"
        );
        assert!(
            lower.contains("authored flux-lang") || lower.contains("authored flow"),
            "{rel} must say that authored Flux-Lang owns control flow"
        );
        assert!(
            lower.contains("provider-native") || lower.contains("native schemas"),
            "{rel} must place model judgment inside provider-native typed stages/operation schemas"
        );
        assert!(
            lower.contains("action batch"),
            "{rel} must name the host-frozen action-batch boundary"
        );
    }

    for rel in [
        "website/docs/agent/saved-flows.md",
        "website/docs/language/node-reference.md",
    ] {
        let source = read(rel);
        // Node-kind prose is generated from AST doc comments and has its own guarded regeneration
        // contract. This test owns the hand-written runtime preamble, not that generated block.
        let hand_written = source
            .split("<!-- BEGIN generated:node-kinds -->")
            .next()
            .expect("text before an optional generated node table");
        let docs = normalized_prose(hand_written);
        let present: Vec<&&str> = retired_claims
            .iter()
            .filter(|claim| docs.contains(**claim))
            .collect();
        assert!(
            present.is_empty(),
            "{rel} revives the retired per-turn model-generated Flux plan story: {present:?}"
        );
    }

    let registration = read("website/docs/agent/saved-flows.md");
    let registration_lower = normalized_prose(&registration);
    for boundary in ["exactly one top-level", "analyz", "scope", "guarded"] {
        assert!(
            registration_lower.contains(boundary),
            "website/docs/agent/saved-flows.md must document `{boundary}` as part of the explicit \
             op.register seam"
        );
    }
    assert!(
        registration.contains("`op.register`") && registration_lower.contains("agent-proposed"),
        "saved-flow guidance must identify op.register as the scoped seam for agent-proposed \
         composite-operation source"
    );

    // The language and SDK entry points used to compress the default-loop rule into the absolute
    // claim that models "never emit Flux". That erases the explicit op.register seam documented
    // above. Keep both pages precise: no per-turn executable Flux in the default loop, but exactly
    // one proposed composite operation may cross the analyzed/scoped/guarded registration boundary.
    for rel in [
        "website/docs/language/overview.md",
        "website/docs/sdk/overview.md",
    ] {
        let source = read(rel);
        let docs = normalized_prose(&source);
        assert!(
            docs.contains("default") && docs.contains("per-turn executable flux"),
            "{rel} must scope the no-model-generated-Flux claim to the default per-turn loop"
        );
        assert!(
            source.contains("`op.register`")
                && docs.contains("exactly one")
                && docs.contains("analyz")
                && docs.contains("scope")
                && docs.contains("guard"),
            "{rel} must name the analyzed, scoped, guarded op.register exception"
        );
        assert!(
            !docs.contains("models do not emit flux")
                && !docs.contains("model never emits executable flux"),
            "{rel} must not replace the qualified default-loop rule with an absolute claim"
        );
    }
}

#[test]
fn tutorial_agent_copy_matches_the_adaptive_batch_semantics() {
    let lesson = read("website/docs/tutorial/first-agent.md");
    let loop_docs = read("website/docs/agent/agent-loop.md");
    for (rel, docs) in [
        ("tutorial/first-agent.md", lesson.as_str()),
        ("agent/agent-loop.md", loop_docs.as_str()),
    ] {
        assert!(
            docs.contains("native") && docs.contains("schema"),
            "{rel} must disclose provider-native operation schemas"
        );
        assert!(
            docs.contains("action batch") && docs.contains("captur"),
            "{rel} must distinguish effect capture from execution"
        );
        assert!(
            docs.contains("one-shot") && docs.contains("receipt"),
            "{rel} must explain receipt-bound batch execution"
        );
        assert!(
            !docs.contains("flux plan"),
            "{rel} mentions the retired compiler CLI"
        );
    }
}

/// Execute the public lesson's exact Flux fence over the exact handbook facts with a hermetic model.
/// This catches the failure the parser-only contract missed: `ctx` used to pass only `members` names
/// into `ai.reason`, so a syntactically-valid tutorial flow could not answer from either file.
#[tokio::test]
async fn tutorial_flow_materializes_handbook_context_for_ai_reason() {
    let root = test_dir("website-tutorial-flow");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(
        root.join("docs/product.md"),
        "# Northstar Notes\n\nOffline edits synchronize automatically when a device reconnects.\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/policies.md"),
        "# Northstar policies\n\nCustomers can request a refund within 14 days of their first payment. A deleted workspace can be recovered for 30 days.\n",
    )
    .unwrap();

    let lesson = read("website/docs/tutorial/first-flow.md");
    let flow = fenced_blocks(&lesson, "flux")
        .into_iter()
        .next()
        .expect("tutorial flow fence");
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(PromptCapture {
        prompts: prompts.clone(),
    });
    let client = flux_sdk::flow::FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(provider, &root)
        .unwrap();
    let ast = client.parse(&flow).unwrap();
    let mut inputs = serde_json::Map::new();
    inputs.insert(
        "question".into(),
        serde_json::json!("How long can a deleted workspace be recovered?"),
    );
    let result = client.execute_with(&ast, inputs).await.unwrap();
    assert!(result.result.contains("30 days"));

    let captured = prompts.lock().unwrap();
    assert_eq!(
        captured.len(),
        1,
        "the authored flow has one model boundary"
    );
    let prompt = &captured[0];
    for fact in [
        "Offline edits synchronize automatically",
        "within 14 days of their first payment",
        "recovered for 30 days",
    ] {
        assert!(prompt.contains(fact), "context omitted `{fact}`: {prompt}");
    }
    assert!(prompt.contains("## $product"));
    assert!(prompt.contains("## $policies"));
    assert!(
        !prompt.contains("\"members\""),
        "Ctx metadata replaced content: {prompt}"
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn tutorial_app_declares_forced_scoped_retrieval() {
    let lesson = read("website/docs/tutorial/first-app.md");
    let source = fenced_blocks(&lesson, "flux")
        .into_iter()
        .find(|block| block.contains("journey answer-question"))
        .expect("deterministic tutorial app fence");
    let flux_lang::program::Module::Program(program) =
        flux_lang::parse::parse_program(&source).expect("parse tutorial app")
    else {
        panic!("tutorial app fence must be a Program")
    };
    let guide = program
        .agents
        .iter()
        .find(|agent| agent.name == "guide")
        .expect("guide agent");
    assert_eq!(guide.tools, ["search"]);
    assert_eq!(guide.datasources, ["handbook"]);
    let app_permissions = program.permissions.as_ref().expect("app permissions");
    assert_eq!(
        app_permissions.allow.as_deref(),
        Some(&["search".into(), "ai.reason".into(), "send".into()][..])
    );
    let journey = program
        .journeys
        .iter()
        .find(|journey| journey.name == "answer-question")
        .expect("answer journey");
    assert_eq!(journey.agent.as_deref(), Some("guide"));
    let mut calls = Vec::new();
    flux_lang::analyze::for_each_node(&journey.flow.body, &mut |node| {
        if let flux_lang::ast::Node::Call { op, .. } = node {
            calls.push(op.clone());
        }
    });
    assert_eq!(calls, ["search", "ai.reason", "send"]);
    let trigger = program
        .triggers
        .iter()
        .find(|trigger| trigger.name == "questions")
        .expect("questions trigger");
    assert_eq!(trigger.run, "answer-question");
    assert_eq!(trigger.agent, None, "the trigger runs the authored journey");
}

#[tokio::test]
async fn tutorial_owned_journey_searches_before_every_reasoning_call() {
    let lesson = read("website/docs/tutorial/first-app.md");
    let source = fenced_blocks(&lesson, "flux")
        .into_iter()
        .find(|block| block.contains("journey answer-question"))
        .expect("deterministic tutorial app fence");
    let flux_lang::program::Module::Program(program) =
        flux_lang::parse::parse_program(&source).expect("parse tutorial app")
    else {
        panic!("tutorial app fence must be a Program")
    };
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let searches = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(PromptCapture {
        prompts: prompts.clone(),
    });
    let search: Arc<dyn Tool> = Arc::new(TutorialSearch {
        calls: searches.clone(),
    });
    let app = flux_app::App::try_with_tools(program, Some(provider), "mock", false, vec![search])
        .expect("validated tutorial app");

    for question in [
        "What happens to my edits if I work offline?",
        "What are the support hours and timezone?",
    ] {
        app.deliver("user_input", serde_json::json!({"text": question}))
            .await
            .expect("answer journey");
    }

    let searches = searches.lock().unwrap();
    assert_eq!(searches.len(), 2, "one mandatory search per question");
    assert!(searches.iter().all(|call| call["source"] == "handbook"));
    let prompts = prompts.lock().unwrap();
    assert_eq!(prompts.len(), 2, "one reasoning call per searched question");
    assert!(prompts.iter().all(|prompt| {
        prompt.contains("Offline edits synchronize automatically")
            && prompt.contains("09:00–17:00 Central European Time")
    }));
    assert_eq!(
        app.bus().sent().len(),
        2,
        "one terminal answer per question"
    );
}

/// The earlier manual E2E needed SIGTERM only when `flux` was nested under a PTY-recording process.
/// Send SIGINT directly to the real app process and require a clean status, proving the product's
/// shutdown handler works independently of terminal-forwarding behavior in the harness.
#[cfg(unix)]
#[test]
fn tutorial_app_exits_cleanly_on_direct_sigint() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let root = test_dir("website-tutorial-sigint");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/product.md"), "# Product\n\nOffline sync.\n").unwrap();
    fs::write(
        root.join("docs/policies.md"),
        "# Policies\n\n30 day recovery.\n",
    )
    .unwrap();
    let lesson = read("website/docs/tutorial/first-app.md");
    let app_source = fenced_blocks(&lesson, "flux")
        .into_iter()
        .find(|block| block.contains("journey answer-question"))
        .expect("deterministic tutorial app fence");
    fs::write(root.join("assistant.flux"), app_source).unwrap();

    // `--no-sandbox` states the posture explicitly (C-266). Since C-410 a `<program.flux>` run is a
    // channel daemon pinned to the fail-closed profile with or without `--yes`, so a spawn that said
    // nothing would pass on a developer machine with `bwrap` and refuse to start on a runner
    // without one. What this test asserts is the SIGINT shutdown handler, which has nothing to do
    // with confinement — so opting out is the honest declaration rather than a workaround.
    let mut child = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["--no-sandbox", "app", "run", "assistant.flux", "-m", "mock"])
        .current_dir(&root)
        .env("HOME", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start tutorial app");
    let stdout = child.stdout.take().expect("app stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let ready_by = Instant::now() + Duration::from_secs(15);
    let mut ready = false;
    while Instant::now() < ready_by {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains("Northstar handbook assistant ready") => {
                ready = true;
                break;
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if !ready {
        let early = child.try_wait().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        panic!("tutorial app never reached its welcome message (status: {early:?})");
    }

    // Let `serve` move from startup delivery into its signal-select loop before sending SIGINT.
    std::thread::sleep(Duration::from_millis(50));
    let signal = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(signal.success());

    let exit_by = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= exit_by {
            let _ = child.kill();
            let _ = child.wait();
            panic!("tutorial app did not stop within five seconds of direct SIGINT");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        status.success(),
        "SIGINT was not handled gracefully: {status}"
    );
    fs::remove_dir_all(root).ok();
}

#[test]
fn plugin_and_sdk_docs_track_the_shipped_surfaces() {
    let plugins_toml: toml::Value = toml::from_str(&read("plugins/Cargo.toml")).unwrap();
    let members = plugins_toml["workspace"]["members"]
        .as_array()
        .expect("plugins workspace members");
    let plugin_docs = read("website/docs/plugins/using-plugins.md");
    for member in members.iter().filter_map(toml::Value::as_str) {
        if matches!(member, "host-kit" | "pack-index") {
            continue;
        }
        assert!(
            plugin_docs.contains(&format!("`{member}`")),
            "plugin pack docs omit `{member}`"
        );
    }
    assert!(plugin_docs.contains("process is still spawned"));

    let sdk = read("website/docs/sdk/overview.md");
    assert!(sdk.contains("cargo add codewandler-flux-sdk codewandler-flux-providers"));
    assert!(sdk.contains("cargo run -p codewandler-flux-sdk --example client_basic"));
    assert!(!sdk.contains("not published to crates.io"));
    assert!(!sdk.contains("tag = \"v0.6.0\""));
    assert!(!sdk.contains("cargo run -p flux-sdk"));
    assert!(!sdk.contains("flow_compile"));
}

#[test]
fn sdk_docs_map_the_shared_engine_and_flowclient_lifecycle() {
    let overview = read("website/docs/sdk/overview.md");
    for surface in [
        "`flux_sdk::Client`",
        "`flux_sdk::FlowClient`",
        "`flux_sdk::dsl`",
        "`flux_lang`",
        "`flux_flow`",
        "`flux_flow::engine::FlowEngine`",
    ] {
        assert!(
            overview.contains(surface),
            "website SDK overview omits the `{surface}` surface"
        );
    }
    assert!(overview.contains("There is only one agent turn engine"));
    assert!(!overview.contains("classic agent loop"));

    let flow = read("website/docs/sdk/flow-client.md");
    for method in [
        "model",
        "allow",
        "deny",
        "auto_approve",
        "approver",
        "with_sandbox",
        "storage",
        "without_prelude",
        "register_op",
        "register_pack",
        "with_sub_agents",
        "register_composites",
        "register_prelude",
        "parse",
        "parse_module",
        "analyze",
        "analyze_seeded",
        "optimize",
        "optimize_seeded",
        "execute",
        "execute_with",
        "execute_optimized",
        "run_flow",
        "run_voice_session",
    ] {
        assert!(
            flow.contains(&format!("`{method}`")),
            "website FlowClient guide omits `{method}`"
        );
    }
    for boundary in [
        "`ExecutionResult`",
        "`FlowEngine::start_flow_turn`",
        "`VoiceSessionDriver::run_flow_turns`",
        "`EngineVoiceHandler`",
    ] {
        assert!(
            flow.contains(boundary),
            "website FlowClient guide omits the {boundary} boundary"
        );
    }
    assert!(flow.contains("one-flow façade, not a durable conversation host"));
    assert!(overview.contains("`agent_loop(AgentLoopSpec)`"));
    assert!(overview.contains("`register_op(stage_fn(...))`"));
    assert!(!overview.contains("model emits Flux-Lang plans"));

    let crate_readme = read("crates/flux-sdk/README.md");
    assert!(crate_readme.contains("There is one turn engine"));
    assert!(!crate_readme.contains("classic agent loop"));
}

/// Every occurrence of the "not OS-sandboxed" disclaimer must be immediately qualified "by
/// default" — the D-133 truth pass. Before D-130..D-132, the claim was unconditionally true
/// (`Backend::Unsupported` on every platform); now a real bubblewrap (Linux) / Seatbelt (macOS)
/// backend exists behind opt-in `[sandbox]` config, so an unqualified "not OS-sandboxed" (whatever
/// punctuation trails it — a colon, semicolon, period, or "processes.") would silently regress the
/// docs back to the old overclaim. Scanning every occurrence (rather than banning one fixed old
/// sentence) is what makes this robust to each page's own old phrasing without also needing to
/// avoid colliding with the new one.
fn assert_every_os_sandboxed_disclaimer_is_qualified(docs: &str, rel: &str) {
    let phrase = "not OS-sandboxed";
    let qualifier = " by default";
    let mut search_from = 0;
    let mut occurrences = 0;
    while let Some(found) = docs[search_from..].find(phrase) {
        let start = search_from + found;
        let after = &docs[start + phrase.len()..];
        assert!(
            after.starts_with(qualifier),
            "{rel}: found \"not OS-sandboxed\" not immediately followed by \" by default\" — the \
             old unqualified claim regressed (context: {:?})",
            &docs[start.saturating_sub(30)..(start + phrase.len() + 30).min(docs.len())]
        );
        occurrences += 1;
        search_from = start + phrase.len();
    }
    assert!(
        occurrences > 0,
        "{rel} must state the trusted-native plugin boundary"
    );
}

#[test]
fn plugin_security_copy_keeps_the_native_code_trust_boundary_explicit() {
    for rel in [
        "website/docs/plugins/using-plugins.md",
        "website/docs/plugins/authoring.md",
        "website/docs/security/plugin-sandbox.md",
        "website/docs/agent/safety.md",
        "website/docs/infrastructure.md",
    ] {
        let docs = read(rel);
        assert_every_os_sandboxed_disclaimer_is_qualified(&docs, rel);
        assert!(!docs.contains("Plugins do **no privileged IO of their own**"));
        assert!(!docs.contains("A plugin never opens a socket"));
    }
}

/// D-133: the new OS-sandbox security page exists and carries its key claims — platform coverage,
/// the config surface, the fail-closed `require` promise, and (verbatim, per the design doc) the
/// honesty list of what v1 does not defend against.
#[test]
fn os_sandbox_page_exists_and_states_its_key_claims() {
    let docs = read("website/docs/security/os-sandbox.md");
    assert!(
        docs.contains("What v1 does not defend against"),
        "os-sandbox.md must carry the honesty-list heading"
    );
    assert!(docs.contains("bubblewrap"), "must name the Linux backend");
    assert!(docs.contains("Seatbelt"), "must name the macOS backend");
    assert!(
        docs.contains("fails closed"),
        "must state the require-mode fail-closed promise"
    );
    assert!(
        docs.contains("[sandbox]"),
        "must reference the `[sandbox]` config table"
    );
    assert!(
        docs.contains("spawn_debug_pipe") || docs.contains("browser"),
        "must document the browser exemption"
    );
}

/// C-250: the enumeration that has now rotted twice — the board pages listed **seven** generated ops
/// while the code generated nine (C-236's `query`/`comments`), then nine while it generated eleven
/// (C-240's `reassign`/`record_evidence`). Both times the docs went stale within hours and nothing
/// went red, because a board's ops are *generated per backend* and never enter the builtin catalog
/// `operations_reference_covers_the_registered_public_catalog` walks.
///
/// The generator is public, though, so the list can be read off the code rather than retyped: the
/// same discipline that keeps the op names in that test honest applies here. Read the names and the
/// `query` row fields off `Tool::spec`, so adding an op or a row field is a red test rather than a
/// silent documentation gap.
#[test]
fn board_pages_enumerate_every_generated_board_operation_and_query_row_field() {
    let tools = flux_capabilities::work_board_tools(
        "board",
        Arc::new(flux_capabilities::MemoryBoard::new()),
    )
    .expect("the in-memory board satisfies its own contract");
    let names: Vec<String> = tools.iter().map(|tool| tool.spec().name).collect();

    // The typed `query` rows are the half a Program consumes as data, and the field list is the same
    // closed-enumeration hazard one level down: `attempts` was omitted next to a sentence asserting
    // "every row carries every field".
    let row_fields: Vec<String> = tools
        .iter()
        .map(|tool| tool.spec())
        .find(|spec| spec.name == "board.query")
        .and_then(|spec| spec.output_schema)
        .and_then(|schema| schema["items"]["properties"].as_object().cloned())
        .expect("`board.query` declares an output schema of typed row objects")
        .keys()
        .cloned()
        .collect();

    for rel in [
        "website/docs/agent/fleet.md",
        "website/docs/agent/datasources.md",
    ] {
        let page = read(rel);
        let missing: Vec<&String> = names
            .iter()
            .filter(|name| !page.contains(&format!("`{name}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "{rel} omits {} of the {} generated board operation(s): {missing:?}",
            missing.len(),
            names.len()
        );
    }

    // Only `fleet.md` enumerates the row fields; `datasources.md` links to it rather than repeating
    // the list, which is the right shape — a second copy is a second thing to let rot.
    let rows = read("website/docs/agent/fleet.md");
    let missing: Vec<&String> = row_fields
        .iter()
        .filter(|field| !rows.contains(&format!("`{field}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "website/docs/agent/fleet.md omits {} of the {} `board.query` row field(s): {missing:?}",
        missing.len(),
        row_fields.len()
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The context-management page (C-441)
// ─────────────────────────────────────────────────────────────────────────────────────────────────

const CONTEXT_PAGE: &str = "website/docs/agent/context-management.md";

/// C-441: the context-management page's numbers and knob names come off the code, not off prose.
///
/// The page exists because compaction's entire user-facing documentation was one row in a 500-line
/// config table. The hazard in fixing that by writing a concept page is the usual one — a second
/// copy of a value that then drifts from the constant. So every quantity the page states is read
/// back out of the source that owns it: the threshold default off `DEFAULT_COMPACT_THRESHOLD_CHARS`,
/// the per-result cap off `tool_output_cap`, and the message floor and keep count off
/// `compaction_attempt` itself.
///
/// Two of these assertions are about *honesty* rather than coverage, and they are the ones worth
/// keeping if this test is ever trimmed:
///
/// - the threshold is a **character** count, not a fraction of the model's context window
///   (C-462 is filed against exactly that), so the page must not imply the latter; and
/// - the summary **replaces** the live history, which changes what a later reader of the session
///   sees — the page must say so rather than leave a user to discover it.
#[test]
fn context_management_page_matches_the_compaction_the_code_implements() {
    let page = read(CONTEXT_PAGE);

    // The threshold default, read off the constant that owns it rather than retyped. Both the
    // grouped Rust spelling and the digit-grouped prose spelling are accepted — the page is prose.
    let agent_src = read("crates/flux-agent/src/lib.rs");
    let default_threshold: usize = agent_src
        .split_once("pub const DEFAULT_COMPACT_THRESHOLD_CHARS: usize = ")
        .expect("DEFAULT_COMPACT_THRESHOLD_CHARS is declared in flux-agent")
        .1
        .split(';')
        .next()
        .expect("terminated constant")
        .trim()
        .replace('_', "")
        .parse()
        .expect("the compaction default is a number");
    let grouped = |n: usize| {
        let digits = n.to_string();
        let mut out = String::new();
        for (idx, ch) in digits.chars().enumerate() {
            if idx > 0 && (digits.len() - idx).is_multiple_of(3) {
                out.push(',');
            }
            out.push(ch);
        }
        out
    };
    assert!(
        page.contains(&grouped(default_threshold)) || page.contains(&default_threshold.to_string()),
        "{CONTEXT_PAGE} never states the compaction default ({default_threshold}) the code uses"
    );

    // The per-result cap is the other half of what bounds the transcript, and it has the same
    // one-row-in-a-table problem. Same treatment: read the default off the resolver.
    let runtime_src = read("crates/flux-runtime/src/lib.rs");
    let tool_cap: usize = runtime_src
        .split_once("pub fn tool_output_cap() -> usize {")
        .expect("tool_output_cap resolves the per-result cap")
        .1
        .split_once(".unwrap_or(")
        .expect("tool_output_cap has a default")
        .1
        .split(')')
        .next()
        .expect("terminated default")
        .trim()
        .replace('_', "")
        .parse()
        .expect("the per-result cap default is a number");
    assert!(
        page.contains(&grouped(tool_cap)) || page.contains(&tool_cap.to_string()),
        "{CONTEXT_PAGE} never states the per-result output cap ({tool_cap}) the code uses"
    );

    // Every knob the page names must be a variable shipped code actually reads, and must be
    // documented in the reference the page links to rather than re-specified here.
    let config = read("website/docs/reference/config.md");
    for knob in [
        "FLUX_COMPACT_CHARS",
        "FLUX_TOOL_OUTPUT_CAP",
        "FLUX_TURN_TOKEN_BUDGET",
    ] {
        assert!(
            page.contains(knob),
            "{CONTEXT_PAGE} omits `{knob}`, one of the controls that bounds context"
        );
        assert!(
            config.contains(knob),
            "website/docs/reference/config.md omits `{knob}` — the page links there for the value"
        );
    }

    // The gates compaction actually applies, read off `compaction_attempt`. A page that says
    // "at least four messages" while the code says three is worse than one that says neither.
    let engine_src = read("crates/flux-flow/src/engine.rs");
    let attempt = engine_src
        .split_once("async fn compaction_attempt(")
        .expect("compaction_attempt is the one compaction path")
        .1;
    assert!(
        attempt.contains("if messages.len() < 4 {"),
        "the message floor moved — update {CONTEXT_PAGE} and this assertion together"
    );
    assert!(
        attempt.contains("let keep = 2.min(messages.len());"),
        "the keep count moved — update {CONTEXT_PAGE} and this assertion together"
    );
    assert!(
        attempt.contains("if self.compact_threshold_chars == 0 {"),
        "`0` no longer disables compaction — update {CONTEXT_PAGE} and this assertion together"
    );
    for claim in ["four messages", "0"] {
        assert!(
            page.contains(claim),
            "{CONTEXT_PAGE} must state the `{claim}` half of when compaction does not fire"
        );
    }

    // C-462: the threshold counts characters of serialized history. It does NOT consult the
    // model's context window — nothing in the tree does, since `TokenCounter` has no
    // implementation. A page that implies a window fraction would be documenting a feature that
    // does not exist, and would paper over the defect C-462 is filed for.
    assert!(
        page.contains("does not")
            && (page.contains("context window") || page.contains("context-window")),
        "{CONTEXT_PAGE} must say plainly that the threshold does not consult the model's context \
         window (C-462), not imply it scales with the model"
    );

    // The claim a user is most surprised by, so the page carries it explicitly: compaction
    // replaces the live history. `SessionLog::rewrite` is simultaneously the only
    // history-replacement path and the only `Compacted` writer (C-443), which is what makes the
    // "the superseded messages stay in the log" half true — pin both halves.
    let log_src = read("crates/flux-events/src/session_log.rs");
    assert!(
        log_src.contains("NewEvent::compacted("),
        "`SessionLog::rewrite` no longer writes the `Compacted` event — the page's durability \
         claim rests on rewrite being the sole writer"
    );
    assert!(
        attempt.contains("log.rewrite(rewritten)"),
        "compaction no longer replaces history through `SessionLog::rewrite` — recheck what the \
         page claims a replay or export can still see"
    );
    let projection_src = read("crates/flux-events/src/projection.rs");
    assert!(
        projection_src.contains("EventKind::Compacted { messages } => {")
            && projection_src.contains("out.clear();"),
        "the conversation projection no longer resets on `Compacted` — the page describes it as \
         replacing what the model sees"
    );
    assert!(
        page.contains("replace") || page.contains("Replace"),
        "{CONTEXT_PAGE} must state that compaction replaces the live history"
    );
    assert!(
        page.contains("Compacted"),
        "{CONTEXT_PAGE} must name the `Compacted` event a session reader will meet"
    );

    // Honest about the absences (the story's hardest requirement). These are the things a reader
    // arriving from another harness assumes exist; each is verified absent above in review, and
    // the page must say so rather than let the reader infer it.
    for absent in ["retrieval", "per-tool"] {
        assert!(
            page.contains(absent),
            "{CONTEXT_PAGE} must say explicitly whether flux does `{absent}` — a reader who \
             assumes it exists is the failure this page is for"
        );
    }

    // The three pages that look like alternatives must stop looking like alternatives.
    for neighbour in ["context-packs", "project-context"] {
        assert!(
            page.contains(neighbour),
            "{CONTEXT_PAGE} must relate itself to `{neighbour}` — three pages that each look \
             complete is why this gap was invisible"
        );
    }

    // A page nothing links to is a page nobody finds, which was half the original defect.
    let sidebar = read("website/sidebars.js");
    assert!(
        sidebar.contains("agent/context-management"),
        "website/sidebars.js does not list the context-management page — an unlinked page \
         reproduces the findability half of C-441"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The topologies page (C-440)
// ─────────────────────────────────────────────────────────────────────────────────────────────────

const TOPOLOGIES: &str = "website/docs/topologies.md";

/// `flux <path…> --help`, as the shipped binary renders it.
///
/// `FLUX_SANDBOX=off` is declared rather than inherited, per C-266: the subcommand path is forwarded
/// in bulk, so the posture gate cannot see that every call renders help and executes nothing. Off is
/// the honest declaration for a spawn that never reaches an effect.
fn flux_help(path: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .env("FLUX_SANDBOX", "off")
        .args(path)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("run `flux {} --help`: {e}", path.join(" ")));
    assert!(
        output.status.success(),
        "`flux {} --help` exited {}",
        path.join(" "),
        output.status
    );
    String::from_utf8(output.stdout).expect("UTF-8 help")
}

/// The subcommand names clap lists under `Commands:` for one help text.
fn subcommand_names(help: &str) -> Vec<String> {
    let Some((_, after)) = help.split_once("Commands:\n") else {
        return Vec::new();
    };
    after
        .split("\n\n")
        .next()
        .expect("Commands body")
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_string)
        .collect()
}

/// Every `flux …` line the topologies page prints as runnable is real CLI surface today.
///
/// The page's entire value is that a reader can copy a line and have it work, so a renamed
/// subcommand or flag must break CI rather than quietly turn a documented topology into a lie. This
/// resolves each line against the shipped binary's own `--help` — the same discipline as
/// `cli_reference_covers_every_public_subcommand`, one level deeper: that test asks whether a
/// command is *mentioned*, this one asks whether what is printed actually parses.
///
/// **The convention this shares with the page**: a `sh` fence holds commands that run *today*. A
/// topology that has not landed shows its proposed spelling in a `text` fence instead, so neither a
/// reader nor this check can confuse the two. Shipping surfaces are separately pinned by
/// `topologies_page_remote_system_surface_is_real_and_shipping`.
#[test]
fn topologies_page_runnable_commands_are_real_cli_surface() {
    let page = read(TOPOLOGIES);
    let blocks = fenced_blocks(&page, "sh");
    assert!(
        !blocks.is_empty(),
        "{TOPOLOGIES} prints no runnable command — every shipping row owes the reader one"
    );

    let mut checked = 0;
    for block in &blocks {
        for line in block.lines() {
            // Strip a trailing `# …` gloss; the page annotates several commands inline.
            let line = line.split_once(" #").map_or(line, |(code, _)| code).trim();
            let Some(rest) = line.strip_prefix("flux ") else {
                continue;
            };
            let tokens: Vec<&str> = rest.split_whitespace().collect();

            // Walk as far down the subcommand tree as the line actually goes. A token that is not a
            // subcommand of the level reached is a positional (a prompt, a URL, a program path).
            let mut path: Vec<&str> = Vec::new();
            for token in tokens.iter().take_while(|t| !t.starts_with('-')) {
                let names = subcommand_names(&flux_help(&path));
                if names.iter().any(|name| name == token) {
                    path.push(token);
                } else {
                    break;
                }
            }
            assert!(
                !path.is_empty(),
                "{TOPOLOGIES} prints `{line}`, but `{}` is not a flux subcommand",
                tokens.first().unwrap_or(&"")
            );

            let help = flux_help(&path);
            for token in &tokens {
                let Some(flag) = token.strip_prefix("--") else {
                    continue;
                };
                let flag = flag.split('=').next().expect("flag name");
                if flag.is_empty() {
                    continue;
                }
                // Match the whole flag, not a prefix of one: a plain `contains` would accept a
                // documented `--serv` because the help lists `--serve`.
                let offered = help.match_indices(&format!("--{flag}")).any(|(at, text)| {
                    help[at + text.len()..]
                        .chars()
                        .next()
                        .is_none_or(|next| !next.is_ascii_alphanumeric() && next != '-')
                });
                assert!(
                    offered,
                    "{TOPOLOGIES} prints `{line}`, but `flux {} --help` does not offer `--{flag}`",
                    path.join(" ")
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 4,
        "expected the page's runnable commands, found {checked}"
    );
}

/// C-436: the shipping remote-system row stays pinned to both ends of its real CLI surface.
///
/// A docs-only claim that remote effects ship is worthless if either the daemon or client flag is
/// renamed. The runnable-fence test above validates individual commands; this test also prevents the
/// row from being downgraded to proposed while the surface remains present.
#[test]
fn topologies_page_remote_system_surface_is_real_and_shipping() {
    let page = read(TOPOLOGIES);

    assert!(
        mentions_flag(&flux_help(&["tui"]), "--remote"),
        "{TOPOLOGIES} says remote effects ship, but `flux tui --remote` is absent"
    );
    assert!(
        flux_help(&["system", "serve"]).contains("--workspace"),
        "{TOPOLOGIES} says remote effects ship, but `flux system serve --workspace` is absent"
    );
    assert!(
        page.contains("| [Local runtime, remote system](#local-runtime-remote-system) | **ships**"),
        "{TOPOLOGIES} must mark the local-runtime / remote-system row as shipping"
    );

    assert!(
        fenced_blocks(&page, "sh")
            .iter()
            .any(|block| mentions_flag(block, "--remote")),
        "{TOPOLOGIES} must show the shipping `--remote` mode in a runnable `sh` fence"
    );
}

/// C-437/C-438: the remote-system row commits to semantics that shape the wire.
/// Keep those decisions executable so a later implementation cannot quietly
/// choose local fallback, claim every native guarantee travels, or repeat the old "credentials
/// never leave" promise after an operation-bound secret has to cross the link.
#[test]
fn remote_system_topology_states_workspace_and_guarantee_boundaries() {
    let page = normalized_prose(&read(TOPOLOGIES));

    for required in [
        "remote workspace is canonical",
        "no implicit synchronization",
        "authorization and approval stay local",
        "path confinement becomes the remote system s responsibility",
        "model credentials stay local",
        "operation-bound secret",
        "crosses the encrypted link",
        "remote reported",
    ] {
        assert!(
            page.contains(required),
            "{TOPOLOGIES} is missing the remote-system contract `{required}`"
        );
    }
}

/// Whether `text` names `flag` as a *whole* flag, rather than as the prefix of a longer one.
///
/// A plain `contains` would be wrong here and quietly so: `--remote-approval` (C-453, which ships)
/// contains `--remote` (C-436, which does not), so the substring form fails a page that is telling
/// the truth. The guard has to key on the flag, not on its first eight characters.
fn mentions_flag(text: &str, flag: &str) -> bool {
    let mut from = 0usize;
    while let Some(offset) = text[from..].find(flag) {
        let at = from + offset;
        let ends_here = text[at + flag.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '-' && c != '_');
        if ends_here {
            return true;
        }
        from = at + flag.len();
    }
    false
}

/// The commitments that make this page a decision aid rather than a brochure, pinned so that
/// deleting one is a red gate rather than an edit nobody notices.
///
/// - Every row of the summary table carries one of the three status words. A row with no status is
///   the failure the story was written to prevent.
/// - `ssh` is named. Running flux on the remote box over `ssh` works today and is the right answer
///   for some readers; a page that hides the free alternative to make the product look necessary is
///   not credible about anything else on it.
/// - Both of the questions a reader actually has are column headings, so no row can answer one and
///   skip the other.
#[test]
fn topologies_page_states_a_status_for_every_row_and_names_ssh() {
    let page = read(TOPOLOGIES);

    assert!(
        page.contains("ssh"),
        "{TOPOLOGIES} must name `ssh` as a legitimate option"
    );

    // The first contiguous run of table lines after the heading — not "everything up to the next
    // blank line", which would be empty, and not "every `|` line on the page", which would sweep in
    // the per-topology tables below it.
    let table: Vec<&str> = page
        .split_once("## At a glance")
        .unwrap_or_else(|| panic!("{TOPOLOGIES} carries an at-a-glance table"))
        .1
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .collect();

    let header = table.first().copied().unwrap_or_default();
    for question in ["Your files", "Approval prompt"] {
        assert!(
            header.contains(question),
            "the at-a-glance table must answer `{question}` for every topology"
        );
    }

    let rows = &table[2.min(table.len())..]; // past the header and its separator
    assert!(
        rows.len() >= 8,
        "expected a row per topology, found {}",
        rows.len()
    );
    for row in rows {
        assert!(
            ["**ships**", "**partial**", "**proposed**"]
                .iter()
                .any(|status| row.contains(status)),
            "this topology row carries no status — ships, partial or proposed is mandatory:\n{row}"
        );
    }
}
