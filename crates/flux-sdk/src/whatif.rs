//! Counterfactual session comparison (D-174 slice) plus D-176's Tune: `Session::what_if()`'s
//! world-pinned re-plan and `Client::what_if_over`'s corpus sweep.
//!
//! A [`Counterfactual`] is a diverged branch of a recorded session — [`Session::fork`] +
//! [`Fork::inject`]/[`Fork::edit`] (the Test Kit's fault-injection door, `Scenario::inject_at`), or
//! [`WhatIf::run`]'s world-pinned re-run. Both doors share the same comparison surface (`diff`,
//! `first_divergence`, …); `hermetic`/`cost` are D-176 additions meaningful for either.
//!
//! [`WhatIf`] re-runs a recorded session under **exactly one changed variable** — a substituted tool
//! output (pure, offline, no model call), or a different model/instruction set (a genuine re-plan) —
//! with the rest of the recorded world byte-frozen via a [`flux_flow::cassette::FrozenTape`]. Two
//! drivers do the re-execution: [`flux_flow::whatif::rerun_pinned`] (the pure-substitution path — it
//! never dispatches the model, by construction) and [`flux_flow::whatif::replay_turns_prefix`] +
//! [`flux_flow::engine::FlowEngine::run_turn_pinned`] (the re-plan path — earlier turns rebuild
//! hermetically, then exactly one live turn runs under the pinned scope). **Honesty is a hard
//! requirement**: [`Counterfactual::hermetic`] is `false` the moment the pinned world is left (a
//! substitution that shifts a downstream input, or a re-plan that reads different history), and
//! [`Counterfactual::first_divergence`] localizes it — never a faked complete diff.

use std::sync::Arc;

use flux_core::{PricingTable, Result};
use flux_events::{DiffRow, ModelCost, RunDiff, SessionLog};
use flux_flow::cassette::{CassetteScope, RecordScope};
use flux_flow::host::OpOutcome;
use flux_flow::AgentSink;

use crate::assembly::VariantOverrides;
use crate::session::{Fork, Session};
use crate::Client;

/// How a [`WhatIf`] dispatch that misses the pinned world behaves: `Halt` (hermetic — the frozen
/// world IS the whole world, [`WhatIf::off_tape`]'s default) or `Live` (bridge to a real dispatch,
/// recorded into the counterfactual session's own tail — a re-plan may legitimately read past the
/// frozen prefix). Re-exported from `flux-flow`'s cassette-scope family (D-175).
pub use flux_flow::cassette::OffTape;

/// A no-op [`AgentSink`] — every `WhatIf`/`Client::what_if_over` door reads its result back from the
/// event store afterward rather than observing the turn live.
struct NullSink;
impl AgentSink for NullSink {}

/// A diverged branch of a recorded [`Session`], produced by [`Session::fork`] +
/// [`Fork::inject`]/[`Fork::edit`] (the Test Kit's fault-injection door) — or, from D-176, by
/// [`WhatIf::run`]. Compares against the `original` it diverged from.
pub struct Counterfactual {
    original: Session,
    outcome: Session,
    /// `true` iff this counterfactual never left the pinned/recorded world — always `false` for a
    /// fork's diverged tail (which runs live through the real envelope by construction), computed
    /// honestly for a `WhatIf::run()` result from the pinned scope's own `is_hermetic()`.
    hermetic: bool,
}

impl Counterfactual {
    /// Wrap an already-diverged [`Fork`] together with the [`Session`] it diverged from.
    ///
    /// The default (no `test-kit`) build has no caller — `crate::test::Scenario::inject_at` is the
    /// only one, feature-gated in `lib.rs`.
    #[allow(dead_code)]
    pub(crate) fn from_fork(original: Session, fork: Fork) -> Self {
        let outcome = fork.session();
        Self {
            original,
            outcome,
            // The fork's diverged tail always runs live through the real envelope — never hermetic.
            hermetic: false,
        }
    }

    /// Wrap two independently-built sessions (D-176's `WhatIf::run()`): `original` unchanged,
    /// `outcome` the world-pinned rerun, `hermetic` the pinned scope's own honest verdict.
    pub(crate) fn from_sessions(original: Session, outcome: Session, hermetic: bool) -> Self {
        Self {
            original,
            outcome,
            hermetic,
        }
    }

    /// The diverged branch as an ordinary [`Session`] — itself replayable, forkable, and readable
    /// (`history`/`turns`/`run_trace`/`cost`), since it is a real, recorded session.
    pub fn session(&self) -> Session {
        self.outcome.clone()
    }

