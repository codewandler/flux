use super::*;

/// `flux render <file.flux> [--view source|tree] [-o out.svg]` (L-77) — the non-gated entry point
/// to the L-76 renderer, and the generator for flux's own doc images (replaces the
/// flux-tree-sitter repo's `scripts/render-example.mjs`). Builds the workspace from the
/// environment like every production construction site, then delegates to [`run_render_in`].
pub(super) async fn run_render(file: &str, view: RenderView, out: Option<&str>) -> Result<()> {
    let system = System::from_env(std::env::current_dir()?).map_err(|e| anyhow::anyhow!("{e}"))?;
    run_render_in(&system, file, view, out).await
}

/// The testable core of `flux render`. The INPUT is read like the sibling file-input subcommands
/// (`flow run`, `app run`): a plain filesystem read relative to the invocation cwd, so `../` and
/// absolute paths work — only the `-o` WRITE is workspace-confined (through `System::write_file`;
/// SVG is text, parents are created). A UTF-8 BOM is stripped before parsing (a PowerShell/
/// Notepad-authored file would otherwise fail the parser with an invisible U+FEFF in the first
/// token). Without `out` the SVG streams to stdout; an early-closing consumer (`flux render
/// x.flux | head`) never panics — on Unix the process ends with the conventional SIGPIPE exit
/// (`main` resets `SIG_DFL`, A-61), on Windows the `BrokenPipe` write error is treated as
/// success. A hard parse error in `tree` view propagates — the CLI exits non-zero with the
/// parser's message — while `source` view is total.
pub(super) async fn run_render_in(
    system: &System,
    file: &str,
    view: RenderView,
    out: Option<&str>,
) -> Result<()> {
    let source = std::fs::read_to_string(file).with_context(|| format!("read {file}"))?;
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    let svg = flux_tools::render::render_flux_svg(source, view.into())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match out {
        Some(path) => {
            system
                .write_file(path, &svg)
                .await
                .map_err(|e| anyhow::anyhow!("write {path}: {e}"))?;
            let view_word = match view {
                RenderView::Source => "source",
                RenderView::Tree => "tree",
            };
            eprintln!("rendered {file} ({view_word} view) → {path}");
        }
        None => {
            use std::io::Write;
            // Not `println!`: a consumer that stops reading early (`| head`, a converter erroring
            // out) must not turn the write into a panic. On Unix this arm is normally moot —
            // `main`'s A-61 `reset_sigpipe` restores `SIG_DFL`, so the process ends on SIGPIPE
            // (conventional exit 141, like `cat`) before the write ever returns EPIPE. The arm IS
            // the path on Windows (no SIGPIPE — the closed pipe surfaces as a BrokenPipe io
            // error) and under std's default SIG_IGN (unit tests). A broken pipe means the
            // consumer has everything it wants — exit cleanly.
            let mut stdout = std::io::stdout();
            match stdout
                .write_all(svg.as_bytes())
                .and_then(|()| stdout.flush())
            {
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                r => r.context("write SVG to stdout")?,
            }
        }
    }
    Ok(())
}

pub(super) struct LoadedCliFlow {
    pub(super) ast: flux_flow::ast::DraftAst,
    pub(super) composites: Vec<flux_lang::program::CompositeOpDecl>,
}

/// `flux flow list` / `ls`: discovery only. This deliberately constructs just a guarded `System`
/// and the shared catalog — no provider, event store, session, plugin process, or agent engine.
pub(super) fn run_flow_list() -> Result<()> {
    let cwd = std::env::current_dir().context("current dir")?;
    let system =
        System::new(workspace_with_flow_roots(&cwd, false)?).with_sandbox(resolved_sandbox());
    println!("{}", flux_tools::StoredFlowCatalog::load(&system).render());
    Ok(())
}

/// Parse an existing path using the long-standing file semantics. JSON DraftAst files remain
/// supported; a native module path must still select exactly one flow/journey.
pub(super) fn parse_cli_flow_source(label: &str, source: &str) -> Result<LoadedCliFlow> {
    if source.trim_start().starts_with('{') {
        return Ok(LoadedCliFlow {
            ast: serde_json::from_str(source)
                .with_context(|| format!("parse {label} as a Flux-Lang DraftAst (JSON)"))?,
            composites: Vec::new(),
        });
    }
    match flux_lang::program::Module::parse_str(source)
        .map_err(|e| anyhow::anyhow!("parse {label} as Flux-Lang text: {e}"))?
    {
        flux_lang::program::Module::Flow(ast) => Ok(LoadedCliFlow {
            ast,
            composites: Vec::new(),
        }),
        flux_lang::program::Module::Program(program) => {
            let ast = match (program.flows.as_slice(), program.journeys.as_slice()) {
                ([flow], []) => flow.clone(),
                ([], [journey]) => journey.flow.clone(),
                _ => bail!(
                    "`flux flow run` needs a bare flow or a module with exactly one flow/journey"
                ),
            };
            Ok(LoadedCliFlow {
                ast,
                composites: program.ops,
            })
        }
    }
}

