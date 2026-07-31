//! `pane.*` — the model-facing half of the agent-authored surface (C-223).
//!
//! Three ops over the L2 [`SurfaceSink`](flux_runtime::SurfaceSink) vocabulary C-220 fixed:
//! `pane.open`, `pane.update`, `pane.close`. Each one validates the shape of its arguments and then
//! **delegates to the host**, which owns the pane store, the geometry, the trust chrome and every
//! bound. That is `op.register`'s posture (`crate::reflect`) rather than a new one: this module
//! holds no pane state of its own, because a second copy of the surface's store would go stale the
//! moment a `turn`-scoped pane expired or `/resume` cleared the surface, and the model would be
//! reading a lie.
//!
//! # Surfacing is decided by sink presence, at assembly time — not by a `ToolGroup`
//!
//! [`try_register_surface_ops`] is the whole mechanism, and its `surface_sink_installed` argument is
//! the one fact a registry cannot ask for: whether the host that is assembling this catalog has a
//! human surface to draw on. A host with one registers the vocabulary; every other host — headless
//! `flux run`, `flux-server`, an SDK embedding — registers nothing and its model never sees a
//! `pane.*` op at all.
//!
//! It is deliberately **not** a [`crate::groups`] entry. A group is surfaced when a `project.signal`
//! matches (`groups.rs`), and there is no workspace signal for "a human is watching a terminal" —
//! the evidence for a pane channel is the channel itself. The precedent is `[consult]`, whose
//! *config* presence registers the op once, in the assembly path, and never re-decides it
//! (`crates/flux-cli/src/execution.rs`, `flux_config::ConsultConfig`): "within a session the
//! surfacing decision is made once at assembly time and never churns" — the A-95 cache-stability
//! lesson. Registry membership is fixed for the life of the catalog, so no mid-session change to the
//! installed sink can move the advertised tool set and invalidate a prompt prefix.
//!
//! A call is a second, independent question from surfacing: [`ToolContext::surface`] is read per
//! call and returns `None` when this dispatch context carries no sink, which is a clear op failure
//! (never a silent success). With the surfacing rule above that path exists for correctness — a
//! sub-agent or a nested runtime with no sink of its own — rather than as a routine occurrence.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{
    PaneCommand, PaneData, PaneLifetime, PaneSlot, SurfaceReporter, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::{Idempotency, IntentSet, Risk, ToolSpec};

/// The pane vocabulary, in one place, so the surfacing decision, the catalog and the reference
/// tables cannot drift apart.
pub const PANE_OPS: [&str; 3] = ["pane.open", "pane.update", "pane.close"];

/// Pane ids the **surface** owns. The host's own panes are addressed under this prefix
/// (`flux-tui`'s `host:fleet`, C-224), and a command aimed at one is dropped by the surface — so an
/// op that forwarded it would report a success the user never sees. Refused here instead, with the
/// reason.
const HOST_ID_PREFIX: &str = "host:";

/// Register the `pane.*` ops — **only** when the assembling host installed a
/// [`SurfaceSink`](flux_runtime::SurfaceSink).
///
/// This is C-223's surfacing mechanism; see the module docs for why it is not a `ToolGroup`. Call it
/// once, from the same place the catalog is assembled, with the decision the host is the only one
/// able to make. `false` registers nothing: fail-closed, so a surface that never installs a pane
/// channel cannot advertise a vocabulary it has nowhere to draw.
pub fn try_register_surface_ops(
    registry: &mut ToolRegistry,
    surface_sink_installed: bool,
) -> Result<()> {
    if !surface_sink_installed {
        return Ok(());
    }
    registry.try_register_all_from(
        "flux-tools surface pane pack",
        vec![
            Arc::new(PaneOpenOp) as Arc<dyn Tool>,
            Arc::new(PaneUpdateOp),
            Arc::new(PaneCloseOp),
        ],
    )
}

