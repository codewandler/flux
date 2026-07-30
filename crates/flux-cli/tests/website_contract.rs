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

fn fenced_blocks<'a>(markdown: &'a str, language: &str) -> Vec<&'a str> {
    let open = format!("```{language}\n");
    let mut rest = markdown;
    let mut blocks = Vec::new();
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        let end = rest.find("\n```").expect("closed markdown code fence");
        blocks.push(&rest[..end]);
        rest = &rest[end + 4..];
    }
    blocks
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
    let help_table = tui_src
        .split_once("const HELP_KEYS:")
        .expect("HELP_KEYS table")
        .1
        .split_once("];")
        .expect("terminated HELP_KEYS table")
        .0;

    // The chord spellings out of the overlay table, minus the prose glosses. Each entry may list
    // alternatives ("Ctrl-J / Alt-↵ / Shift-↵"); requiring the first is enough to prove the
    // binding is on the page, without pinning the page to the overlay's exact typography.
    let mut chords: Vec<String> = Vec::new();
    for (idx, _) in help_table.match_indices("        (\"") {
        let after = &help_table[idx + "        (\"".len()..];
        let literal = after.split('"').next().expect("terminated chord literal");
        if let Some(first) = literal.split('/').next() {
            let first = first.trim();
            if !first.is_empty() {
                chords.push(first.to_string());
            }
        }
    }
    for (idx, _) in help_table.match_indices("    (\"") {
        let after = &help_table[idx + "    (\"".len()..];
        let literal = after.split('"').next().expect("terminated chord literal");
        if let Some(first) = literal.split('/').next() {
            let first = first.trim();
            if !first.is_empty() && !chords.iter().any(|c| c == first) {
                chords.push(first.to_string());
            }
        }
    }
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
        ]
        .into_iter()
        .map(|tool| tool.spec().name),
    );
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
    "FLUX_MOCK_HANG",
    "FLUX_MOCK_RESPONSE",
    "FLUX_MOCK_TOOL",
    "FLUX_MOCK_TOOL_INPUT",
    "FLUX_LIVE_BROWSER_SMOKE",
    "FLUX_LIVE_SANDBOX_SMOKE",
    "FLUX_WEB_DEFINITELY_UNSET",
    "FLUX_WEB_SECRET_ALLOW",
    "FLUX_WEB_STOLEN_TOKEN",
    "FLUX_WEB_TEST_TOKEN",
    // A `format!` prefix, not a variable: the D-116 endpoint e2e mints a per-process credential
    // key (`FLUX_D116_PGPASS_<pid>`) to prove a credential *location* is never part of the URL.
    "FLUX_D116_PGPASS_",
    // Markers flux sets for its own child processes — observable, but not knobs a user sets.
    "FLUX_BG_MARKER",
    "FLUX_C67_CWD_CHILD",
    "FLUX_EVAL_MARKER",
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
            let cfg: flux_config::Config = toml::from_str(block)
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

#[test]
fn complete_flux_fences_parse_and_legacy_syntax_stays_out() {
    let docs_root = repo_path("website/docs");
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
    for path in markdown_files(&docs_root) {
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
                flux_lang::parse::parse_program(block)
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
                flux_lang::parse::parse_program(block).unwrap_or_else(|e| {
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
    let ast = client.parse(flow).unwrap();
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
        flux_lang::parse::parse_program(source).expect("parse tutorial app")
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
        flux_lang::parse::parse_program(source).expect("parse tutorial app")
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

    let mut child = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["app", "run", "assistant.flux", "-m", "mock"])
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
