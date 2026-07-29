//! The `flux` binary.
//!
//! Product surface for adaptive agent turns, authored Flux-Lang flows and apps, replay, plugins,
//! authentication, and developer tooling. Every effect enters through the shared guarded runtime.

/// The C-208 metadata-coherence gate over the production op catalog. Test-only: it assembles the
/// registry `build_agent_with` assembles and walks every `ToolSpec`, so it must live inside the
/// crate (there is no `flux-cli` library target to test from `tests/`).
#[cfg(test)]
mod catalog_coherence;
mod changelog;
mod plugin_skill;
mod preset;
mod skill_cmd;
mod style;
mod usage;

mod a2a_cmd;
mod app_cmd;
mod args;
mod auth_cmd;
mod dispatch;
mod doctor;
mod execution;
mod export_cmd;
mod flow_cmd;
mod lab_cmd;
mod plugin_cmd;
mod policy_cmd;
mod rendering;
mod review;
mod session;
mod splash;
mod stream_json;
mod wakeup_cmd;

use a2a_cmd::*;
use app_cmd::*;
use args::*;
use auth_cmd::*;
use dispatch::*;
use doctor::*;
use execution::*;
use export_cmd::*;
use flow_cmd::*;
use lab_cmd::*;
use plugin_cmd::*;
use policy_cmd::*;
use rendering::*;
use review::*;
use session::*;
use stream_json::*;
use wakeup_cmd::*;

use std::future::Future;
use std::io::{IsTerminal, Write};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser};
use futures::StreamExt;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_agent::{
    AdaptiveLoopPolicy, AgentLoopSpec, AgentSpec, AgentStagePolicy, DEFAULT_SYSTEM_PROMPT,
};
use flux_core::{Chunk, ContentBlock, StopReason, Usage};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_orchestrate::{ProviderFactory, Role, RoleRegistry, SubAgents, TaskTool};
use flux_provider::{ChunkStream, Effort, NativeProvider, Provider, Request};
use flux_runtime::context::{
    ContextFragments, EnvContext, GitContext, ProjectFiles, Projector, RepoSignal,
};
use flux_runtime::{
    scope_runtime_turn, AllowApprover, ApprovalChoice, Approver, ExecutionAuthorization,
    ExecutionEnvironment, PermissionManager, RuntimeTurnContext, SpawnActivitySink, ToolRegistry,
    ToolResult,
};
use flux_spec::IntentSet;
use flux_system::{System, Workspace};
use reedline::{FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};
use std::borrow::Cow;

fn main() -> Result<()> {
    dispatch::run()
}

#[cfg(test)]
mod tests {
    use super::{
        build_datasources, build_invoke_input, coerce_arg_value, cost_annotation,
        credential_location, direct_flow_runtime_turn, endpoint_ref_from_parts, format_evidence,
        implicit_plugin_group, integration_plugin_caps, loop_machinery_label,
        merge_static_endpoints, new_render_suffix, parse_labels, plugin_binaries_in,
        plugin_status_one, redact_plugin_echo, render_endpoint_row, render_review_markdown,
        resolve_plugin_operation_name, run_endpoint_in, run_plugin_in, run_usage_with, should_fail,
        tool_preview, truncate, url_has_userinfo, usage_annotation, write_generated_skill,
        EndpointAction, EventStore, EventStoreCrossPluginAudit, EventStoreEgressAudit, Liveness,
        PluginAction, RedactorSecretSink, ReviewSeverity,
    };
    use flux_flow::AgentSink;
    use flux_provider::{ChunkStream, Provider, Request};
    use flux_runtime::{active_runtime_turn_context, SpawnActivity, SpawnActivitySink};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct CapturingModelProvider(Arc<Mutex<Vec<Request>>>);

    struct IgnoredSpawnActivity;

    impl SpawnActivitySink for IgnoredSpawnActivity {
        fn emit(&self, _activity: SpawnActivity) {}
    }

    /// A-80 review regression: `EngineLoopHost::set_turn` no longer stores its reporter. The CLI's
    /// direct/resumable flow runner must put that returned capability in the lexical turn scope.
    #[tokio::test]
    async fn direct_flow_runtime_scope_carries_session_and_child_reporter() {
        let turn = direct_flow_runtime_turn("s_cli", Arc::new(IgnoredSpawnActivity));

        flux_runtime::scope_runtime_turn(turn, async {
            let active = active_runtime_turn_context().expect("direct flow turn is scoped");
            assert_eq!(active.session_id().as_deref(), Some("s_cli"));
            assert!(active.spawn_activity_sink().is_some());
        })
        .await;

        assert!(active_runtime_turn_context().is_none());
    }

    #[async_trait::async_trait]
    impl Provider for CapturingModelProvider {
        fn name(&self) -> &str {
            "capture"
        }