// ---------------------------------------------------------------------------
// shape validation — everything below delegates, so this is the only place a
// malformed call is turned into a repairable error rather than a dropped command.
// ---------------------------------------------------------------------------

/// The pane handle a call addresses, validated: present, non-blank, and not one of the surface's
/// own.
fn pane_id(params: &Value, op: &str) -> Result<String> {
    let id = crate::str_param(params, "id", op)?.trim();
    if id.is_empty() {
        return Err(Error::Other(format!("{op}: `id` must not be blank")));
    }
    if id.starts_with(HOST_ID_PREFIX) {
        return Err(Error::Other(format!(
            "{op}: `{HOST_ID_PREFIX}` ids belong to the surface's own panes — pick another `id`"
        )));
    }
    Ok(id.to_string())
}

/// The typed payload, from the externally tagged form the L2 [`PaneData`] serializes as (exactly one
/// key, naming the kind). Deserialized through `PaneData` itself so the op cannot drift from the
/// contract it forwards to.
fn pane_data(params: &Value, op: &str) -> Result<PaneData> {
    let data = params
        .get("data")
        .ok_or_else(|| Error::Other(format!("{op}: required param `data` missing")))?;
    serde_json::from_value(data.clone()).map_err(|e| {
        Error::Other(format!(
            "{op}: `data` must be one of {{\"rows\": {{header, rows}}}}, {{\"kv\": {{pairs}}}}, \
             {{\"log\": {{lines}}}}, {{\"progress\": {{label, done, total}}}}, \
             {{\"tree\": {{roots}}}} or {{\"markdown\": {{text}}}} ({e})"
        ))
    })
}

/// One of a closed set of string values, or a model-repairable error naming all of them.
fn one_of<T: Copy>(
    params: &Value,
    key: &str,
    op: &str,
    values: &[(&str, T)],
    default: T,
) -> Result<T> {
    let Some(raw) = params.get(key) else {
        return Ok(default);
    };
    if raw.is_null() {
        return Ok(default);
    }
    let name = raw
        .as_str()
        .ok_or_else(|| Error::Other(format!("{op}: `{key}` must be a string")))?;
    values
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
        .ok_or_else(|| {
            let allowed: Vec<&str> = values.iter().map(|(name, _)| *name).collect();
            Error::Other(format!(
                "{op}: unknown `{key}` `{name}` — use one of {}",
                allowed.join(", ")
            ))
        })
}

/// The redacting handle onto the host's pane channel, or the actionable failure a context without
/// one owes its caller.
fn surface(ctx: &ToolContext, op: &str) -> Result<SurfaceReporter> {
    ctx.surface().ok_or_else(|| {
        Error::Other(format!(
            "{op}: this session has no pane surface — nothing is watching a terminal here, so there \
             is nowhere to draw a pane. Put the content in your reply instead."
        ))
    })
}