    /// The aligned per-statement divergence against the original session
    /// ([`flux_events::run_diff`] over the two run traces).
    pub fn diff(&self) -> Result<RunDiff> {
        let a = self.original.run_trace()?;
        let b = self.outcome.run_trace()?;
        Ok(flux_events::run_diff(&a, &b))
    }

    /// Whether this counterfactual stayed entirely on the pinned/recorded world: no live IO, no
    /// latched divergence, no policy denial. `false` the instant the world is left — a pure
    /// `.substitute()` run is `true` by construction (no model call, ever); a `.model()`/
    /// `.instructions()` re-plan is `true` only if the new plan happened to read nothing the
    /// recording didn't already cover.
    pub fn hermetic(&self) -> bool {
        self.hermetic
    }

    /// The counterfactual session's priced cost — `$0` for a pure substitution (no model call was
    /// ever made), the real re-plan spend otherwise.
    pub fn cost(&self, pricing: &PricingTable) -> Result<Vec<ModelCost>> {
        self.outcome.cost(pricing)
    }

    /// The first point the counterfactual diverges from the original, or `None` if the two runs are
    /// identical. `DivergenceKind::Plan` means the statement content itself differs;
    /// `DivergenceKind::Output` means the same statement hit a different recorded/live world.
    ///
    /// # Panics
    /// If the underlying run traces can't be read back (a store error) — mirrors the panicking style
    /// of every other Test Kit assertion, which render a diagnostic rather than propagate a `Result`
    /// through `cargo test`'s ordinary failure path.
    pub fn first_divergence(&self) -> Option<Divergence> {
        let diff = self.diff().expect("counterfactual diff");
        diff.rows.into_iter().find_map(|row| match row {
            DiffRow::Same { .. } => None,
            DiffRow::Plan {
                node,
                a_stmt,
                b_stmt,
            } => Some(Divergence {
                node,
                kind: DivergenceKind::Plan,
                detail: format!(
                    "plan diverges: {} vs {}",
                    a_stmt.as_deref().unwrap_or("∅"),
                    b_stmt.as_deref().unwrap_or("∅")
                ),
            }),
            DiffRow::Output { node, op, a, b, .. } => Some(Divergence {
                node,
                kind: DivergenceKind::Output,
                detail: format!("op `{op}` recorded a different output: {a:?} vs {b:?}"),
            }),
        })
    }

    /// Assert the counterfactual's first divergence is at exactly `node`.
    ///
    /// # Panics
    /// If the first divergence is at a different node, or there is none at all.
    pub fn assert_diverges_at(&self, node: u32) {
        match self.first_divergence() {
            Some(d) if d.node == node => {}
            Some(d) => panic!(
                "expected the first divergence at node {node}, but it was at node {} ({:?}: {})",
                d.node, d.kind, d.detail
            ),
            None => panic!(
                "expected a divergence at node {node}, but the counterfactual session is \
                 identical to the original"
            ),
        }
    }