/// Resolve the positional target as a real file first, then as a saved-flow filename stem or
/// declaration. Saved-name runs do not return their file's ops as module-local declarations: those
/// ops are already in the engine's auto-loaded composite snapshot and must be installed once.
pub(super) fn load_cli_flow_target(target: &str) -> Result<LoadedCliFlow> {
    if std::path::Path::new(target).is_file() {
        let source =
            std::fs::read_to_string(target).with_context(|| format!("read flow {target}"))?;
        return parse_cli_flow_source(target, &source);
    }

    let cwd = std::env::current_dir().context("current dir")?;
    let system =
        System::new(workspace_with_flow_roots(&cwd, false)?).with_sandbox(resolved_sandbox());
    let resolved = flux_tools::StoredFlowCatalog::load(&system)
        .resolve(target)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(LoadedCliFlow {
        ast: resolved.ast,
        composites: Vec::new(),
    })
}

pub(super) fn validate_flow_input_value(
    key: &str,
    value: &serde_json::Value,
    ty: &flux_lang::ast::TypeRef,
) -> Result<()> {
    use flux_lang::ast::TypeRef;
    let valid = match ty {
        TypeRef::Any | TypeRef::Named(_) => true,
        TypeRef::Bool => value.is_boolean(),
        TypeRef::Number => value.is_number(),
        TypeRef::String => value.is_string(),
        TypeRef::List(inner) => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| value_matches_type(item, inner))),
    };
    if valid {
        Ok(())
    } else {
        bail!(
            "input `{key}` expects {}, got {}",
            ty.label(),
            json_value_kind(value)
        )
    }
}

pub(super) fn value_matches_type(value: &serde_json::Value, ty: &flux_lang::ast::TypeRef) -> bool {
    use flux_lang::ast::TypeRef;
    match ty {
        TypeRef::Any | TypeRef::Named(_) => true,
        TypeRef::Bool => value.is_boolean(),
        TypeRef::Number => value.is_number(),
        TypeRef::String => value.is_string(),
        TypeRef::List(inner) => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| value_matches_type(item, inner))),
    }
}

pub(super) fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "Bool",
        serde_json::Value::Number(_) => "Number",
        serde_json::Value::String(_) => "String",
        serde_json::Value::Array(_) => "List",
        serde_json::Value::Object(_) => "object",
    }
}

/// Coerce one final (last-wins) `--arg` value from its declared TypeRef. Any/named values accept
/// either JSON or plain text; concrete scalar/list types are deliberately strict.
pub(super) fn coerce_flow_arg(
    key: &str,
    raw: &str,
    ty: &flux_lang::ast::TypeRef,
) -> Result<serde_json::Value> {
    use flux_lang::ast::TypeRef;
    let value = match ty {
        TypeRef::String => serde_json::Value::String(raw.to_string()),
        TypeRef::Any | TypeRef::Named(_) => {
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
        }
        TypeRef::Number | TypeRef::Bool | TypeRef::List(_) => serde_json::from_str(raw)
            .with_context(|| format!("--arg {key} expects {} JSON", ty.label()))?,
    };
    validate_flow_input_value(key, &value, ty)?;
    Ok(value)
}

pub(super) fn mapper_schema(params: &[flux_lang::ast::Param]) -> serde_json::Value {
    let properties: serde_json::Map<String, serde_json::Value> = params
        .iter()
        .map(|param| (param.name.0.clone(), schema_for_type(&param.ty)))
        .collect();
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": params.iter().map(|param| param.name.0.clone()).collect::<Vec<_>>(),
    })
}

pub(super) fn schema_for_type(ty: &flux_lang::ast::TypeRef) -> serde_json::Value {
    use flux_lang::ast::TypeRef;
    match ty {
        TypeRef::Any => serde_json::json!({}),
        TypeRef::Bool => serde_json::json!({"type": "boolean"}),
        TypeRef::Number => serde_json::json!({"type": "number"}),
        TypeRef::String => serde_json::json!({"type": "string"}),
        TypeRef::List(inner) => {
            serde_json::json!({"type": "array", "items": schema_for_type(inner)})
        }
        TypeRef::Named(name) => {
            serde_json::json!({"description": format!("Flux value of type {name}")})
        }
    }
}

