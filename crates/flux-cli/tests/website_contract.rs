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

use flux_cognition::CognitionPack;
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

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|x| x.to_str()) == Some("md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
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
    let docs = read("website/docs/agent/cli.md");
    for name in commands
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
    {
        assert!(
            docs.contains(&format!("`flux {name}")),
            "website CLI reference omits `flux {name}`"
        );
    }
}

#[test]
fn operations_reference_covers_the_registered_public_catalog() {
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    flux_web::register_web(&mut registry, &flux_web::WebOptions::default());
    CognitionPack::new(Arc::new(NullProvider), "mock").register(&mut registry);

    let docs = read("website/docs/language/ops.md");
    let mut names = registry.names();
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
    for name in names {
        assert!(
            docs.contains(&format!("`{name}`")),
            "website operations reference omits `{name}`"
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