    /// Assert the counterfactual's diverged tail called `op` (a saga-compensation check: "injecting
    /// this failure must trigger the agent's own rollback/compensating op").
    ///
    /// # Panics
    /// If `op` never appears in the counterfactual session's recorded run trace, or the trace can't
    /// be read back.
    pub fn assert_compensated_with(&self, op: &str) {
        let trace = self
            .session()
            .run_trace()
            .expect("counterfactual session run trace");
        let called = trace.iter().any(|ev| {
            matches!(ev, flux_lang::ast::RunEvent::OpRecorded { op: recorded, .. } if recorded == op)
        });
        assert!(
            called,
            "expected compensating op `{op}` to run in the counterfactual's diverged tail, but \
             it never did (ops called: {:?})",
            trace
                .iter()
                .filter_map(|ev| match ev {
                    flux_lang::ast::RunEvent::OpRecorded { op, .. } => Some(op.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );
    }
}

/// One point where a [`Counterfactual`] first differs from the session it diverged from.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Divergence {
    /// The statement's node index in the executed plan.
    pub node: u32,
    /// Whether the plan itself changed, or the same plan hit a different world.
    pub kind: DivergenceKind,
    /// A human-readable description of the divergence.
    pub detail: String,
}

/// Which of the two `run_diff` divergence shapes a [`Divergence`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DivergenceKind {
    /// The statement content itself differs from the original at this position.
    Plan,
    /// The same statement recorded a different op output (a different world).
    Output,
}

// --- D-176: Session::what_if -------------------------------------------------

/// Turn a caller-supplied substitution `Value` into the [`OpOutcome`] a [`FrozenTape`] serves: a
/// JSON string substitutes as its bare text (matching how a real op's `content` is plain text, not
/// a quoted JSON string); any other JSON shape substitutes as its canonical JSON text.
fn substitution_outcome(value: &serde_json::Value) -> OpOutcome {
    let content = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    OpOutcome::ok(content)
}

/// Map a target statement `node` to its FIRST cassette cell's index in the trace's flat
/// `OpRecorded` sequence — the same order [`flux_flow::cassette::Cell::collect`] flattens into (and
/// [`FrozenTape::substitute_cell`] indexes by), since [`flux_events::stmt_rows`] visits rows and
/// their cells in that same stream order. `None` if `node` has no statement, or that statement made
/// no dispatch at all.
fn node_to_cell_index(trace: &[flux_lang::ast::RunEvent], node: u32) -> Option<usize> {
    let mut index = 0usize;
    for row in flux_events::stmt_rows(trace) {
        if row.node == node {
            return if row.cells.is_empty() {
                None
            } else {
                Some(index)
            };
        }
        index += row.cells.len();
    }
    None
}

/// C-254: the one door to a counterfactual session id.
///
/// [`WhatIf::run`] has two paths and each mints exactly one throwaway session. That mint is the first
/// trace a what-if leaves, and `WhatIf` exists to answer "what would happen" **without** it
/// happening — so a refusal that fires after the mint has already broken the feature's single
/// promise, and broken it *silently*: nothing downstream distinguishes a minted-then-refused run from
/// a run that never minted at all. C-211 and C-247 closed four such sites by moving statements above
/// the mint. That fix is correct the day it lands and evaporates the moment someone reorders the
/// function, so this module makes the ordering a property of the **types** instead.
///
/// The shape: each path's entire refusal surface is discharged by its `resolve` constructor, which
/// reads only the SOURCE session; `mint` is a method on the resolved value; and the
/// `create_session_with_context` call itself is private to this module. Reaching a `dst` therefore
/// requires holding a fully-resolved path first — a `Cleared*` value cannot be forged from the parent
/// module, its fields being private to this one — and hoisting the mint back above a validation
/// simply does not compile, because the mint's receiver is what that validation produces.
///
/// The one thing types cannot forbid is `run` calling `variant.events.create_session_with_context`
/// directly and bypassing the gate. That would be a conspicuous new statement in review, and the
/// three C-254 tests in `tests/whatif.rs` fail the moment it appears — they assert the *absence of a
/// trace* (session count unchanged across a refusal), not merely that an error came back.
mod mint_gate {
    use flux_core::Result;
    use flux_events::{EventContext, EventStore, ValidHistory};
    use flux_flow::cassette::{FrozenTape, OffTape, RecordScope, ReplayTape};
    use flux_flow::engine::FlowEngine;
    use flux_flow::host::OpOutcome;
    use flux_flow::whatif::RerunSelection;

    use super::{node_to_cell_index, substitution_outcome};

    /// Every `.substitute`/`.substitute_at` resolved against the source trace — the fallible half of
    /// building the pinned [`FrozenTape`], split out so it happens before anything is minted.
    ///
    /// D-182/D-184's refusal lives here: a `.substitute_at(node, _)` whose target maps to no recorded
    /// dispatch at all (`node_to_cell_index` → `None` — a typo'd node id, a node with no statement,
    /// or a statement that made no dispatch) errors naming the node. It used to be a silent no-op:
    /// the substitution was simply dropped, and the caller could not tell "targeted node, no change"
    /// apart from "node doesn't exist" — both looked like an honest identical run. Once it did error,
    /// it errored *after* the mint on **both** of `run`'s paths, which is the pair of sites C-254
    /// closes.
    pub(super) struct ResolvedSubstitutions {
        ops: Vec<(String, OpOutcome)>,
        cells: Vec<(usize, OpOutcome)>,
    }

    impl ResolvedSubstitutions {
        fn resolve(
            trace: &[flux_lang::ast::RunEvent],
            substitute_ops: &[(String, serde_json::Value)],
            substitute_nodes: &[(u32, serde_json::Value)],
        ) -> Result<Self> {
            let ops = substitute_ops
                .iter()
                .map(|(op, value)| (op.clone(), substitution_outcome(value)))
                .collect();
            let mut cells = Vec::with_capacity(substitute_nodes.len());
            for (node, value) in substitute_nodes {
                let index = node_to_cell_index(trace, *node).ok_or_else(|| {
                    flux_core::Error::Other(format!(
                        "substitute_at({node}, _) targets a node with no recorded dispatch — it has \
                         no statement, or that statement never called an op, so there is no cell to \
                         substitute"
                    ))
                })?;
                cells.push((index, substitution_outcome(value)));
            }
            Ok(Self { ops, cells })
        }