pub(super) fn used_flow_symbols(
    ast: &flux_flow::ast::DraftAst,
) -> std::collections::HashSet<String> {
    use flux_flow::ast::Node;
    let mut used: std::collections::HashSet<String> = ast
        .params
        .iter()
        .map(|param| param.name.0.clone())
        .collect();
    flux_lang::analyze::for_each_node(&ast.body, &mut |node| match node {
        Node::Bind { name, .. }
        | Node::Memo { name, .. }
        | Node::Peek { name }
        | Node::Var { name } => {
            used.insert(name.0.clone());
        }
        Node::Each { item, collect, .. } => {
            used.insert(item.0.clone());
            if let Some(name) = collect {
                used.insert(name.0.clone());
            }
        }
        Node::Repeat {
            collect: Some(name),
            ..
        } => {
            used.insert(name.0.clone());
        }
        Node::Pipe { bind, .. }
        | Node::Seq { bind, .. }
        | Node::Retry { bind, .. }
        | Node::Loop { bind, .. }
        | Node::Fallback { bind, .. }
        | Node::Timeout { bind, .. }
        | Node::Budget { bind, .. }
        | Node::CapScope { bind, .. }
        | Node::Scope { bind, .. }
        | Node::Once { bind, .. } => {
            if let Some(name) = bind {
                used.insert(name.0.clone());
            }
        }
        Node::Race { bind, branches, .. } => {
            if let Some(name) = bind {
                used.insert(name.0.clone());
            }
            used.extend(branches.iter().map(|branch| branch.name.0.clone()));
        }
        Node::Try {
            catch: Some(name), ..
        } => {
            used.insert(name.0.clone());
        }
        Node::Await {
            binding: Some(name),
            ..
        } => {
            used.insert(name.0.clone());
        }
        Node::Parallel { branches } => {
            used.extend(branches.iter().map(|branch| branch.name.0.clone()));
        }
        Node::Ctx {
            name,
            include,
            exclude,
            ..
        } => {
            used.insert(name.0.clone());
            used.extend(include.iter().chain(exclude).map(|name| name.0.clone()));
        }
        Node::CtxAppend { ctx, add } => {
            used.insert(ctx.0.clone());
            used.extend(add.iter().map(|name| name.0.clone()));
        }
        _ => {}
    });
    used
}

pub(super) fn fresh_mapper_symbol(
    base: &str,
    used: &mut std::collections::HashSet<String>,
) -> flux_lang::ast::SymbolName {
    let mut candidate = base.to_string();
    let mut suffix = 0usize;
    while used.contains(&candidate) {
        suffix += 1;
        candidate = format!("{base}_{suffix}");
    }
    used.insert(candidate.clone());
    candidate.into()
}

/// Lower opt-in natural-language mapping into ordinary, recorded Flux nodes. Strict `jq` field
/// reads make a missing field/non-object fatal before the original body begins; bind annotations
/// retain each declared TypeRef in the plan.
pub(super) fn mapper_nodes(
    ast: &flux_flow::ast::DraftAst,
    missing: &[flux_lang::ast::Param],
    text: &str,
) -> Result<Vec<flux_flow::ast::Node>> {
    use flux_flow::ast::{FlowEffect, Node, TypeRef};
    let mut used = used_flow_symbols(ast);
    let raw = fresh_mapper_symbol("__flux_map_raw", &mut used);
    let parsed = fresh_mapper_symbol("__flux_map_json", &mut used);
    let object = fresh_mapper_symbol("__flux_map_args", &mut used);
    let schema =
        serde_json::to_string(&mapper_schema(missing)).context("serialize input schema")?;

    let call_fields = [
        (
            "ask".to_string(),
            Box::new(Node::Lit {
                value: serde_json::Value::String(
                    "Extract exactly one argument object for the requested flow parameters. Return a JSON array containing exactly that one object and no prose."
                        .into(),
                ),
            }),
        ),
        (
            "from".to_string(),
            Box::new(Node::Lit {
                value: serde_json::Value::String(text.to_string()),
            }),
        ),
        (
            "schema".to_string(),
            Box::new(Node::Lit {
                value: serde_json::Value::String(schema),
            }),
        ),
    ]
    .into_iter()
    .collect();

    let mut nodes = vec![
        Node::Bind {
            name: raw.clone(),
            value: Box::new(Node::Call {
                op: "ai.extract".into(),
                args: vec![Node::Obj {
                    fields: call_fields,
                }],
            }),
            ty: Some(TypeRef::String),
            effect: Some(FlowEffect::Model),
        },
        Node::Bind {
            name: parsed.clone(),
            value: Box::new(Node::Parse {
                value: Box::new(Node::Var { name: raw }),
                as_type: "json".into(),
            }),
            ty: Some(TypeRef::List(Box::new(TypeRef::Any))),
            effect: None,
        },
        Node::Assert {
            cond: Box::new(Node::Expr {
                formula: "len(items) == 1".into(),
                vars: [(
                    "items".to_string(),
                    Box::new(Node::Var {
                        name: parsed.clone(),
                    }),
                )]
                .into_iter()
                .collect(),
            }),
            message: Some(
                "--map-inputs must return exactly one argument object in a JSON array".into(),
            ),
        },
        Node::Bind {
            name: object.clone(),
            value: Box::new(Node::Jq {
                path: "[0]".into(),
                input: Box::new(Node::Var { name: parsed }),
                optional: false,
            }),
            ty: Some(TypeRef::Any),
            effect: None,
        },
    ];
    nodes.extend(missing.iter().map(|param| Node::Bind {
        name: param.name.clone(),
        value: Box::new(Node::Jq {
            path: format!(".{}", param.name.0),
            input: Box::new(Node::Var {
                name: object.clone(),
            }),
            optional: false,
        }),
        ty: Some(param.ty.clone()),
        effect: None,
    }));
    Ok(nodes)
}