        async fn stream(&self, request: Request) -> flux_core::Result<ChunkStream> {
            self.0.lock().unwrap().push(request);
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[test]
    fn reasoning_controls_are_visible_in_agent_help() {
        use clap::CommandFactory;

        let help = super::AgentFlagsOnly::command()
            .render_long_help()
            .to_string();
        assert!(help.contains("--think"), "{help}");
        assert!(help.contains("--effort"), "{help}");
        assert!(help.contains("--loop"), "{help}");
        assert!(help.contains("adaptive"), "{help}");
        assert!(help.contains("low"), "{help}");
        assert!(help.contains("high"), "{help}");
        assert!(help.contains("--max-model-calls"), "{help}");
        assert!(help.contains("--max-iterations"), "{help}");
    }

    #[tokio::test]
    async fn lazy_provider_resolves_only_the_inherited_default_model() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let lazy = super::LazyProvider::new("codex/unresolved-parent".into());
        let initialized = lazy.cell.set((
            Box::new(CapturingModelProvider(requests.clone())),
            "resolved-parent".into(),
        ));
        assert!(initialized.is_ok());

        let _stage_stream = lazy
            .stream(Request::new("stage-model", "stage"))
            .await
            .unwrap();
        let _default_stream = lazy
            .stream(Request::new("unresolved-parent", "default"))
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].model, "stage-model");
        assert_eq!(requests[1].model, "resolved-parent");
    }

    #[test]
    fn adaptive_config_rejects_zero_stage_limits_before_provider_setup() {
        let flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let mut config = flux_config::AgentConfig::default();
        config.adaptive.explore.max_calls = Some(0);
        let error = super::adaptive_loop_policy(&flags, &config)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("[agent.adaptive.explore] max_calls must be greater than zero"),
            "{error}"
        );
    }

    #[test]
    fn outer_loop_iterations_follow_cli_then_config_then_default_precedence() {
        use clap::Parser;

        let default_flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let mut config = flux_config::AgentConfig {
            max_iterations: Some(37),
            ..Default::default()
        };
        assert_eq!(
            super::agent_max_iterations(&default_flags, &config).unwrap(),
            37
        );

        let cli_flags = super::AgentFlagsOnly::parse_from(["flux", "--max-iterations", "41"]).agent;
        assert_eq!(
            super::agent_max_iterations(&cli_flags, &config).unwrap(),
            41
        );

        config.max_iterations = None;
        assert_eq!(
            super::agent_max_iterations(&default_flags, &config).unwrap(),
            flux_flow::DEFAULT_AGENT_LOOP_ITERATIONS
        );
        config.max_iterations = Some(0);
        assert!(super::agent_max_iterations(&default_flags, &config)
            .unwrap_err()
            .to_string()
            .contains("[agent] max_iterations must be greater than zero"));
    }

    #[test]
    fn outer_loop_iterations_reject_cli_and_config_values_above_the_practical_cap() {
        use clap::Parser;

        let default_flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let at_max = flux_config::AgentConfig {
            max_iterations: Some(flux_flow::MAX_AGENT_LOOP_ITERATIONS),
            ..Default::default()
        };
        assert_eq!(
            super::agent_max_iterations(&default_flags, &at_max).unwrap(),
            flux_flow::MAX_AGENT_LOOP_ITERATIONS
        );

        let above_max = flux_flow::MAX_AGENT_LOOP_ITERATIONS + 1;
        let config = flux_config::AgentConfig {
            max_iterations: Some(above_max),
            ..Default::default()
        };
        let config_error = super::agent_max_iterations(&default_flags, &config)
            .unwrap_err()
            .to_string();
        assert!(
            config_error.contains("[agent] max_iterations"),
            "{config_error}"
        );
        assert!(
            config_error.contains(&format!(
                "maximum of {}",
                flux_flow::MAX_AGENT_LOOP_ITERATIONS
            )),
            "{config_error}"
        );

        let cli_flags =
            super::AgentFlagsOnly::parse_from(["flux", "--max-iterations", &above_max.to_string()])
                .agent;
        let cli_error = super::agent_max_iterations(&cli_flags, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(cli_error.contains("--max-iterations"), "{cli_error}");
        assert!(
            cli_error.contains(&format!(
                "maximum of {}",
                flux_flow::MAX_AGENT_LOOP_ITERATIONS
            )),
            "{cli_error}"
        );
    }

    #[test]
    fn operation_timing_names_approval_and_execution_separately() {
        let rendered = super::format_operation_timing(flux_core::OperationTiming {
            total_us: 30_005_000,
            approval_wait_us: Some(30_000_000),
            execution_us: Some(5_000),
        });
        assert_eq!(rendered, "exec 5ms + approval 30.0s");
    }

    /// C-11: every subcommand builds providers through the ONE factory (`build_provider` /
    /// `provider_for`), and the factory owns the aws chain — with static env creds present the
    /// chain no-ops (no network) and the `aws` provider constructs from any (sync) caller, which
    /// is exactly the `flux review` sub-agent-factory path that used to fail
    /// "AWS_ACCESS_KEY_ID is not set".
    #[test]
    fn provider_factory_constructs_aws_from_static_env() {
        // Serialized implicitly: this is the only flux-cli test touching AWS_* env.
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKIATEST");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("AWS_REGION", "us-east-1");
        let (native, provider, model) =
            super::build_provider("aws/sonnet").expect("factory constructs aws from static env");
        assert_eq!(provider, "aws");
        assert_eq!(model, "us.anthropic.claude-sonnet-4-6");
        drop(native);
        let boxed = super::provider_for("aws/sonnet").expect("sub-agent factory path too");
        drop(boxed);
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        std::env::remove_var("AWS_REGION");
    }

    /// C-11: the lazy provider used by deterministic execution paths (`flow run`, `preset --run`)
    /// constructs WITHOUT touching any credential; its display name is the provider prefix.
    #[test]
    fn lazy_provider_constructs_without_credentials() {
        use flux_provider::Provider as _;
        let p = super::LazyProvider::new("anthropic/claude-sonnet-4-6".to_string());
        assert_eq!(p.name(), "anthropic");
    }

    #[test]
    fn tui_model_resolver_routes_mock_to_the_offline_provider() {
        let resolved = flux_tui::ModelResolver::resolve(&super::CliTuiModelResolver, "mock")
            .expect("mock resolution is credential-free");
        assert_eq!(resolved.provider.name(), "mock");
        assert_eq!(resolved.wire_model, "mock");
        assert_eq!(resolved.model_spec, "mock");
    }

    /// L-77: `flux render` is an explicit subcommand — positional `.flux` file, `--view
    /// source|tree` (default `source`), `-o <out>` (a `.png` suffix rasterizes, L-78).
    #[test]
    fn render_subcommand_parses() {
        use super::{Cli, Commands, RenderView};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "flux",
            "render",
            "greet.flux",
            "--view",
            "tree",
            "-o",
            "out.svg",
        ])
        .expect("`render` parses");
        match cli.command {
            Some(Commands::Render { file, view, out }) => {
                assert_eq!(file, "greet.flux");
                assert_eq!(view, RenderView::Tree);
                assert_eq!(out.as_deref(), Some("out.svg"));
            }
            other => panic!("expected Render, got {other:?}"),
        }
        // The view defaults to `source` and `-o` is optional (SVG then prints to stdout).
        let cli2 =
            Cli::try_parse_from(["flux", "render", "greet.flux"]).expect("bare render parses");
        match cli2.command {
            Some(Commands::Render { view, out, .. }) => {
                assert_eq!(view, RenderView::Source);
                assert_eq!(out, None);
            }
            other => panic!("expected Render, got {other:?}"),
        }
        // `-o` takes any path — a `.png` suffix selects rasterization downstream (L-78).
        let cli3 = Cli::try_parse_from(["flux", "render", "greet.flux", "-o", "out.png"])
            .expect("render with png out parses");
        match cli3.command {
            Some(Commands::Render { out, .. }) => assert_eq!(out.as_deref(), Some("out.png")),
            other => panic!("expected Render, got {other:?}"),
        }
    }

    #[test]
    fn saved_flow_subcommands_and_input_flags_parse() {
        use super::{Cli, Commands, FlowAction};
        use clap::Parser;

        for list_word in ["list", "ls"] {
            let cli = Cli::try_parse_from(["flux", "flow", list_word]).unwrap();
            assert!(matches!(
                cli.command,
                Some(Commands::Flow {
                    action: FlowAction::List
                })
            ));
        }

        let cli = Cli::try_parse_from([
            "flux",
            "flow",
            "run",
            "deploy",
            "--inputs",
            r#"{"env":"dev"}"#,
            "--arg",
            "replicas=2",
            "--arg",
            "replicas=3",
            "--map-inputs",
            "deploy three replicas",
            "-m",
            "aws/sonnet",
            "--yes",
            "--resumable",
            "--resume",
            "last",
            "--resume-value",
            "42",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Flow {
                action:
                    FlowAction::Run {
                        target,
                        inputs,
                        args,
                        map_inputs,
                        model,
                        yes,
                        resumable,
                        resume,
                        resume_value,
                    },
            }) => {
                assert_eq!(target, "deploy");
                assert_eq!(inputs.as_deref(), Some(r#"{"env":"dev"}"#));
                assert_eq!(args, ["replicas=2", "replicas=3"]);
                assert_eq!(map_inputs.as_deref(), Some("deploy three replicas"));
                assert_eq!(model.as_deref(), Some("aws/sonnet"));
                assert!(yes && resumable);
                assert_eq!(resume.as_deref(), Some("last"));
                assert_eq!(resume_value.as_deref(), Some("42"));
            }
            other => panic!("expected flow run, got {other:?}"),
        }
    }

    fn cli_input_ast(params: Vec<(&str, flux_flow::ast::TypeRef)>) -> flux_flow::ast::DraftAst {
        flux_flow::ast::DraftAst {
            name: Some("input-test".into()),
            params: params
                .into_iter()
                .map(|(name, ty)| flux_flow::ast::Param {
                    name: name.into(),
                    ty,
                })
                .collect(),
            body: vec![flux_flow::ast::Node::Return {
                value: Box::new(flux_flow::ast::Node::Lit {
                    value: serde_json::json!("body"),
                }),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn cli_flow_inputs_merge_and_coerce_by_declared_type() {
        use flux_flow::ast::{Node, TypeRef};
        let mut ast = cli_input_ast(vec![
            ("env", TypeRef::String),
            ("replicas", TypeRef::Number),
            ("enabled", TypeRef::Bool),
            ("tags", TypeRef::List(Box::new(TypeRef::String))),
            ("payload", TypeRef::Any),
            ("named", TypeRef::Named("DeploySpec".into())),
        ]);
        super::prepare_cli_flow_inputs(
            &mut ast,
            Some(
                r#"{"env":"json","replicas":1,"enabled":false,"tags":["old"],"payload":null,"named":{"old":true}}"#,
            ),
            &[
                "env=arg".into(),
                "replicas=not-a-number".into(),
                "replicas=3".into(),
                "enabled=true".into(),
                "tags=[\"blue\",\"green\"]".into(),
                "payload={\"mode\":\"safe\"}".into(),
                "named=plain-text".into(),
            ],
            Some("this mapper must be skipped"),
        )
        .unwrap();

        let values: std::collections::BTreeMap<String, serde_json::Value> = ast.body[..6]
            .iter()
            .map(|node| match node {
                Node::Bind { name, value, .. } => match value.as_ref() {
                    Node::Lit { value } => (name.0.clone(), value.clone()),
                    other => panic!("expected literal input bind, got {other:?}"),
                },
                other => panic!("expected input bind, got {other:?}"),
            })
            .collect();
        assert_eq!(values["env"], serde_json::json!("arg"));
        assert_eq!(values["replicas"], serde_json::json!(3));
        assert_eq!(values["enabled"], serde_json::json!(true));
        assert_eq!(values["tags"], serde_json::json!(["blue", "green"]));
        assert_eq!(values["payload"], serde_json::json!({"mode": "safe"}));
        assert_eq!(values["named"], serde_json::json!("plain-text"));
        assert!(
            !ast.body.iter().any(|node| matches!(
                node,
                Node::Bind { value, .. }
                    if matches!(value.as_ref(), Node::Call { op, .. } if op == "ai.extract")
            )),
            "a fully deterministic contract must skip --map-inputs"
        );
    }

    #[test]
    fn cli_flow_inputs_reject_bad_json_unknown_missing_and_type_mismatches() {
        use flux_flow::ast::TypeRef;
        let base = || cli_input_ast(vec![("env", TypeRef::String), ("n", TypeRef::Number)]);

        let mut ast = base();
        assert!(
            super::prepare_cli_flow_inputs(&mut ast, Some("{"), &[], None)
                .unwrap_err()
                .to_string()
                .contains("valid JSON object")
        );
        let mut ast = base();
        assert!(
            super::prepare_cli_flow_inputs(&mut ast, Some("[]"), &[], None)
                .unwrap_err()
                .to_string()
                .contains("must be a JSON object")
        );
        let mut ast = base();
        assert!(super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"env":"dev","n":1,"extra":true}"#),
            &[],
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("unknown flow input parameter(s): extra"));
        let mut ast = base();
        assert!(
            super::prepare_cli_flow_inputs(&mut ast, Some(r#"{"env":"dev"}"#), &[], None,)
                .unwrap_err()
                .to_string()
                .contains("missing required flow parameter(s): n (Number)")
        );
        let mut ast = base();
        assert!(super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"env":"dev","n":"3"}"#),
            &[],
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("input `n` expects Number, got String"));
        let mut ast = base();
        assert!(super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"env":"dev","n":3}"#),
            &["broken".into()],
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("--arg expects KEY=VALUE"));
    }

    #[test]
    fn mapper_ast_uses_missing_schema_strict_fields_and_collision_free_symbols() {
        use flux_flow::ast::{Node, TypeRef};
        let mut ast = cli_input_ast(vec![
            ("known", TypeRef::String),
            ("env", TypeRef::String),
            ("replicas", TypeRef::Number),
        ]);
        ast.body.splice(
            0..0,
            ["__flux_map_raw", "__flux_map_json", "__flux_map_args"]
                .into_iter()
                .map(|name| Node::Bind {
                    name: name.into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!("occupied"),
                    }),
                    ty: None,
                    effect: None,
                }),
        );
        super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"known":"fixed"}"#),
            &[],
            Some("three replicas in dev"),
        )
        .unwrap();

        let Node::Bind {
            name: raw,
            value: extract,
            ..
        } = &ast.body[0]
        else {
            panic!("mapper must begin with ai.extract bind")
        };
        assert_eq!(raw.0, "__flux_map_raw_1");
        let Node::Call { op, args } = extract.as_ref() else {
            panic!("mapper first bind must be a call")
        };
        assert_eq!(op, "ai.extract");
        let Node::Obj { fields } = &args[0] else {
            panic!("ai.extract must receive named args")
        };
        let Node::Lit { value } = fields["schema"].as_ref() else {
            panic!("schema must be literal")
        };
        let schema: serde_json::Value = serde_json::from_str(value.as_str().unwrap()).unwrap();
        assert_eq!(schema["required"], serde_json::json!(["env", "replicas"]));
        assert!(schema["properties"].get("known").is_none());
        assert_eq!(schema["properties"]["env"]["type"], "string");
        assert_eq!(schema["properties"]["replicas"]["type"], "number");

        assert!(matches!(&ast.body[1], Node::Bind { name, .. } if name.0 == "__flux_map_json_1"));
        assert!(matches!(&ast.body[3], Node::Bind { name, .. } if name.0 == "__flux_map_args_1"));
        for (node, expected) in ast.body[4..6].iter().zip(["env", "replicas"]) {
            let Node::Bind { name, value, .. } = node else {
                panic!("mapped field must bind")
            };
            assert_eq!(name.0, expected);
            assert!(matches!(
                value.as_ref(),
                Node::Jq {
                    optional: false,
                    ..
                }
            ));
        }
        assert!(matches!(&ast.body[6], Node::Bind { name, value, .. }
            if name.0 == "known" && matches!(value.as_ref(), Node::Lit { .. })));
    }

    /// L-77: the render handler reads the `.flux` file from the plain filesystem (absolute and
    /// out-of-workspace paths work, like `flow run`), strips a UTF-8 BOM before parsing, writes
    /// the SVG through the workspace-confined `System` (`-o`), tree view propagates a hard parse
    /// error (non-zero exit), and source view is total — malformed input still renders. The
    /// inputs live OUTSIDE the workspace root, so re-jailing the read through
    /// `System::read_file` fails this test — the un-jailed read is a pinned decision, not an
    /// oversight.
    #[tokio::test]
    async fn run_render_writes_svg_and_propagates_tree_parse_errors() {
        use super::{run_render_in, RenderView};
        let base = std::env::temp_dir().join(format!("flux-render-cli-{}", std::process::id()));
        // `ws` is the System workspace root (`-o` writes land here); the inputs live in a SIBLING
        // dir the workspace envelope does not cover.
        let ws = base.join("ws");
        let srcdir = base.join("elsewhere");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&srcdir).unwrap();
        let greet = srcdir.join("greet.flux");
        std::fs::write(&greet, "flow greet(name: String)\n  do notify \"hi\"\n").unwrap();
        let broken = srcdir.join("broken.flux");
        std::fs::write(&broken, "flow ((((\n").unwrap();
        // A BOM'd but otherwise-valid file (PowerShell Out-File / Notepad) must render in tree
        // view — the BOM is stripped before the parser sees it.
        let bommed = srcdir.join("bommed.flux");
        std::fs::write(&bommed, "\u{feff}flow greet(name: String)\n  return 1\n").unwrap();
        let system = super::System::new(super::Workspace::new(&ws).unwrap());

        // The input is an ABSOLUTE path outside the workspace root (the read is NOT jailed —
        // parity with `flow run`); `-o` writes into the workspace.
        run_render_in(
            &system,
            greet.to_str().unwrap(),
            RenderView::Tree,
            Some("img/out.svg"),
        )
        .await
        .expect("tree render of a valid flow succeeds");
        let svg = std::fs::read_to_string(ws.join("img/out.svg")).unwrap();
        assert!(svg.starts_with("<svg"), "got: {svg}");

        run_render_in(&system, bommed.to_str().unwrap(), RenderView::Tree, None)
            .await
            .expect("a UTF-8 BOM is stripped, not fed to the parser");

        // A hard parse error in `tree` view surfaces the parser's message as an Err.
        let err = run_render_in(&system, broken.to_str().unwrap(), RenderView::Tree, None)
            .await
            .expect_err("tree view needs parseable source");
        assert!(err.to_string().contains("parse"), "got: {err:#}");

        // `source` view is total: the same malformed file still renders.
        run_render_in(
            &system,
            broken.to_str().unwrap(),
            RenderView::Source,
            Some("broken.svg"),
        )
        .await
        .expect("source view renders malformed input");
        assert!(std::fs::read_to_string(ws.join("broken.svg"))
            .unwrap()
            .starts_with("<svg"));
        std::fs::remove_dir_all(&base).ok();
    }

    /// L-78: `-o out.png` (any case) rasterizes through `render_flux_png` and writes BYTES
    /// through the workspace-confined `System::write_file_bytes`; a non-png extension stays SVG
    /// text, and the jail rejects a PNG escape exactly like a text escape. Stdout stays SVG.
    #[cfg(feature = "png")]
    #[tokio::test]
    async fn run_render_writes_png_bytes_and_keeps_the_jail() {
        use super::{run_render_in, RenderView};
        let base = std::env::temp_dir().join(format!("flux-render-png-cli-{}", std::process::id()));
        let ws = base.join("ws");
        let srcdir = base.join("elsewhere");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&srcdir).unwrap();
        let greet = srcdir.join("greet.flux");
        let src = "flow greet(name: String)\n  do notify \"hi\"\n";
        std::fs::write(&greet, src).unwrap();
        let system = super::System::new(super::Workspace::new(&ws).unwrap());

        run_render_in(
            &system,
            greet.to_str().unwrap(),
            RenderView::Tree,
            Some("img/out.png"),
        )
        .await
        .expect("png render of a valid flow succeeds");
        let on_disk = std::fs::read(ws.join("img/out.png")).unwrap();
        assert_eq!(&on_disk[..8], b"\x89PNG\r\n\x1a\n", "PNG magic");
        // The file is byte-identical to what the rasterizer produces for the same source, and
        // the IHDR dims match the reported canvas.
        let expected = flux_tools::render::render_flux_png(src, flux_tools::render::View::Tree)
            .expect("rasterize");
        assert_eq!(on_disk, expected.bytes);
        let w = u32::from_be_bytes(on_disk[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(on_disk[20..24].try_into().unwrap());
        assert_eq!((w, h), (expected.width, expected.height));

        // The extension match is case-insensitive…
        run_render_in(
            &system,
            greet.to_str().unwrap(),
            RenderView::Source,
            Some("UP.PNG"),
        )
        .await
        .expect("case-insensitive .png");
        assert_eq!(
            &std::fs::read(ws.join("UP.PNG")).unwrap()[..8],
            b"\x89PNG\r\n\x1a\n"
        );
        // …and any other extension writes SVG text like before.
        run_render_in(
            &system,
            greet.to_str().unwrap(),
            RenderView::Source,
            Some("out.txt"),
        )
        .await
        .expect("other extension writes svg text");
        assert!(std::fs::read_to_string(ws.join("out.txt"))
            .unwrap()
            .starts_with("<svg"));

        // The byte writer keeps the workspace jail.
        let err = run_render_in(
            &system,
            greet.to_str().unwrap(),
            RenderView::Source,
            Some("../escape.png"),
        )
        .await
        .expect_err("png escape is jailed");
        assert!(err.to_string().contains("../escape.png"), "got: {err:#}");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A registered plugin whose ABSOLUTE recorded binary is confirmed gone (a deleted checkout,
    /// a pruned pack store) is a STALE registration: it is skipped up front and reported as one
    /// aggregated warning line, not spawn-failed with a dim line per plugin on every command.
    /// Everything else defers to the spawn: absolute paths that exist, bare PATH-resolved names,
    /// and RELATIVE paths (which would resolve against whatever the current cwd happens to be —
    /// a plugin registered with `install --dir` from its checkout must not be called "missing"
    /// just because flux runs elsewhere).
    #[test]
    fn split_stale_plugins_partitions_missing_binaries() {
        let dir = std::env::temp_dir().join(format!("flux-stale-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("flux-plugin-live");
        std::fs::write(&live, b"#!/bin/sh\n").unwrap();
        let plugin = |name: &str, program: String| flux_plugin::DiscoveredPlugin {
            name: name.to_string(),
            descriptor: flux_plugin::PluginDescriptor {
                program,
                ..Default::default()
            },
        };
        let discovered = vec![
            plugin("live", live.to_string_lossy().into_owned()),
            plugin(
                "gone",
                dir.join("flux-plugin-gone").to_string_lossy().into_owned(),
            ),
            plugin("bare", "some-command-resolved-on-path".to_string()),
            plugin(
                "relative",
                "plugins/target/release/flux-plugin-rel".to_string(),
            ),
        ];
        let (loadable, stale) = super::split_stale_plugins(discovered);
        let names: Vec<&str> = loadable.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["live", "bare", "relative"],
            "existing, PATH-resolved, and cwd-relative programs all stay loadable"
        );
        assert_eq!(
            stale,
            ["gone"],
            "only an absolute program confirmed absent is stale"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-115: the `endpoint` group manifest and `endpoint_tools()` cannot drift — every
    /// registered endpoint op must be listed in the group. (Membership was never actually
    /// broken: `effective_group` falls back to each spec's own group tag — but the manifest is
    /// what config reassignment edits, so the explicit list must stay complete.)
    #[test]
    fn endpoint_group_manifest_matches_endpoint_tools() {
        use flux_capabilities::{
            EndpointBroker, EndpointRegistry, HostProviderInvoker, PluginRegistry,
        };
        use std::sync::Arc;
        let broker = Arc::new(EndpointBroker::new(
            Arc::new(HostProviderInvoker::new(Arc::new(PluginRegistry::new()))),
            Arc::new(PluginRegistry::new()),
            Arc::new(EndpointRegistry::new()),
        ));
        let tools = flux_capabilities::endpoint_tools(broker, Arc::new(EndpointRegistry::new()));
        let mut op_names: Vec<String> = tools.iter().map(|t| t.spec().name).collect();
        op_names.sort();
        let group = flux_tools::groups::builtin_groups()
            .into_iter()
            .find(|g| g.name == "endpoint")
            .expect("endpoint group exists");
        let mut listed = group.tools.clone();
        listed.sort();
        assert_eq!(
            listed, op_names,
            "the endpoint group manifest must gate every registered endpoint op"
        );
        // Registry-side gating agrees: every endpoint op self-declares the group.
        for t in &tools {
            assert_eq!(
                t.spec().group.as_deref(),
                Some("endpoint"),
                "{}",
                t.spec().name
            );
        }
    }

    /// D-115: a non-empty endpoints store injects the ambient `endpoint` signal — computed once
    /// from the startup-loaded registry, never a per-turn re-read of `endpoints.toml` — which
    /// surfaces the endpoint group with NO kubernetes signal. An empty/missing store injects
    /// nothing, and without a kubeconfig the group stays gated.
    #[test]
    fn endpoint_store_signal_surfaces_group_without_kubeconfig() {
        use flux_capabilities::EndpointRegistry;
        let dir = std::env::temp_dir().join(format!("flux-ep-signal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");

        // Empty/missing store → no ambient signal.
        let empty = EndpointRegistry::with_path(path.clone());
        empty.load().unwrap();
        assert!(
            super::session_ambient_signals(&empty).is_empty(),
            "an empty store injects nothing"
        );

        // Persist one record, reload fresh (the CLI's startup shape), and the signal appears.
        let writer = EndpointRegistry::with_path(path.clone());
        writer.put(flux_secret::endpoint::EndpointRecord {
            endpoint: flux_secret::endpoint::EndpointRef::discovered(
                "orders-pg",
                "postgres://db.internal:5432",
                "postgres",
            ),
            owner: "config".into(),
            ttl_secs: None,
            discovered_at_secs: None,
            health: None,
        });
        writer.save().unwrap();
        let loaded = EndpointRegistry::with_path(path);
        loaded.load().unwrap();
        let signals = super::session_ambient_signals(&loaded);
        assert_eq!(signals, vec!["endpoint".to_string()]);

        // With ONLY that ambient signal (no kubernetes), the built-in endpoint group surfaces;
        // with no signals at all it stays gated. `Observation::signal` is the SAME constructor
        // the engine's ambient injection uses, so this asserts the production shape, not a copy.
        let obs: Vec<flux_evidence::Observation> = signals
            .iter()
            .map(|s| flux_evidence::Observation::signal(s))
            .collect();
        let groups = flux_tools::groups::builtin_groups();
        let active = flux_evidence::resolve_active_groups(&groups, &obs);
        assert!(active.contains("endpoint"), "surfaced by the store signal");
        let none = flux_evidence::resolve_active_groups(&groups, &[]);
        assert!(!none.contains("endpoint"), "gated with no signals");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-116: `flux endpoint add` persists a weak, credential-free config-bound ref to the store, and
    /// `list`/`show` render it. The persisted file carries the credential *location*, never a value.
    #[test]
    fn endpoint_add_persists_weak_ref_and_lists() {
        use flux_capabilities::EndpointRegistry;
        let dir = std::env::temp_dir().join(format!("flux-ep-add-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");

        run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "pg-prod".into(),
                url: "postgres://db.example:5432/app".into(),
                product: Some("postgres".into()),
                protocol: Some("postgres".into()),
                credential_ref: Some("env/PGPASSWORD".into()),
                labels: vec!["region=eu".into()],
            },
        )
        .unwrap();

        // The record round-trips as a config-bound (source=Config), owner=config weak ref.
        let reg = EndpointRegistry::with_path(path.clone());
        reg.load().unwrap();
        let rec = reg.resolve("pg-prod").expect("added ref persisted");
        assert_eq!(rec.endpoint.url, "postgres://db.example:5432/app");
        assert_eq!(rec.endpoint.product, "postgres");
        assert_eq!(
            rec.endpoint.source,
            flux_secret::endpoint::SourceKind::Config
        );
        assert_eq!(rec.owner, "config");
        assert_eq!(
            rec.endpoint.credential_ref.as_ref().map(|r| r.to_string()),
            Some("env/PGPASSWORD".to_string())
        );
        assert_eq!(
            rec.endpoint.labels.get("region").map(String::as_str),
            Some("eu")
        );

        // Persisted on disk as a *location* only (the `Ref` serializes as scheme+slot, never a
        // value) — the credential slot name is present, the scheme is `env`.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("PGPASSWORD"),
            "credential slot (location) persisted"
        );
        assert!(
            on_disk.contains("env"),
            "credential scheme persisted as a location"
        );
        // The list renderer produces a row for it (reuses the same helper `flux endpoint list` uses).
        let row = render_endpoint_row(&rec);
        assert!(row.contains("pg-prod") && row.contains("postgres://db.example:5432/app"));
        // list/show/resolve all succeed against the persisted store.
        run_endpoint_in(&path, EndpointAction::List).unwrap();
        run_endpoint_in(
            &path,
            EndpointAction::Show {
                id: "pg-prod".into(),
            },
        )
        .unwrap();
        run_endpoint_in(
            &path,
            EndpointAction::Resolve {
                id: "pg-prod".into(),
            },
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-116: `flux endpoint add` rejects a credential-bearing URL, an `@endpoint/` id, and an
    /// unparseable credential ref — and leaves the store untouched on rejection.
    #[test]
    fn endpoint_add_rejects_credential_bearing_url_and_bad_inputs() {
        use flux_capabilities::EndpointRegistry;
        let dir = std::env::temp_dir().join(format!("flux-ep-add-reject-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");

        // Inline `user:pass@` is rejected with a pointer to `--credential-ref`.
        let err = run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "pg".into(),
                url: "postgres://user:secret@db.example:5432/app".into(),
                product: None,
                protocol: None,
                credential_ref: None,
                labels: vec![],
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must not embed credentials"), "got: {msg}");
        assert!(msg.contains("--credential-ref"), "points at the fix: {msg}");
        // Nothing was written — the store file does not exist yet.
        assert!(!path.exists(), "a rejected add persists nothing");

        // An `@endpoint/` id (reserved for discovered) is rejected.
        assert!(run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "@endpoint/pg".into(),
                url: "postgres://db.example:5432/app".into(),
                product: None,
                protocol: None,
                credential_ref: None,
                labels: vec![],
            },
        )
        .is_err());

        // An unparseable credential ref is rejected.
        assert!(run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "pg".into(),
                url: "postgres://db.example:5432/app".into(),
                product: None,
                protocol: None,
                credential_ref: Some("not-a-ref".into()),
                labels: vec![],
            },
        )
        .is_err());

        // The store never came into existence across all three rejections.
        let reg = EndpointRegistry::with_path(path.clone());
        reg.load().unwrap();
        assert!(reg.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-116: the shared validator's low-level invariants (also exercised by `[[endpoint.static]]`).
    #[test]
    fn endpoint_ref_from_parts_validates() {
        // A valid, unauthenticated named ref.
        let r = endpoint_ref_from_parts(
            "m",
            "http://prom:9090",
            None,
            None,
            None,
            parse_labels(&[]).unwrap(),
        )
        .unwrap();
        assert_eq!(r.id, "m");
        assert_eq!(r.source, flux_secret::endpoint::SourceKind::Config);
        assert!(r.credential_ref.is_none());

        // Userinfo detection: authority `@` is a credential, a path `@` is not.
        assert!(url_has_userinfo("postgres://u:p@host:5432/db"));
        assert!(!url_has_userinfo("postgres://host:5432/db"));
        assert!(!url_has_userinfo("https://host/path@thing"));

        // Empty id / empty url are rejected.
        assert!(
            endpoint_ref_from_parts("", "http://x", None, None, None, Default::default()).is_err()
        );
        assert!(endpoint_ref_from_parts("m", "  ", None, None, None, Default::default()).is_err());
        // A malformed label is rejected at parse time.
        assert!(parse_labels(&["novalue".to_string()]).is_err());
    }

    /// D-116: `[[endpoint.static]]` bindings merge into the registry as config-bound records that
    /// then populate the StaticResolver binding table (via `config_bindings`); an invalid entry is
    /// skipped, not fatal.
    #[test]
    fn static_endpoint_config_merges_into_registry_bindings() {
        use flux_capabilities::EndpointRegistry;
        let cfg = flux_config::Config {
            endpoint: flux_config::EndpointConfig {
                static_endpoints: vec![
                    flux_config::StaticEndpoint {
                        id: "pg-prod".into(),
                        url: "postgres://db.example:5432/app".into(),
                        product: "postgres".into(),
                        credential_ref: Some("env/PGPASSWORD".into()),
                        ..Default::default()
                    },
                    // Invalid (credential-bearing URL) — must be skipped, not abort the merge.
                    flux_config::StaticEndpoint {
                        id: "bad".into(),
                        url: "postgres://u:p@host/db".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let reg = EndpointRegistry::new();
        merge_static_endpoints(&reg, &cfg);
        let bindings = reg.config_bindings();
        assert!(
            bindings.contains_key("pg-prod"),
            "valid static binding wired"
        );
        assert!(!bindings.contains_key("bad"), "invalid entry skipped");
        assert_eq!(bindings["pg-prod"].url, "postgres://db.example:5432/app");
    }

    /// D-116 e2e (gated on `TEST_POSTGRES_URL`, like the pg backend tests): an operator-added
    /// Postgres endpoint resolves end-to-end through the broker's resolver chain — the named ref
    /// (`sql.endpoint`, the sql plugin's default dial-by-reference target) binds to its bare URL and
    /// the credential ref materializes host-side. These are exactly the two things the sql plugin
    /// asks the host for when it dials by reference and runs host-terminated SCRAM (D-31); the SCRAM
    /// leg itself is that story's tested contract, so this proof stops at the resolution seam D-116
    /// closes (before D-116 the StaticResolver had an empty map and `sql.endpoint` never resolved).
    #[tokio::test]
    async fn endpoint_add_postgres_resolves_through_broker_e2e() {
        use flux_capabilities::{
            EndpointBroker, EndpointRegistry, HostProviderInvoker, PluginRegistry, StaticResolver,
        };
        use flux_plugin::ReferenceResolver; // brings `resolve_endpoint`/`resolve_credential` in scope
        use std::sync::Arc;
        let Ok(pg_url) = std::env::var("TEST_POSTGRES_URL") else {
            eprintln!(
                "skipping endpoint_add_postgres_resolves_through_broker_e2e: TEST_POSTGRES_URL unset"
            );
            return;
        };
        // The stored URL must be credential-free — strip any userinfo the test DSN carries.
        let bare = {
            match pg_url.split_once("://") {
                Some((scheme, rest)) => {
                    let slash = rest.find('/').unwrap_or(rest.len());
                    match rest[..slash].find('@') {
                        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
                        None => pg_url.clone(),
                    }
                }
                None => pg_url.clone(),
            }
        };

        let dir = std::env::temp_dir().join(format!("flux-ep-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");
        // The credential is a *location*: an env var the host materializes, never part of the URL.
        let cred_key = format!("FLUX_D116_PGPASS_{}", std::process::id());
        std::env::set_var(&cred_key, "host-side-only");

        // Operator wires the service in one command → a weak, credential-free ref is persisted.
        run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "sql.endpoint".into(),
                url: bare.clone(),
                product: Some("postgres".into()),
                protocol: Some("postgres".into()),
                credential_ref: Some(format!("env/{cred_key}")),
                labels: vec![],
            },
        )
        .unwrap();

        // A fresh session loads the store and builds the resolver from its config bindings.
        let registry = Arc::new(EndpointRegistry::with_path(path.clone()));
        registry.load().unwrap();
        assert!(
            registry.resolve("sql.endpoint").is_some(),
            "endpoint.list / `flux endpoint list` would show the added ref"
        );
        let system = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let resolver = Arc::new(StaticResolver::new(system, registry.config_bindings()));
        let broker = EndpointBroker::new(
            Arc::new(HostProviderInvoker::new(Arc::new(PluginRegistry::new()))),
            Arc::new(PluginRegistry::new()),
            registry,
        )
        .with_static_resolver(resolver);

        // Dial-by-reference: the named ref binds to its bare URL through the broker chain.
        let resolved = broker.resolve_endpoint("sql.endpoint").await.unwrap();
        assert_eq!(resolved.url, bare);
        // Host-terminated auth: the credential ref materializes host-side (the value never enters a
        // plugin — this is the same host-side read `host.conn_authenticate` performs for SCRAM).
        let material = broker
            .resolve_credential(&flux_secret::Ref::env(&cred_key))
            .await
            .unwrap();
        assert_eq!(material.value, "host-side-only");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// F6: `flux plugin list` is accepted as an alias of the terse `ls` default.
    #[test]
    fn plugin_list_is_alias_for_ls() {
        use super::{Cli, Commands};
        use clap::Parser;
        let cli = Cli::try_parse_from(["flux", "plugin", "list"]).expect("`plugin list` parses");
        assert!(
            matches!(
                cli.command,
                Some(Commands::Plugin {
                    action: Some(PluginAction::Ls)
                })
            ),
            "`plugin list` should resolve to the Ls action"
        );
        // The terse form still resolves the same way.
        let cli2 = Cli::try_parse_from(["flux", "plugin", "ls"]).expect("`plugin ls` parses");
        assert!(matches!(
            cli2.command,
            Some(Commands::Plugin {
                action: Some(PluginAction::Ls)
            })
        ));
    }

    /// D-87: the `--git` source install parses its ref/bin/force flags and enforces the mode
    /// exclusivity (a third mode beside names/`--all` and `--dir`), and the ref flags are mutually
    /// exclusive and require `--git`.
    #[test]
    fn plugin_install_git_flags_parse_and_conflict() {
        use super::{Cli, Commands};
        use clap::Parser;
        // A well-formed source install parses into the git fields.
        let cli = Cli::try_parse_from([
            "flux",
            "plugin",
            "install",
            "--git",
            "https://gitlab.example/g/flux-plugin-x.git",
            "--tag",
            "v1.2.3",
            "--bin",
            "flux-plugin-x",
            "--force",
        ])
        .expect("`install --git … --tag … --bin … --force` parses");
        match cli.command {
            Some(Commands::Plugin {
                action:
                    Some(PluginAction::Install {
                        git,
                        tag,
                        rev,
                        branch,
                        bin,
                        force,
                        names,
                        all,
                        dir,
                    }),
            }) => {
                assert_eq!(
                    git.as_deref(),
                    Some("https://gitlab.example/g/flux-plugin-x.git")
                );
                assert_eq!(tag.as_deref(), Some("v1.2.3"));
                assert_eq!(bin.as_deref(), Some("flux-plugin-x"));
                assert!(force && rev.is_none() && branch.is_none());
                assert!(names.is_empty() && !all && dir.is_none());
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        // `--git` is exclusive with `--dir` and with plugin names.
        assert!(Cli::try_parse_from(
            ["flux", "plugin", "install", "--git", "u", "--dir=some/dir",]
        )
        .is_err());
        assert!(
            Cli::try_parse_from(["flux", "plugin", "install", "--git", "u", "gitlab"]).is_err()
        );
        // Ref flags are mutually exclusive.
        assert!(Cli::try_parse_from([
            "flux", "plugin", "install", "--git", "u", "--tag", "t", "--branch", "b",
        ])
        .is_err());
        // A ref flag with no `--git` and no positional errors at the clap layer.
        assert!(Cli::try_parse_from(["flux", "plugin", "install", "--tag", "t"]).is_err());
    }

    /// D-87: the `--git` ref/bin/force flags require `--git` even when a positional name is present
    /// (clap skips its `requires` there because `--git` conflicts with the name) — a runtime guard
    /// rejects the misuse rather than running a remote/`--dir` install that silently ignores them.
    #[tokio::test]
    async fn plugin_install_ref_flags_require_git_at_runtime() {
        let dir =
            std::env::temp_dir().join(format!("flux-install-refguard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec!["gitlab".into()],
                all: false,
                dir: None,
                git: None,
                tag: Some("v1".into()),
                rev: None,
                branch: None,
                bin: None,
                force: false,
            }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--git"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// F2: the zero-arg ambient reads (`now`/`cwd`/`home_dir`/`sys_info`) are pre-allowed by the
    /// default permission set, so a `now()` in a stored flow never reaches the approval gate (which
    /// auto-denies on a non-TTY). Workspace reads stay allowed; a mutating op still gates.
    #[test]
    fn default_allow_covers_ambient_reads() {
        use flux_runtime::{PermDecision, PermissionManager};
        let allow: Vec<String> = super::DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect();
        let m = PermissionManager::from_rules(&allow, &[]);
        for op in ["now", "cwd", "home_dir", "sys_info", "read"] {
            assert_eq!(
                m.check(op, &[]),
                PermDecision::Allow,
                "`{op}` should be pre-allowed by the default permission set"
            );
        }
        // A mutating op is not in the default set — it still gates.
        assert_eq!(m.check("write", &[]), PermDecision::Ask);
    }

    /// The grouped `flux auth status` renderer: summary line, active-default marker, the two state
    /// groups, and per-provider setup hints.
    #[test]
    fn auth_status_groups_by_state() {
        use flux_credentials::ProviderAuth;
        let rows = vec![
            ProviderAuth {
                provider: "anthropic",
                available: true,
                source: "ANTHROPIC_API_KEY (env)".into(),
                hint: None,
            },
            ProviderAuth {
                provider: "claude",
                available: false,
                source: "not found".into(),
                hint: Some("flux auth login claude".into()),
            },
            ProviderAuth {
                provider: "openai",
                available: true,
                source: "OPENAI_API_KEY (env)".into(),
                hint: None,
            },
        ];
        let out = super::format_auth_status(&rows, "sonnet", Some("anthropic"));
        assert!(out.contains("Providers · 2 of 3 configured"), "{out}");
        assert!(out.contains("default model: sonnet → anthropic ✓"), "{out}");
        assert!(out.contains("Available"));
        assert!(out.contains("Not configured"));
        assert!(out.contains("flux auth login claude"), "{out}");
        // The active marker lands on anthropic only.
        let active_line = out
            .lines()
            .find(|l| l.contains("← active"))
            .expect("an active row");
        assert!(active_line.contains("anthropic"));
        assert!(!out.contains("openai   ← active"));
    }

    /// The `provider/model` spec → auth-status-row mapping used to flag the active provider.
    #[test]
    fn auth_row_mapping() {
        assert_eq!(super::auth_row_for_spec("sonnet"), Some("anthropic"));
        assert_eq!(super::auth_row_for_spec("fable"), Some("anthropic"));
        assert_eq!(super::auth_row_for_spec("claude"), Some("claude"));
        assert_eq!(super::auth_row_for_spec("claude/sonnet"), Some("claude"));
        // C-169: one OpenRouter key, one row, for every model the gateway proxies.
        assert_eq!(
            super::auth_row_for_spec("openrouter/anthropic/claude-opus-4.6"),
            Some("openrouter")
        );
        assert_eq!(
            super::auth_row_for_spec("openrouter/google/gemini-3.5-flash"),
            Some("openrouter")
        );
        assert_eq!(super::auth_row_for_spec("ollama/llama"), None);
    }

    /// C-49: spec parsing — bare aliases, bare-provider defaults, and the client-side empty-model
    /// rejection (a spec like `claude/` previously shipped an empty model id to the API and came
    /// back as a confusing HTTP 400). D-152 moved the parser into `flux-providers`; this asserts the
    /// CLI's view of the shared function still surfaces the exact provider-error strings.
    #[test]
    fn parse_model_spec_covers_aliases_defaults_and_rejects_empty_models() {
        let parse = flux_providers::spec::parse_model_spec;
        // Bare anthropic short-names carry the alias through as the model.
        assert_eq!(
            parse("sonnet").unwrap(),
            ("anthropic".into(), "sonnet".into())
        );
        assert_eq!(
            parse("fable").unwrap(),
            ("anthropic".into(), "fable".into())
        );
        // Bare `claude` defaults to the subscription's sonnet, like bare `codex`/`aws` defaults.
        assert_eq!(parse("claude").unwrap(), ("claude".into(), "sonnet".into()));
        assert_eq!(parse("codex").unwrap(), ("codex".into(), "".into()));
        assert_eq!(parse("aws").unwrap(), ("aws".into(), "".into()));
        // Fully-qualified specs pass through.
        assert_eq!(
            parse("claude/claude-fable-5").unwrap(),
            ("claude".into(), "claude-fable-5".into())
        );
        // Empty model after the slash: rejected client-side with an actionable hint…
        let err = parse("claude/").unwrap_err().to_string();
        assert!(err.contains("no model"), "unexpected: {err}");
        assert!(err.contains("claude/sonnet"), "unexpected: {err}");
        let err = parse("anthropic/").unwrap_err().to_string();
        assert!(err.contains("no model"), "unexpected: {err}");
        // …except for the two providers whose resolvers document an "" → default mapping.
        assert_eq!(parse("codex/").unwrap(), ("codex".into(), "".into()));
        // Unknown bare words still point at the spec shape and the alias set.
        let err = parse("gpt-5.5").unwrap_err().to_string();
        assert!(err.contains("claude/sonnet"), "unexpected: {err}");
        assert!(!err.contains("claude/gpt-5.5"), "unexpected: {err}");
    }

    #[test]
    fn app_serve_provider_honors_mock() {
        // A-60 / F-014: a served program under `--serve -m mock` must resolve to the offline mock
        // provider, not fall through to the Anthropic path (which fails on low credits).
        let (provider, model) = super::app_provider_for("mock");
        assert_eq!(model, "mock");
        assert_eq!(
            provider.expect("mock provider built").name(),
            "mock",
            "served -m mock resolves to the offline mock, not Anthropic"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reset_sigpipe_installs_sig_dfl() {
        // A-61 / F-006: after the reset, SIGPIPE must be SIG_DFL — Rust's std defaults it to SIG_IGN,
        // which is exactly what makes `println!` panic on a broken pipe. `signal()` returns the
        // PREVIOUS disposition, so reading it back right after the reset proves it installed SIG_DFL
        // (a no-op reset would read back SIG_IGN and fail this).
        super::reset_sigpipe();
        let prev = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
        assert_eq!(prev, libc::SIG_DFL, "reset_sigpipe installs SIG_DFL");
    }

    #[test]
    fn diagnostics_header_matches_the_failure_class() {
        // A-62 / F-010: the "references unknown operations" header/refusal must appear ONLY when every
        // diagnostic is genuinely an unknown-op error — a non-unknown-op failure under that header
        // misleads both the reader and a repair-reading model stage.
        use flux_flow::analyze::Diagnostic;
        let unknown = vec![Diagnostic::new("unknown operation: `foo`")];
        assert!(super::diagnostics_all_unknown_op(&unknown));
        let other = vec![Diagnostic::new(
            "a value template (`obj`/`list`) may only contain pure value leaves",
        )];
        assert!(
            !super::diagnostics_all_unknown_op(&other),
            "a non-unknown-op failure is not labeled 'unknown operations'"
        );
        let mixed = vec![
            Diagnostic::new("unknown operation: `foo`"),
            Diagnostic::new("`return` is not allowed inside a `parallel` branch"),
        ];
        assert!(
            !super::diagnostics_all_unknown_op(&mixed),
            "a mixed set is not all-unknown-op"
        );
        assert!(
            !super::diagnostics_all_unknown_op(&[]),
            "empty is not unknown-op"
        );
    }

    /// L-02: skill discovery layers CLI `--skill-dir` above `[skills] dirs` from config, above the
    /// well-known defaults — earlier layers win a name clash.
    #[test]
    fn load_skills_layers_cli_over_config_over_defaults() {
        let root = std::env::temp_dir().join(format!("flux-cli-skills-{}", std::process::id()));
        for (dir, body) in [
            (".flux/skills", "from default"),
            ("cfg-skills", "from config"),
            ("cli-skills", "from cli"),
        ] {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("s.md"),
                format!("---\nname: l02-cli-layering\n---\n{body}"),
            )
            .unwrap();
        }
        let cfg = flux_config::Config {
            skills: flux_config::SkillsConfig {
                dirs: vec!["cfg-skills".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        // Config layer beats the well-known default...
        let enabled = vec!["l02-cli-layering".to_string()];
        let skills = super::load_skills(&root, &cfg, &[], &enabled).unwrap();
        let s = skills
            .iter()
            .find(|s| s.name == "l02-cli-layering")
            .unwrap();
        assert_eq!(s.body, "from config");

        // ...and a CLI --skill-dir beats the config layer.
        let skills = super::load_skills(&root, &cfg, &[root.join("cli-skills")], &enabled).unwrap();
        let s = skills
            .iter()
            .find(|s| s.name == "l02-cli-layering")
            .unwrap();
        assert_eq!(s.body, "from cli");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn production_role_discovery_rejects_malformed_tools_before_agent_assembly() {
        let root = std::env::temp_dir().join(format!(
            "flux-cli-role-guard-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join(".flux/agents")).unwrap();
        std::fs::write(
            root.join(".flux/agents/broken.md"),
            "---\ntools: read\n---\nThis role must not inherit everything.",
        )
        .unwrap();

        let error = super::load_roles(&root).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(".flux/agents/broken.md"), "{message}");
        assert!(message.contains("tools"), "{message}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skills_are_disabled_until_named_explicitly() {
        let root =
            std::env::temp_dir().join(format!("flux-cli-manual-skills-{}", std::process::id()));
        let dir = root.join(".flux/skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("automatic.md"),
            "---\nname: automatic\ntriggers: [hello]\n---\nlarge body",
        )
        .unwrap();
        let cfg = flux_config::Config::default();
        assert!(
            super::load_skills(&root, &cfg, &[], &[])
                .unwrap()
                .is_empty(),
            "discovery and prompt triggers must not enable a skill"
        );
        let enabled = super::load_skills(&root, &cfg, &[], &["automatic".to_string()]).unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "automatic");
        std::fs::remove_dir_all(&root).ok();
    }

    /// D-188: with the opt-in off (`model_invoked: false`), `load_model_invoked_skill_catalog`
    /// discovers nothing at all — no directory walk, no catalog — matching the default-off
    /// invariant `skills_are_disabled_until_named_explicitly` pins for `load_skills`.
    #[test]
    fn model_invoked_catalog_is_empty_when_the_opt_in_is_off() {
        let root =
            std::env::temp_dir().join(format!("flux-cli-model-invoked-off-{}", std::process::id()));
        let dir = root.join(".flux/skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pdf.md"),
            "---\nname: pdf-extract\ndescription: extract PDFs\n---\nbody",
        )
        .unwrap();
        let cfg = flux_config::Config::default();
        assert!(
            super::load_model_invoked_skill_catalog(&root, &cfg, &[], false)
                .unwrap()
                .is_empty(),
            "the opt-in is off, so nothing should be discovered"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// D-188: with the opt-in on, every discovered skill is surfaced EXCEPT one that declares
    /// `disable-model-invocation: true` — that field opts a skill out of both surfacing and
    /// on-demand loading, not just loading.
    #[test]
    fn model_invoked_catalog_excludes_disable_model_invocation_skills() {
        let root = std::env::temp_dir().join(format!(
            "flux-cli-model-invoked-exclude-{}",
            std::process::id()
        ));
        let dir = root.join(".flux/skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pdf.md"),
            "---\nname: pdf-extract\ndescription: extract PDFs\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.join("private.md"),
            "---\nname: private-only\ndescription: manual-only skill\ndisable-model-invocation: \
             true\n---\nbody",
        )
        .unwrap();
        let cfg = flux_config::Config::default();
        let catalog = super::load_model_invoked_skill_catalog(&root, &cfg, &[], true).unwrap();
        let names: Vec<&str> = catalog.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"pdf-extract"), "got {names:?}");
        assert!(
            !names.contains(&"private-only"),
            "disable-model-invocation must exclude the skill from the catalog: {names:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_explicit_skill_fails_before_agent_construction() {
        let root =
            std::env::temp_dir().join(format!("flux-cli-unknown-skill-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let error = super::load_skills(
            &root,
            &flux_config::Config::default(),
            &[],
            &["missing".to_string()],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("unknown skill `missing` (discovered:"),
            "{error}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// D-189: a skill's `model` frontmatter is a precedence tier between the explicit `--model`
    /// flag and config/default — same spirit as `Role::to_spec`'s `model.unwrap_or(default_model)`,
    /// just one tier lower than the caller's own explicit choice rather than above it.
    #[test]
    fn skill_model_sits_between_explicit_cli_model_and_config_default() {
        let cfg = flux_config::Config {
            model: Some("config-model".to_string()),
            ..Default::default()
        };
        let mut skill = flux_skill::parse("---\nname: fast\nmodel: haiku\n---\nbody", None);

        // No CLI flag, an enabled skill sets `model` → the skill wins over config.
        assert_eq!(
            super::resolve_model_spec_with_skill(&None, &cfg, std::slice::from_ref(&skill)),
            "haiku"
        );
        // An explicit CLI/SDK model always wins over the skill's request.
        assert_eq!(
            super::resolve_model_spec_with_skill(
                &Some("cli-model".to_string()),
                &cfg,
                std::slice::from_ref(&skill)
            ),
            "cli-model"
        );
        // No skill model, no CLI flag → falls through to config.
        skill.model = None;
        assert_eq!(
            super::resolve_model_spec_with_skill(&None, &cfg, std::slice::from_ref(&skill)),
            "config-model"
        );
        // Nothing at all → the hardcoded default.
        let empty_cfg = flux_config::Config::default();
        assert_eq!(
            super::resolve_model_spec_with_skill(&None, &empty_cfg, &[]),
            "sonnet"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bounded_collector_polls_plugin_loads_concurrently() {
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let futures = (0..4)
            .map(|value| {
                let active = active.clone();
                let maximum = maximum.clone();
                async move {
                    let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    // Deliberately block before this future can yield. `buffer_unordered` alone does
                    // not provide concurrency for this shape; each plugin loader performs a small
                    // synchronous verify/spawn prefix with the same property.
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    value
                }
            })
            .collect();
        let mut values = super::collect_bounded(futures, 4).await.unwrap();
        values.sort_unstable();
        assert_eq!(values, [0, 1, 2, 3]);
        assert!(
            maximum.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "plugin handshakes were polled sequentially"
        );
    }

    /// C-08: `flux auth login codex` drives a full PKCE flow — authorize URL with challenge+state,
    /// callback code exchanged (form-encoded `authorization_code` grant with the verifier), token
    /// persisted under the `codex` provider. Hermetic: a loopback stub stands in for
    /// auth.openai.com's token endpoint and the callback is injected (no browser, no port 1455).
    /// Serialized implicitly: the only flux-cli test that repoints HOME (the store is ~/.flux).
    #[tokio::test]
    async fn auth_login_codex_runs_pkce_flow() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let home = std::env::temp_dir().join(format!("flux-login-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        // Stub token endpoint: answers one POST with a token response, captures the request.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
            }
            let body =
                r#"{"access_token":"at_cli_c08","refresh_token":"rt_cli_c08","expires_in":3600}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });

        // Injected callback: assert the authorize URL carries PKCE + this login's state, then
        // return the `code#state` shape the real localhost:1455 listener produces.
        super::codex_login_flow(
            &format!("http://{addr}/oauth/token"),
            |url, state| async move {
                assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
                assert!(url.contains("code_challenge="));
                assert!(url.contains("code_challenge_method=S256"));
                assert!(url.contains(&format!("state={state}")));
                Ok(format!("cli-test-code#{state}"))
            },
        )
        .await
        .expect("login flow completes against the stub endpoint");

        // The exchange was a PKCE authorization_code grant…
        let req = server.await.unwrap();
        assert!(req.contains("grant_type=authorization_code"));
        assert!(req.contains("code=cli-test-code"));
        assert!(req.contains("code_verifier="));

        // …and the token landed under the `codex` provider, in the same store import fills.
        let store = std::fs::read_to_string(home.join(".flux").join("credentials.toml")).unwrap();
        std::fs::remove_dir_all(&home).ok();
        assert!(store.contains("[codex]"), "stored under `codex`: {store}");
        assert!(store.contains("at_cli_c08"));
    }

    /// D-82: the plugin `authorization_code` login builds a PKCE authorize URL from the manifest
    /// config and exchanges the callback code against the token endpoint, yielding a storable token
    /// (the store→resolve path a later `plugin call` uses is covered in flux-plugin). No `$HOME`
    /// mutation, so it can't race the codex login test.
    #[tokio::test]
    async fn plugin_oauth_code_grant_builds_pkce_url_and_exchanges() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
            }
            let body =
                r#"{"access_token":"at_plugin","refresh_token":"rt_plugin","expires_in":3600}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });

        let token = super::plugin_oauth_code_grant(
            &format!("http://{addr}/oauth/token"),
            "https://auth.example.com/oauth/authorize",
            "plugin-client",
            "read write",
            "http://localhost:9876/cb",
            |url, state| async move {
                assert!(url.starts_with("https://auth.example.com/oauth/authorize?"));
                assert!(url.contains("client_id=plugin-client"));
                assert!(url.contains("code_challenge="));
                assert!(url.contains("code_challenge_method=S256"));
                assert!(url.contains(&format!("state={state}")));
                Ok(format!("plugin-code#{state}"))
            },
        )
        .await
        .expect("plugin code grant completes against the stub endpoint");

        assert_eq!(token.access, "at_plugin");
        assert_eq!(token.refresh.as_deref(), Some("rt_plugin"));

        let req = server.await.unwrap();
        assert!(req.contains("grant_type=authorization_code"));
        assert!(req.contains("code=plugin-code"));
        assert!(req.contains("code_verifier="));
        assert!(req.contains("client_id=plugin-client"));
    }

    /// C-08: the OAuth callback parser — happy path, provider error, and junk.
    #[test]
    fn parse_codex_callback_extracts_code_and_state() {
        let (code, state) =
            super::parse_codex_callback("code=abc%2F123&state=st8&scope=openid").unwrap();
        assert_eq!(code, "abc/123");
        assert_eq!(state, "st8");
        let err = super::parse_codex_callback("error=access_denied&state=st8").unwrap_err();
        assert!(err.to_string().contains("access_denied"));
        assert!(super::parse_codex_callback("foo=bar").is_err());
    }

    /// `build_datasources` walks a `markdown` datasource's directory and ingests its docs into a shared
    /// backend; an unknown `kind` is a clean error.
    #[tokio::test]
    async fn build_datasources_ingests_markdown_and_rejects_unknown_kinds() {
        use flux_lang::program::DatasourceDecl;
        use flux_system::{System, Workspace};

        let dir = std::env::temp_dir().join(format!("flux-ds-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Canonicalize so the program dir matches the (canonicalized) workspace root on platforms where
        // the temp dir is a symlink (e.g. macOS `/tmp` → `/private/tmp`).
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("note.md"), "# Title\nhello from a markdown note").unwrap();
        let system = System::new(Workspace::new(&dir).unwrap());

        let ok = vec![DatasourceDecl {
            name: "docs".into(),
            kind: "markdown".into(),
            path: Some(".".into()),
            settings: serde_json::Value::Null,
        }];
        let bound = build_datasources(&ok, &dir, &system).await.unwrap();
        assert!(
            !bound.knowledge.is_empty(),
            "the markdown note was ingested"
        );
        assert!(
            bound.boards.is_empty(),
            "a knowledge kind declares no board"
        );

        let bad = vec![DatasourceDecl {
            name: "x".into(),
            kind: "nope".into(),
            path: None,
            settings: serde_json::Value::Null,
        }];
        assert!(
            build_datasources(&bad, &dir, &system).await.is_err(),
            "an unknown datasource kind is a clean error"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A relative datasource `path` resolves against the PROGRAM FILE's directory, not the process cwd —
    /// so `flux app run <elsewhere>/support-bot.flux` indexes the `./docs` shipped beside the program even
    /// when launched from an unrelated directory. Here the workspace root (the "cwd") and the program dir
    /// are siblings: `./docs` must pull the program dir's corpus and ignore a decoy under the cwd root.
    #[tokio::test]
    async fn build_datasources_resolves_relative_path_against_program_dir() {
        use flux_datasource::SearchInput;
        use flux_lang::program::DatasourceDecl;
        use flux_system::{System, Workspace};

        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("flux-ds-cwd-{}", std::process::id())); // the launch "cwd"
        let progdir = base.join(format!("flux-ds-prog-{}", std::process::id())); // where the .flux lives
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&progdir);
        std::fs::create_dir_all(progdir.join("docs")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            progdir.join("docs/faq.md"),
            "# FAQ\nReset your password from the account settings page.",
        )
        .unwrap();
        // A decoy under the cwd root — it must NOT be indexed (proves resolution is program-relative).
        std::fs::write(
            root.join("decoy.md"),
            "# Decoy\nkielbasa should not be indexed",
        )
        .unwrap();

        // The workspace is rooted at the cwd; the program dir is registered as a read-only root, exactly
        // as `run_app` does for an out-of-cwd program.
        let mut ws = Workspace::new(&root).unwrap();
        ws.add_read_root(&progdir).unwrap();
        let system = System::new(ws);

        let decls = vec![DatasourceDecl {
            name: "docs".into(),
            kind: "markdown".into(),
            path: Some("./docs".into()),
            settings: serde_json::Value::Null,
        }];
        let backend = build_datasources(&decls, &progdir, &system)
            .await
            .unwrap()
            .knowledge;

        // The program dir's corpus is searchable...
        let hits = backend
            .search(&SearchInput {
                query: "reset password settings".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            hits.iter().any(|h| h.record.entity == "file.document"),
            "the ./docs beside the program was indexed"
        );
        // ...and the decoy under the cwd root was not.
        let decoy = backend
            .search(&SearchInput {
                query: "kielbasa".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            decoy.is_empty(),
            "a file under the cwd (not the program dir) must not be indexed"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&progdir).ok();
    }

    /// `build_datasources` ingests an `openapi` source (via the existing `ingest_openapi`) alongside a
    /// `markdown` one, so a declarative bot's help-center docs AND its OpenAPI spec are both searchable —
    /// the `flux app run` knowledge gap D-11 closes.
    #[tokio::test]
    async fn build_datasources_ingests_markdown_and_openapi_searchable() {
        use flux_datasource::SearchInput;
        use flux_lang::program::DatasourceDecl;
        use flux_system::{System, Workspace};

        let dir = std::env::temp_dir().join(format!("flux-ds-oa-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(
            dir.join("guide.md"),
            "# Booking\nHow to book a widget appointment.",
        )
        .unwrap();
        std::fs::write(
            dir.join("api.json"),
            r#"{"openapi":"3.0.0","paths":{"/widgets":{"get":{"operationId":"listWidgets","summary":"List widgets"}}}}"#,
        )
        .unwrap();
        let system = System::new(Workspace::new(&dir).unwrap());

        let decls = vec![
            DatasourceDecl {
                name: "docs".into(),
                kind: "markdown".into(),
                path: Some(".".into()),
                settings: serde_json::Value::Null,
            },
            DatasourceDecl {
                name: "api".into(),
                kind: "openapi".into(),
                path: Some("api.json".into()),
                settings: serde_json::Value::Null,
            },
        ];
        let backend = build_datasources(&decls, &dir, &system)
            .await
            .unwrap()
            .knowledge;

        // The markdown note is indexed as a `file.document`...
        let md = backend
            .search(&SearchInput {
                query: "book widget appointment".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            md.iter().any(|h| h.record.entity == "file.document"),
            "markdown ingested as a file.document record"
        );
        // ...and the OpenAPI operation as an `openapi.operation`.
        let oa = backend
            .search(&SearchInput {
                query: "list widgets".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            oa.iter().any(|h| h.record.entity == "openapi.operation"),
            "OpenAPI op ingested as an openapi.operation record"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin install` scans a directory for `flux-plugin-<name>` executables: it picks those up
    /// (sorted, by stripped name) and skips sidecars (`*.d`), non-prefixed files, and an empty name.
    #[test]
    fn plugin_binaries_in_picks_flux_plugin_executables() {
        let dir = std::env::temp_dir().join(format!("flux-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "flux-plugin-gitlab",
            "flux-plugin-slack",
            "flux-plugin-jira.exe", // a Windows binary — must be picked up, not skipped (D-47)
            "flux-plugin-slack.d",  // a cargo sidecar — must be skipped
            "flux-plugin-slack.exe.d", // a sidecar on a Windows-shaped name — must also be skipped
            "flux-plugin-",         // empty name — skipped
            "flux-plugin-.exe",     // empty name, `.exe` — skipped
            "not-a-plugin",         // wrong prefix — skipped
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let found = plugin_binaries_in(&dir).unwrap();
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["gitlab", "jira", "slack"]);
        // programs are absolute (canonicalized) paths to the binaries
        assert!(found.iter().all(|(_, p)| p.contains("flux-plugin-")));
        // the Windows binary's registered program path keeps the `.exe` suffix
        assert!(
            found
                .iter()
                .any(|(n, p)| n == "jira" && p.ends_with("flux-plugin-jira.exe")),
            "{found:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin uninstall <name>` removes the descriptor; a missing name is a clean error
    /// (non-zero), never a panic (D-19).
    #[tokio::test]
    async fn plugin_uninstall_removes_descriptor() {
        let dir = std::env::temp_dir().join(format!("flux-uninstall-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        flux_plugin::add_descriptor(
            &dir,
            "p",
            &flux_plugin::PluginDescriptor {
                program: "/bin/true".into(),
                args: vec![],
                pinned: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            flux_plugin::discover(&dir).len(),
            1,
            "the descriptor is registered"
        );

        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "p".into(),
                purge: false,
            }),
        )
        .await
        .unwrap();
        assert!(
            flux_plugin::discover(&dir).is_empty(),
            "uninstall removed the descriptor"
        );

        // A missing name is a clean error, not a panic.
        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "ghost".into(),
                purge: false,
            }),
        )
        .await;
        assert!(err.is_err(), "uninstall of a missing name is a clean error");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// N-003: `flux plugin install --dir` prunes a stale LOCAL descriptor whose binary is absent
    /// from the re-scanned dir (a partial pack build), but never touches a verified pack install or
    /// a plugin registered from elsewhere, and an empty scan prunes nothing.
    #[tokio::test]
    async fn plugin_install_dir_prunes_absent_local_descriptors() {
        let base =
            std::env::temp_dir().join(format!("flux-installdir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let desc_dir = base.join("descriptors");
        let bin_dir = base.join("bin");
        std::fs::create_dir_all(&desc_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        let write_bin =
            |name: &str| std::fs::write(bin_dir.join(format!("flux-plugin-{name}")), b"x").unwrap();
        write_bin("alpha");
        write_bin("beta");

        let install = |d: &std::path::Path| PluginAction::Install {
            names: vec![],
            all: false,
            dir: Some(d.to_string_lossy().into_owned()),
            git: None,
            tag: None,
            rev: None,
            branch: None,
            bin: None,
            force: false,
        };
        let names = |d: &std::path::Path| {
            let mut v: Vec<String> = flux_plugin::discover(d)
                .into_iter()
                .map(|p| p.name)
                .collect();
            v.sort();
            v
        };

        // First scan registers both local binaries.
        run_plugin_in(&desc_dir, Some(install(&bin_dir)))
            .await
            .unwrap();
        assert_eq!(names(&desc_dir), vec!["alpha", "beta"]);

        // A plugin `add`ed from elsewhere and a synthetic VERIFIED pack install — both must survive.
        flux_plugin::add_descriptor(
            &desc_dir,
            "gamma",
            &flux_plugin::PluginDescriptor {
                program: "/bin/true".into(),
                ..Default::default()
            },
        )
        .unwrap();
        flux_plugin::add_descriptor(
            &desc_dir,
            "delta",
            &flux_plugin::PluginDescriptor {
                program: bin_dir
                    .join("flux-plugin-delta")
                    .to_string_lossy()
                    .into_owned(),
                sha256: Some("deadbeef".into()),
                version: Some("1.0.0".into()),
                source: Some("plugins-v1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // `beta` fails to rebuild: its binary disappears from the scan dir.
        std::fs::remove_file(bin_dir.join("flux-plugin-beta")).unwrap();
        run_plugin_in(&desc_dir, Some(install(&bin_dir)))
            .await
            .unwrap();
        assert_eq!(
            names(&desc_dir),
            vec!["alpha", "delta", "gamma"],
            "absent local `beta` is pruned; alpha (present), gamma (elsewhere), delta (verified) \
             survive"
        );

        // An empty scan dir prunes NOTHING (a typo'd `--dir` can't wipe the set).
        let empty_dir = base.join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        run_plugin_in(&desc_dir, Some(install(&empty_dir)))
            .await
            .unwrap();
        assert_eq!(
            names(&desc_dir),
            vec!["alpha", "delta", "gamma"],
            "an empty scan prunes nothing"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// `flux plugin uninstall <name>` rejects a path-traversal name (non-zero) and deletes nothing
    /// outside the plugins dir (D-35). A name like `../../config` would otherwise `remove_file` a
    /// path outside `dir`.
    #[tokio::test]
    async fn plugin_uninstall_rejects_traversal_names() {
        let dir =
            std::env::temp_dir().join(format!("flux-uninstall-traversal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A sentinel file *outside* `dir`, reachable via `..`. An unsanitized `uninstall` would
        // delete `<dir>/../flux-uninstall-traversal-sentinel.toml` — the traversal name below
        // MUST point exactly at this sentinel (one `..`), or a regression would `remove_file` a
        // non-existent path, return "no such plugin", and both assertions would pass vacuously.
        let outside = dir
            .parent()
            .unwrap()
            .join("flux-uninstall-traversal-sentinel.toml");
        std::fs::write(&outside, b"keep me").unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "../flux-uninstall-traversal-sentinel".into(),
                purge: false,
            }),
        )
        .await;
        assert!(
            err.is_err(),
            "uninstall of a traversal name is a clean error, not a destructive delete"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "keep me",
            "the traversal name did not delete a file outside the plugins dir"
        );

        // An absolute name is also rejected.
        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "/etc/passwd".into(),
                purge: false,
            }),
        )
        .await;
        assert!(
            err.is_err(),
            "uninstall of an absolute name is a clean error"
        );

        std::fs::remove_file(&outside).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-48 acceptance: `status` re-hashes the binary against the descriptor's recorded sha256 —
    /// drift shows in the verification column (and the doomed liveness probe is skipped);
    /// a matching hash reports `Verified`; a hashless dev descriptor stays `UnverifiedLocal`.
    #[tokio::test]
    async fn status_reports_hash_drift() {
        let dir = std::env::temp_dir().join(format!("flux-status-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("flux-plugin-alpha");
        std::fs::write(&bin, b"alpha-bytes").unwrap();
        let good = flux_plugin::pack::sha256_hex(b"alpha-bytes");
        flux_plugin::add_descriptor(
            &dir,
            "alpha",
            &flux_plugin::PluginDescriptor {
                program: bin.to_string_lossy().into_owned(),
                sha256: Some(good.clone()),
                version: Some("0.9.0".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // Untampered: verified. (The probe still runs and fails — a text file is no plugin —
        // but the verification column is independent of liveness.)
        let r = plugin_status_one(&dir, "alpha").await.unwrap();
        assert_eq!(r.verification, flux_plugin::Verification::Verified);

        // Tamper the binary → drift, and the spawn probe is refused/skipped.
        std::fs::write(&bin, b"tampered-bytes").unwrap();
        let r = plugin_status_one(&dir, "alpha").await.unwrap();
        match &r.verification {
            flux_plugin::Verification::HashDrift { expected, actual } => {
                assert_eq!(expected, &good);
                assert_eq!(actual, &flux_plugin::pack::sha256_hex(b"tampered-bytes"));
            }
            other => panic!("expected drift, got {other:?}"),
        }
        assert!(
            matches!(&r.liveness, Liveness::Unloadable(msg) if msg.contains("hash drift")),
            "drift refuses the probe: {:?}",
            r.liveness
        );

        // Hashless dev descriptor: unverified (local), exactly as before D-48.
        flux_plugin::add_descriptor(
            &dir,
            "dev",
            &flux_plugin::PluginDescriptor {
                program: bin.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        let r = plugin_status_one(&dir, "dev").await.unwrap();
        assert_eq!(r.verification, flux_plugin::Verification::UnverifiedLocal);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-48 acceptance: `uninstall --purge` also removes the plugin's versioned-store directory;
    /// without `--purge` the store is left in place (unchanged pre-D-48 behavior).
    #[tokio::test]
    async fn uninstall_purge_removes_versioned_store() {
        let dir = std::env::temp_dir().join(format!("flux-uninst-purge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let seed = |name: &str| {
            let store = dir.join("bin").join(name).join("0.9.0");
            std::fs::create_dir_all(&store).unwrap();
            std::fs::write(store.join(format!("flux-plugin-{name}")), b"bytes").unwrap();
            flux_plugin::add_descriptor(
                &dir,
                name,
                &flux_plugin::PluginDescriptor {
                    program: "/bin/true".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        };

        // Without --purge: descriptor gone, store kept (unchanged behavior).
        seed("keep");
        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "keep".into(),
                purge: false,
            }),
        )
        .await
        .unwrap();
        assert!(flux_plugin::load_descriptor(&dir, "keep")
            .unwrap()
            .is_none());
        assert!(
            dir.join("bin").join("keep").exists(),
            "store kept without --purge"
        );

        // With --purge: descriptor AND the versioned store dir are gone.
        seed("gone");
        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "gone".into(),
                purge: true,
            }),
        )
        .await
        .unwrap();
        assert!(flux_plugin::load_descriptor(&dir, "gone")
            .unwrap()
            .is_none());
        assert!(
            !dir.join("bin").join("gone").exists(),
            "--purge removed the store"
        );

        // --purge on a name with no descriptor still cleans an orphaned store dir.
        let orphan = dir.join("bin").join("orphan").join("0.9.0");
        std::fs::create_dir_all(&orphan).unwrap();
        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "orphan".into(),
                purge: true,
            }),
        )
        .await
        .unwrap();
        assert!(
            !dir.join("bin").join("orphan").exists(),
            "orphaned store purged"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin status <name>` reports a registered-but-missing binary as `missing`, not a
    /// crash — and never spawns a process to find out (D-19).
    #[tokio::test]
    async fn plugin_status_reports_manifest_and_liveness() {
        let dir = std::env::temp_dir().join(format!("flux-status-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        flux_plugin::add_descriptor(
            &dir,
            "ghost",
            &flux_plugin::PluginDescriptor {
                program: "/nonexistent/binary".into(),
                args: vec![],
                pinned: None,
                ..Default::default()
            },
        )
        .unwrap();

        let r = plugin_status_one(&dir, "ghost").await.unwrap();
        assert_eq!(
            r.liveness,
            Liveness::Missing,
            "a missing binary is `missing`, not a crash"
        );
        assert!(
            r.manifest.is_none(),
            "no manifest is loaded for a missing binary"
        );

        // A name that is not registered at all is a clean error (the caller surfaces it).
        let err = plugin_status_one(&dir, "nope").await;
        assert!(err.is_err(), "an unknown name is a clean error");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Bare `flux plugin install` (no names, no `--all`, no `--dir`) is a clean error naming both
    /// modes — the pre-D-47 implicit default (`plugins/target/release`) no longer applies (clean
    /// cutover, no guessing).
    #[tokio::test]
    async fn plugin_install_bare_errors_naming_both_modes() {
        let dir = std::env::temp_dir().join(format!("flux-install-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec![],
                all: false,
                dir: None,
                git: None,
                tag: None,
                rev: None,
                branch: None,
                bin: None,
                force: false,
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--all"), "{msg}");
        assert!(msg.contains("--dir"), "{msg}");
        assert!(
            msg.contains("--git"),
            "the error now names the third source: {msg}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--dir` (local scan) and explicit names/`--all` (remote install) are exclusive modes.
    #[tokio::test]
    async fn plugin_install_dir_rejects_combination_with_names_or_all() {
        let dir = std::env::temp_dir().join(format!("flux-install-combo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec!["gitlab".into()],
                all: false,
                dir: Some("plugins/target/release".into()),
                git: None,
                tag: None,
                rev: None,
                branch: None,
                bin: None,
                force: false,
            }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--dir"), "{err}");

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec![],
                all: true,
                dir: Some("plugins/target/release".into()),
                git: None,
                tag: None,
                rev: None,
                branch: None,
                bin: None,
                force: false,
            }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--dir"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin install --dir <path>` (the pre-D-47 local scan) registers a hashless descriptor
    /// — `ls`/`status` label it `unverified (local)`, never `verified`.
    #[tokio::test]
    async fn plugin_install_dir_scan_registers_unverified_local_descriptor() {
        let dir = std::env::temp_dir().join(format!("flux-install-dirscan-{}", std::process::id()));
        let bin_dir = dir.join("bin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("flux-plugin-gitlab"), b"x").unwrap();

        run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec![],
                all: false,
                dir: Some(bin_dir.to_string_lossy().into_owned()),
                git: None,
                tag: None,
                rev: None,
                branch: None,
                bin: None,
                force: false,
            }),
        )
        .await
        .unwrap();

        let desc = flux_plugin::load_descriptor(&dir, "gitlab")
            .unwrap()
            .unwrap();
        assert!(
            desc.version.is_none(),
            "a local-scan descriptor carries no version"
        );
        assert!(
            desc.sha256.is_none(),
            "a local-scan descriptor carries no sha256"
        );
        assert_eq!(
            flux_plugin::verify_descriptor(&desc),
            flux_plugin::Verification::UnverifiedLocal
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-48 superseded D-47's descriptor-field-only `verified` label: a hash-carrying descriptor
    /// is now **re-hashed** — one whose binary cannot be read is drift (never a silent
    /// `verified`), and a hashless (local/dev) one stays `unverified (local)`.
    #[tokio::test]
    async fn plugin_status_rehashes_hash_carrying_descriptors() {
        let dir = std::env::temp_dir().join(format!("flux-status-verified-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        flux_plugin::add_descriptor(
            &dir,
            "remote-plugin",
            &flux_plugin::PluginDescriptor {
                program: "/nonexistent/remote-plugin".into(),
                version: Some("0.9.0".into()),
                sha256: Some("deadbeef".into()),
                source: Some("plugins-v0.9.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
        flux_plugin::add_descriptor(
            &dir,
            "local-plugin",
            &flux_plugin::PluginDescriptor {
                program: "/nonexistent/local-plugin".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let remote = plugin_status_one(&dir, "remote-plugin").await.unwrap();
        assert!(
            matches!(
                &remote.verification,
                flux_plugin::Verification::HashDrift { expected, .. } if expected == "deadbeef"
            ),
            "a recorded hash over an unreadable binary is drift, not verified: {:?}",
            remote.verification
        );
        assert_eq!(remote.version.as_deref(), Some("0.9.0"));

        let local = plugin_status_one(&dir, "local-plugin").await.unwrap();
        assert_eq!(
            local.verification,
            flux_plugin::Verification::UnverifiedLocal,
            "a hashless descriptor is unverified (local)"
        );
        assert!(local.version.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plugin_call_resolves_short_op_to_manifest_qualified_name() {
        let manifest = flux_plugin::PluginManifest {
            name: "grafana".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "grafana.search".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            resolve_plugin_operation_name("grafana", "search", &manifest).unwrap(),
            "grafana.search"
        );
    }

    #[test]
    fn plugin_call_preserves_explicit_fully_qualified_op() {
        let manifest = flux_plugin::PluginManifest {
            name: "grafana".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "grafana.search".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            resolve_plugin_operation_name("grafana", "grafana.search", &manifest).unwrap(),
            "grafana.search"
        );
    }

    #[test]
    fn plugin_call_echo_masks_declared_secret_fields() {
        // GL-031: a `flux plugin call` echo (dry-run input preview OR live result) must mask the
        // op's declared secret-like fields so a CI-variable write's `value` never hits scrollback.
        let manifest = flux_plugin::PluginManifest {
            name: "gitlab".into(),
            operations: vec![
                flux_plugin::OperationSpec {
                    name: "gitlab.ci.variable.create".into(),
                    redact_fields: vec!["value".into()],
                    ..Default::default()
                },
                flux_plugin::OperationSpec {
                    name: "gitlab.project.show".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // The secret-declaring op masks `value` but leaves `key` intact.
        let mut echoed = json!({ "project": "grp/app", "key": "TOKEN", "value": "s3cr3t" });
        redact_plugin_echo(&mut echoed, &manifest, "gitlab.ci.variable.create");
        assert_eq!(echoed["key"], "TOKEN");
        assert_eq!(echoed["value"], flux_plugin::REDACTED_MARKER);
        assert!(
            !echoed.to_string().contains("s3cr3t"),
            "secret leaked: {echoed}"
        );

        // An op that declares no secret fields echoes verbatim.
        let mut plain = json!({ "project": "grp/app", "value": "not-secret" });
        redact_plugin_echo(&mut plain, &manifest, "gitlab.project.show");
        assert_eq!(plain["value"], "not-secret");
    }

    #[test]
    fn plugin_call_unknown_op_lists_available_ops() {
        let manifest = flux_plugin::PluginManifest {
            name: "grafana".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "grafana.search".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = resolve_plugin_operation_name("grafana", "dashboards", &manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("tried `grafana.dashboards`"), "{err}");
        assert!(err.contains("grafana.search"), "{err}");
    }

    #[test]
    fn ungrouped_plugin_ops_get_an_implicit_turn_intent_group() {
        let manifest = flux_plugin::PluginManifest {
            name: "slack".into(),
            groups: vec![flux_evidence::ToolGroup {
                name: "slack.health".into(),
                tools: vec!["slack.test".into()],
                surface_when: Vec::new(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let specs = vec![
            flux_spec::ToolSpec::read_only("slack.message.send", "send", json!({})),
            flux_spec::ToolSpec::read_only("slack.test", "test", json!({})),
        ];

        let group = implicit_plugin_group(&manifest, &specs).expect("one ungrouped operation");
        assert_eq!(group.name, "plugin.slack");
        assert_eq!(group.tools, vec!["slack.message.send"]);
        assert_eq!(group.surface_when.len(), 1);
        assert_eq!(group.surface_when[0].kind, flux_evidence::KIND_TURN_INTENT);
        assert_eq!(group.surface_when[0].signal.as_deref(), Some("slack"));
    }

    // ─── Track A1: `flux plugin call/run --arg` schema-coerced input building ──────────

    /// A representative schemars-derived op schema (a string field, a required integer, a
    /// nullable boolean, an enum, a string-array, and an unknown/extra field path).
    fn sample_op_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"},
                "flag": {"type": ["boolean", "null"]},
                "mode": {"type": "string", "enum": ["a", "b"]},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["count"]
        })
    }

    #[test]
    fn build_invoke_input_coerces_arg_types() {
        let schema = sample_op_schema();
        let args = vec![
            "count=42".to_string(),
            "flag=true".to_string(),
            "mode=b".to_string(),
            "tags=foo,bar,baz".to_string(),
        ];
        let (input, problems) = build_invoke_input(&schema, None, &args, true);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(input["count"], 42);
        assert_eq!(input["flag"], true);
        assert_eq!(input["mode"], "b");
        assert_eq!(input["tags"], serde_json::json!(["foo", "bar", "baz"]));
    }

    #[test]
    fn build_invoke_input_reports_type_and_enum_and_required_problems() {
        let schema = sample_op_schema();
        let args = vec![
            "count=notanint".to_string(),
            "mode=zzz".to_string(),
            "unknownfield=x".to_string(),
        ];
        let (input, problems) = build_invoke_input(&schema, None, &args, true);
        // Required `count` is present (as a string fallback), so only the coercion/enum/unknown
        // problems fire — not the missing-required one.
        assert_eq!(problems.len(), 3, "problems: {problems:?}");
        assert!(problems
            .iter()
            .any(|p| p.contains("`count`") && p.contains("integer")));
        assert!(problems
            .iter()
            .any(|p| p.contains("`mode`") && p.contains("not one of")));
        assert!(problems
            .iter()
            .any(|p| p.contains("`unknownfield`") && p.contains("not a declared field")));
        // The count fallback is inserted as a string so the call can still proceed under --no-validate.
        assert_eq!(input["count"], "notanint");
    }

    #[test]
    fn build_invoke_input_flags_missing_required() {
        let schema = sample_op_schema();
        let (input, problems) = build_invoke_input(&schema, None, &[], true);
        assert_eq!(input, serde_json::json!({}));
        assert!(problems
            .iter()
            .any(|p| p.contains("missing required field `count`")));
    }

    #[test]
    fn build_invoke_input_merges_args_over_json_base() {
        let schema = sample_op_schema();
        let base = serde_json::json!({"count": 1, "name": "base"});
        let args = vec!["count=99".to_string(), "flag=false".to_string()];
        let (input, problems) = build_invoke_input(&schema, Some(base), &args, true);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(input["count"], 99); // arg overrides base
        assert_eq!(input["name"], "base"); // base preserved
        assert_eq!(input["flag"], false);
    }

    #[test]
    fn build_invoke_input_no_validate_passes_strings_through() {
        let schema = sample_op_schema();
        let args = vec!["count=notanint".to_string(), "unknownfield=x".to_string()];
        let (input, problems) = build_invoke_input(&schema, None, &args, false);
        assert!(
            problems.is_empty(),
            "--no-validate should produce no problems: {problems:?}"
        );
        assert_eq!(input["count"], "notanint");
        assert_eq!(input["unknownfield"], "x");
    }

    #[test]
    fn build_invoke_input_parses_json_array_literal() {
        let schema = sample_op_schema();
        let args = vec!["count=1".to_string(), "tags=[\"x\",\"y\"]".to_string()];
        let (input, problems) = build_invoke_input(&schema, None, &args, true);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(input["tags"], serde_json::json!(["x", "y"]));
    }

    #[test]
    fn coerce_arg_value_handles_nullable_and_refs() {
        // schemars nullable form: type: ["string","null"].
        let nullable = serde_json::json!({"type": ["string", "null"]});
        assert_eq!(
            coerce_arg_value(&nullable, &serde_json::json!({}), "hi").unwrap(),
            "hi"
        );
        // enum via anyOf → $ref → definitions (schemars Option<Enum> shape).
        let schema = serde_json::json!({
            "definitions": { "Mode": {"type": "string", "enum": ["on", "off"]} },
            "anyOf": [{"$ref": "#/definitions/Mode"}, {"type": "null"}]
        });
        let defs = schema["definitions"].clone();
        assert_eq!(coerce_arg_value(&schema, &defs, "on").unwrap(), "on");
        let err = coerce_arg_value(&schema, &defs, "nope").unwrap_err();
        assert!(err.to_string().contains("not one of"));
    }

    #[test]
    fn generated_skill_install_writes_skill_dir_and_references() {
        let root = std::env::temp_dir().join(format!("flux-skill-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let skill = super::skill_cmd::RenderedSkill {
            name: "flux-test".into(),
            skill_md: "---\nname: flux-test\ndescription: test\n---\nbody\n".into(),
            references: vec![("ops".into(), "# Ops\n".into())],
        };

        let dir = write_generated_skill(&root, &skill).unwrap();
        assert_eq!(dir, root.join("flux-test"));
        assert!(dir.join("SKILL.md").is_file());
        assert!(dir.join("references").join("ops.md").is_file());

        std::fs::remove_dir_all(&root).ok();
    }

    /// The turn-end token annotation reports all four figures the user asked for: context-window
    /// occupancy (fresh input + both cache tiers), generated output, the cached tokens, and the
    /// hit-rate (cached ÷ context). It is empty when nothing was billed (offline `-m mock`).
    #[test]
    fn usage_annotation_shows_context_output_and_cache_hit_rate() {
        use flux_core::Usage;
        // 1000 fresh + 9000 cache-read = 10k context; 9000/10000 = 90% hit.
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_input_tokens: 9_000,
            cache_creation_input_tokens: 0,
            reasoning_tokens: 0,
            ..Default::default()
        };
        let s = usage_annotation(&u);
        assert_eq!(s, " · ctx 10.0k · out 500 · cache 90% ↺9.0k ✎0");

        // No cache → no cache segment, but context + output still show.
        let u = Usage {
            input_tokens: 320,
            output_tokens: 80,
            ..Default::default()
        };
        assert_eq!(usage_annotation(&u), " · ctx 320 · out 80");

        // Nothing billed → empty (so `-m mock` turns render a clean rule).
        assert_eq!(usage_annotation(&Usage::default()), "");
    }

    /// C-06 cache-aware surfacing: `usage_annotation` must show cache-WRITE tokens and reasoning
    /// tokens too — before C-06 only cache-READ appeared, silently dropping the other tiers a
    /// caching-heavy or reasoning-heavy turn actually spent. Combined with `cost_annotation` (the
    /// dollar-cost suffix `CliSink::cost_inline` appends alongside this), the turn-end rule shows
    /// every tier + cost — the story's named failing-first test.
    #[test]
    fn usage_annotation_includes_cache_and_cost() {
        use flux_core::{Money, Usage};

        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 2_000,
            cache_read_input_tokens: 9_000,
            reasoning_tokens: 300,
            ..Default::default()
        };
        let s = usage_annotation(&u);
        assert!(s.contains("↺9.0k"), "cache-read still shown: {s}");
        assert!(
            s.contains("✎2.0k"),
            "cache-WRITE tokens must be surfaced too (previously dropped entirely): {s}"
        );
        assert!(
            s.contains("reasoning 300"),
            "reasoning tokens must be surfaced: {s}"
        );

        // Zero cache-write / zero reasoning ⇒ neither segment appears (no clutter on an ordinary
        // metered turn that never wrote to cache or reasoned).
        let plain = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_input_tokens: 9_000,
            ..Default::default()
        };
        let s2 = usage_annotation(&plain);
        assert!(
            s2.contains("✎0"),
            "the write tier renders as zero, not absent: {s2}"
        );
        assert!(!s2.contains("reasoning"));

        // The dollar-cost suffix (rendered alongside, via `cost_annotation`) completes the picture:
        // the turn-end rule shows tokens (this function) AND cost (this one) together.
        let cost = cost_annotation(&Money {
            usd: 0.0456,
            subscription: false,
            source: flux_core::CostSource::Estimated,
        });
        assert_eq!(format!("{s}{cost}"), format!("{s} · $0.0456"));
    }

    /// C-139: the rendered hit rate must be the TURN's, folded per model call — not the last
    /// round's, which is what `Usage::accumulate` leaves in the turn snapshot and therefore the
    /// worst ratio of the turn.
    #[test]
    fn usage_annotation_hit_rate_is_the_turn_not_the_last_round() {
        let call = |read: u64, fresh: u64| flux_core::Usage {
            input_tokens: fresh,
            output_tokens: 10,
            cache_read_input_tokens: read,
            ..Default::default()
        };
        let calls = [
            call(90_000, 10_000),
            call(60_000, 40_000),
            call(20_000, 80_000),
        ];

        let mut turn = flux_core::Usage::default();
        let mut cache = flux_core::CacheEfficiency::default();
        for c in &calls {
            turn.accumulate(c);
            cache.add(c);
        }

        // The old shape: hit rate read straight off the turn snapshot ⇒ round three's 20%.
        let last_round = crate::rendering::usage_annotation(&turn);
        assert!(last_round.contains("cache 20%"), "{last_round}");

        // The fixed shape: 170k of 300k prompt tokens ⇒ 57%.
        let turn_level = crate::rendering::usage_annotation_with_cache(&turn, &cache);
        assert!(turn_level.contains("cache 57%"), "{turn_level}");
        assert!(turn_level.contains("↺170.0k"), "{turn_level}");
        // `ctx` keeps its occupancy meaning — the last round's prompt size, not the sum.
        assert!(turn_level.contains("ctx 100.0k"), "{turn_level}");
    }

    /// A surface that emits no `model.call` observations (the flow path's `ai_segment`) leaves the
    /// per-call fold empty. Rendering it anyway drops the cache segment entirely — worse than the
    /// last-round approximation it replaced — so an empty fold must fall back to the turn snapshot.
    #[test]
    fn an_empty_per_call_fold_falls_back_to_the_turn_snapshot() {
        let turn = flux_core::Usage {
            input_tokens: 10_000,
            output_tokens: 100,
            cache_read_input_tokens: 90_000,
            ..Default::default()
        };
        let empty = flux_core::CacheEfficiency::default();
        assert!(empty.is_empty());
        // Rendered against the empty fold, the cache tiers vanish…
        let dropped = crate::rendering::usage_annotation_with_cache(&turn, &empty);
        assert!(!dropped.contains("cache"), "{dropped}");
        // …so the fallback (what `turn_end` now selects) must still surface them.
        let fallback = crate::rendering::usage_annotation(&turn);
        assert!(fallback.contains("cache 90%"), "{fallback}");
        assert!(fallback.contains("↺90.0k"), "{fallback}");
    }

    /// `cost_annotation` formats metered spend as `$X`, subscription spend (claude/codex) as the
    /// *equivalent metered cost* `~$X (sub)` (it bills against a flat sub, not the API), and a
    /// zero-cost turn as empty (C-05).
    #[test]
    fn cost_annotation_labels_metered_vs_subscription() {
        use flux_core::Money;
        // Metered spend → raw dollar amount.
        let metered = cost_annotation(&Money {
            usd: 0.0023,
            subscription: false,
            source: flux_core::CostSource::Estimated,
        });
        assert_eq!(metered, " · $0.0023");
        // Subscription spend → equivalent metered cost, tagged `(sub)`.
        let sub = cost_annotation(&Money {
            usd: 0.0023,
            subscription: true,
            source: flux_core::CostSource::Estimated,
        });
        assert_eq!(sub, " · ~$0.0023 (sub)");
        // A zero-cost turn (e.g. fully cached, or no usage) → empty, so the rule stays clean.
        assert_eq!(
            cost_annotation(&Money {
                usd: 0.0,
                subscription: false,
                source: flux_core::CostSource::Estimated,
            }),
            ""
        );
        assert_eq!(
            cost_annotation(&Money {
                usd: 0.0,
                subscription: true,
                source: flux_core::CostSource::Estimated,
            }),
            ""
        );
    }

    /// `flux usage` reports per-model tokens + cost for the current (latest) session AND an
    /// all-sessions total — the story's named failing-first test. Two sessions on different models,
    /// each with a `CallUsage`-carrying turn: the latest session's report must show ONLY its own
    /// model, while the all-sessions total rolls up both.
    #[test]
    fn flux_usage_reports_per_model_cost() {
        use flux_core::Usage;

        let store = EventStore::in_memory().unwrap();

        let older = store.create_session("claude-opus-4-8").unwrap();
        let t1 = store
            .begin_turn(&older, "first", "claude-opus-4-8")
            .unwrap();
        store
            .record_call_usage(
                &older,
                t1,
                "claude-opus-4-8",
                Usage {
                    input_tokens: 1_000_000,
                    output_tokens: 1_000_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&older, t1, "accepted", 1, "done", None)
            .unwrap();

        let latest = store.create_session("claude-sonnet-4-6").unwrap();
        let t2 = store
            .begin_turn(&latest, "second", "claude-sonnet-4-6")
            .unwrap();
        store
            .record_call_usage(
                &latest,
                t2,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 500_000,
                    output_tokens: 50_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&latest, t2, "accepted", 1, "done", None)
            .unwrap();

        assert_eq!(
            store.latest_session().unwrap().as_deref(),
            Some(latest.as_str()),
            "the second session is the most recently active"
        );

        let pricing = flux_core::PricingTable::builtin();
        // Doesn't panic and (indirectly, via the projection it wraps) reports the right rows —
        // asserted precisely below through the projection it's built on, since `run_usage_with`
        // itself only prints.
        run_usage_with(&store, &pricing).unwrap();

        // The precise per-model figures `run_usage_with` prints, checked directly:
        let latest_rows = store.cost_summary(&latest, &pricing).unwrap();
        assert_eq!(
            latest_rows.len(),
            1,
            "the latest session shows only its own model"
        );
        assert_eq!(latest_rows[0].model, "claude-sonnet-4-6");
        assert_eq!(latest_rows[0].usage.input_tokens, 500_000);

        let all_rows = store.cost_summary_all(&pricing).unwrap();
        assert_eq!(
            all_rows.len(),
            2,
            "the all-sessions total rolls up both models"
        );
        let opus = all_rows
            .iter()
            .find(|r| r.model == "claude-opus-4-8")
            .unwrap();
        assert_eq!(opus.usage.input_tokens, 1_000_000);
        assert!(opus.cost.unwrap().usd > 0.0);
    }

    /// A `CliSink` with an attached model spec + pricing table prices a turn's usage through the
    /// cost model end-to-end (the wiring that makes C-05's `cost()` live, not dead code). The codex
    /// path resolves on `gpt-5.5` and is labelled subscription spend (C-03 model resolution + C-05).
    #[test]
    fn sink_prices_a_codex_turn_as_subscription() {
        use flux_core::Usage;
        let sink = super::CliSink::new(0).with_cost(
            "codex/gpt-5.5".to_string(),
            flux_core::PricingTable::builtin(),
        );
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        };
        let inline = sink.cost_inline(Some(&u));
        assert!(
            inline.contains("(sub)"),
            "codex spend is subscription-labelled, got: {inline}"
        );
        assert!(
            inline.contains('$'),
            "a non-zero turn shows a dollar cost, got: {inline}"
        );
        // A metered spec on the same usage is not tagged `(sub)`.
        let metered = super::CliSink::new(0)
            .with_cost(
                "anthropic/claude-sonnet-4-6".to_string(),
                flux_core::PricingTable::builtin(),
            )
            .cost_inline(Some(&u));
        assert!(
            !metered.contains("(sub)"),
            "anthropic is metered, got: {metered}"
        );
        // No spec attached → no cost suffix (sub-paths that don't show cost).
        assert_eq!(super::CliSink::new(0).cost_inline(Some(&u)), "");
    }

    /// C-30: an attached METERED CLOUD model missing from the pricing table renders the visible
    /// ` · $? (unpriced)` marker — never silent nothing (silence hid real spend); local
    /// (`ollama*`) and unknown/mock specs stay silent so hermetic e2e output is byte-identical.
    #[test]
    fn unpriced_model_renders_visible_marker() {
        use flux_core::Usage;
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        };
        let table = flux_core::PricingTable::builtin();
        // A cloud provider with a model the table doesn't know → marker.
        let unpriced = super::CliSink::new(0)
            .with_cost("openrouter/acme/not-in-table".into(), table.clone())
            .cost_inline(Some(&u));
        assert_eq!(unpriced, " · $? (unpriced)", "got: {unpriced:?}");
        // Local ollama and unknown/mock specs: silent, as before.
        for quiet in ["ollama/llama3", "mock", "some-ad-hoc-model"] {
            let s = super::CliSink::new(0)
                .with_cost(quiet.into(), table.clone())
                .cost_inline(Some(&u));
            assert_eq!(s, "", "`{quiet}` must stay silent, got: {s:?}");
        }
        // No usage → silent regardless.
        let none = super::CliSink::new(0)
            .with_cost("openrouter/acme/not-in-table".into(), table)
            .cost_inline(None);
        assert_eq!(none, "");
    }

    /// C-34: a call that reported its own cost (OpenRouter, both wires) prices even though the
    /// static builtin table has no row for it — the `$? (unpriced)` marker (and its once-per-run
    /// note) must NOT fire; `cost_suffix` takes the `Some(money) => cost_annotation` branch, never
    /// reaching `unpriced_marker_applies`/`note_unpriced_once` at all.
    #[test]
    fn cost_suffix_prefers_reported_cost_over_unpriced_marker() {
        use flux_core::Usage;
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            reported_cost_usd: Some(0.0023),
            ..Default::default()
        };
        let table = flux_core::PricingTable::builtin();
        let inline = super::CliSink::new(0)
            .with_cost("openrouter/deepseek/deepseek-v4-flash:nitro".into(), table)
            .cost_inline(Some(&u));
        assert!(
            !inline.contains("$?"),
            "reported cost must beat the unpriced marker, got: {inline:?}"
        );
        assert_eq!(inline, " · $0.0023", "the real reported figure, not $?");
    }

    /// C-30: the REPL's per-turn sink derives its spec from the LIVE engine — the same
    /// `canonical_model_spec` derivation loop_host stamps usage with — so a `/model` switch
    /// changes what the next sink prices, and an openrouter passthrough keeps its serving
    /// provider (metered), while a claude switch turns the suffix subscription-shaped.
    #[tokio::test]
    async fn repl_sink_cost_derives_from_the_live_engine_spec() {
        use flux_core::Usage;
        let u = Usage {
            input_tokens: 100_000,
            output_tokens: 5_000,
            ..Default::default()
        };
        let table = flux_core::PricingTable::builtin();
        // The derivation the TurnCost factory applies (provider name + live model string):
        let spec = flux_core::canonical_model_spec(
            Some("openrouter-anthropic"),
            "anthropic/claude-sonnet-4.6",
        );
        assert_eq!(spec, "openrouter-anthropic/anthropic/claude-sonnet-4.6");
        let inline = super::CliSink::new(0)
            .with_cost(spec, table.clone())
            .cost_inline(Some(&u));
        assert!(
            inline.contains('$') && !inline.contains("(sub)") && !inline.contains("$?"),
            "openrouter passthrough is metered and priced, got: {inline}"
        );
        // Simulated /model switch to a subscription provider: the NEXT sink derives the new spec.
        let spec = flux_core::canonical_model_spec(Some("claude"), "claude-opus-4-8");
        let inline = super::CliSink::new(0)
            .with_cost(spec, table)
            .cost_inline(Some(&u));
        assert!(
            inline.contains("(sub)"),
            "a switched-to claude model is subscription-labelled, got: {inline}"
        );
    }

    /// A-15 named acceptance (`phase_observations_emitted_per_pass`'s surface half): each
    /// `loop.phase` observation updates the phase-labeled spinner. Historical phase names remain
    /// supported, and a phase-less turn uses a neutral fallback.
    #[test]
    fn loop_phase_observations_drive_the_phase_labeled_spinner() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "working…",
            "no loop.phase observed yet -> neutral fallback"
        );

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "orient" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "orienting…"
        );
        assert!(!sink.gather_mode);

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "gather" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "gathering…"
        );
        assert!(sink.gather_mode, "a gather-phase round renders compact");

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "intent" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "routing intent…"
        );

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "explore" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "exploring…"
        );

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "execute" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "planning…",
            "the execute phase's first round this turn is a plain plan, not a revision"
        );
        assert!(!sink.gather_mode, "execute is never a gather round");

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "execute" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "revising…",
            "a second execute-phase round this turn means the prior one didn't finish"
        );
    }

    /// C-91: while a prompt holds the gate the spinner ticker gets no paint permit — the prompt
    /// owns the stderr line; painting resumes once the guard drops.
    #[test]
    fn prompt_gate_blocks_painting_while_held() {
        let gate = super::PromptGate::new();
        assert!(gate.begin_paint().is_some(), "free gate paints");
        let guard = gate.acquire();
        assert!(gate.begin_paint().is_none(), "held gate must not paint");
        drop(guard);
        assert!(gate.begin_paint().is_some(), "released gate paints again");
    }

    /// C-91: `stop_spinner`'s line clear is suppressed while a prompt holds the gate — a
    /// `planning(false)` drained during the approval wait must not wipe the prompt line.
    #[test]
    fn prompt_gate_suppresses_clear_only_while_held() {
        let gate = super::PromptGate::new();
        gate.painter_started();
        let guard = gate.acquire();
        assert!(
            !gate.painter_stopped(),
            "clear suppressed while the prompt owns the line"
        );
        drop(guard);
        gate.painter_started();
        assert!(
            gate.painter_stopped(),
            "no holder -> caller clears normally"
        );
    }

    /// C-91: the whole-plan prompt carries the batch content — the plain CLI renders no plan tree
    /// before the confirm, so this prompt is the only place the user sees what they approve.
    #[test]
    fn plan_prompt_lists_ops_subjects_and_answer_line() {
        use flux_policy::ResourceRef;
        use flux_runtime::AuthorityRequirement;

        let plan = flux_runtime::PlanApprovalRequest {
            summary: "medium".to_string(),
            ops: vec!["write".to_string(), "bash".to_string()],
            destructive: false,
            mutating: true,
            intents: flux_spec::IntentSet {
                intents: vec![flux_spec::Intent {
                    behavior: flux_spec::IntentBehavior::CommandExecution,
                    target: flux_spec::IntentTarget::Process {
                        command: "cargo test".to_string(),
                    },
                    role: flux_spec::IntentRole::ProcessCommand,
                    certainty: flux_spec::IntentCertainty::Certain,
                }],
            },
            requirements: vec![
                AuthorityRequirement::new(
                    "workspace.write",
                    ResourceRef::path("/home/user/notes/flux-mock.txt"),
                ),
                // Operation requirements only repeat the ops line — skipped.
                AuthorityRequirement::new(
                    "op.invoke",
                    ResourceRef::named(flux_policy::ResourceKind::Operation, "write"),
                ),
            ],
        };

        let prompt = super::plan_prompt(&plan);
        assert!(
            prompt.contains("run this plan? (2 op(s) · medium)"),
            "{prompt}"
        );
        assert!(prompt.contains("ops: write, bash"), "{prompt}");
        assert!(
            prompt.contains("workspace.write → notes/flux-mock.txt"),
            "paths are trimmed to the last two components: {prompt}"
        );
        assert!(prompt.contains("process.exec → $ cargo test"), "{prompt}");
        assert!(
            !prompt.contains("op.invoke"),
            "operation requirements are skipped: {prompt}"
        );
        assert!(
            !prompt.contains("destructive"),
            "no destructive warning unless flagged: {prompt}"
        );
        assert!(prompt.ends_with("\n[y]es / [a]lways / [N]o: "), "{prompt}");

        let destructive = flux_runtime::PlanApprovalRequest {
            destructive: true,
            ..plan
        };
        assert!(super::plan_prompt(&destructive).contains("⚠ contains a destructive operation"),);
    }

    #[test]
    fn staged_intent_summary_is_concise_and_verbose_is_explicit() {
        let data = serde_json::json!({
            "intent": "  answer   the account and incident questions\nfrom evidence  ",
            "families": ["workspace.read"],
            "operations": ["glob", "read", "grep"]
        });
        assert_eq!(
            super::intent_lines(&data, false, 80),
            vec![
                "◆ intent: answer the account and incident questions from evidence",
                "  capabilities: workspace.read · 3 operations",
            ]
        );
        assert_eq!(
            super::intent_lines(&data, true, 80),
            vec![
                "◆ intent: answer the account and incident questions from evidence",
                "  capabilities: workspace.read · 3 operations",
                "  operations: glob, read, grep",
            ]
        );

        let none = serde_json::json!({
            "intent": "chat",
            "families": [],
            "operations": []
        });
        assert_eq!(
            super::intent_lines(&none, false, 80)[1],
            "  capabilities: none · 0 operations"
        );
    }

    #[test]
    fn first_planning_consultation_starts_cli_turn_timing_without_reset() {
        let mut sink = super::CliSink::new(0);
        assert!(sink.turn_start.is_none());
        sink.planning(true);
        let started = sink.turn_start.expect("planning starts the turn clock");
        sink.planning(false);
        sink.planning(true);
        assert_eq!(sink.turn_start, Some(started));
        sink.planning(false);
    }

    /// A-15: a `flow.brief` observation marks gather mode (a brief only ever accompanies a
    /// `gather: true` plan, per `compile.rs`'s `parse_brief` call site) even when it arrives right
    /// after `orient` — the only phase where a gather round is otherwise indistinguishable from a
    /// full plan emitted directly. `brief_lines` renders the grounding artifact immediately and
    /// compactly: `◆ goal: …` plus a dim needs list.
    #[test]
    fn flow_brief_observation_marks_gather_mode_and_formats_goal_and_needs() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "orient" }),
        ));
        assert!(!sink.gather_mode);

        sink.observation(&Observation::new(
            "flow.brief",
            Phase::Turn,
            serde_json::json!({ "goal": "find the bug", "needs": ["stack trace", "repro steps"] }),
        ));
        assert!(
            sink.gather_mode,
            "the brief that just landed accompanies orient's gather plan"
        );

        let lines = super::brief_lines(&serde_json::json!({
            "goal": "find the bug",
            "needs": ["stack trace", "repro steps"],
        }));
        assert_eq!(lines[0], "◆ goal: find the bug");
        assert_eq!(lines[1], "  needs: stack trace, repro steps");

        // No needs -> just the goal line (an empty needs list adds no clutter).
        let goal_only = super::brief_lines(&serde_json::json!({ "goal": "answer a question" }));
        assert_eq!(goal_only, vec!["◆ goal: answer a question".to_string()]);
    }

    /// A-15: a gather plan (small, read-only) renders as a compact one-liner — op names pulled off
    /// the plan's call nodes, joined `·`-separated after a `gathering` label — never the full tree
    /// + risk badge a full execution plan keeps (`render_plan`, unchanged by this story).
    #[test]
    fn gather_plan_renders_as_a_compact_one_liner_not_the_full_tree() {
        use flux_flow::ast::{DraftAst, Node};

        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "a".into(),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!({ "path": "Cargo.toml" }),
                        }],
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Call {
                    op: "grep".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!({ "pattern": "LoopHost" }),
                    }],
                },
            ],
            ..Default::default()
        };
        let data = serde_json::json!({
            "plan_ast": serde_json::to_value(&ast).unwrap(),
            "plan": "flow\n└─ ...",
            "risk": "low",
            "ops": 2,
        });
        let line = super::gather_compact_line(&data);
        assert!(
            line.starts_with("gathering · "),
            "compact one-liner, not a tree: {line}"
        );
        assert!(line.contains("read"), "op names: {line}");
        assert!(line.contains("Cargo.toml"), "and their args: {line}");
        assert!(line.contains("grep"), "every call node listed: {line}");
        assert!(
            !line.contains('\n'),
            "one line, not the multi-line tree render: {line}"
        );

        // An AST-less payload (defensive) falls back to a bare op count rather than panicking.
        let bare = super::gather_compact_line(&serde_json::json!({ "ops": 3 }));
        assert_eq!(bare, "gathering · 3 ops");
    }

    /// A-15: the `flow.plan` dispatch itself — `observation()` picks the compact render while
    /// `gather_mode` is set (entered via a `gather`-phase `loop.phase`) and the full tree once
    /// `execute` clears it back. This only smoke-tests that both paths run without panicking (the
    /// terminal painting itself goes straight to stderr, like every other `CliSink` render in this
    /// file); the render CONTENT is covered by `gather_compact_line` above and the pre-existing
    /// `flow.plan` full-tree behavior this story leaves untouched.
    #[test]
    fn flow_plan_dispatches_compact_or_full_by_gather_mode() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        let plan_data = serde_json::json!({
            "plan": "flow\n└─ $x = read(\"README.md\")   !read",
            "risk": "low",
            "ops": 1,
        });

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "gather" }),
        ));
        assert!(sink.gather_mode);
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            plan_data.clone(),
        ));

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "execute" }),
        ));
        assert!(!sink.gather_mode);
        sink.observation(&Observation::new("flow.plan", Phase::Turn, plan_data));
    }

    /// A-17 (closes the A-15 residual): `flow.plan`'s own `gather` field is honored directly, even
    /// when it DISAGREES with the surface's tracked `gather_mode` state — this is exactly the gap
    /// A-15 recorded (an orient-phase gather plan the state machine couldn't tell apart from orient
    /// emitting the full plan directly). The direct field must win.
    #[test]
    fn flow_plan_gather_field_is_honored_directly_even_when_state_inference_disagrees() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        // `orient` clears the surface's own `gather_mode` inference to false...
        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "orient" }),
        ));
        assert!(!sink.gather_mode);
        // ...but the plan itself says otherwise (`gather: true`) — the direct field must be
        // consulted at dispatch time, not the stale inferred state. Smoke-tests only that the
        // gather branch runs without panicking; content is covered by `gather_compact_line`.
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            serde_json::json!({ "plan": "flow\n└─ ...", "risk": "low", "ops": 1, "gather": true }),
        ));

        // A payload with NO `gather` field at all (a phase-less/stale caller) falls back to the
        // tracked state — backward compatible with the pre-A-17 wire shape.
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            serde_json::json!({ "plan": "flow\n└─ ...", "risk": "low", "ops": 1 }),
        ));
    }

    /// A-17: `halt_line` formats a `flow.halt` observation's `data` as the design's `✗ step N/M <op>
    /// failed — revising…` line, falling back to a plain "failed" when the op isn't derivable.
    #[test]
    fn flow_halt_observation_renders_the_step_and_op() {
        let with_op = super::halt_line(&serde_json::json!({ "step": 4, "of": 9, "op": "edit" }));
        assert_eq!(with_op, "✗ step 4/9 edit failed — revising…");

        let without_op = super::halt_line(&serde_json::json!({ "step": 2, "of": 2 }));
        assert_eq!(without_op, "✗ step 2/2 failed — revising…");
    }

    /// A-17: `render_halt`'s dispatch — smoke-tests that a `flow.halt` observation reaches the
    /// sink without panicking (the rendered CONTENT is covered by `halt_line` above).
    #[test]
    fn flow_halt_dispatches_to_render_halt() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "flow.halt",
            Phase::Turn,
            serde_json::json!({ "step": 1, "of": 2, "op": "boom", "kind": "runtime", "fatal": false }),
        ));
    }

    /// A-39 (`--trace-loop`/`FLUX_TRACE_LOOP`): `trace_node_line` formats every structural `loop.node`
    /// kind the interpreter can emit, table-driven like `halt_line`'s test above — including the
    /// defensive fallback for a `node` kind this formatter hasn't been taught yet.
    #[test]
    fn trace_node_line_formats_every_structural_kind() {
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (
                serde_json::json!({"node": "call", "op": "plan", "bind": "draft"}),
                "· plan → $draft",
            ),
            (serde_json::json!({"node": "call", "op": "grep"}), "· grep"),
            (
                serde_json::json!({"node": "when", "cond": "$draft", "branch": "then"}),
                "· when $draft → then",
            ),
            (
                serde_json::json!({"node": "when", "branch": "else"}),
                "· when → else",
            ),
            (
                serde_json::json!({"node": "unless", "cond": "$done", "entered": false}),
                "· unless $done → skip",
            ),
            (
                serde_json::json!({"node": "unless", "entered": true}),
                "· unless → enter",
            ),
            (
                serde_json::json!({
                    "node": "match",
                    "subject": "$kind",
                    "value": "\"chat\"",
                    "arm": "case \"chat\"",
                }),
                "· match $kind = \"chat\" → case \"chat\"",
            ),
            (
                serde_json::json!({"node": "match", "value": "1", "arm": "default"}),
                "· match 1 → default",
            ),
            (
                serde_json::json!({"node": "return", "value": "$answer"}),
                "· return $answer",
            ),
            (serde_json::json!({"node": "return"}), "· return"),
            (
                serde_json::json!({"node": "repeat", "until_hit": true, "rounds": 3, "max": 25}),
                "· until hit — exit after 3/25",
            ),
            (
                serde_json::json!({"node": "parallel.branch", "name": "left"}),
                "· parallel branch $left",
            ),
        ];
        for (data, expected) in cases {
            assert_eq!(super::trace_node_line(&data), expected, "data: {data}");
        }

        // An unrecognized `node` kind falls back to the raw JSON rather than panicking (defensive:
        // the interpreter's trace helper is meant to grow new emission sites over time).
        let unknown = serde_json::json!({"node": "each", "foo": "bar"});
        assert_eq!(super::trace_node_line(&unknown), format!("· {unknown}"));
    }

    /// A-39: `loop.round`/`loop.node` observations dispatch without panicking (the rendered CONTENT
    /// is covered by `trace_node_line` above).
    #[test]
    fn loop_round_and_node_dispatch_without_panicking() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "loop.round",
            Phase::Turn,
            serde_json::json!({ "round": 1, "max": 25 }),
        ));
        sink.observation(&Observation::new(
            "loop.node",
            Phase::Turn,
            serde_json::json!({ "node": "call", "op": "plan", "bind": "draft" }),
        ));
    }

    /// A-17: a resumed/halted plan's marker-prefixed text is colored per line (✓ green / ✗ red / ·
    /// dim) rather than left plain — the CLI/TUI residual this story closes (the `flow.plan`
    /// observation carries markers, but the surface used to always reconstruct an unmarked tree
    /// from `plan_ast` instead of rendering them).
    #[test]
    fn style_marked_plan_colors_each_line_by_its_status_marker() {
        // Color is off by default in tests (no tty) — style::* helpers no-op, so this proves the
        // per-line DISPATCH logic (which marker maps to which styler) without depending on a tty.
        let text = "✓ 0: $a = echo(\"first\")\n✗ 1: boom()\n· 2: $b = echo(\"fixed\")";
        let styled = super::style_marked_plan(text);
        // With color disabled the bytes are unchanged, but every line must still be present in
        // order (the function must not drop or reorder lines).
        for line in text.lines() {
            assert!(styled.contains(line), "{styled}");
        }
        assert_eq!(styled.lines().count(), 3);
    }

    /// A-17: `render_plan`'s dispatch — a `resumed: true` payload prefers the marked `plan` text
    /// (smoke-tested; content covered by `style_marked_plan`), a normal payload still prefers
    /// `plan_ast` (pre-existing behavior, unchanged).
    #[test]
    fn render_plan_prefers_marked_text_when_resumed() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            serde_json::json!({
                "plan": "✓ 0: $a = echo(\"first\")\n✗ 1: boom()",
                "plan_ast": {"body": [
                    {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"first"}]}},
                    {"kind":"call","op":"boom","args":[]}
                ]},
                "risk": "low",
                "ops": 2,
                "resumed": true,
            }),
        ));
    }

    /// clap validates the whole command tree (catches duplicate arg ids, the global-args + subcommand
    /// wiring, conflicts) at test time rather than only when `flux --help` is first run.
    #[test]
    fn cli_command_tree_is_valid() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }

    /// Every subcommand is registered so `flux --help` / `flux <cmd> --help` are complete.
    #[test]
    fn help_lists_every_subcommand() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        for want in [
            "run",
            "tui",
            "app",
            "eval",
            "flow",
            "review",
            "loop",
            "sessions",
            "auth",
            "plugin",
            "skill",
            "completion",
            "preset",
        ] {
            assert!(
                names.contains(&want),
                "missing subcommand `{want}` in {names:?}"
            );
        }
    }

    /// The top level is clean: its only declared flag is the global `--color`. No agent/turn flags or
    /// the promoted mode flags (`tui`/`plan`) leak onto it — they live on the subcommands now (`--serve`
    /// likewise lives on `app run`, never the top level). Inspecting the declared arguments (not the
    /// rendered text) avoids false hits on flag names that appear inside a subcommand's *description*.
    #[test]
    fn top_level_has_only_the_color_flag() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let longs: Vec<String> = cmd
            .get_arguments()
            .filter_map(|a| a.get_long().map(String::from))
            .collect();
        for leaked in [
            "max-tokens",
            "model",
            "yes",
            "serve",
            "tui",
            "plan",
            "continue",
            "verbose",
        ] {
            assert!(
                !longs.iter().any(|l| l == leaked),
                "top-level leaks --{leaked}: {longs:?}"
            );
        }
        assert!(
            longs.iter().any(|l| l == "color"),
            "top-level missing --color: {longs:?}"
        );
    }

    /// `flux skill` is the generated-skill surface: optional type plus install/global flags.
    #[test]
    fn skill_help_documents_types_and_install_flags() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let skill = cmd.find_subcommand("skill").expect("skill subcommand");
        let help = skill.clone().render_long_help().to_string();
        for want in ["--install", "--global", "cli", "lang", "plugin", "ops"] {
            assert!(help.contains(want), "`flux skill --help` missing {want:?}");
        }
    }

    /// `flux eval --help` carries its own typed flags + the adapter list (the original ask).
    #[test]
    fn eval_help_documents_its_flags() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let eval = cmd.find_subcommand("eval").expect("eval subcommand");
        let help = eval.clone().render_long_help().to_string();
        for want in ["--watch", "--report", "--tasks", "--members", "synthetic"] {
            assert!(help.contains(want), "`flux eval --help` missing {want:?}");
        }
    }

    /// `flux plugin …` help tells the truth about the current lifecycle and follows the naming
    /// trio (the protocol crate / a pack binary / the CLI, D-49): verified remote install from the
    /// signed pack with `--dir` as the local-scan mode (D-47), and enforced pin/rollback over the
    /// versioned store (D-48).
    #[test]
    fn plugin_help_documents_install_modes_and_pin_rollback() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let plugin = cmd.find_subcommand("plugin").expect("plugin subcommand");
        let top = plugin.clone().render_long_help().to_string();
        assert!(
            top.contains("plugin CLI"),
            "`flux plugin --help` should name the plugin CLI leg of the trio"
        );
        for want in ["install", "pin", "rollback", "status", "uninstall", "skill"] {
            assert!(top.contains(want), "`flux plugin --help` missing {want:?}");
        }
        let sub_help = |name: &str| {
            plugin
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("plugin subcommand {name}"))
                .clone()
                .render_long_help()
                .to_string()
        };
        let install = sub_help("install");
        for want in [
            "signed",
            "sha256",
            "versioned store",
            "--dir",
            "flux-plugin-*",
            // D-87: the third install source and its from-source trust label.
            "--git",
            "from-source",
        ] {
            assert!(
                install.contains(want),
                "`flux plugin install --help` missing {want:?}"
            );
        }
        let pin = sub_help("pin");
        for want in ["versioned store", "sha256", "spawn", "rollback"] {
            assert!(
                pin.contains(want),
                "`flux plugin pin --help` missing {want:?}"
            );
        }
        let rollback = sub_help("rollback");
        for want in ["offline", "versioned store"] {
            assert!(
                rollback.contains(want),
                "`flux plugin rollback --help` missing {want:?}"
            );
        }
    }

    /// The turn flags are scoped to the agent path, not leaked onto other subcommands — checked
    /// against the DECLARED arguments (not rendered help text), like
    /// `top_level_has_only_the_color_flag`, so a subcommand description that merely *mentions*
    /// `--continue` can't false-trip this.
    #[test]
    fn agent_flags_are_scoped_off_other_subcommands() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let longs_of = |name: &str| -> Vec<String> {
            cmd.find_subcommand(name)
                .unwrap_or_else(|| panic!("subcommand {name}"))
                .get_arguments()
                .filter_map(|a| a.get_long().map(String::from))
                .collect()
        };
        let has = |longs: &[String], flag: &str| longs.iter().any(|l| l == flag);
        for sub in ["sessions", "loop", "completion", "auth", "plugin"] {
            let longs = longs_of(sub);
            assert!(
                !has(&longs, "max-tokens"),
                "`{sub}` declares --max-tokens: {longs:?}"
            );
            assert!(
                !has(&longs, "continue"),
                "`{sub}` declares --continue: {longs:?}"
            );
        }
        // The agent-path subcommands carry the full turn-flag set.
        for agent_cmd in ["run", "tui"] {
            let longs = longs_of(agent_cmd);
            assert!(
                has(&longs, "max-tokens")
                    && has(&longs, "max-model-calls")
                    && has(&longs, "continue"),
                "`{agent_cmd}` should carry the turn flags: {longs:?}"
            );
        }
        // `review` carries only its scoped-down ReviewFlags: the session/approval flags its
        // FlowClient path can't honor are parse errors, not accepted-and-ignored.
        let review = longs_of("review");
        assert!(has(&review, "max-tokens"));
        assert!(
            !has(&review, "continue") && !has(&review, "resume"),
            "review must not accept session flags it ignores: {review:?}"
        );
        assert!(
            !has(&review, "yes"),
            "review must not accept --yes (it always auto-approves its fixed read-only flow)"
        );
        // `eval` has its own `-m` but not the turn-flag set.
        let eval = longs_of("eval");
        assert!(has(&eval, "model"), "eval should keep its own --model");
        assert!(
            !has(&eval, "max-tokens"),
            "eval should not carry the turn flags"
        );
    }

    /// The clap-level constraints reject contradictory or path-dead flag combinations at parse
    /// time (exit 2 + usage), instead of accepting-and-ignoring or failing deep in a handler.
    #[test]
    fn contradictory_flag_combinations_are_parse_errors() {
        use clap::Parser;
        let err = |args: &[&str]| {
            super::Cli::try_parse_from(args)
                .err()
                .unwrap_or_else(|| panic!("{args:?} should be rejected at parse time"));
        };
        // completion: an unknown shell is a usage error, not a silent empty script + exit 0.
        err(&["flux", "completion", "bassh"]);
        // fork: --prompt belongs to mode B (replan) only.
        err(&[
            "flux", "fork", "s_1", "--at", "2", "--inject", "1", "--prompt", "x",
        ]);
        err(&[
            "flux", "fork", "s_1", "--at", "2", "--edit", "f.flux", "--prompt", "x",
        ]);
        // flow run: --resume-value binds a halted await — meaningless without --resume.
        err(&["flux", "flow", "run", "f.flux", "--resume-value", "42"]);
        // changelog: one selection mode at a time.
        err(&["flux", "changelog", "0.11.6", "--all"]);
        err(&["flux", "changelog", "0.11.6", "--unreleased"]);
        err(&["flux", "changelog", "--all", "--unreleased"]);
        // plugin install: local-scan and remote modes are exclusive.
        err(&["flux", "plugin", "install", "--dir=some/dir", "gitlab"]);
        err(&["flux", "plugin", "install", "--all", "gitlab"]);
        // plugin call: --dry-run validates; --no-validate skips validation.
        err(&[
            "flux",
            "plugin",
            "call",
            "p",
            "op",
            "--dry-run",
            "--no-validate",
        ]);
        // skill surfaces: --global picks the install destination; --out is a different one.
        err(&["flux", "skill", "--global"]);
        err(&["flux", "plugin", "skill", "--global"]);
        err(&["flux", "plugin", "skill", "--install", "--out", "x.md"]);
        // Zero is invalid where it would alias (1-based --turn) or instantly fail/mislead.
        err(&["flux", "replay", "--turn", "0"]);
        err(&["flux", "run", "--max-tokens", "0", "hi"]);
        err(&["flux", "run", "--max-model-calls", "0", "hi"]);
        err(&["flux", "run", "--max-iterations", "0", "hi"]);
        err(&["flux", "run", "--turn-budget", "0", "hi"]);
        err(&["flux", "eval", "not-an-adapter"]);
        err(&["flux", "eval", "synthetic", "--trials", "0"]);
        // review's scoped-down flags: the flags its FlowClient path ignores are parse errors.
        err(&["flux", "review", "--files", "x.rs", "--yes"]);
        err(&["flux", "review", "--files", "x.rs", "--continue"]);
        // D-130: --sandbox and --no-sandbox are mutually exclusive.
        err(&["flux", "--sandbox", "--no-sandbox", "run", "hi"]);
    }

    /// …and the legitimate forms of the same flags still parse.
    #[test]
    fn valid_flag_combinations_parse() {
        use clap::Parser;
        let ok = |args: &[&str]| {
            super::Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        };
        ok(&["flux", "completion", "zsh"]);
        ok(&["flux", "completion"]);
        ok(&[
            "flux", "fork", "s_1", "--at", "2", "--replan", "--prompt", "x",
        ]);
        ok(&[
            "flux",
            "flow",
            "run",
            "f.flux",
            "--resume",
            "last",
            "--resume-value",
            "42",
        ]);
        ok(&["flux", "changelog", "0.11.6"]);
        ok(&["flux", "plugin", "install", "--dir"]);
        ok(&["flux", "plugin", "install", "--dir=plugins/target/release"]);
        ok(&["flux", "plugin", "install", "gitlab", "slack@1.2.0"]);
        ok(&["flux", "plugin", "install", "--all"]);
        ok(&["flux", "skill", "--install", "--global"]);
        ok(&["flux", "replay", "--turn", "1"]);
        ok(&["flux", "eval", "terminal-bench"]);
        ok(&["flux", "eval", "multi", "--members", "synthetic,mock"]);
        // D-130: --sandbox and --no-sandbox parse fine on their own (only combined do they conflict).
        ok(&["flux", "--sandbox", "run", "hi"]);
        ok(&["flux", "--no-sandbox", "run", "hi"]);
        // --serve's optional value: the common documented shape (no program, space-separated
        // address) still parses; a program BEFORE a bare --serve avoids the ambiguity entirely.
        ok(&["flux", "app", "run", "--serve", "0.0.0.0:1234", "--yes"]);
        ok(&["flux", "app", "run", "p.flux", "--serve", "--yes"]);
        ok(&[
            "flux",
            "app",
            "run",
            "p.flux",
            "--serve=0.0.0.0:1234",
            "--yes",
        ]);
        ok(&["flux", "review", "--files", "x.rs", "-m", "mock"]);
    }

    /// `program_resolves` PATH-searches a bare name. A one-component relative path has
    /// `Path::parent() == Some("")`, which must not be mistaken for "has a directory component" —
    /// that pre-fix bug reported every bare-name plugin as `missing` in `flux plugin status`
    /// while `call` (which spawns via PATH) worked fine.
    #[test]
    fn program_resolves_finds_bare_names_on_path() {
        let dir =
            std::env::temp_dir().join(format!("flux-program-resolves-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("flux-plugin-resolve-probe");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let _guard = EnvVarGuard::new("PATH");
        let old = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old}", dir.display()));

        assert!(
            super::program_resolves("flux-plugin-resolve-probe"),
            "bare name on PATH must resolve"
        );
        assert!(!super::program_resolves("flux-plugin-definitely-absent"));
        // A path with a separator is checked directly, never PATH-searched.
        assert!(super::program_resolves(bin.to_str().unwrap()));
        assert!(!super::program_resolves("./flux-plugin-resolve-probe"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux run <app.flux> extra words` errors loudly — before the fix, everything after the
    /// program path was silently discarded.
    #[tokio::test]
    async fn run_app_cmd_rejects_trailing_words() {
        let flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let err = super::run_app_cmd(
            vec!["app.flux".into(), "with".into(), "inputs".into()],
            &flags,
        )
        .await
        .expect_err("trailing words after a program path must error");
        assert!(
            err.to_string().contains("takes no further arguments"),
            "got: {err:#}"
        );
    }

    /// `--members` pairs with the `multi` adapter only — both mismatches are caught before any
    /// suite runs (previously: multi-without-members failed deep in flux-eval, members-without-
    /// multi was silently ignored).
    #[tokio::test]
    async fn eval_members_pairing_is_validated_up_front() {
        let err = super::run_eval_cmd(
            super::EvalAdapter::Multi,
            vec![],
            vec![],
            0,
            1,
            None,
            false,
            None,
        )
        .await
        .expect_err("multi without --members");
        assert!(err.to_string().contains("--members"), "got: {err:#}");
        let err = super::run_eval_cmd(
            super::EvalAdapter::Synthetic,
            vec![],
            vec!["mock".into()],
            0,
            1,
            None,
            false,
            None,
        )
        .await
        .expect_err("--members without multi");
        assert!(err.to_string().contains("--members"), "got: {err:#}");
    }

    #[test]
    fn truncate_caps_with_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "hel…");
    }

    #[test]
    fn format_evidence_empty_is_a_hint() {
        let log = flux_evidence::EvidenceLog::new();
        assert!(format_evidence(&log).contains("no evidence recorded yet"));
    }

    #[test]
    fn format_evidence_summarizes_and_lists_observations() {
        use flux_evidence::{EvidenceLog, Observation, Phase};
        let mut log = EvidenceLog::new();
        log.record(Observation::new(
            "tool_call",
            Phase::Turn,
            json!({"tool": "read"}),
        ));
        log.record(Observation::new(
            "tool_error",
            Phase::Turn,
            json!({"tool": "cargo_test"}),
        ));
        log.record(Observation::new(
            "turn.iteration",
            Phase::Turn,
            json!({"steps": 3}),
        ));

        let out = format_evidence(&log);
        // Summary line counts observations, iterations, and errors (correctly pluralized).
        assert!(out.contains("3 observations"), "{out}");
        assert!(out.contains("1 iteration,"), "singular iteration: {out}");
        assert!(out.contains("1 error"), "{out}");
        // Each observation kind is listed verbatim (the kind column is not colored).
        assert!(out.contains("tool_call"), "{out}");
        assert!(out.contains("tool_error"), "{out}");
        assert!(out.contains("turn.iteration"), "{out}");
    }

    #[test]
    fn loop_machinery_label_only_relabels_machinery_ops() {
        assert!(loop_machinery_label("detect_intent", &json!({}))
            .unwrap()
            .contains("classify the request"));
        assert!(loop_machinery_label("execute_batch", &json!({}))
            .unwrap()
            .contains("approved actions"));
        // `observe` surfaces its kind; ordinary ops fall through (None) to the normal label path.
        assert!(
            loop_machinery_label("observe", &json!({"kind": "turn.iteration"}))
                .unwrap()
                .contains("turn.iteration")
        );
        assert!(loop_machinery_label("read", &json!({"file": "x"})).is_none());
    }

    #[test]
    fn tool_preview_single_line_unchanged() {
        assert_eq!(tool_preview("no matches", false), "no matches");
    }

    #[test]
    fn tool_preview_caps_lines_by_default_and_shows_all_when_full() {
        // Default: up to 40 lines shown, the rest counted (with a `-v for full` hint).
        let many: String = (1..=50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let p = tool_preview(&many, false);
        assert!(p.contains("line 40"), "40th line shown: {p}");
        assert!(!p.contains("line 41"), "41st line elided: {p}");
        assert!(
            p.contains("(+10 more lines; -v for full)"),
            "elision note: {p}"
        );
        assert!(p.contains("\n  line 2"), "continuation lines indented: {p}");

        // Full (`-v`): every line shown, no elision note.
        let p = tool_preview(&many, true);
        assert!(p.contains("line 50"), "all lines shown when full: {p}");
        assert!(!p.contains("more lines"), "no elision note when full: {p}");
    }

    #[test]
    fn tool_preview_caps_a_long_single_line_unless_full() {
        let p = tool_preview(&"x".repeat(600), false);
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= 501);
        // Full: the whole line, untruncated.
        let p = tool_preview(&"x".repeat(600), true);
        assert_eq!(p.chars().count(), 600);
        assert!(!p.ends_with('…'));
    }

    #[test]
    fn endpoint_list_redacts() {
        // The `flux endpoint list` row renders the credential REFERENCE LOCATION, never a value: a
        // record with a kubernetes-scheme credential_ref shows `kubernetes/<ns>/<name>/<key>` and the
        // bare URL — and no secret-shaped string.
        use flux_secret::endpoint::{EndpointRecord, EndpointRef};
        use flux_secret::Ref;
        let rec = EndpointRecord {
            owner: "kubernetes".into(),
            ttl_secs: Some(900),
            health: Some("ok".into()),
            ..EndpointRecord::config(EndpointRef {
                credential_ref: Some(Ref::kubernetes("prod", "rds-creds", "password")),
                protocol: Some("postgres".into()),
                ..EndpointRef::discovered(
                    "prod-orders",
                    "postgres://orders.prod.svc:5432",
                    "postgres",
                )
            })
        };
        let row = render_endpoint_row(&rec);
        // The credential column is the LOCATION string only.
        assert!(
            row.contains("credential: kubernetes/prod/rds-creds/password"),
            "row must show the credential location: {row}"
        );
        // The bare URL + owner + ttl/health are present.
        assert!(row.contains("postgres://orders.prod.svc:5432"));
        assert!(row.contains("owner=kubernetes"));
        assert!(row.contains("ttl=900s") && row.contains("health=ok"));
        // No secret value leaks (the location names the key, never a value; nothing "secret"-shaped).
        assert!(!row.to_lowercase().contains("secret"));
        assert!(!row.contains("Bearer "));
        // A credential-less record renders `none`, not a placeholder value.
        let plain = EndpointRecord::config(EndpointRef::discovered(
            "svc-1",
            "https://svc.internal",
            "service",
        ));
        assert_eq!(credential_location(&plain), "none");
        assert!(render_endpoint_row(&plain).contains("credential: none"));
    }

    #[test]
    fn a2a_render_suffix_handles_delta_and_snapshot() {
        // Delta stream: each chunk is new; nothing is the prior prefix → render the whole chunk.
        assert_eq!(new_render_suffix("Hello wor", "ld"), "ld");
        assert_eq!(new_render_suffix("", "Hello"), "Hello");
        // Snapshot stream: each event repeats the whole text so far → render only the new tail.
        assert_eq!(new_render_suffix("Hello", "Hello world"), " world");
        assert_eq!(new_render_suffix("Hello world", "Hello world"), "");
        // A delta that coincidentally doesn't extend the prefix is rendered verbatim.
        assert_eq!(new_render_suffix("abc", "xyz"), "xyz");
    }

    // -----------------------------------------------------------------------
    // `flux review` (L-13): exit-code logic + output rendering
    // -----------------------------------------------------------------------

    fn finding(severity: &str) -> flux_tools::cognition::ReviewFinding {
        flux_tools::cognition::ReviewFinding {
            fingerprint: format!("fp-{severity}"),
            severity: severity.to_string(),
            category: "correctness".to_string(),
            file: Some("src/lib.rs".to_string()),
            line: Some(42),
            title: format!("a {severity} finding"),
            evidence: "some evidence".to_string(),
            recommendation: "fix it".to_string(),
            confidence: 0.8,
            reviewer: "correctness".to_string(),
            agreement: 1,
        }
    }

    fn report_with(severities: &[&str]) -> flux_tools::cognition::ReviewReport {
        flux_tools::cognition::ReviewReport {
            summary: "test report".to_string(),
            findings: severities.iter().map(|s| finding(s)).collect(),
            checked_files: vec!["src/lib.rs".to_string()],
            reviewers: vec!["correctness".to_string()],
            gaps: Vec::new(),
        }
    }

    /// `should_fail` is the pure decision factored out of `run_review` so the exit-code logic is
    /// unit-testable without going through `std::process::exit`: `None` (no `--fail-on`) never fails;
    /// a threshold fails iff some finding's severity is at or above it.
    #[test]
    fn should_fail_is_off_by_default() {
        let report = report_with(&["critical"]);
        assert!(
            !should_fail(&report, None),
            "no --fail-on must never fail, regardless of findings"
        );
    }

    #[test]
    fn should_fail_trips_at_or_above_the_threshold_only() {
        let report = report_with(&["low", "medium"]);
        assert!(
            !should_fail(&report, Some(ReviewSeverity::High)),
            "no finding reaches High"
        );
        assert!(
            should_fail(&report, Some(ReviewSeverity::Medium)),
            "the medium finding meets a Medium threshold"
        );
        assert!(
            should_fail(&report, Some(ReviewSeverity::Low)),
            "Low is at-or-above the Low threshold too"
        );
    }

    #[test]
    fn should_fail_is_false_when_there_are_no_findings() {
        let report = report_with(&[]);
        assert!(!should_fail(&report, Some(ReviewSeverity::Info)));
    }

    /// An unrecognized/malformed severity string must fail safe: it trips even the strictest
    /// (`Critical`) threshold rather than silently being ranked as harmless.
    #[test]
    fn should_fail_treats_an_unrecognized_severity_as_critical() {
        let report = report_with(&["not-a-real-severity"]);
        assert!(should_fail(&report, Some(ReviewSeverity::Critical)));
    }

    /// `--format json` must emit valid, round-trippable `ReviewReport` JSON — the CLI's own
    /// `serde_json::to_string_pretty` output parses back into an equivalent report.
    #[test]
    fn review_report_serializes_to_valid_json() {
        let report = report_with(&["high", "low"]);
        let s = serde_json::to_string_pretty(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert_eq!(parsed["findings"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["summary"], "test report");
    }

    /// `render_review_markdown`'s default output mode names each finding's severity/title/category
    /// and reports the checked files + reviewers — a human-readable summary, not raw JSON.
    #[test]
    fn render_review_markdown_lists_findings_and_metadata() {
        let report = report_with(&["critical", "low"]);
        let md = render_review_markdown(&report);
        assert!(md.contains("# Strict review"));
        assert!(md.contains("test report"));
        assert!(md.contains("CRITICAL"));
        assert!(md.contains("a critical finding"));
        assert!(md.contains("correctness"));
        assert!(md.contains("src/lib.rs:42"));
    }

    #[test]
    fn render_review_markdown_reports_no_findings_and_gaps() {
        let mut report = report_with(&[]);
        report.gaps.push("dropped malformed entry".to_string());
        let md = render_review_markdown(&report);
        assert!(md.contains("No findings."));
        assert!(md.contains("## Gaps"));
        assert!(md.contains("dropped malformed entry"));
    }

    /// L-25 — `flux flow run --resume <session|last>`'s own session-resolution logic (the CLI-level
    /// seam, distinct from flux-flow's engine-level fast-forward tests): a literal session id passes
    /// straight through; an unnamed flow can't use `last` (nothing disambiguates it from any other
    /// unnamed halted flow, including a host-derived action flow from an agent turn — same store,
    /// same ledger machinery); and `last` finds the most recent halted session matching THIS flow's
    /// declared name, skipping a more-recent halted session that belongs to a different flow.
    #[test]
    fn resolve_resume_session_passes_through_literals_and_last_matches_by_flow_name() {
        use flux_flow::ast::{DraftAst, FailureKind, NodeId, RunEvent};
        use flux_flow::state::FlowStore;
        use std::sync::Arc;

        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let named = DraftAst {
            name: Some("greet".into()),
            ..Default::default()
        };

        // A literal (non-"last") argument passes straight through, whatever it is — the caller
        // finds out soon enough (via `open_halted_plan` returning `None`) if it's wrong.
        assert_eq!(
            super::resolve_resume_session(&events, &flow, &named, "s_999").unwrap(),
            "s_999"
        );

        // An unnamed flow can't use `last` — refused with a clear, actionable error.
        let unnamed = DraftAst::default();
        let err = super::resolve_resume_session(&events, &flow, &unnamed, "last")
            .unwrap_err()
            .to_string();
        assert!(err.contains("declare a name"), "{err}");

        // `last` with nothing halted yet for this name is a clean error, not a silent no-op.
        assert!(super::resolve_resume_session(&events, &flow, &named, "last").is_err());

        // OLDER session, halted under THIS flow's name.
        let this_flow_session = events.create_session("mock").unwrap();
        flow.append_event(
            &this_flow_session,
            &RunEvent::PlanHalted {
                plan: "greet#aaaaaaaaaaaaaaaa".into(),
                node: NodeId(0),
                stmt: "s1".into(),
                op: None,
                kind: FailureKind::Runtime,
                error: "boom".into(),
            },
        )
        .unwrap();
        // NEWER session, halted under a DIFFERENT flow's name — `last` must not just grab the
        // newest halted session overall.
        let other_flow_session = events.create_session("mock").unwrap();
        flow.append_event(
            &other_flow_session,
            &RunEvent::PlanHalted {
                plan: "other-flow#bbbbbbbbbbbbbbbb".into(),
                node: NodeId(0),
                stmt: "s1".into(),
                op: None,
                kind: FailureKind::Runtime,
                error: "boom".into(),
            },
        )
        .unwrap();

        assert_eq!(
            super::resolve_resume_session(&events, &flow, &named, "last").unwrap(),
            this_flow_session,
            "matches by flow name, not just recency"
        );
    }

    // --- D-65: app-path redaction + audit parity -----------------------------------------------

    /// Direct unit test of the `flux_plugin::EgressAudit` L6 binding both the `build_agent` and
    /// `flux app run` plugin-wiring sites construct (`EventStoreEgressAudit`): appends a
    /// `PrivateNetAdmit` event onto the given run's stream — never a fabricated one — so a private-net
    /// admission is auditable regardless of which surface's plugin loop installed the hook.
    #[test]
    fn egress_audit_adapter_records_private_net_admit_on_the_runs_stream() {
        use flux_plugin::EgressAudit;
        use std::sync::Arc;

        let events = Arc::new(EventStore::in_memory().unwrap());
        let stream = events.create_session("mock").unwrap();
        let audit = EventStoreEgressAudit {
            store: events.clone(),
            stream: stream.clone(),
        };
        audit.record_private_admit("some-plugin", "127.0.0.1", "config:plugin/some-plugin");

        let recorded = events.load_by_kind(&stream, "private_net_admit").unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "exactly one PrivateNetAdmit landed on this run's stream"
        );
        match &recorded[0].kind {
            flux_events::EventKind::PrivateNetAdmit {
                caller,
                host,
                grant_source,
            } => {
                assert_eq!(caller, "some-plugin");
                assert_eq!(host, "127.0.0.1");
                assert_eq!(grant_source, "config:plugin/some-plugin");
            }
            other => panic!("expected PrivateNetAdmit, got {other:?}"),
        }
    }

    /// Restores (or removes) an env var on drop — panic-safe cleanup for env-mutating tests, so a
    /// failed assertion can't leak a widened grant into every later test in the process.
    struct EnvVarGuard {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                prior: std::env::var_os(key),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// D-95: endpoint-level private-net grants (`[private_net.endpoints]`) parse into the config
    /// (`endpoint_private_hosts` merges them on top of the plugin grant), but the grant the plugin
    /// host actually enforces is resolved by `effective_plugin_private_hosts`, which consults only the
    /// plugin-level `[private_net.plugins]` grant. So an endpoint-only grant is INERT on every
    /// plugin-invocation path (agent / `app run` / direct `flux plugin call`) — there is no
    /// direct-call-specific gap and no path that honours endpoint grants. This pins that documented
    /// reality (`docs/designs/scoped-private-net-egress.md` § *Enforcement status*), so wiring true
    /// per-endpoint scoping later must deliberately update both the enforcement path and the docs.
    #[test]
    fn endpoint_private_net_grants_are_inert_on_the_enforced_plugin_path() {
        let mut cfg = flux_config::Config::default();
        cfg.private_net.endpoints.insert(
            "gitlab:api".to_string(),
            flux_config::PrivateNetGrant::Hosts(vec!["api.internal".to_string()]),
        );

        // The config layer parses + merges the declared endpoint grant …
        assert_eq!(
            cfg.endpoint_private_hosts("gitlab", "api"),
            vec!["api.internal".to_string()],
            "endpoint_private_hosts must surface a declared [private_net.endpoints] grant"
        );
        // … but the enforced path (used by all three plugin-invocation sites) ignores an
        // endpoint-only grant: with no plugin-level grant, no private host is admitted.
        assert!(
            super::effective_plugin_private_hosts(&cfg, "gitlab").is_empty(),
            "endpoint-only grants must stay inert until per-endpoint scoping is deliberately wired"
        );
    }

    /// D-96: the ephemeral `--allow-private-net` override widens the *operator* grant to `*` for this
    /// process and stamps a distinct `cli:--allow-private-net` audit grant-source, while its absence
    /// preserves deny-by-default (an empty config yields no private grant). The manifest-declaration
    /// intersection that still gates each plugin lives in `flux_plugin::SystemHostCaps` and is covered
    /// there; this pins the CLI-surface wiring — including the truthy-value semantics: an explicit
    /// "off" value (`0`) must never widen an SSRF-relevant grant.
    #[test]
    fn allow_private_net_override_widens_grant_and_labels_audit() {
        let cfg = flux_config::Config::default();
        let _guard = EnvVarGuard::new("FLUX_ALLOW_PRIVATE_NET");

        // Off (default): deny-by-default. Empty config → no private grant; audit source is the normal
        // per-plugin config label (matching SystemHostCaps::with_manifest's default).
        std::env::remove_var("FLUX_ALLOW_PRIVATE_NET");
        assert!(!super::private_net_cli_override());
        assert!(super::effective_plugin_private_hosts(&cfg, "gitlab").is_empty());
        assert!(super::effective_web_private_hosts(&cfg).is_empty());
        assert_eq!(
            super::private_net_grant_source_for("gitlab"),
            "config:plugin/gitlab"
        );

        // An explicit "off" value stays OFF — presence alone must not grant (the pre-fix bug).
        for off in ["0", "false", "no", "off", ""] {
            std::env::set_var("FLUX_ALLOW_PRIVATE_NET", off);
            assert!(
                !super::private_net_cli_override(),
                "FLUX_ALLOW_PRIVATE_NET={off:?} must not widen the grant"
            );
            assert!(super::effective_plugin_private_hosts(&cfg, "gitlab").is_empty());
        }

        // On: the operator grant widens to `*` and the audit source becomes the CLI-flag label.
        std::env::set_var("FLUX_ALLOW_PRIVATE_NET", "1");
        assert!(super::private_net_cli_override());
        assert_eq!(
            super::effective_plugin_private_hosts(&cfg, "gitlab"),
            vec!["*".to_string()]
        );
        assert_eq!(
            super::effective_web_private_hosts(&cfg),
            vec!["*".to_string()]
        );
        assert_eq!(
            super::private_net_grant_source_for("gitlab"),
            "cli:--allow-private-net"
        );
    }

    /// D-130 (findings 6/7/9b): `apply_sandbox_env` resolves posture **tightest-wins** — the
    /// strictest of `Require > On > Off` across `--sandbox`, a pre-set `FLUX_SANDBOX`, and config —
    /// so a laxer source can never silently downgrade a stricter one; the sole override is the
    /// explicit kill switch (`--no-sandbox` / `FLUX_SANDBOX=off`). The startup preflight then fails
    /// closed under `require` when no backend is usable.
    ///
    /// Real backends shipped (D-131 bubblewrap, D-132 Seatbelt), so this forces BOTH discovery
    /// vars — `FLUX_BWRAP_BIN` (Linux) and `FLUX_SANDBOX_EXEC_BIN` (macOS) — at nonexistent paths so
    /// the backend resolves `Unsupported` deterministically on either platform (finding 9b: forcing
    /// only `FLUX_BWRAP_BIN` let macOS resolve a real Seatbelt backend and the `.unwrap_err()`
    /// below panicked). `FLUX_SANDBOXED` is cleared too, so an ambient nested-run marker can't make
    /// `resolve()` report `AlreadyConfined` (which would satisfy `require` and defeat the test).
    #[test]
    fn apply_sandbox_env_resolves_tightest_wins_and_fails_closed_under_require() {
        use clap::Parser;

        let _g_mode = EnvVarGuard::new("FLUX_SANDBOX");
        let _g_net = EnvVarGuard::new("FLUX_SANDBOX_NET");
        let _g_writable = EnvVarGuard::new("FLUX_SANDBOX_WRITABLE");
        let _g_bwrap = EnvVarGuard::new("FLUX_BWRAP_BIN");
        let _g_exec = EnvVarGuard::new("FLUX_SANDBOX_EXEC_BIN");
        let _g_confined = EnvVarGuard::new("FLUX_SANDBOXED");
        std::env::set_var(
            "FLUX_BWRAP_BIN",
            "/nonexistent/definitely-not-a-real-bwrap-d126",
        );
        std::env::set_var(
            "FLUX_SANDBOX_EXEC_BIN",
            "/nonexistent/definitely-not-a-real-sandbox-exec-d132",
        );
        // No ambient "already confined by a parent flux" marker — that would satisfy `require`.
        std::env::remove_var("FLUX_SANDBOXED");

        let bare = super::Cli::try_parse_from(["flux", "run", "hi"]).unwrap();
        let sandboxed = super::Cli::try_parse_from(["flux", "--sandbox", "run", "hi"]).unwrap();
        let no_sandbox = super::Cli::try_parse_from(["flux", "--no-sandbox", "run", "hi"]).unwrap();

        let mut cfg_require = flux_config::Config::default();
        cfg_require.sandbox.require = true;

        // Nothing set anywhere: off, and no startup error.
        std::env::remove_var("FLUX_SANDBOX");
        super::apply_sandbox_env(&bare, &flux_config::Config::default()).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("off"));

        // Config alone (`require`) propagates when nothing else overrides it, and — with no usable
        // backend (forced above) — fails closed at the startup preflight.
        std::env::remove_var("FLUX_SANDBOX");
        let err = super::apply_sandbox_env(&bare, &cfg_require).unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
        assert_eq!(
            std::env::var("FLUX_SANDBOX").as_deref(),
            Ok("require"),
            "the var is exported even though the call then errors"
        );

        // (a) TIGHTEST-WINS: `--sandbox` (asks for `On`) alongside config `require` resolves to
        // `Require`, NOT `On` — the soft flag must not downgrade the fail-closed config posture
        // (finding 6). So it still fails closed against the unavailable backend.
        std::env::remove_var("FLUX_SANDBOX");
        let err = super::apply_sandbox_env(&sandboxed, &cfg_require).unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
        assert_eq!(
            std::env::var("FLUX_SANDBOX").as_deref(),
            Ok("require"),
            "tightest-wins: --sandbox must not downgrade a configured `require` to `on`"
        );

        // (b) A pre-set `FLUX_SANDBOX` that is empty or a typo must NOT downgrade config `require` —
        // the old `_ => Off` arm silently dropped a fail-closed posture (finding 6). Both still
        // resolve to `Require` and fail closed.
        for garbage in ["", "requird"] {
            std::env::set_var("FLUX_SANDBOX", garbage);
            let err = super::apply_sandbox_env(&bare, &cfg_require).unwrap_err();
            assert!(err.to_string().contains("unavailable"), "{err}");
            assert_eq!(
                std::env::var("FLUX_SANDBOX").as_deref(),
                Ok("require"),
                "a garbage FLUX_SANDBOX={garbage:?} must not downgrade a configured `require`"
            );
        }

        // A pre-set `on` with default config is a soft request: it only warns (Ok), never fails
        // closed — `On`-mode auto-degrades against the unavailable backend.
        std::env::set_var("FLUX_SANDBOX", "on");
        super::apply_sandbox_env(&bare, &flux_config::Config::default()).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("on"));

        // (c) `--no-sandbox` is the kill switch: forces Off over a pre-set `require` env AND config.
        std::env::set_var("FLUX_SANDBOX", "require");
        super::apply_sandbox_env(&no_sandbox, &cfg_require).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("off"));

        // (c) A pre-set `FLUX_SANDBOX=off` is the other kill switch: forces Off even over config
        // `require` (mirrors `FLUX_OP_CACHE=off`).
        std::env::set_var("FLUX_SANDBOX", "off");
        super::apply_sandbox_env(&bare, &cfg_require).unwrap();
        assert_eq!(
            std::env::var("FLUX_SANDBOX").as_deref(),
            Ok("off"),
            "FLUX_SANDBOX=off is the kill switch, even over config `require`"
        );

        // `--sandbox` with no pre-set env and default config resolves to `On` (soft): warns and
        // runs unconfined against the unavailable backend, no error.
        std::env::remove_var("FLUX_SANDBOX");
        super::apply_sandbox_env(&sandboxed, &flux_config::Config::default()).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("on"));

        // Network: an explicit `false` in config narrows and is exported; the default stays open
        // and exports nothing (mirrors FLUX_ADD_DIRS' "only set what changes" style). Applies
        // regardless of mode.
        std::env::remove_var("FLUX_SANDBOX");
        std::env::remove_var("FLUX_SANDBOX_NET");
        let mut cfg_net = flux_config::Config::default();
        cfg_net.sandbox.network = Some(false);
        super::apply_sandbox_env(&bare, &cfg_net).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX_NET").as_deref(), Ok("0"));

        std::env::remove_var("FLUX_SANDBOX_NET");
        super::apply_sandbox_env(&bare, &flux_config::Config::default()).unwrap();
        assert!(
            std::env::var("FLUX_SANDBOX_NET").is_err(),
            "the unrestricted default exports nothing"
        );

        // Writable: config entries are absolutized against the cwd and exported as a `:`-list.
        std::env::remove_var("FLUX_SANDBOX_WRITABLE");
        let mut cfg_writable = flux_config::Config::default();
        cfg_writable.sandbox.writable = vec!["relative-sandbox-dir".to_string()];
        super::apply_sandbox_env(&bare, &cfg_writable).unwrap();
        let exported = std::env::var("FLUX_SANDBOX_WRITABLE").unwrap();
        assert!(
            std::path::Path::new(&exported).is_absolute(),
            "expected an absolutized path, got {exported:?}"
        );
        assert!(exported.ends_with("relative-sandbox-dir"), "{exported:?}");

        std::env::remove_var("FLUX_SANDBOX");
        std::env::remove_var("FLUX_SANDBOX_NET");
        std::env::remove_var("FLUX_SANDBOX_WRITABLE");
    }

    /// Direct unit test of the `flux_capabilities::CrossPluginAudit` L6 binding
    /// (`EventStoreCrossPluginAudit`): records a `CrossPluginResolve` per successful cross-plugin
    /// credential resolution (D-27) and an `EndpointDiscovered` per provider whose discovery returned
    /// candidates (D-30), both onto the given run's stream. The SAME struct backs
    /// `.with_cross_plugin_audit(...)` on both the `build_agent` and `flux app run` paths' brokers.
    #[test]
    fn cross_plugin_audit_adapter_records_resolve_and_discovery_on_the_runs_stream() {
        use flux_capabilities::CrossPluginAudit;
        use std::sync::Arc;

        let events = Arc::new(EventStore::in_memory().unwrap());
        let stream = events.create_session("mock").unwrap();
        let audit = EventStoreCrossPluginAudit {
            store: events.clone(),
            stream: stream.clone(),
        };
        audit.record_cross_plugin_resolve("consumer", "kubernetes", "kubernetes/ns/name/key");
        audit.record_discovery("postgres", "kubernetes", 3);

        let resolves = events
            .load_by_kind(&stream, "cross_plugin_resolve")
            .unwrap();
        assert_eq!(resolves.len(), 1);
        match &resolves[0].kind {
            flux_events::EventKind::CrossPluginResolve {
                consumer,
                provider,
                reference_location,
            } => {
                assert_eq!(consumer, "consumer");
                assert_eq!(provider, "kubernetes");
                assert_eq!(reference_location, "kubernetes/ns/name/key");
            }
            other => panic!("expected CrossPluginResolve, got {other:?}"),
        }

        let discoveries = events.load_by_kind(&stream, "endpoint_discovered").unwrap();
        assert_eq!(discoveries.len(), 1);
        match &discoveries[0].kind {
            flux_events::EventKind::EndpointDiscovered {
                product,
                provider,
                count,
            } => {
                assert_eq!(product, "postgres");
                assert_eq!(provider, "kubernetes");
                assert_eq!(*count, 3);
            }
            other => panic!("expected EndpointDiscovered, got {other:?}"),
        }
    }

    /// D-65's acceptance centerpiece — mirror of flux-app's C-13 seeding guarantee, but through the
    /// CROSS-PLUGIN credential path (`SystemHostCaps`'s `credential` capability, resolved via the
    /// endpoint broker) that both the `build_agent` and `flux app run` plugin-wiring sites install a
    /// `RedactorSecretSink` on. Drives `integration_plugin_caps` — the same function both CLI paths
    /// calls to build a plugin's caps — so a regression in the production wiring (e.g. dropping
    /// `.with_secret_sink(...)`) fails this test too, not just a hand-rolled re-implementation. A
    /// credential resolved this way must land in the SAME redactor an executor dispatches with, so it
    /// is scrubbed from model-visible tool output even though the trusted plugin binary received the
    /// raw value.
    #[tokio::test]
    async fn cross_plugin_credential_resolution_seeds_the_redactor_used_by_dispatch() {
        use async_trait::async_trait;
        use flux_capabilities::{
            CredentialReader, CrossPluginGrants, EndpointBroker, EndpointRegistry,
            HostProviderInvoker, MemoryBackend, PluginRegistry,
        };
        use flux_plugin::{PluginCapabilities, PluginManifest};
        use flux_runtime::{
            AllowApprover, Approver, Executor, PermissionManager, ToolContext, ToolRegistry,
            ToolResult,
        };
        use flux_secret::{Redactor, Ref};
        use flux_system::{System, Workspace};
        use std::sync::Arc;

        /// A fake credential reader (mirrors flux-capabilities' own broker-test double) so the
        /// cross-plugin gate resolves without a provider subprocess.
        struct FakeReader {
            value: String,
        }
        #[async_trait]
        impl CredentialReader for FakeReader {
            async fn read(&self, _provider: &str, _reference: &Ref) -> Result<String, String> {
                Ok(self.value.clone())
            }
        }

        let secret = "k8s-pg-password-d65";
        let broker = Arc::new(
            EndpointBroker::new(
                Arc::new(HostProviderInvoker::new(Arc::new(PluginRegistry::new()))),
                Arc::new(PluginRegistry::new()),
                Arc::new(EndpointRegistry::new()),
            )
            .with_credential_reader(Arc::new(FakeReader {
                value: secret.to_string(),
            }))
            .with_cross_plugin_grants(CrossPluginGrants::new(vec!["consumer:kubernetes".into()])),
        );

        let redactor = Redactor::new();
        let secret_sink = Arc::new(RedactorSecretSink {
            redactor: redactor.clone(),
        }) as Arc<dyn flux_plugin::SecretSink>;
        let events = Arc::new(EventStore::in_memory().unwrap());
        let stream = events.create_session("mock").unwrap();
        let audit: Arc<dyn flux_plugin::EgressAudit> = Arc::new(EventStoreEgressAudit {
            store: events,
            stream,
        });
        let manifest = PluginManifest {
            name: "consumer".into(),
            capabilities: PluginCapabilities {
                credential: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let dir = std::env::temp_dir().join(format!("flux-d65-secret-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let backend =
            Arc::new(MemoryBackend::new()) as Arc<dyn flux_capabilities::DatasourceBackend>;
        let caps = integration_plugin_caps(
            Arc::new(flux_plugin::FixedSystem(system.clone())),
            backend,
            true,
            &manifest,
            Vec::new(),
            broker.clone() as Arc<dyn flux_plugin::ReferenceResolver>,
            audit,
            secret_sink,
            broker.clone(),
        );

        let cred = Ref::kubernetes("monitoring", "pg-creds", "password");
        let result = caps
            .handle("credential", &json!({ "credential_ref": cred.to_string() }))
            .await
            .expect("credential capability granted + resolver installed");
        assert_eq!(
            result["value"], secret,
            "the trusted plugin still receives the raw value"
        );

        // The resolved credential is now a known secret to `redactor` — a tool leaking it comes back
        // scrubbed, exactly like flux-app's C-13 guarantee (`journey_executor_scrubs_resolved_secrets_
        // from_tool_output`).
        struct LeakyTool {
            secret: String,
        }
        #[async_trait]
        impl flux_runtime::Tool for LeakyTool {
            fn spec(&self) -> flux_spec::ToolSpec {
                flux_spec::ToolSpec::read_only("search", "leaks", json!({"type": "object"}))
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _params: serde_json::Value,
            ) -> flux_core::Result<ToolResult> {
                Ok(ToolResult::ok(format!("found: {}", self.secret)))
            }
        }
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(LeakyTool {
            secret: secret.to_string(),
        }));
        let ctx = ToolContext::new(system).with_redactor(redactor);
        let perms = PermissionManager::from_rules(&["search".to_string()], &[]);
        let approver: Arc<dyn Approver> = Arc::new(AllowApprover);
        let executor = Executor::new(registry, perms, approver, ctx);
        let r = executor.dispatch("search", json!({})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            !r.content.contains(secret),
            "the cross-plugin-resolved credential must be scrubbed from tool output: {}",
            r.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