        /// The [`FrozenTape`] a `WhatIf::run()` pins: the source's recorded `trace`, every resolved
        /// substitution applied, off-tape mode `off_tape`.
        ///
        /// **Infallible by construction** — every index was resolved by [`Self::resolve`], so nothing
        /// is left here that can say "no". That is the whole reason tape construction is split from
        /// resolution at all: `OffTape::Live` needs `bridge`, a [`RecordScope`] pointed at the
        /// destination session, which cannot exist until `dst` does. Resolution therefore runs before
        /// the mint and construction after it, with no refusal on the far side.
        pub(super) fn freeze(
            self,
            trace: &[flux_lang::ast::RunEvent],
            off_tape: OffTape,
            bridge: Option<RecordScope>,
            reauthorize: bool,
        ) -> FrozenTape {
            let tape = ReplayTape::from_trace(trace);
            let mut frozen = match off_tape {
                OffTape::Halt => FrozenTape::hermetic(tape),
                OffTape::Live => FrozenTape::live_bridge(
                    tape,
                    bridge.expect("OffTape::Live always supplies a bridge"),
                ),
            };
            frozen = frozen.with_reauthorize(reauthorize);
            for (op, outcome) in self.ops {
                frozen = frozen.substitute_op(op, outcome);
            }
            for (index, outcome) in self.cells {
                frozen = frozen.substitute_cell(index, outcome);
            }
            frozen
        }
    }

    /// Mint the throwaway counterfactual session on `variant`, correlated back to the source session
    /// `src` and labelled with the targeted turn.
    ///
    /// Private to this module: the only way to a `dst` is a `Cleared*` value's `mint`, and the only
    /// way to one of those is its fallible `resolve`.
    fn mint(variant: &FlowEngine, src: &str, label: &str) -> Result<String> {
        variant.events.create_session_with_context(
            &variant.model,
            &EventContext {
                correlation_id: Some(src.to_string()),
                agent_id: Some(format!("what_if:{src}@{label}")),
                ..Default::default()
            },
        )
    }

    /// The pure-substitution (`!replan`) path, every refusal discharged: the `.substitute_at`
    /// targets, and [`rerun_pinned`](flux_flow::whatif::rerun_pinned)'s own execution selection — the
    /// refusal that lives one crate down and so could not possibly fire before the mint until
    /// `flux-flow` grew a seam for it. Both read only the source session.
    pub(super) struct ClearedSubstitution {
        subs: ResolvedSubstitutions,
        selection: RerunSelection,
    }

    impl ClearedSubstitution {
        pub(super) fn resolve(
            events: &EventStore,
            src: &str,
            turn: Option<usize>,
            trace: &[flux_lang::ast::RunEvent],
            substitute_ops: &[(String, serde_json::Value)],
            substitute_nodes: &[(u32, serde_json::Value)],
        ) -> Result<Self> {
            let subs = ResolvedSubstitutions::resolve(trace, substitute_ops, substitute_nodes)?;
            let selection = RerunSelection::resolve(events, src, turn)
                .map_err(|e| flux_core::Error::Other(e.to_string()))?;
            Ok(Self { subs, selection })
        }

        pub(super) fn mint(&self, variant: &FlowEngine, src: &str, label: &str) -> Result<String> {
            mint(variant, src, label)
        }

        pub(super) fn into_parts(self) -> (ResolvedSubstitutions, RerunSelection) {
            (self.subs, self.selection)
        }
    }

    /// The re-plan path, every refusal discharged: the source history's provider-validity and the
    /// target turn's existence (both C-247's), plus the `.substitute_at` targets — C-254's third
    /// site, which C-247 left below its mint because its Acceptance enumerated the other two. All
    /// three read only the source session.
    pub(super) struct ClearedReplan {
        subs: ResolvedSubstitutions,
        history: ValidHistory,
        target_turn: usize,
        user_input: String,
    }