/// The JSON Schema for a `data` argument. Hand-written rather than `schemars`-derived because
/// [`PaneData`] lives on the L2 contract and does not carry a schema derive; the round-trip test in
/// this module pins one example of every shape against `PaneData` itself, so a contract change that
/// this schema failed to follow fails the gate.
fn pane_data_schema() -> Value {
    let strings = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "type": "object",
        "description": "The pane's content: an object with exactly ONE key, naming the shape.",
        "oneOf": [
            {
                "type": "object", "required": ["rows"], "additionalProperties": false,
                "title": "rows — a table",
                "properties": {"rows": {
                    "type": "object", "required": ["rows"], "additionalProperties": false,
                    "properties": {
                        "header": strings,
                        "rows": {"type": "array", "items": strings},
                    },
                }},
            },
            {
                "type": "object", "required": ["kv"], "additionalProperties": false,
                "title": "kv — labelled values",
                "properties": {"kv": {
                    "type": "object", "required": ["pairs"], "additionalProperties": false,
                    "properties": {"pairs": {
                        "type": "array",
                        "items": {"type": "array", "items": {"type": "string"},
                                  "minItems": 2, "maxItems": 2},
                    }},
                }},
            },
            {
                "type": "object", "required": ["log"], "additionalProperties": false,
                "title": "log — newest-last lines",
                "properties": {"log": {
                    "type": "object", "required": ["lines"], "additionalProperties": false,
                    "properties": {"lines": strings},
                }},
            },
            {
                "type": "object", "required": ["progress"], "additionalProperties": false,
                "title": "progress — one counted task",
                "properties": {"progress": {
                    "type": "object", "required": ["label", "done", "total"],
                    "additionalProperties": false,
                    "properties": {
                        "label": {"type": "string"},
                        "done": {"type": "integer", "minimum": 0},
                        "total": {"type": "integer", "minimum": 0},
                    },
                }},
            },
            {
                "type": "object", "required": ["tree"], "additionalProperties": false,
                "title": "tree — nested labels",
                "properties": {"tree": {
                    "type": "object", "required": ["roots"], "additionalProperties": false,
                    "properties": {"roots": {"type": "array", "items": {
                        "type": "object", "required": ["label"], "additionalProperties": false,
                        "properties": {
                            "label": {"type": "string"},
                            "children": {"type": "array", "items": {"type": "object"}},
                        },
                    }}},
                }},
            },
            {
                "type": "object", "required": ["markdown"], "additionalProperties": false,
                "title": "markdown — prose the surface renders",
                "properties": {"markdown": {
                    "type": "object", "required": ["text"], "additionalProperties": false,
                    "properties": {"text": {"type": "string"}},
                }},
            },
        ],
    })
}

/// The metadata every pane op declares, and the reasoning behind each field.
///
/// * `effects` / `access` are **empty**: a pane reaches no filesystem, no process, no network and no
///   host state. It hands a typed value to a channel the host installed. Declaring a carrier it does
///   not touch would over-state it, and `Effect::Write` without a typed write resource is not even
///   representable (`flux_runtime::authority_requirements_from_declaration`).
/// * `semantic_effects` carries `human_visible` — the one thing a pane genuinely does. It lowers to
///   no host effect and no policy action (`FlowEffect::lower`), so it neither inflates the risk tier
///   nor invents an authority; it is the honest tag, and C-210 reads this channel too.
/// * `Idempotency::Conditional`, not `Idempotent`: repeating a pane command is safe, but
///   `Idempotent` + `Risk::Low` + no non-`Read` effect is exactly the dispatcher's op-cache
///   predicate, and a cached hit *returns without executing* — the surface would silently never see
///   the repeat. `Conditional` is the declaration for "safely repeatable, must still run".
fn pane_spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        output_schema: None,
        effects: Vec::new(),
        risk: Risk::Low,
        idempotency: Idempotency::Conditional,
        access: Vec::new(),
        group: None,
    }
}