/// Apply the CLI-only strict parameter contract and prepend the normalized AST nodes. Merge order:
/// mapper base, then `--inputs`, then repeatable `--arg` (last duplicate wins).
pub(super) fn prepare_cli_flow_inputs(
    ast: &mut flux_flow::ast::DraftAst,
    inputs: Option<&str>,
    args: &[String],
    map_inputs: Option<&str>,
) -> Result<()> {
    let mut deterministic = match inputs {
        Some(raw) => {
            let value: serde_json::Value = serde_json::from_str(raw)
                .with_context(|| "--inputs must be a valid JSON object")?;
            value
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("--inputs must be a JSON object"))?
        }
        None => serde_json::Map::new(),
    };

    // Preserve last-wins semantics even when an earlier duplicate is malformed for the declared
    // type: only the final raw value is coerced.
    let mut raw_args = std::collections::BTreeMap::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--arg expects KEY=VALUE (got `{arg}`)"))?;
        if key.is_empty() {
            bail!("--arg expects a non-empty key in KEY=VALUE");
        }
        raw_args.insert(key.to_string(), value.to_string());
    }

    let declared: std::collections::HashMap<&str, &flux_lang::ast::Param> = ast
        .params
        .iter()
        .map(|param| (param.name.0.as_str(), param))
        .collect();
    let unknown: std::collections::BTreeSet<String> = deterministic
        .keys()
        .chain(raw_args.keys())
        .filter(|key| !declared.contains_key(key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        bail!(
            "unknown flow input parameter(s): {} — declared parameters: {}",
            unknown.into_iter().collect::<Vec<_>>().join(", "),
            ast.params
                .iter()
                .map(|param| param.name.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for (key, raw) in raw_args {
        let param = declared[&key.as_str()];
        deterministic.insert(key.clone(), coerce_flow_arg(&key, &raw, &param.ty)?);
    }
    for param in &ast.params {
        if let Some(value) = deterministic.get(&param.name.0) {
            validate_flow_input_value(&param.name.0, value, &param.ty)?;
        }
    }

    let missing: Vec<flux_lang::ast::Param> = ast
        .params
        .iter()
        .filter(|param| !deterministic.contains_key(&param.name.0))
        .cloned()
        .collect();
    if !missing.is_empty() && map_inputs.is_none() {
        bail!(
            "missing required flow parameter(s): {} — pass --inputs, --arg, or opt in with --map-inputs",
            missing
                .iter()
                .map(|param| format!("{} ({})", param.name.0, param.ty.label()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let mut prefix = Vec::new();
    // If deterministic overlays cover the whole contract, skip the mapper (and therefore the model)
    // even when --map-inputs was supplied.
    if !missing.is_empty() {
        if let Some(text) = map_inputs {
            prefix.extend(mapper_nodes(ast, &missing, text)?);
        }
    }
    prefix.extend(ast.params.iter().filter_map(|param| {
        deterministic
            .get(&param.name.0)
            .map(|value| flux_flow::ast::Node::Bind {
                name: param.name.clone(),
                value: Box::new(flux_flow::ast::Node::Lit {
                    value: value.clone(),
                }),
                ty: Some(param.ty.clone()),
                effect: None,
            })
    }));
    prefix.append(&mut ast.body);
    ast.body = prefix;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_flow(
    target: &str,
    inputs: Option<String>,
    args: Vec<String>,
    map_inputs: Option<String>,
    model: Option<String>,
    yes: bool,
    resumable: bool,
    resume: Option<String>,
    resume_value: Option<String>,
) -> Result<()> {
    let LoadedCliFlow {
        mut ast,
        composites,
    } = load_cli_flow_target(target)?;
    prepare_cli_flow_inputs(&mut ast, inputs.as_deref(), &args, map_inputs.as_deref())?;

    // Build the agent only after target/input validation, so malformed deterministic input cannot
    // create a session and no flow effect can run before the strict contract passes.
    let flags = AgentFlags::from_model_yes(model.as_deref(), yes);
    run_draft_ast_with_composites_resumable(
        &flags,
        &ast,
        &composites,
        resumable,
        resume,
        resume_value,
    )
    .await
}

/// Execute a pre-built `DraftAst` through the full envelope — the shared core behind both
/// `flux flow run <name|file>` and `flux preset <name> --run`. Builds the agent, validates the flow
/// against the live op registry, previews risk + installs the per-op approver, runs it, and prints the
/// outcome. The only inputs are the agent flags (model/`--yes`) and the AST itself.
pub(crate) async fn run_draft_ast(
    flags: &AgentFlags,
    ast: &flux_flow::ast::DraftAst,
) -> Result<()> {
    run_draft_ast_with_composites(flags, ast, &[]).await
}

pub(crate) async fn run_draft_ast_with_composites(
    flags: &AgentFlags,
    ast: &flux_flow::ast::DraftAst,
    composites: &[flux_lang::program::CompositeOpDecl],
) -> Result<()> {
    run_draft_ast_with_composites_resumable(flags, ast, composites, false, None, None).await
}

/// Build the lexical turn snapshot for the top-level authored-flow CLI path. `EngineLoopHost`
/// returns the reporter instead of retaining it, so this helper keeps the ownership boundary hard
/// to accidentally undo at either the direct or resumable call site.
pub(super) fn direct_flow_runtime_turn(
    session_id: &str,
    activity: Arc<dyn SpawnActivitySink>,
) -> RuntimeTurnContext {
    RuntimeTurnContext::new()
        .with_session(session_id)
        .with_spawn_activity_sink(activity)
}

/// [`run_draft_ast_with_composites`] plus L-25's opt-in resumable mode for `flux flow run`.
/// `resumable` alone reifies a halting top-level statement (a failure, or the L-24 `Awaiting`
/// reified pause) as a printed, structured halt report + non-zero exit instead of erroring the
/// whole run (design `multipass-agent-loop.md`'s "L-25: pre-authored resumable mode"); `resume`
/// additionally targets a PRIOR halted session (a literal id, or `last`) and folds its statement
/// ledger before executing, so a corrected re-run fast-forwards the matching completed prefix.
/// `resume` implies resumable execution even when `--resumable` was not also passed. `flux preset
/// --run` and every other caller of [`run_draft_ast_with_composites`] pass `false, None` here and
/// keep today's exact strict (non-resumable) behavior — this is additive, not a mode switch.
pub(crate) async fn run_draft_ast_with_composites_resumable(
    flags: &AgentFlags,
    ast: &flux_flow::ast::DraftAst,
    composites: &[flux_lang::program::CompositeOpDecl],
    resumable: bool,
    resume: Option<String>,
    resume_value: Option<String>,
) -> Result<()> {
    let resumable = resumable || resume.is_some();

    // L-25: `--resume` targets a specific, ALREADY-halted session instead of minting a fresh one.
    // Resolved against throwaway store handles before `build_agent_lazy` opens its own — SQLite/WAL
    // supports the sequential opens, and this avoids wasting a session record or mis-tagging plugin
    // audit streams the way overriding `session_id` after construction would.
    let resume_session = match &resume {
        Some(arg) => {
            let events = Arc::new(open_event_store()?);
            let flow = open_flow_store(events.clone())?;
            Some(resolve_resume_session(&events, &flow, ast, arg)?)
        }
        None => None,
    };

    // Lazy provider (C-11): a pre-authored flow is deterministic unless it actually reaches a
    // model op — replaying one must not demand credentials.
    let (engine, session_id, model_spec, _spawner) =
        build_agent_lazy(flags, resume_session).await?;
    eprintln!(
        "{}",
        style::dim(&format!("flow · {} · session {session_id}", engine.model))
    );
    // C-43: authored flow runs record the cassette too (the engine arms it per agent turn; this
    // path executes directly, so it arms its own) — and persist the executed plan as an accepted
    // `plan_source` attempt (this path has no loop host to record it), so `flux flow run`
    // results are replayable with `flux replay` exactly like agent turns. Off with
    // FLUX_CASSETTE=0.
    if flux_flow::cassette::enabled() {
        engine
            .flow
            .set_cassette(Some(Arc::new(flux_flow::cassette::CassetteScope::Record(
                flux_flow::cassette::RecordScope::new(engine.events.clone(), &session_id),
            ))));
        // A recording failure (locked/full events.db) must be VISIBLE at record time — silently
        // dropping it would only surface later as replay's "no stored plan_source … skipped",
        // with the cause long gone.
        let recorded = engine
            .events
            .begin_turn(&session_id, "<flow run>", &engine.model)
            .and_then(|turn_id| {
                let source = flux_lang::format::format(ast);
                let redactor = &engine.executor.context().redactor;
                engine.events.record_plan_attempt(
                    &session_id,
                    turn_id,
                    flux_events::PlanAttempt {
                        step: 1,
                        outcome: "accepted".into(),
                        error: None,
                        fingerprint: Some(flux_lang::runtime::sha256_hex(
                            &serde_json::to_string(ast).unwrap_or_default(),
                        )),
                        plan_text: None,
                        phase: None,
                        plan_source: Some(redactor.redact(&source)),
                        delta_source: None,
                    },
                )
            });
        if let Err(e) = recorded {
            eprintln!(
                "{} this run won't be replayable — recording the plan failed: {e}",
                style::yellow("warning:")
            );
        }
    }
    engine
        .composites
        .ensure_session_loaded(&engine.flow, &session_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut active_composites = engine.composites.active_for_session(&session_id);
    // A path-loaded module owns its local declarations. If that path also lives under a flows home,
    // the same ops are already present in the auto-loaded snapshot; remove those copies before
    // installing the explicit declarations so module-local ops shadow rather than collide. A
    // saved-NAME run passes no explicit declarations and therefore uses the auto-loaded copy once.
    let explicit_names: std::collections::HashSet<&str> =
        composites.iter().map(|op| op.name.as_str()).collect();
    active_composites.retain(|op| !explicit_names.contains(op.name.as_str()));
    active_composites.extend(composites.iter().cloned());

    // Validate against the live op registry before running anything.
    if let Err(diags) =
        flux_flow::registry::analyze_composites(&active_composites, engine.executor.registry())
    {
        print_diagnostics(&diags);
        bail!("composite validation failed — see diagnostics above");
    }
    let oreg = flux_flow::registry::OpRegistry::new(engine.executor.registry())
        .with_composites(&active_composites);
    // Typed gate (L-16/F9): full structural analysis + lowering, with the session's already-bound
    // symbols satisfying definedness (a resumed session may legitimately reference prior turns).
    // A store read error must propagate — swallowed into an empty set it would resurface as a
    // bogus "unbound symbol" diagnostic on resume, pointing at the flow instead of the store.
    let session_symbols: std::collections::HashSet<String> = engine
        .flow
        .view(&session_id)
        .map(|v| v.symbols.into_iter().map(|s| s.name.0).collect())
        .map_err(|e| anyhow::anyhow!("read session symbols from flow store: {e}"))?;
    if let Err(diags) = flux_flow::analyze::lower(ast, &oreg, &session_symbols) {
        print_diagnostics(&diags);
        bail!("flow validation failed — see diagnostics above");
    }

    // L-25: fold the session's open halt latch. `None` for a fresh `--resumable`-only run (a
    // brand-new session never halted before); on `--resume`, the ledger to fast-forward against.
    // A resume target MUST have an open halt — silently continuing on a stale/typo'd session id
    // would hide a mistake rather than fail loudly.
    let open_halt = if resumable {
        engine.flow.open_halted_plan(&session_id)?
    } else {
        None
    };
    if resume.is_some() && open_halt.is_none() {
        bail!("session {session_id} has no open halt to resume — nothing to fast-forward");
    }

    // A-58 / F-015: a resume that lands on a value-awaiting `await` (`$reply = await …`) must supply
    // its payload. The resumable driver fast-forwards *past* the await, so bind `--resume-value` into
    // the awaited symbol first — otherwise post-await statements die on `unbound symbol`. When a
    // value-await gets no payload, refuse clearly (naming the symbol) instead of advancing into that
    // failure. A bare `await` binds nothing, so it needs no value.
    if let Some(open) = &open_halt {
        let awaited = flux_lang::runtime::awaited_binding(&ast.body, open.halt.node);
        match (&resume_value, awaited) {
            (Some(raw), _) => {
                // Parse as JSON so `42`/`true`/`"x"`/`{…}` keep their type; a bare word is a string.
                let value = serde_json::from_str::<serde_json::Value>(raw.trim())
                    .unwrap_or_else(|_| serde_json::Value::String(raw.clone()));
                let bound = flux_lang::runtime::bind_resume_value(
                    engine.flow.as_ref(),
                    &session_id,
                    &ast.body,
                    open.halt.node,
                    value,
                )
                .map_err(|e| anyhow::anyhow!("bind resume value: {e}"))?;
                if let Some(sym) = bound {
                    eprintln!("{}", style::dim(&format!("resume: bound ${sym} = {raw}")));
                }
            }
            (None, Some(sym)) => {
                bail!(
                    "session {session_id} halted awaiting a value for `${sym}` — pass \
                     --resume-value <json> (e.g. --resume-value '\"hello\"', --resume-value 42)"
                );
            }
            (None, None) => {}
        }
    }

    // Denied-statement resume guard: a statement policy or the user already
    // refused must never be silently re-dispatched just because it re-appears unchanged in a
    // corrected file. Checked BEFORE executing anything.
    if let Some(open) = &open_halt {
        if flux_flow::runtime::denied_resume_guard(&ast.body, &open.halt) {
            eprintln!(
                "{}",
                style::red(&flux_flow::runtime::render_halt_report(
                    ast,
                    &open.halt,
                    &session_id
                ))
            );
            eprintln!(
                "{}",
                style::dim(
                    "the statement previously refused is unchanged in this file — it was NOT \
                     re-run. Edit it to a different approach, or have an operator re-approve."
                )
            );
            std::process::exit(1);
        }
    }

    // Risk preview (informational; every op still gates at dispatch through the engine's approver,
    // which `build_agent` set from `--yes`). Scoped to the whole plan even when resuming — dispatch
    // itself never re-runs the skipped prefix, so this stays a harmless over-approval preview.
    let risk = if active_composites.is_empty() {
        flux_flow::runtime::plan_risk(ast, engine.executor.registry())
    } else {
        flux_flow::runtime::plan_risk_with_composites(
            ast,
            engine.executor.registry(),
            &active_composites,
        )
    };
    eprintln!(
        "\n{}  {}{}",
        style::bold("flow"),
        risk_badge(&risk.summary()),
        style::dim(&format!(" · {} op(s)", risk.ops.len()))
    );

    // Point the installed loop host at this run's session + sink. A flow may call `ai_segment` or
    // `flow_run`; the shared sink keeps nested stage and operation events on one surface.
    let shared: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
        CliSink::new(0).with_cost(model_spec, flux_credentials::load_pricing_table()),
    ));
    // `None` advertised set: this is the pre-authored `flow run` path, which is deliberately
    // unrestricted by surfacing because the authored file names its operations explicitly.
    let activity = engine.loop_host.set_turn(
        session_id.clone(),
        Some(engine.system_prompt.clone()),
        shared.clone(),
        None,
        None,
    );
    // `set_turn` deliberately returns (rather than retains) the live child reporter. Scope that
    // reporter together with this authored run's session so `task(...)` reached through either the
    // direct or resumable interpreter inherits the same A-80 turn context. The executor is reused
    // by the CLI, so pinning this on its long-lived ToolContext would leak an obsolete reporter into
    // a later run.
    let runtime_turn = direct_flow_runtime_turn(&session_id, activity);

    let mut sink = flux_flow::loop_host::SharedSink::new(shared.clone());
    let outcome = scope_runtime_turn(runtime_turn, async {
        if resumable {
            // A failing top-level statement reifies onto `outcome.failure` instead of
            // propagating `Err`; `open_halt`'s ledger (when resuming) fast-forwards the matching
            // prefix.
            flux_flow::runtime::execute_flow_resumable_with_composites(
                engine.flow.as_ref(),
                engine.executor.as_ref(),
                &session_id,
                ast,
                &active_composites,
                open_halt.as_ref().map(|o| &o.ledger),
                &mut sink,
            )
            .await
        } else {
            // Also the no-composites case (empty slice is equivalent): this entry point self-wires
            // the C-43 cassette scope from the store — plain `execute_flow` deliberately does not
            // (it is shared with the outer agent loop, whose host stages are never cassetted).
            flux_flow::runtime::execute_flow_with_composites(
                engine.flow.as_ref(),
                engine.executor.as_ref(),
                &session_id,
                ast,
                &active_composites,
                &mut sink,
            )
            .await
        }
    })
    .await
    .context("execute flow")?;

    // A reified halt (L-25): print the structured report and exit non-zero instead of the normal
    // success printing below — the caller corrects the file and re-runs with `--resume`.
    if let Some(halt) = &outcome.failure {
        eprintln!(
            "{}",
            flux_flow::runtime::render_halt_report(ast, halt, &session_id)
        );
        let u = engine.loop_host.turn_usage();
        shared
            .lock()
            .unwrap()
            .turn_end((u.total() > 0).then_some(u));
        std::process::exit(1);
    }

    if !outcome.result.trim().is_empty() {
        println!("{}", outcome.result);
    } else {
        // Always surface a closing summary so a direct flow turn never ends silently.
        eprintln!(
            "{}",
            style::dim(&format!("done \u{00b7} {} step(s)", outcome.steps))
        );
    }
    // A deterministic flow bills nothing (usage stays zero → `None`, today's output); a flow that
    // reached a model op via `ai_segment` reports its real spend.
    let u = engine.loop_host.turn_usage();
    shared
        .lock()
        .unwrap()
        .turn_end((u.total() > 0).then_some(u));
    Ok(())
}

/// Resolve `flux flow run <file> --resume <arg>` to a concrete session id (L-25). A literal id is
/// used as-is (the caller finds out soon enough — via [`FlowStore::open_halted_plan`] returning
/// `None` — if it names a session with no open halt). `last` searches the workspace's session store
/// (newest-first) for the most recent session with an open halt latch whose halted plan's key is
/// prefixed by this flow's declared name (the same `name#`/`h:` prefix
/// [`flow_key`](flux_lang::runtime) derives) — an UNNAMED flow can't be disambiguated this way (a
/// bare `h:<hash>` prefix could match ANY unnamed halted flow, including a host-derived action flow
/// from an agent turn, since they share the same session store and ledger machinery), so `last`
/// is refused for it and the caller is pointed at the explicit session id the halt report printed.
pub(super) fn resolve_resume_session(
    events: &EventStore,
    flow: &FlowStore,
    ast: &flux_flow::ast::DraftAst,
    arg: &str,
) -> Result<String> {
    if arg != "last" {
        return Ok(arg.to_string());
    }
    let name = ast
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "`--resume last` needs the flow to declare a name (`flow <name> -> …`) to find its \
                 halted session unambiguously — pass the explicit session id the halt report \
                 printed instead"
            )
        })?;
    let prefix = format!("{name}#");
    const SEARCH_LIMIT: usize = 500;
    for s in events.list(SEARCH_LIMIT).context("list sessions")? {
        if let Some(open) = flow
            .open_halted_plan(&s.id)
            .with_context(|| format!("open halted plan for session {}", s.id))?
        {
            if open.halt.plan.starts_with(&prefix) {
                return Ok(s.id);
            }
        }
    }
    bail!("no halted `flow run` session found for flow `{name}` — nothing to resume");
}

/// Whether *every* analyzer diagnostic is an unknown-op error (message shape `unknown operation: …`).
/// Picks an accurate header: a validation failure of another class (bad arg, arity, type/shape,
/// composability, unbound symbol, …) must not be filed under "references unknown operations" (A-62 /
/// F-010) — that header misleads both the reader and any model stage that reads diagnostics back to
/// repair. Empty ⇒ false (no header is printed for an empty set).
pub(super) fn diagnostics_all_unknown_op(diags: &[flux_flow::analyze::Diagnostic]) -> bool {
    !diags.is_empty()
        && diags
            .iter()
            .all(|d| d.message.starts_with("unknown operation"))
}

/// Print analyzer diagnostics to stderr, if any, under a header matching their actual failure class.
pub(super) fn print_diagnostics(diags: &[flux_flow::analyze::Diagnostic]) {
    if diags.is_empty() {
        return;
    }
    let header = if diagnostics_all_unknown_op(diags) {
        "diagnostics — the plan references unknown operations"
    } else {
        "diagnostics — the plan failed validation"
    };
    eprintln!("{}", style::yellow(header));
    for d in diags {
        eprintln!("{}", style::dim(&format!("  - {}", d.message)));
    }
}