    impl ClearedReplan {
        pub(super) fn resolve(
            events: &EventStore,
            src: &str,
            turn: Option<usize>,
            trace: &[flux_lang::ast::RunEvent],
            substitute_ops: &[(String, serde_json::Value)],
            substitute_nodes: &[(u32, serde_json::Value)],
        ) -> Result<Self> {
            let subs = ResolvedSubstitutions::resolve(trace, substitute_ops, substitute_nodes)?;
            let history = ValidHistory::new(events.conversation(src)?)?;
            let turns = events.turns(src)?;
            let target_turn = turn.unwrap_or(turns.len().max(1));
            let user_input = turns
                .get(target_turn.saturating_sub(1))
                .map(|t| t.user_input.clone())
                .ok_or_else(|| {
                    flux_core::Error::Other(format!(
                        "session {src} has no turn {target_turn} to re-plan"
                    ))
                })?;
            Ok(Self {
                subs,
                history,
                target_turn,
                user_input,
            })
        }

        pub(super) fn mint(&self, variant: &FlowEngine, src: &str, label: &str) -> Result<String> {
            mint(variant, src, label)
        }

        pub(super) fn into_parts(self) -> (ResolvedSubstitutions, ValidHistory, usize, String) {
            (self.subs, self.history, self.target_turn, self.user_input)
        }
    }
}

impl Session {
    /// Re-run this recorded session under **exactly one changed variable** — a substituted tool
    /// output, or a different model/agent instructions — with the rest of the recorded world
    /// byte-frozen. See [`WhatIf`].
    pub fn what_if(&self) -> WhatIf {
        WhatIf {
            session: self.clone(),
            turn: None,
            model: None,
            instructions: None,
            substitute_ops: Vec::new(),
            substitute_nodes: Vec::new(),
            off_tape: OffTape::Halt,
            permissions: None,
        }
    }
}

/// A builder for a world-pinned counterfactual re-run of a [`Session`] — see [`Session::what_if`]
/// and the [module docs](self). Reusable across a whole corpus of sessions as a [`WhatIfSpec`] (via
/// [`WhatIf::spec`]/[`WhatIf::from_spec`]) for [`Client::what_if_over`].
pub struct WhatIf {
    session: Session,
    turn: Option<usize>,
    model: Option<String>,
    instructions: Option<String>,
    substitute_ops: Vec<(String, serde_json::Value)>,
    substitute_nodes: Vec<(u32, serde_json::Value)>,
    off_tape: OffTape,
    permissions: Option<flux_agent::Permissions>,
}

impl WhatIf {
    /// Narrow the rerun to one 1-based turn (`None`, the default, reruns the whole session).
    pub fn turn(mut self, n: usize) -> Self {
        self.turn = Some(n);
        self
    }

    /// Re-plan under a different model — this session's other turns are unaffected; only the
    /// counterfactual re-run uses it.
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = Some(m.into());
        self
    }

    /// Re-plan under different caller-authored instructions; the Flux harness prefix remains.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// **Policy mode** (D-177): re-run the recorded plan against the frozen world, but re-decide
    /// every dispatch under `permissions` instead of the ones the recording ran with — the "would
    /// the tightened policy have blocked the destructive action?" gate.
    ///
    /// The rules replace the original's wholesale rather than merging with them: the question is
    /// about the policy AS GIVEN, and quietly unioning the recording's own allows would make a
    /// stricter policy impossible to test.
    ///
    /// This is not a re-plan — the model is never called, exactly as for a pure
    /// [`substitute`](Self::substitute). What changes is that the frozen world no longer answers
    /// unconditionally: each dispatch is first put through the authorize-only gates
    /// ([`flux_runtime::Executor::authorize`] — a decision, never an execution), and a refusal
    /// surfaces as a real denial in the counterfactual's trace instead of being masked by the taped
    /// output. A denial also makes [`Counterfactual::hermetic`] `false`: the run left the recorded
    /// world, which is exactly what the caller wanted to find out.
    pub fn policy(mut self, permissions: flux_agent::Permissions) -> Self {
        self.permissions = Some(permissions);
        self
    }

    /// Swap the recorded outcome of every call to `op` (pure, offline — no model call, ever, when
    /// this is the only variable changed).
    pub fn substitute(mut self, op: &str, output: serde_json::Value) -> Self {
        self.substitute_ops.push((op.to_string(), output));
        self
    }

    /// Swap the recorded outcome of the specific statement bound at `node` (wins over
    /// [`substitute`](Self::substitute) for that one call site).
    pub fn substitute_at(mut self, node: u32, output: serde_json::Value) -> Self {
        self.substitute_nodes.push((node, output));
        self
    }