/// The pane a call addresses, as a permission subject, so an operator can scope a rule to one pane
/// by name (`pane.update:build`) instead of the bare op. Never empty for a well-formed call: the
/// subject IS the resource these ops act on, and per AGENTS.md returning nothing to sidestep a gate
/// is not an option.
fn pane_subjects(params: &Value) -> Vec<String> {
    params
        .get("id")
        .and_then(Value::as_str)
        .map(|id| vec![id.trim().to_string()])
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// the ops
// ---------------------------------------------------------------------------

/// `pane.open(id, title, data[, slot, kind, lifetime])` — ask the surface for a durable container.
struct PaneOpenOp;

#[async_trait]
impl Tool for PaneOpenOp {
    fn spec(&self) -> ToolSpec {
        pane_spec(
            "pane.open",
            "Open a pane on the user's terminal: a durable container for status or results they \
             should keep seeing while you work (a build's progress, a checklist, a table of \
             findings). It is NOT where your answer goes — prose belongs in your reply, and a pane \
             that repeats what you just said is noise. `id` is your handle for later \
             `pane.update`/`pane.close`; opening an `id` you already opened replaces that pane \
             rather than adding a second one. `slot` (default `right`) and `lifetime` (default \
             `session`) are PROPOSALS: the surface owns geometry, colour, ordering and placement, \
             may demote or suppress a pane entirely, and always marks it as yours — you cannot \
             style it, and you cannot make it look like the harness. `lifetime: turn` closes the \
             pane when this turn ends; `project` is not implemented and is refused.",
            json!({
                "type": "object",
                "required": ["id", "title", "data"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string", "description": "your handle for this pane, reused by pane.update/pane.close"},
                    "title": {"type": "string", "description": "the pane's heading, content only"},
                    "slot": {"type": "string", "enum": ["left", "right", "bottom", "overlay"],
                             "description": "where the pane asks to sit (default `right`)"},
                    "kind": {"type": "string", "enum": ["rows", "kv", "log", "progress", "tree", "markdown"],
                             "description": "optional, and redundant: the renderer is taken from `data`. Supply it only to assert the shape you meant; a disagreement is an error"},
                    "lifetime": {"type": "string", "enum": ["turn", "session"],
                                 "description": "how long the pane survives (default `session`)"},
                    "data": pane_data_schema(),
                },
            }),
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        pane_subjects(params)
    }

    /// No intents: an intent names a filesystem, process, network or browser target the runtime
    /// then gates (`flux_spec::IntentBehavior`). A pane touches none of them, and inventing one
    /// would put a target in front of the approval machinery that no call ever reaches.
    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    fn semantic_effects(&self) -> Vec<String> {
        vec!["human_visible".to_string()]
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = pane_id(&params, "pane.open")?;
        let title = crate::str_param(&params, "title", "pane.open")?.to_string();
        let slot = one_of(
            &params,
            "slot",
            "pane.open",
            &[
                ("left", PaneSlot::Left),
                ("right", PaneSlot::Right),
                ("bottom", PaneSlot::Bottom),
                ("overlay", PaneSlot::Overlay),
            ],
            PaneSlot::Right,
        )?;
        let lifetime = one_of(
            &params,
            "lifetime",
            "pane.open",
            &[
                ("turn", PaneLifetime::Turn),
                ("session", PaneLifetime::Session),
                // `project` parses at the contract so the vocabulary stays stable, and the reporter
                // refuses it (C-220). Accepted here so the caller gets that reason rather than
                // "unknown lifetime", which would read as a spelling mistake.
                ("project", PaneLifetime::Project),
            ],
            PaneLifetime::Session,
        )?;
        let data = pane_data(&params, "pane.open")?;
        // `kind` is derived from `data` by `PaneSpec::new`, so the two can never disagree. A caller
        // that stated one anyway is held to it here rather than having it silently overridden.
        if let Some(declared) = params.get("kind").and_then(Value::as_str) {
            let actual = serde_json::to_value(data.kind())
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            if declared != actual {
                return Err(Error::Other(format!(
                    "pane.open: declared kind `{declared}` does not match `data` (`{actual}`)"
                )));
            }
        }
        let spec = flux_runtime::PaneSpec::new(&id, title, slot, lifetime, data);
        surface(ctx, "pane.open")?.send(PaneCommand::Open(spec))?;
        Ok(ToolResult::ok(format!("pane '{id}' open")))
    }
}

/// `pane.update(id, data)` — replace an open pane's content.
struct PaneUpdateOp;

#[async_trait]
impl Tool for PaneUpdateOp {
    fn spec(&self) -> ToolSpec {
        pane_spec(
            "pane.update",
            "Replace the content of a pane you opened, addressed by its `id`. This is how a pane \
             stays live across a long task — cheap, repeatable, and the whole payload each time \
             (there are no deltas); a payload of a different shape re-renders the pane in that \
             shape. An update addressed to an `id` that is not open — never opened, already \
             closed, or `lifetime: turn` after the turn ended — is dropped by the surface and \
             draws nothing, so re-open it with `pane.open` instead of updating blind.",
            json!({
                "type": "object",
                "required": ["id", "data"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string", "description": "the id you passed to pane.open"},
                    "data": pane_data_schema(),
                },
            }),
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        pane_subjects(params)
    }

    /// See [`PaneOpenOp::intents`].
    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    fn semantic_effects(&self) -> Vec<String> {
        vec!["human_visible".to_string()]
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = pane_id(&params, "pane.update")?;
        let data = pane_data(&params, "pane.update")?;
        surface(ctx, "pane.update")?.send(PaneCommand::Update {
            id: id.clone(),
            data,
        })?;
        Ok(ToolResult::ok(format!("pane '{id}' updated")))
    }
}

/// `pane.close(id)` — retire a pane.
struct PaneCloseOp;

#[async_trait]
impl Tool for PaneCloseOp {
    fn spec(&self) -> ToolSpec {
        pane_spec(
            "pane.close",
            "Close the pane you opened under this `id`, once its content stops being worth the \
             screen space. Closing an `id` that is not open is not an error and changes nothing, so \
             it is safe to close a pane you are unsure about. Panes opened with `lifetime: turn` \
             are closed for you at the end of the turn, and no pane outlives the session.",
            json!({
                "type": "object",
                "required": ["id"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string", "description": "the id you passed to pane.open"},
                },
            }),
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        pane_subjects(params)
    }

    /// See [`PaneOpenOp::intents`].
    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    fn semantic_effects(&self) -> Vec<String> {
        vec!["human_visible".to_string()]
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = pane_id(&params, "pane.close")?;
        surface(ctx, "pane.close")?.send(PaneCommand::Close { id: id.clone() })?;
        Ok(ToolResult::ok(format!("pane '{id}' closed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_runtime::{AllowApprover, Executor, PermissionManager, SurfaceSink};
    use std::sync::Mutex;

    /// A host sink that keeps what it was handed.
    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<PaneCommand>>);

    impl SurfaceSink for RecordingSink {
        fn emit(&self, command: PaneCommand) {
            self.0.lock().unwrap().push(command);
        }
    }

    impl RecordingSink {
        fn seen(&self) -> Vec<PaneCommand> {
            self.0.lock().unwrap().clone()
        }
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("flux-pane-ops-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A dispatch context, with or without a host pane channel.
    fn ctx(label: &str, sink: Option<Arc<RecordingSink>>) -> ToolContext {
        let root = temp_root(label);
        let system = flux_system::System::new(flux_system::Workspace::new(&root).unwrap());
        let ctx = ToolContext::new(Arc::new(system));
        if let Some(sink) = sink {
            ctx.set_surface_sink(sink);
        }
        ctx
    }

    fn a_log() -> Value {
        json!({"log": {"lines": ["one", "two"]}})
    }

    /// The catalog a host assembles, given whether it installed a pane channel.
    fn catalog(surface_sink_installed: bool) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        crate::try_register_builtins(&mut registry).expect("built-ins register");
        try_register_surface_ops(&mut registry, surface_sink_installed).expect("pane ops resolve");
        registry
    }

    /// C-223's named failing-first test: the pane vocabulary reaches the catalog of a host that
    /// installed a `SurfaceSink`, and no other catalog — decided by sink presence at assembly time,
    /// not by a `ToolGroup` and not per call.
    #[test]
    fn the_pane_ops_are_surfaced_only_for_a_host_with_a_sink() {
        let with_surface = catalog(true).names();
        for op in PANE_OPS {
            assert!(
                with_surface.iter().any(|name| name == op),
                "a host with a pane channel does not advertise `{op}`: {with_surface:?}"
            );
        }

        // Headless: `flux run`, `flux-server`, an SDK embedding. Not narrowed later, not advertised
        // and then refused — absent.
        let headless = catalog(false).names();
        for op in PANE_OPS {
            assert!(
                !headless.iter().any(|name| name == op),
                "a host with no pane channel still sees `{op}`"
            );
        }

        // Fail-closed: the default built-in pack carries no pane op, so a host that never takes the
        // decision cannot advertise a vocabulary it has nowhere to draw.
        let mut builtins = ToolRegistry::new();
        crate::try_register_builtins(&mut builtins).expect("built-ins register");
        for op in PANE_OPS {
            assert!(
                builtins.get(op).is_none(),
                "`{op}` is in the default built-in pack — surfacing is no longer fail-closed"
            );
        }
    }

    /// A-95: the decision is taken once. A dispatch context whose sink went away mid-session does
    /// not change the assembled catalog — it fails the call — so the advertised tool set (and the
    /// prompt prefix cached against it) cannot churn.
    #[tokio::test]
    async fn the_surfacing_decision_does_not_churn_when_the_sink_goes_away() {
        let assembled = catalog(true);
        let before: Vec<String> = assembled.names();

        let error = PaneOpenOp
            .execute(
                &ctx("churn", None),
                json!({"id": "build", "title": "Build", "data": a_log()}),
            )
            .await
            .expect_err("a context with no sink cannot open a pane");
        assert!(error.to_string().contains("no pane surface"), "{error}");

        assert_eq!(
            before,
            assembled.names(),
            "the catalog moved because a call found no sink"
        );
    }

    /// Item: a `pane.*` call reaching a context with no sink fails with an actionable error — never
    /// a silent success the model reads as "the user can see it".
    #[tokio::test]
    async fn every_pane_op_fails_actionably_without_a_sink() {
        let headless = ctx("headless", None);
        let calls: Vec<(Arc<dyn Tool>, Value)> = vec![
            (
                Arc::new(PaneOpenOp),
                json!({"id": "p", "title": "T", "data": a_log()}),
            ),
            (Arc::new(PaneUpdateOp), json!({"id": "p", "data": a_log()})),
            (Arc::new(PaneCloseOp), json!({"id": "p"})),
        ];
        for (tool, params) in calls {
            let op = tool.spec().name;
            let error = tool
                .execute(&headless, params)
                .await
                .expect_err(&format!("{op} must not succeed without a surface"));
            let message = error.to_string();
            assert!(
                message.starts_with(&format!("{op}: this session has no pane surface"))
                    && message.contains("Put the content in your reply"),
                "`{op}` failed without saying what to do instead: {message}"
            );
        }
    }

    #[tokio::test]
    async fn open_update_and_close_reach_the_sink_as_typed_commands() {
        let sink = Arc::new(RecordingSink::default());
        let ctx = ctx("typed", Some(sink.clone()));

        PaneOpenOp
            .execute(
                &ctx,
                json!({"id": "build", "title": "Build", "slot": "bottom",
                       "lifetime": "turn", "data": a_log()}),
            )
            .await
            .unwrap();
        PaneUpdateOp
            .execute(
                &ctx,
                json!({"id": "build", "data": {"progress": {"label": "compiling", "done": 3, "total": 9}}}),
            )
            .await
            .unwrap();
        PaneCloseOp
            .execute(&ctx, json!({"id": "build"}))
            .await
            .unwrap();

        let seen = sink.seen();
        assert_eq!(seen.len(), 3, "{seen:?}");
        let PaneCommand::Open(spec) = &seen[0] else {
            panic!("first command is not an open: {seen:?}")
        };
        assert_eq!(spec.id, "build");
        assert_eq!(spec.slot, PaneSlot::Bottom);
        assert_eq!(spec.lifetime, PaneLifetime::Turn);
        // The declared kind is derived from the payload, never taken on trust.
        assert_eq!(spec.kind, flux_runtime::PaneKind::Log);
        assert_eq!(
            seen[1],
            PaneCommand::Update {
                id: "build".into(),
                data: PaneData::Progress {
                    label: "compiling".into(),
                    done: 3,
                    total: 9
                }
            }
        );
        assert_eq!(seen[2], PaneCommand::Close { id: "build".into() });
    }

    /// The three cases the story names, each with defined behaviour rather than a panic or a
    /// silently different one. All three are the *host's* rules (`flux-tui`'s pane store: an open
    /// for a live id replaces it in place, an update for an unknown id is dropped, a close is a
    /// retain), so what an op owes is a well-formed command addressed to exactly the id asked for —
    /// and a description that states the rule, since this channel is send-only and no op can
    /// confirm the outcome.
    #[tokio::test]
    async fn reopen_unknown_update_and_repeated_close_are_all_defined() {
        let sink = Arc::new(RecordingSink::default());
        let ctx = ctx("defined", Some(sink.clone()));

        // A duplicate `open`: same id, new content. One command per call, both addressed to `dup`.
        for title in ["first", "second"] {
            PaneOpenOp
                .execute(&ctx, json!({"id": "dup", "title": title, "data": a_log()}))
                .await
                .unwrap();
        }
        // An update for an id that was never opened, and two closes of the same id.
        PaneUpdateOp
            .execute(&ctx, json!({"id": "ghost", "data": a_log()}))
            .await
            .unwrap();
        for _ in 0..2 {
            PaneCloseOp
                .execute(&ctx, json!({"id": "dup"}))
                .await
                .unwrap();
        }

        let seen = sink.seen();
        assert_eq!(seen.len(), 5, "{seen:?}");
        assert!(matches!(&seen[1], PaneCommand::Open(spec) if spec.id == "dup"));
        assert_eq!(
            seen[2],
            PaneCommand::Update {
                id: "ghost".into(),
                data: PaneData::Log {
                    lines: vec!["one".into(), "two".into()]
                }
            },
            "an unknown id is still addressed as asked, not swallowed"
        );
        assert_eq!(
            seen[3], seen[4],
            "a repeated close is the same command twice"
        );

        // …and each op says the rule out loud, because the model cannot observe the outcome.
        assert!(PaneOpenOp.spec().description.contains("replaces that pane"));
        assert!(PaneUpdateOp
            .spec()
            .description
            .contains("is dropped by the surface"));
        assert!(PaneCloseOp.spec().description.contains("is not an error"));
    }

    #[tokio::test]
    async fn a_malformed_call_is_a_repairable_error_and_reaches_no_sink() {
        let sink = Arc::new(RecordingSink::default());
        let ctx = ctx("malformed", Some(sink.clone()));
        let cases: Vec<(Value, &str)> = vec![
            (
                json!({"id": "  ", "title": "T", "data": a_log()}),
                "must not be blank",
            ),
            (
                // The surface's own panes (C-224's fleet view is `host:fleet`) are not addressable.
                json!({"id": "host:fleet", "title": "T", "data": a_log()}),
                "belong to the surface's own panes",
            ),
            (
                json!({"id": "p", "title": "T", "slot": "middle", "data": a_log()}),
                "unknown `slot`",
            ),
            (
                json!({"id": "p", "title": "T", "kind": "rows", "data": a_log()}),
                "does not match `data`",
            ),
            (
                json!({"id": "p", "title": "T", "data": {"log": {"lines": "not a list"}}}),
                "`data` must be one of",
            ),
            (
                // C-220 keeps `project` in the vocabulary and refuses it at the reporter; the op
                // forwards the reason instead of pretending the value is a typo.
                json!({"id": "p", "title": "T", "lifetime": "project", "data": a_log()}),
                "lifetime 'project' is not supported yet",
            ),
        ];
        for (params, expected) in cases {
            let error = PaneOpenOp
                .execute(&ctx, params.clone())
                .await
                .expect_err(&format!("{params} must be refused"));
            assert!(
                error.to_string().contains(expected),
                "expected `{expected}` in `{error}` for {params}"
            );
        }
        assert!(
            sink.seen().is_empty(),
            "a refused call still reached the surface: {:?}",
            sink.seen()
        );
    }

    /// Every documented `data` shape deserializes into the L2 [`PaneData`] the op forwards — the
    /// guard on the hand-written schema in [`pane_data_schema`].
    #[test]
    fn every_documented_data_shape_round_trips_into_the_contract_type() {
        let shapes = [
            json!({"rows": {"header": ["a"], "rows": [["1"]]}}),
            json!({"kv": {"pairs": [["k", "v"]]}}),
            json!({"log": {"lines": ["l"]}}),
            json!({"progress": {"label": "p", "done": 1, "total": 2}}),
            json!({"tree": {"roots": [{"label": "r", "children": [{"label": "c"}]}]}}),
            json!({"markdown": {"text": "# t"}}),
        ];
        let schema = pane_data_schema();
        let documented = schema["oneOf"].as_array().expect("a oneOf of shapes");
        assert_eq!(
            documented.len(),
            shapes.len(),
            "the schema documents {} shapes but {} are exercised here",
            documented.len(),
            shapes.len()
        );
        for shape in shapes {
            let key = shape.as_object().unwrap().keys().next().unwrap().clone();
            let parsed: PaneData = serde_json::from_value(shape.clone())
                .unwrap_or_else(|e| panic!("`{shape}` is not a PaneData: {e}"));
            assert!(
                documented
                    .iter()
                    .any(|variant| variant["properties"].get(&key).is_some()),
                "`{key}` round-trips but the schema does not document it"
            );
            let _ = parsed.kind();
        }
    }

    /// AGENTS.md: `permission_subjects` must be accurate — a policy can scope a pane by name, and
    /// no op reports an empty subject list for a well-formed call.
    #[test]
    fn every_pane_op_names_the_pane_it_addresses() {
        let calls: Vec<(Arc<dyn Tool>, Value)> = vec![
            (
                Arc::new(PaneOpenOp),
                json!({"id": "build", "title": "T", "data": a_log()}),
            ),
            (
                Arc::new(PaneUpdateOp),
                json!({"id": "build", "data": a_log()}),
            ),
            (Arc::new(PaneCloseOp), json!({"id": "build"})),
        ];
        for (tool, params) in calls {
            assert_eq!(
                tool.permission_subjects(&params),
                vec!["build".to_string()],
                "`{}` does not name the pane it addresses",
                tool.spec().name
            );
        }
    }

    /// The declaration gate C-191/C-208 apply to the production catalog, run over this pack at
    /// build time: coherent metadata (I1/I2/I3, semantic effects included) and a typed authority
    /// contract that resolves for the least-specific call.
    #[test]
    fn the_pane_pack_declares_coherent_metadata_and_authority() {
        let registry = catalog(true);
        for op in PANE_OPS {
            let tool = registry.get(op).expect("the pane op is registered");
            let spec = tool.spec();
            assert!(
                spec.effects.is_empty() && spec.access.is_empty(),
                "`{op}` declares a host resource it does not reach: {spec:?}"
            );
            assert_eq!(
                tool.semantic_effects(),
                vec!["human_visible".to_string()],
                "`{op}` no longer declares the one thing it does"
            );
            let violations = flux_spec::metadata_violations(&spec, &tool.semantic_effects());
            assert!(violations.is_empty(), "`{op}`: {violations:?}");
        }
        registry
            .validate_authority_contracts()
            .expect("the pane ops declare a coherent typed authority contract");
    }

    /// The envelope, end to end: an op surfaced by sink presence is dispatchable through
    /// `Executor::dispatch` — the only path a tool ever runs on — and its command lands on the
    /// host's sink.
    #[tokio::test]
    async fn a_surfaced_pane_op_dispatches_through_the_executor() {
        let sink = Arc::new(RecordingSink::default());
        let executor = Executor::new(
            catalog(true),
            PermissionManager::from_rules(&["pane.open".into()], &[]),
            Arc::new(AllowApprover),
            ctx("dispatch", Some(sink.clone())),
        );
        let result = executor
            .dispatch(
                "pane.open",
                json!({"id": "findings", "title": "Findings", "data": a_log()}),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(result.content, "pane 'findings' open");
        assert_eq!(sink.seen().len(), 1, "{:?}", sink.seen());
    }
}