    /// How a dispatch that misses the pinned world behaves: `Halt` (the default — the frozen world
    /// is the whole world, hermetic by construction) or `Live` (bridge to a real dispatch, recorded
    /// into the counterfactual session's own tail).
    pub fn off_tape(mut self, mode: OffTape) -> Self {
        self.off_tape = mode;
        self
    }

    /// This builder's state as a reusable, cloneable [`WhatIfSpec`] — the shape
    /// [`Client::what_if_over`] applies across a whole corpus of sessions.
    pub fn spec(&self) -> WhatIfSpec {
        WhatIfSpec {
            turn: self.turn,
            model: self.model.clone(),
            instructions: self.instructions.clone(),
            substitute_ops: self.substitute_ops.clone(),
            substitute_nodes: self.substitute_nodes.clone(),
            off_tape: self.off_tape,
            permissions: self.permissions.clone(),
        }
    }

    /// Rebuild a `WhatIf` targeting `session` from a previously captured [`WhatIfSpec`].
    pub fn from_spec(session: &Session, spec: WhatIfSpec) -> Self {
        WhatIf {
            session: session.clone(),
            turn: spec.turn,
            model: spec.model,
            instructions: spec.instructions,
            substitute_ops: spec.substitute_ops,
            substitute_nodes: spec.substitute_nodes,
            off_tape: spec.off_tape,
            permissions: spec.permissions,
        }
    }

    /// Run the counterfactual: a pure `.substitute()`/`.substitute_at()` change re-executes the
    /// recorded plan(s) directly under the pinned world — **no model call, ever, by construction**;
    /// a `.model()`/`.instructions()` change hermetically rebuilds every turn before the target,
    /// then drives exactly one LIVE turn (the re-plan) under the pinned scope.
    ///
    /// Runs on a throwaway variant engine sharing this client's event log (so the counterfactual
    /// session is a real, correlated session visible to the same client) but a fresh in-memory flow
    /// store, so it never perturbs this session's own suspended-flow state.
    pub async fn run(self) -> Result<Counterfactual> {
        let src = self.session.id.clone();
        let events = self.session.engine.events.clone();
        let trace = events.run_trace(&src)?;
        if trace.is_empty() {
            return Err(flux_core::Error::Other(format!(
                "session {src} has no run trace recorded — nothing to run a what-if over"
            )));
        }

        let replan = self.model.is_some() || self.instructions.is_some();
        let overrides = VariantOverrides {
            model: self.model.clone(),
            instructions: self.instructions.clone(),
            permissions: self.permissions.clone(),
            ..Default::default()
        };
        let variant = self.session.assembly.variant(overrides)?;

        let label = self
            .turn
            .map(|t| t.to_string())
            .unwrap_or_else(|| "latest".to_string());

        let mut sink = NullSink;

        if !replan {
            // C-254: everything that can REFUSE this path is discharged here, before anything is
            // minted — the `.substitute_at` targets, and `rerun_pinned`'s own execution selection.
            // Both read only the source session. The ordering is structural rather than incidental:
            // `dst` is reachable only through `cleared.mint`, so no reordering of the statements
            // below can put the mint back in front of a refusal.
            let cleared = mint_gate::ClearedSubstitution::resolve(
                &events,
                &src,
                self.turn,
                &trace,
                &self.substitute_ops,
                &self.substitute_nodes,
            )?;
            let dst = cleared.mint(&variant, &src, &label)?;
            let (subs, selection) = cleared.into_parts();

            let bridge = matches!(self.off_tape, OffTape::Live)
                .then(|| RecordScope::new(variant.events.clone(), dst.clone()));
            let frozen = subs.freeze(&trace, self.off_tape, bridge, self.permissions.is_some());
            let scope = Arc::new(CassetteScope::Frozen(frozen));

            flux_flow::whatif::rerun_pinned(
                &variant.events,
                &variant.flow,
                &variant.executor,
                &selection,
                &dst,
                scope.clone(),
                &mut sink,
            )
            .await
            .map_err(|e| flux_core::Error::Other(e.to_string()))?;

            let hermetic = matches!(scope.as_ref(), CassetteScope::Frozen(f) if f.is_hermetic());
            let outcome = Session {
                engine: Arc::new(variant),
                id: dst,
                assembly: self.session.assembly.clone(),
                turn_guard: Arc::new(tokio::sync::Mutex::new(())),
                // A counterfactual is a throwaway branch, never a crashed production session.
                auto_resurrect: false,
            };
            return Ok(Counterfactual::from_sessions(
                self.session.clone(),
                outcome,
                hermetic,
            ));
        }

        // Re-plan path. Everything that can REFUSE the re-plan is resolved first, before anything is
        // minted (C-247, the same move C-211 made at the fork sites; C-254 for the third refusal):
        // all three used to fire after `dst` existed, so a refused re-plan left an orphan session
        // behind — an artifact that did not exist before the refusal paths did. Holding "a failed
        // operation leaves no trace" is cheaper than re-deriving it from a pruning rule on every read
        // of this path, and the turn refusal did not even have that fallback: it fired after the
        // rewrite below, so its orphan carried a full copy of the parent's conversation and
        // `prune_empty` never saw an empty stream to collect. All three resolutions read only the
        // source session.
        //
        // The pure-substitution path above deliberately checks neither history nor turn: it can never
        // reach a provider, so it has no stake in the source being a valid *provider* history, and
        // `RerunSelection` resolves its turn out of the run trace instead.
        let cleared = mint_gate::ClearedReplan::resolve(
            &events,
            &src,
            self.turn,
            &trace,
            &self.substitute_ops,
            &self.substitute_nodes,
        )?;

        // Now mint: copy the conversation so the re-planned turn sees the same history, rebuild every
        // earlier turn hermetically, then drive exactly one LIVE turn under the pinned scope. The
        // copy is one checked rewrite (A-102) — the variant's live turn goes to a real provider, so a
        // source history that is not a valid provider history must fail above, not there.
        let dst = cleared.mint(&variant, &src, &label)?;
        let (subs, history, target_turn, user_input) = cleared.into_parts();
        SessionLog::open(&variant.events, &dst)?.rewrite(history)?;

        flux_flow::whatif::replay_turns_prefix(
            &variant.events,
            &variant.flow,
            &variant.executor,
            &src,
            &dst,
            target_turn,
            &mut sink,
        )
        .await
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;

        let bridge = RecordScope::new(variant.events.clone(), dst.clone());
        let frozen = subs.freeze(
            &trace,
            self.off_tape,
            Some(bridge),
            self.permissions.is_some(),
        );
        let scope = Arc::new(CassetteScope::Frozen(frozen));

        // D-182: `run_turn_pinned` here re-plans exactly one LIVE turn under `scope` — every
        // dispatch the new plan makes that still matches the frozen world is SERVED, and nothing
        // else ever records a served hit onto `dst` (the cassette layer only ever records a LIVE
        // fall-through's tail). Without self-recording here, a re-plan whose new plan happens to be
        // identical (or served in full) would leave `dst`'s own trace with zero cells, and
        // `Counterfactual::diff()` would read that as a fake total divergence while `hermetic()`
        // reports `true` — exactly the gap `flux_flow::whatif::rerun_pinned` and the SDK's own
        // `Scenario::check` already close with `RerunRecordingSink`. `defer_for_live_bridge` covers
        // `OffTape::Live`: a fall-through dispatch is already recorded by `FrozenTape::record_tail`
        // via `bridge` above, so deferring to `finish()` keeps this sink from double-recording it —
        // see `RerunRecordingSink::defer_for_live_bridge`'s doc for why a real-time check can't do
        // this reliably behind the agent loop's own composite dispatch relay.
        let redactor = variant.executor.context().redactor.clone();
        let record = RecordScope::new(variant.events.clone(), dst.clone());
        let mut rec_sink =
            flux_flow::whatif::RerunRecordingSink::new(&mut sink, record, redactor, true);
        if matches!(self.off_tape, OffTape::Live) {
            rec_sink = rec_sink.defer_for_live_bridge();
        }

        variant
            .run_turn_pinned(&dst, &user_input, scope.clone(), &mut rec_sink)
            .await
            .map_err(|e| flux_core::Error::Other(e.to_string()))?;
        rec_sink.finish();

        let hermetic = matches!(scope.as_ref(), CassetteScope::Frozen(f) if f.is_hermetic());
        let outcome = Session {
            engine: Arc::new(variant),
            id: dst,
            assembly: self.session.assembly.clone(),
            turn_guard: Arc::new(tokio::sync::Mutex::new(())),
            // A counterfactual is a throwaway branch, never a crashed production session.
            auto_resurrect: false,
        };
        Ok(Counterfactual::from_sessions(
            self.session.clone(),
            outcome,
            hermetic,
        ))
    }
}

/// A reusable, cloneable capture of a [`WhatIf`] builder's state — what [`Client::what_if_over`]
/// applies identically across a whole corpus of sessions. Build one via [`WhatIf::spec`], or
/// construct it directly (`#[non_exhaustive]`: every new `WhatIf` knob lands here too).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WhatIfSpec {
    /// Narrow the rerun to one 1-based turn (`None` reruns the whole session).
    pub turn: Option<usize>,
    /// Re-plan under a different model.
    pub model: Option<String>,
    /// Re-plan under different caller-authored instructions.
    pub instructions: Option<String>,
    /// `(op, output)` substitutions — see [`WhatIf::substitute`].
    pub substitute_ops: Vec<(String, serde_json::Value)>,
    /// `(node, output)` substitutions — see [`WhatIf::substitute_at`].
    pub substitute_nodes: Vec<(u32, serde_json::Value)>,
    /// How an off-tape miss behaves.
    pub off_tape: OffTape,
    /// Re-decide every dispatch under these permission rules — see [`WhatIf::policy`].
    pub permissions: Option<flux_agent::Permissions>,
}

impl Default for WhatIfSpec {
    fn default() -> Self {
        Self {
            turn: None,
            model: None,
            instructions: None,
            substitute_ops: Vec::new(),
            substitute_nodes: Vec::new(),
            off_tape: OffTape::Halt,
            permissions: None,
        }
    }
}

/// One session's [`WhatIf`] outcome within a [`Client::what_if_over`] sweep — a failing session
/// lands here as an `Err` row (its id plus a diagnostic), never aborting the whole sweep.
#[non_exhaustive]
pub struct SweepOutcome {
    /// The session id this row's counterfactual (or error) is for.
    pub session: String,
    /// The counterfactual, or a diagnostic if this session couldn't be opened or re-run.
    pub result: std::result::Result<Counterfactual, String>,
}

/// The result of [`Client::what_if_over`]: one [`SweepOutcome`] per session, plus the roll-up a
/// regression gate reads directly — how many sessions changed, out of how many, and the sweep's
/// total offline spend (the re-plan rows' real cost; pure-substitution rows cost `$0`).
#[non_exhaustive]
pub struct SweepReport {
    /// One row per session swept, in the order given to [`Client::what_if_over`].
    pub outcomes: Vec<SweepOutcome>,
    /// How many sessions' counterfactual diverged from the original at all (`diff().identical ==
    /// false`, or the session errored — an error is conservatively counted as "changed").
    pub changed: usize,
    /// How many sessions were swept.
    pub total: usize,
    /// The sweep's total priced spend, [`PricingTable::builtin`] applied to every successful row's
    /// counterfactual session.
    pub offline_cost: f64,
}

impl Client {
    /// Run [`Session::what_if`] identically over every session in `sessions`, per `change` — the
    /// corpus-wide regression gate: "under this one changed variable, which of my recorded runs
    /// diverge, and what would it cost?" A session that fails to open or re-run lands as an `Err`
    /// row in the report rather than aborting the whole sweep.
    pub async fn what_if_over(
        &self,
        sessions: impl IntoIterator<Item = String>,
        change: WhatIfSpec,
    ) -> Result<SweepReport> {
        let mut outcomes = Vec::new();
        let mut changed = 0usize;
        let mut total = 0usize;
        let mut offline_cost = 0.0;
        for session_id in sessions {
            total += 1;
            let session = match self.open_session(&session_id) {
                Ok(s) => s,
                Err(e) => {
                    changed += 1;
                    outcomes.push(SweepOutcome {
                        session: session_id,
                        result: Err(e.to_string()),
                    });
                    continue;
                }
            };
            let what_if = WhatIf::from_spec(&session, change.clone());
            match what_if.run().await {
                Ok(cf) => {
                    let diverged = cf.diff().map(|d| !d.identical).unwrap_or(true);
                    if diverged {
                        changed += 1;
                    }
                    if let Ok(rows) = cf.cost(&PricingTable::builtin()) {
                        offline_cost += rows
                            .iter()
                            .filter_map(|r| r.cost.map(|m| m.usd))
                            .sum::<f64>();
                    }
                    outcomes.push(SweepOutcome {
                        session: session_id,
                        result: Ok(cf),
                    });
                }
                Err(e) => {
                    changed += 1;
                    outcomes.push(SweepOutcome {
                        session: session_id,
                        result: Err(e.to_string()),
                    });
                }
            }
        }
        Ok(SweepReport {
            outcomes,
            changed,
            total,
            offline_cost,
        })
    }
}
