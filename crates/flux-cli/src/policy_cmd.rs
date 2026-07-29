//! `flux policy simulate <proposed.toml>` (C-131): replay a proposed authorization policy against
//! the recorded op history and report, diff-style, which historical ops it would have newly blocked
//! and newly allowed relative to the active policy.
//!
//! The trust-builder for a policy edit: before adopting one, see what adopting it would have done.
//!
//! # One evaluator, no second implementation
//! Both policies are run through [`flux_policy::evaluate`] — the same function `Executor::gate`
//! calls — over requirements produced by each op's own [`flux_runtime::Tool::authority_requirements`],
//! the same typed contract the dispatcher gates on. Nothing here re-implements policy semantics (no
//! second subject/action/resource matcher, no second grant walk), so a simulated verdict cannot
//! drift from the live one.
//!
//! What is simulated is exactly the **mandatory authorization-policy floor**. The permission rules
//! (`[permissions] allow/deny`), the interactive approval gate, and the capability-scope floor are
//! separate layers with their own state and are deliberately not replayed — a policy simulation
//! that folded them in would answer a different question than the one it is named after.
//!
//! # Purity
//! Every path here is a read: the event store is opened exactly as `flux export`/`flux diff` open
//! it and only queried ([`flux_events::EventStore::list`] / `observations`); no event is ever
//! appended. No provider, agent engine, or executor is constructed — the command takes no model
//! spec and never reaches `provider_for`, so it runs with no credential of any kind available.
//! `run_policy` is deliberately synchronous for that reason: every provider-building path in this
//! binary is `async`.
//!
//! # What the log records, and what it therefore cannot decide
//! A dispatch is recorded as one `tool_call` observation carrying `{tool, subjects, caller}`
//! (`flux_runtime`'s `Executor::dispatch_outcome`) — the op name, its invocation subjects, and the
//! caller's **principal id**. The invocation params are not recorded, and neither are the caller's
//! kind, groups, trust level, or scopes.
//!
//! So a recorded op is decidable only when the verdict follows from that recorded context alone.
//! Everything else is reported as **indeterminate** with a reason, never bucketed as allowed or
//! blocked:
//!
//! - an op with no authority contract in this build (a plugin op, a live datasource op, a browser
//!   op, an op since removed) — its requirements are unknowable here, and the params-driven ones
//!   would additionally depend on input the log never captured;
//! - a record missing the `tool`, `subjects`, or `caller` the evaluation reads;
//! - an op whose declaration does not yield a valid authority contract;
//! - an op whose verdict is **not invariant** over the caller facts the log omits — see
//!   [`CallerFact`]. Rather than assume a trust level or a group membership, the simulator brackets
//!   each omitted fact between its minimum (the recorded caller holds none of it) and its maximum
//!   (everything either policy could possibly ask for) and reports the op indeterminate whenever the
//!   two ends disagree. Deciding it would mean inventing the missing fact, which is precisely the
//!   silent classification this command must not make. An op the policies settle the same way at
//!   both ends stays decided — a trust-gated grant elsewhere in the file does not smear the whole
//!   report into "unknown".
//!
//! # The one declared assumption: the caller's kind
//! The log records a principal **id**, not a principal **kind**, and unlike trust/scopes/groups the
//! kind is not an ordered axis to bracket — `user`, `agent`, and `system` are disjoint, and under an
//! `agent` hypothesis a policy written for `user` subjects denies everything, which would make every
//! report empty rather than useful. So the replay reconstructs each recorded caller as a **`user`**
//! principal: what [`flux_policy::local_identity`] mints for every session the CLI itself records.
//! That assumption is **declared in the output**, not buried here ([`REPLAY_ASSUMPTION`]) — and when
//! either policy contains an `agent` or `system` subject the assumption becomes load-bearing, so
//! every op is reported indeterminate instead.

use super::*;

use std::collections::BTreeSet;
use std::fmt::Write as _;

use flux_policy::{
    evaluate, AuthorizationPolicy, Caller, CallerKind, Decision, Principal,
    Request as PolicyRequest, Scope, SubjectKind, Trust, TrustKind, TrustLevel,
};
use flux_runtime::AuthorityRequirement;

/// A limit large enough that no real store hits it — the same "unbounded" convention
/// `EventStore::search` uses, so `--sessions 0` stays the exact query `list` already serves.
const ALL_SESSIONS: usize = i64::MAX as usize;

/// The evidence kind `Executor::dispatch_outcome` records for every admitted dispatch.
const TOOL_CALL: &str = "tool_call";

/// The one thing the replay assumes rather than reads. Surfaced in both renderings so an operator
/// reading the diff knows the shape of the claim it makes; see the module docs.
const REPLAY_ASSUMPTION: &str =
    "only the mandatory authorization-policy floor is replayed: permission rules, the \
     capability-scope floor, `[tools] disable` and the interactive approval gate are not, so \
     `newly blocked` means \"this policy would have refused it\", not \"it would not have run\"";

/// A second limit, and the one most likely to mislead: the diff is computed over dispatches that
/// were *admitted*, because `Executor::gate` returns before the `tool_call` observation is written
/// when the policy denies. An op the active policy denied outright therefore has no record, so
/// `newly allowed` can only ever surface `approval_required -> allow` transitions, never
/// `deny -> allow`. A `newly allowed 0` is not evidence that a proposal grants nothing new.
const NEWLY_ALLOWED_LIMIT: &str =
    "the log holds only admitted dispatches (a policy denial is never recorded), so `newly \
     allowed` surfaces `approval_required -> allow` transitions only — never `deny -> allow`";

/// The params a historical call is re-evaluated with: none.
///
/// The log records a dispatch's subjects but never its input, so the simulator has no params to
/// supply. Passing `null` is sound only because every op it will decide derives its requirements
/// from the declaration and subjects alone — proven for the whole simulation registry by
/// `simulated_ops_derive_authority_from_recorded_context_only`.
const NO_RECORDED_PARAMS: Value = Value::Null;

/// `flux policy …`
pub(super) fn run_policy(action: PolicyAction) -> Result<()> {
    match action {
        PolicyAction::Simulate {
            proposed,
            sessions,
            json,
        } => run_policy_simulate(&proposed, sessions, json),
    }
}

// ---------------------------------------------------------------------------
// Report shapes
// ---------------------------------------------------------------------------

/// One policy's verdict for one recorded op, ordered **restrictive → permissive** so an op's verdict
/// across several requirements is simply the minimum, a proposed-vs-active comparison is an ordering
/// comparison rather than a table of special cases, and the caller-fact bracket in
/// [`decide`] is a two-point equality check on a monotone axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Deny,
    ApprovalRequired,
    Allow,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Deny => "deny",
            Verdict::ApprovalRequired => "approval_required",
            Verdict::Allow => "allow",
        }
    }
}

/// A recorded op both policies decided, with the requirements the decision was made over.
#[derive(Debug, Clone, serde::Serialize)]
struct DecidedOp {
    session: String,
    op: String,
    caller: String,
    subjects: Vec<String>,
    active: Verdict,
    proposed: Verdict,
    requirements: Vec<RequirementView>,
}

/// One `(action, resource)` pair from the op's authority contract, as evaluated.
#[derive(Debug, Clone, serde::Serialize)]
struct RequirementView {
    action: String,
    resource: flux_policy::ResourceRef,
}

impl RequirementView {
    fn of(requirement: &AuthorityRequirement) -> Self {
        Self {
            action: requirement.action.0.clone(),
            resource: requirement.resource.clone(),
        }
    }

    /// `workspace.read on path src/main.rs` — the human one-liner for a deciding requirement.
    fn render(&self) -> String {
        let resource = &self.resource;
        let target = resource
            .path
            .as_deref()
            .or(resource.name.as_deref())
            .unwrap_or(resource.id.as_str());
        let kind = format!("{:?}", resource.kind).to_lowercase();
        format!("{} on {kind} {target}", self.action)
    }
}

/// A recorded op the log cannot re-evaluate, and why.
#[derive(Debug, Clone, serde::Serialize)]
struct IndeterminateOp {
    session: String,
    op: String,
    reason: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct Counts {
    newly_blocked: usize,
    newly_allowed: usize,
    unchanged: usize,
    indeterminate: usize,
}

/// The whole diff-style report. `v` is the payload version, matching the `--stream-json` convention
/// — a tooling consumer keys off it rather than sniffing shapes.
#[derive(Debug, Clone, serde::Serialize)]
struct SimulationReport {
    v: u32,
    sessions: usize,
    ops: usize,
    active_grants: usize,
    proposed_grants: usize,
    /// What the replay assumed rather than read — see [`REPLAY_ASSUMPTION`].
    replay_assumptions: Vec<String>,
    counts: Counts,
    newly_blocked: Vec<DecidedOp>,
    newly_allowed: Vec<DecidedOp>,
    unchanged: Vec<DecidedOp>,
    indeterminate: Vec<IndeterminateOp>,
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// `flux policy simulate <proposed.toml> [--sessions N] [--json]`.
pub(super) fn run_policy_simulate(proposed_path: &str, sessions: usize, json: bool) -> Result<()> {
    // Both sides are assembled the way `build_agent_with` assembles the live policy — the built-in
    // local floor plus the document's `[policy]` grants — so the diff answers exactly "what changes
    // if I adopt this file", with no separate composition rule for the proposal.
    let cwd = std::env::current_dir().context("resolve the current directory")?;
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    let active = with_local_floor(cfg.policy.clone());
    let proposed = with_local_floor(read_proposed_policy(proposed_path)?);

    let registry = simulation_registry()?;
    let events = open_event_store()?;
    let report = simulate(&events, &active, &proposed, &registry, sessions)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_report(&report, proposed_path));
    }
    Ok(())
}

/// Layer a configuration document's `[policy]` grants onto the built-in local floor — the exact
/// composition `build_agent_with` performs when it assembles the live authorization policy.
fn with_local_floor(extra: Option<AuthorizationPolicy>) -> AuthorizationPolicy {
    let mut policy = flux_policy::default_local_grants();
    if let Some(extra) = extra {
        policy.grants.extend(extra.grants);
    }
    policy
}

/// Parse the proposed policy: a flux **configuration document** — the same shape as
/// `.flux/config.toml` — whose `[policy]` grants are the proposal. Reading it as a config rather
/// than as a bare grant list is what makes the simulation faithful: adoption means dropping this
/// file into place, and adoption composes `[policy]` with the built-in floor. A document with no
/// `[policy]` table proposes the bare floor, which is a legitimate (and loudly visible) proposal.
fn read_proposed_policy(path: &str) -> Result<Option<AuthorizationPolicy>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read proposed policy {path}"))?;
    let cfg = flux_config::parse_source(path, &text)
        .with_context(|| format!("parse proposed policy {path}"))?;
    Ok(cfg.policy)
}

/// The op catalog the simulation resolves authority contracts from: the built-in pack plus the
/// sub-agent `task` op, assembled the same way `build_agent_with` assembles them.
///
/// Deliberately **only** the statically-declared ops. Everything registered dynamically (plugin ops,
/// live datasource ops, browser ops) is left out, so those recorded calls land in `indeterminate`
/// rather than being decided from a contract this process would have to guess at — several of them
/// derive their requirements from invocation params the log never recorded.
/// `simulated_ops_derive_authority_from_recorded_context_only` is the guard that keeps that true.
fn simulation_registry() -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry)?;
    registry.try_register_from("flux-cli sub-agent task operation", Arc::new(TaskTool))?;
    Ok(registry)
}

// ---------------------------------------------------------------------------
// The caller facts the log omits
// ---------------------------------------------------------------------------

/// A caller fact `flux_policy::evaluate` reads and the event log does not record.
///
/// Each names an axis along which `evaluate` is **monotone** towards [`Verdict::Allow`]: more trust
/// lets more grants apply, more held scopes turn escalations into allows, and more group memberships
/// let more `group` subjects match — none of them can ever make a verdict more restrictive, because
/// a policy is default-deny and its grants only ever allow or escalate (`flux_policy::evaluate`).
///
/// Monotonicity is what makes a two-point check sufficient, but only if the two points are the
/// **joint** minimum and the **joint** maximum over all three axes at once. Probing one axis at a
/// time with the others held down is *not* sound: a grant gated on two omitted facts together (say
/// `subjects = [group "ops"]` with `required_trust = "privileged"`) is satisfied by neither
/// single-axis probe, so the op would be reported as confidently decided when the log does not
/// determine it at all. That was a real defect here, and it failed in the direction this command
/// exists to prevent — silently under-reporting a change. The bracket below therefore widens every
/// axis simultaneously; these variants survive only to *name* which fact moved a verdict, never to
/// bound the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallerFact {
    Trust,
    Scopes,
    Groups,
}

impl CallerFact {
    /// The operator-facing name of this axis, composed into an [`Indeterminacy`] reason.
    fn noun(self) -> &'static str {
        match self {
            CallerFact::Trust => "trust level",
            CallerFact::Scopes => "scopes",
            CallerFact::Groups => "group memberships",
        }
    }
}

/// The maxima the two policies make reachable, derived once per run.
///
/// Scopes and groups are unbounded in principle but finite in effect: a scope no grant requires
/// cannot change a decision, and a group no `group` subject names cannot match one. Taking the
/// values verbatim out of the grants is therefore exact — and a wildcard subject id (`"team-*"`)
/// held verbatim as a group still matches its own pattern, so wildcards widen correctly too.
#[derive(Debug, Clone, Default)]
struct FactCeiling {
    scopes: Vec<Scope>,
    groups: Vec<String>,
}

impl FactCeiling {
    fn of(policies: [&AuthorizationPolicy; 2]) -> Self {
        let mut scopes = BTreeSet::new();
        let mut groups = BTreeSet::new();
        for policy in policies {
            for grant in &policy.grants {
                scopes.extend(grant.required_scopes.iter().map(|s| s.0.clone()));
                groups.extend(
                    grant
                        .subjects
                        .iter()
                        .filter(|s| s.kind == SubjectKind::Group)
                        .map(|s| s.id.clone()),
                );
            }
        }
        Self {
            scopes: scopes.into_iter().map(Scope).collect(),
            groups: groups.into_iter().collect(),
        }
    }
}

/// Why an op could not be decided from what the log records.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Indeterminacy {
    /// A verdict moved between the joint minimum and joint maximum of the omitted caller facts.
    CallerFacts(String),
    /// Two principal kinds the record is consistent with disagree about the verdict.
    PrincipalKind,
}

impl Indeterminacy {
    fn reason(&self) -> String {
        match self {
            Indeterminacy::CallerFacts(which) => {
                format!("the verdict depends on {which}, which the log does not record")
            }
            Indeterminacy::PrincipalKind => "the verdict depends on the caller's principal kind, \
                 which this record does not carry (it predates the recorder writing `caller_kind`)"
                .to_string(),
        }
    }
}

/// The caller a recorded principal id replays as at the **joint minimum** of every omitted fact:
/// no trust beyond the floor, no scopes, no groups, at the given principal kind.
fn replay_caller(principal_id: &str, kind: CallerKind) -> (Caller, Trust) {
    (
        Caller {
            principal: Principal {
                id: principal_id.to_string(),
                name: principal_id.to_string(),
                kind,
            },
            groups: Vec::new(),
            source: "event-log".to_string(),
        },
        Trust {
            kind: TrustKind::Invocation,
            level: TrustLevel::Untrusted,
            scopes: Vec::new(),
        },
    )
}

/// The same caller at the **joint maximum**: every omitted fact widened at once. This is the upper
/// end of the bracket, and widening jointly is what makes it sound over the product space.
fn joint_max(base: &(Caller, Trust), ceiling: &FactCeiling) -> (Caller, Trust) {
    let (mut caller, mut trust) = base.clone();
    trust.level = TrustLevel::System;
    trust.scopes.clone_from(&ceiling.scopes);
    caller.groups.clone_from(&ceiling.groups);
    (caller, trust)
}

/// The same caller widened along exactly one axis — used only to attribute a reason once the joint
/// bracket has already established that the op is indeterminate.
fn widened(base: &(Caller, Trust), fact: CallerFact, ceiling: &FactCeiling) -> (Caller, Trust) {
    let (mut caller, mut trust) = base.clone();
    match fact {
        CallerFact::Trust => trust.level = TrustLevel::System,
        CallerFact::Scopes => trust.scopes.clone_from(&ceiling.scopes),
        CallerFact::Groups => caller.groups.clone_from(&ceiling.groups),
    }
    (caller, trust)
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// One policy's verdict for a whole op: the most restrictive verdict across its requirements,
/// mirroring `Executor::gate`, where any `Deny` refuses and any `ApprovalRequired` forces the
/// approval gate. An op with no requirements is unconstrained by the policy floor (`Allow`) — the
/// same conclusion the gate's loop reaches by never entering.
fn verdict(
    policy: &AuthorizationPolicy,
    caller: &Caller,
    trust: &Trust,
    requirements: &[AuthorityRequirement],
) -> Verdict {
    let mut worst = Verdict::Allow;
    for requirement in requirements {
        let request = PolicyRequest {
            caller,
            trust,
            action: &requirement.action,
            resource: &requirement.resource,
        };
        let decided = match evaluate(policy, &request).decision {
            Decision::Deny => Verdict::Deny,
            Decision::ApprovalRequired => Verdict::ApprovalRequired,
            Decision::Allow => Verdict::Allow,
        };
        worst = worst.min(decided);
    }
    worst
}

/// Both policies' verdicts for one op — or why the log does not determine them.
///
/// Two nested brackets, because the log omits two *kinds* of fact:
///
/// 1. **Monotone facts** (trust, scopes, groups). For a fixed principal kind, evaluate at the joint
///    minimum and the joint maximum. `evaluate` is monotone in all three, so if those two agree,
///    every caller in between agrees. Widening jointly — not one axis at a time — is what makes
///    this sound for a grant gated on several omitted facts at once.
/// 2. **Principal kind**, which is categorical, not monotone, so it is *enumerated* rather than
///    bracketed. A record that carries `caller_kind` pins it to one; an older record that does not
///    is consistent with any kind, and `SubjectKind::User` itself discriminates on kind
///    (`flux_policy::subject_matches` requires `CallerKind::User`), so this is load-bearing for
///    every grant in the built-in floor — not just for `agent`/`system` subjects.
///
/// The check is over **both** policies: a fact that moves either side's verdict leaves the *diff*
/// undetermined, so it makes the op indeterminate.
fn decide(
    active: &AuthorizationPolicy,
    proposed: &AuthorizationPolicy,
    principal_id: &str,
    recorded_kind: Option<CallerKind>,
    ceiling: &FactCeiling,
    requirements: &[AuthorityRequirement],
) -> std::result::Result<(Verdict, Verdict), Indeterminacy> {
    let feasible: &[CallerKind] = match recorded_kind {
        Some(CallerKind::User) => &[CallerKind::User],
        Some(CallerKind::Agent) => &[CallerKind::Agent],
        Some(CallerKind::System) => &[CallerKind::System],
        None => &[CallerKind::User, CallerKind::Agent, CallerKind::System],
    };

    let mut settled: Option<(Verdict, Verdict)> = None;
    for kind in feasible {
        let min = replay_caller(principal_id, *kind);
        let max = joint_max(&min, ceiling);
        let at = |(caller, trust): &(Caller, Trust)| {
            (
                verdict(active, caller, trust, requirements),
                verdict(proposed, caller, trust, requirements),
            )
        };
        let (lo, hi) = (at(&min), at(&max));
        if lo != hi {
            return Err(Indeterminacy::CallerFacts(attribute(
                active,
                proposed,
                &min,
                ceiling,
                requirements,
                lo,
            )));
        }
        match settled {
            None => settled = Some(lo),
            Some(prev) if prev != lo => return Err(Indeterminacy::PrincipalKind),
            Some(_) => {}
        }
    }
    Ok(settled.expect("at least one feasible principal kind"))
}

/// Name the omitted fact(s) responsible for an op the joint bracket has already ruled
/// indeterminate. Purely explanatory: it never decides *whether* an op is indeterminate, only how to
/// describe it. A fact that moves the verdict on its own is named; when none does, the cause is a
/// grant gated on several at once and the reason says so rather than blaming an arbitrary axis.
fn attribute(
    active: &AuthorizationPolicy,
    proposed: &AuthorizationPolicy,
    min: &(Caller, Trust),
    ceiling: &FactCeiling,
    requirements: &[AuthorityRequirement],
    floor: (Verdict, Verdict),
) -> String {
    let mut movers = Vec::new();
    for fact in [CallerFact::Trust, CallerFact::Scopes, CallerFact::Groups] {
        let (caller, trust) = widened(min, fact, ceiling);
        let moved = (
            verdict(active, &caller, &trust, requirements),
            verdict(proposed, &caller, &trust, requirements),
        );
        if moved != floor {
            movers.push(fact.noun());
        }
    }
    if movers.is_empty() {
        "a combination of caller facts (trust level, scopes, group memberships) that no single one \
         of them settles alone"
            .to_string()
    } else {
        format!("the caller's {}", movers.join(" and "))
    }
}

/// The recorded facts one `tool_call` observation carries.
struct RecordedCall {
    op: String,
    subjects: Vec<String>,
    caller: String,
    /// `None` when the record predates the recorder writing the caller's principal kind.
    kind: Option<CallerKind>,
}

/// Read one `tool_call` observation, or `None` when the payload is missing or malforms something
/// the evaluation reads.
fn recorded_call(data: &Value) -> Option<RecordedCall> {
    let op = data.get("tool")?.as_str()?.to_string();
    // A non-string subject makes the record unreadable rather than merely shorter: dropping it
    // would silently shrink the requirement set, and fewer requirements is a *more permissive*
    // verdict derived from a malformed record. Refuse the record instead.
    let subjects = data
        .get("subjects")?
        .as_array()?
        .iter()
        .map(|s| s.as_str().map(str::to_string))
        .collect::<Option<Vec<String>>>()?;
    let caller = data.get("caller")?.as_str()?.to_string();
    // Absent on records written before the recorder carried it: `None` means "any kind", which
    // `decide` enumerates rather than assumes.
    let kind = match data.get("caller_kind").and_then(Value::as_str) {
        Some("user") => Some(CallerKind::User),
        Some("agent") => Some(CallerKind::Agent),
        Some("system") => Some(CallerKind::System),
        _ => None,
    };
    Some(RecordedCall {
        op,
        subjects,
        caller,
        kind,
    })
}

/// Replay every recorded op in the `sessions` most recent sessions (`0` = all) against both
/// policies. A pure read of `events`.
fn simulate(
    events: &EventStore,
    active: &AuthorizationPolicy,
    proposed: &AuthorizationPolicy,
    registry: &ToolRegistry,
    sessions: usize,
) -> Result<SimulationReport> {
    let ceiling = FactCeiling::of([active, proposed]);
    let limit = if sessions == 0 {
        ALL_SESSIONS
    } else {
        sessions
    };
    let summaries = events.list(limit).map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut report = SimulationReport {
        v: 1,
        sessions: summaries.len(),
        ops: 0,
        active_grants: active.grants.len(),
        proposed_grants: proposed.grants.len(),
        replay_assumptions: {
            let mut limits = vec![
                REPLAY_ASSUMPTION.to_string(),
                NEWLY_ALLOWED_LIMIT.to_string(),
            ];
            // `sessions` in the report is how many were *replayed*, not how many were asked for.
            // Without this an `--sessions 5` run over a 100-session store reads as complete.
            if sessions != 0 && summaries.len() >= sessions {
                limits.push(format!(
                    "the replay window was bounded to the {sessions} most recent session(s); older \
                     sessions were not read"
                ));
            }
            limits
        },
        counts: Counts::default(),
        newly_blocked: Vec::new(),
        newly_allowed: Vec::new(),
        unchanged: Vec::new(),
        indeterminate: Vec::new(),
    };

    for summary in &summaries {
        let observations = events
            .observations(&summary.id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        for observation in observations.iter().filter(|o| o.kind == TOOL_CALL) {
            report.ops += 1;
            let session = summary.id.clone();
            let mut indeterminate = |op: String, reason: String| {
                report.indeterminate.push(IndeterminateOp {
                    session: session.clone(),
                    op,
                    reason,
                });
            };

            let Some(RecordedCall {
                op,
                subjects,
                caller: caller_id,
                kind,
            }) = recorded_call(&observation.data)
            else {
                let op = observation
                    .data
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("<unnamed>")
                    .to_string();
                indeterminate(
                    op,
                    "the recorded dispatch is missing or malforms the op name, subjects, or caller \
                     the evaluation reads"
                        .to_string(),
                );
                continue;
            };

            let Some(tool) = registry.get(&op) else {
                let reason = format!(
                    "`{op}` has no authority contract in this build — a plugin, datasource, or \
                     since-removed op, whose requirements this log cannot supply"
                );
                indeterminate(op, reason);
                continue;
            };
            let requirements = match tool.authority_requirements(&NO_RECORDED_PARAMS, &subjects) {
                Ok(requirements) => requirements,
                Err(err) => {
                    indeterminate(op, format!("invalid authority contract: {err}"));
                    continue;
                }
            };

            let (active_verdict, proposed_verdict) = match decide(
                active,
                proposed,
                &caller_id,
                kind,
                &ceiling,
                &requirements,
            ) {
                Ok(verdicts) => verdicts,
                Err(why) => {
                    indeterminate(op, why.reason());
                    continue;
                }
            };
            let decided = DecidedOp {
                session,
                op,
                caller: caller_id,
                subjects,
                active: active_verdict,
                proposed: proposed_verdict,
                requirements: requirements.iter().map(RequirementView::of).collect(),
            };
            match decided.proposed.cmp(&decided.active) {
                std::cmp::Ordering::Less => report.newly_blocked.push(decided),
                std::cmp::Ordering::Greater => report.newly_allowed.push(decided),
                std::cmp::Ordering::Equal => report.unchanged.push(decided),
            }
        }
    }

    report.counts = Counts {
        newly_blocked: report.newly_blocked.len(),
        newly_allowed: report.newly_allowed.len(),
        unchanged: report.unchanged.len(),
        indeterminate: report.indeterminate.len(),
    };
    Ok(report)
}

// ---------------------------------------------------------------------------
// Human rendering
// ---------------------------------------------------------------------------

/// The default (non-`--json`) report: bucket counts, then per-op detail for everything that moved or
/// could not be decided. `unchanged` stays a count — listing every op a policy edit did not touch is
/// the noise a diff exists to remove; `--json` carries them for tooling that wants them.
fn render_report(report: &SimulationReport, proposed_path: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} — {} recorded op(s) across {} session(s)",
        style::bold("policy simulation"),
        report.ops,
        report.sessions
    );
    let _ = writeln!(
        out,
        "  active:   {} grant(s) (built-in local floor + config)",
        report.active_grants
    );
    let _ = writeln!(
        out,
        "  proposed: {} grant(s) (built-in local floor + {proposed_path})",
        report.proposed_grants
    );
    for assumption in &report.replay_assumptions {
        let _ = writeln!(out, "  {}", style::dim(&format!("assumes: {assumption}")));
    }
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "{} {}",
        style::red("newly blocked"),
        report.counts.newly_blocked
    );
    for op in &report.newly_blocked {
        let _ = writeln!(out, "{}", render_op(op));
    }
    let _ = writeln!(
        out,
        "{} {}",
        style::green("newly allowed"),
        report.counts.newly_allowed
    );
    for op in &report.newly_allowed {
        let _ = writeln!(out, "{}", render_op(op));
    }
    let _ = writeln!(out, "unchanged     {}", report.counts.unchanged);
    let _ = writeln!(
        out,
        "{} {}",
        style::yellow("indeterminate"),
        report.counts.indeterminate
    );
    for op in &report.indeterminate {
        let _ = writeln!(
            out,
            "  {:<18} {} {}",
            op.op,
            style::dim(&op.session),
            style::dim(&op.reason)
        );
    }
    out
}

fn render_op(op: &DecidedOp) -> String {
    let subjects = if op.subjects.is_empty() {
        "—".to_string()
    } else {
        op.subjects.join(", ")
    };
    let why = op
        .requirements
        .iter()
        .map(RequirementView::render)
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "  {:<18} {:<32} {} → {}  {}",
        op.op,
        subjects,
        op.active.label(),
        op.proposed.label(),
        style::dim(&format!("({why})"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_evidence::{Observation, Phase};
    use flux_policy::{Action, Grant, ResourceKind, ResourceRef, SubjectRef};
    use serde_json::json;

    fn user_grant(actions: Vec<&str>, resource: ResourceRef) -> Grant {
        Grant {
            subjects: vec![SubjectRef {
                kind: SubjectKind::User,
                id: "*".into(),
            }],
            resources: vec![resource],
            actions: actions.into_iter().map(Action::from).collect(),
            required_trust: TrustLevel::Untrusted,
            required_scopes: Vec::new(),
            requires_approval: false,
        }
    }

    /// A record as the current recorder writes it — carrying the caller's principal kind.
    fn tool_call(tool: &str, subjects: &[&str]) -> Observation {
        tool_call_as(tool, subjects, "user")
    }

    /// A record from before the recorder carried the principal kind. The kind is genuinely unknown
    /// for these, which is why they decide less.
    fn legacy_tool_call(tool: &str, subjects: &[&str]) -> Observation {
        Observation::new(
            TOOL_CALL,
            Phase::Turn,
            json!({ "tool": tool, "subjects": subjects, "caller": "tester" }),
        )
    }

    /// A record written by a recorder that carries the caller's principal kind.
    fn tool_call_as(tool: &str, subjects: &[&str], kind: &str) -> Observation {
        Observation::new(
            TOOL_CALL,
            Phase::Turn,
            json!({
                "tool": tool,
                "subjects": subjects,
                "caller": "tester",
                "caller_kind": kind,
            }),
        )
    }

    /// The built-in floor with every grant for `action` removed — a proposal that *withdraws*
    /// something the floor allows.
    fn floor_without(action: &str) -> AuthorizationPolicy {
        let mut policy = with_local_floor(None);
        policy
            .grants
            .retain(|g| !g.actions.iter().any(|a| a.0 == action));
        policy
    }

    /// A store seeded with one session's worth of recorded dispatches.
    fn seeded(calls: &[Observation]) -> EventStore {
        let events = EventStore::in_memory().expect("in-memory store");
        let sid = events.create_session("mock").expect("session");
        let turn = events.begin_turn(&sid, "seed", "mock").expect("turn");
        for call in calls {
            events
                .record_observation(&sid, turn, call)
                .expect("record observation");
        }
        events
    }

    fn run(
        events: &EventStore,
        active: &AuthorizationPolicy,
        proposed: &AuthorizationPolicy,
    ) -> SimulationReport {
        let registry = simulation_registry().expect("registry");
        simulate(events, active, proposed, &registry, 0).expect("simulate")
    }

    /// The diff is computed by the shared evaluator over each op's own authority contract: dropping
    /// a config grant that ungated `process.exec` newly blocks a recorded `bash` (back to the
    /// floor's approval gate), adding a `command.invoke` grant newly allows a recorded
    /// `command.invoke` (default-deny without one), and an untouched `read` stays unchanged.
    #[test]
    fn diffs_recorded_ops_between_the_active_and_proposed_policies() {
        let events = seeded(&[
            tool_call("read", &["src/main.rs"]),
            tool_call("bash", &["ls"]),
            tool_call("command.invoke", &["command:deploy"]),
        ]);
        let active = with_local_floor(Some(AuthorizationPolicy {
            grants: vec![user_grant(
                vec!["process.exec"],
                ResourceRef::any(ResourceKind::Process),
            )],
        }));
        let proposed = with_local_floor(Some(AuthorizationPolicy {
            grants: vec![user_grant(
                vec!["command.invoke"],
                ResourceRef::any(ResourceKind::Operation),
            )],
        }));

        let report = run(&events, &active, &proposed);

        assert_eq!(report.ops, 3);
        assert_eq!(report.counts.indeterminate, 0, "{report:?}");
        assert_eq!(report.newly_blocked.len(), 1);
        assert_eq!(report.newly_blocked[0].op, "bash");
        assert_eq!(report.newly_blocked[0].active, Verdict::Allow);
        assert_eq!(
            report.newly_blocked[0].proposed,
            Verdict::ApprovalRequired,
            "the built-in floor approval-gates process.exec"
        );
        assert_eq!(report.newly_allowed.len(), 1);
        assert_eq!(report.newly_allowed[0].op, "command.invoke");
        assert_eq!(report.newly_allowed[0].active, Verdict::Deny);
        assert_eq!(report.newly_allowed[0].proposed, Verdict::Allow);
        assert_eq!(report.unchanged.len(), 1);
        assert_eq!(report.unchanged[0].op, "read");
    }

    /// `--sessions N` bounds the replay to the N most recent sessions.
    #[test]
    fn sessions_bounds_the_replay_window() {
        let events = EventStore::in_memory().expect("in-memory store");
        for tag in ["older", "newer"] {
            let sid = events.create_session("mock").expect("session");
            let turn = events.begin_turn(&sid, tag, "mock").expect("turn");
            events
                .record_observation(&sid, turn, &tool_call("read", &["src/main.rs"]))
                .expect("record observation");
        }
        let policy = with_local_floor(None);
        let registry = simulation_registry().expect("registry");

        let all = simulate(&events, &policy, &policy, &registry, 0).expect("simulate");
        let one = simulate(&events, &policy, &policy, &registry, 1).expect("simulate");

        assert_eq!((all.sessions, all.ops), (2, 2));
        assert_eq!((one.sessions, one.ops), (1, 1));
    }

    /// An op with no authority contract in this build, and a record missing the caller, are both
    /// reported with a reason instead of being decided.
    #[test]
    fn unresolvable_records_are_indeterminate_not_silently_classified() {
        let events = seeded(&[
            tool_call("acme.deploy", &["cluster-a"]),
            Observation::new(
                TOOL_CALL,
                Phase::Turn,
                json!({ "tool": "read", "subjects": ["legacy.txt"] }),
            ),
        ]);
        let policy = with_local_floor(None);

        let report = run(&events, &policy, &policy);

        assert_eq!(report.counts.indeterminate, 2, "{report:?}");
        assert_eq!(report.counts.newly_blocked, 0);
        assert_eq!(report.counts.newly_allowed, 0);
        assert_eq!(report.counts.unchanged, 0);
        assert!(report.indeterminate.iter().all(|i| !i.reason.is_empty()));
        assert!(report.indeterminate[0].reason.contains("acme.deploy"));
        assert!(report.indeterminate[1].reason.contains("caller"));
    }

    /// The caller-fact bracket, both directions. A grant gated on a fact the log omits makes an op
    /// indeterminate **only when the verdict actually turns on it**: gating the sole
    /// `command.invoke` grant refuses that op (it is default-deny without the grant), while `read`
    /// — which the floor allows outright at every point on the axis — stays decided.
    #[test]
    fn omitted_caller_facts_make_only_the_ops_they_could_decide_indeterminate() {
        let events = seeded(&[
            tool_call("read", &["src/main.rs"]),
            tool_call("command.invoke", &["command:deploy"]),
        ]);
        let active = with_local_floor(None);

        let mut trust_gated = user_grant(
            vec!["command.invoke"],
            ResourceRef::any(ResourceKind::Operation),
        );
        trust_gated.required_trust = TrustLevel::Privileged;
        let mut scope_gated = user_grant(
            vec!["command.invoke"],
            ResourceRef::any(ResourceKind::Operation),
        );
        scope_gated.required_scopes = vec![Scope("ops:invoke".into())];
        let mut group_subject = user_grant(
            vec!["command.invoke"],
            ResourceRef::any(ResourceKind::Operation),
        );
        group_subject.subjects = vec![SubjectRef {
            kind: SubjectKind::Group,
            id: "ops-*".into(),
        }];

        for (grant, expected) in [
            (trust_gated, "trust level"),
            (scope_gated, "scopes"),
            (group_subject, "group memberships"),
        ] {
            let proposed = with_local_floor(Some(AuthorizationPolicy {
                grants: vec![grant],
            }));
            let report = run(&events, &active, &proposed);

            assert_eq!(report.counts.indeterminate, 1, "{report:?}");
            assert_eq!(report.indeterminate[0].op, "command.invoke");
            assert!(
                report.indeterminate[0].reason.contains(expected),
                "expected {expected:?} in {:?}",
                report.indeterminate[0].reason
            );
            // The op the floor settles regardless of the omitted fact is still decided.
            assert_eq!(report.counts.unchanged, 1, "{report:?}");
            assert_eq!(report.unchanged[0].op, "read");
        }
    }

    /// A scope gate that cannot change any verdict must not make anything indeterminate: the
    /// bracket has to be evaluated against the ops, not against the mere presence of a gate.
    #[test]
    fn an_inert_gate_leaves_every_op_decided() {
        let events = seeded(&[tool_call("read", &["src/main.rs"])]);
        let active = with_local_floor(None);
        // A trust-gated grant for an action no recorded op requires.
        let mut inert = user_grant(
            vec!["datasource.write"],
            ResourceRef::any(ResourceKind::Datasource),
        );
        inert.required_trust = TrustLevel::Privileged;
        inert.required_scopes = vec![Scope("db:write".into())];
        let proposed = with_local_floor(Some(AuthorizationPolicy {
            grants: vec![inert],
        }));

        let report = run(&events, &active, &proposed);

        assert_eq!(report.counts.indeterminate, 0, "{report:?}");
        assert_eq!(report.counts.unchanged, 1, "{report:?}");
    }

    /// A grant gated on **two** omitted facts at once escapes every single-axis probe: neither the
    /// group alone nor the trust alone satisfies it, so widening one axis at a time finds no change
    /// and reports the op as confidently decided. Only a joint maximum catches it.
    ///
    /// This is the regression test for the real defect: the reviewer's single-axis control cases
    /// passed throughout, so nothing narrower than this would have caught it.
    #[test]
    fn a_grant_gated_on_two_omitted_facts_at_once_is_indeterminate() {
        let events = seeded(&[tool_call("bash", &["ls"])]);
        let active = with_local_floor(None);
        let mut both = user_grant(
            vec!["process.exec"],
            ResourceRef::any(ResourceKind::Process),
        );
        both.subjects = vec![SubjectRef {
            kind: SubjectKind::Group,
            id: "ops".into(),
        }];
        both.required_trust = TrustLevel::Privileged;
        let proposed = with_local_floor(Some(AuthorizationPolicy {
            grants: vec![both],
        }));

        let report = run(&events, &active, &proposed);

        assert_eq!(
            report.counts.indeterminate, 1,
            "a grant gated on group AND trust together must not be reported as decided: {report:?}"
        );
        assert_eq!(report.counts.unchanged, 0, "{report:?}");
    }

    /// `SubjectKind::User` is itself kind-discriminating, so the floor's own grants depend on the
    /// caller's principal kind. A record that does not carry the kind is consistent with an `agent`
    /// principal, for which the floor never applied — so a proposal that withdraws a floor grant is
    /// "newly blocked" for a user and "unchanged" for an agent, and the log cannot tell which.
    #[test]
    fn a_record_without_a_principal_kind_cannot_decide_a_floor_withdrawal() {
        let events = seeded(&[legacy_tool_call("write", &["notes.md"])]);
        let active = with_local_floor(None);
        let proposed = floor_without("workspace.write");

        let report = run(&events, &active, &proposed);

        assert_eq!(
            report.counts.indeterminate, 1,
            "a kind-free record must not be replayed as a `user`: {report:?}"
        );
        assert_eq!(
            report.counts.newly_blocked, 0,
            "reporting `newly blocked` here asserts a user principal the log never recorded"
        );
        assert!(
            report.indeterminate[0].reason.contains("principal kind"),
            "{:?}",
            report.indeterminate[0].reason
        );
    }

    /// ...and once the recorder writes the kind, the very same withdrawal is decided exactly. This
    /// is what keeps the bracket from degenerating into "everything is indeterminate".
    #[test]
    fn a_recorded_principal_kind_decides_the_same_withdrawal() {
        let events = seeded(&[tool_call_as("write", &["notes.md"], "user")]);
        let active = with_local_floor(None);
        let proposed = floor_without("workspace.write");

        let report = run(&events, &active, &proposed);

        assert_eq!(report.counts.indeterminate, 0, "{report:?}");
        assert_eq!(report.counts.newly_blocked, 1, "{report:?}");
    }

    /// The mirrored case: an `agent` record is *not* affected by withdrawing a `user` grant, and
    /// saying so is only possible because the kind is recorded.
    #[test]
    fn an_agent_principal_is_unaffected_by_withdrawing_a_user_grant() {
        let events = seeded(&[tool_call_as("write", &["notes.md"], "agent")]);
        let active = with_local_floor(None);
        let proposed = floor_without("workspace.write");

        let report = run(&events, &active, &proposed);

        assert_eq!(report.counts.indeterminate, 0, "{report:?}");
        assert_eq!(report.counts.newly_blocked, 0, "{report:?}");
        assert_eq!(report.counts.unchanged, 1, "{report:?}");
    }

    /// Pure read: simulating leaves the log exactly as it was.
    #[test]
    fn simulation_appends_nothing_to_the_event_store() {
        let events = seeded(&[
            tool_call("read", &["src/main.rs"]),
            tool_call("bash", &["ls"]),
        ]);
        let streams = events.all_streams().expect("streams");
        let before: Vec<_> = streams
            .iter()
            .map(|s| events.load_stream(s, None).expect("load"))
            .collect();

        let policy = with_local_floor(None);
        run(&events, &policy, &policy);
        run(&events, &policy, &AuthorizationPolicy::default());

        let after: Vec<_> = streams
            .iter()
            .map(|s| events.load_stream(s, None).expect("load"))
            .collect();
        assert_eq!(before, after);
    }

    /// The guard behind [`simulation_registry`]'s contract: every op the simulator will decide must
    /// derive its authority requirements from the declaration and the recorded subjects ALONE.
    ///
    /// The log records `{tool, subjects, caller}` and never the invocation params, so an op whose
    /// `authority_requirements` reads `params` (as the browser and live-datasource ops do — none of
    /// which are in this registry) would be classified from input the simulator does not have.
    /// Probing every registered op with structurally different params, against fixed subject lists,
    /// isolates exactly that sensitivity: adding such an op to the built-in pack reds this test
    /// rather than silently degrading the report.
    #[test]
    fn simulated_ops_derive_authority_from_recorded_context_only() {
        let registry = simulation_registry().expect("registry");
        let probes = [
            json!({}),
            json!({
                "path": "probe.rs",
                "url": "https://example.invalid/probe",
                "command": "rm -rf /",
                "name": "probe",
                "kind": "skill",
                "entity": "probe",
                "role": "probe",
            }),
        ];

        for name in registry.names() {
            let tool = registry.get(&name).expect("registered tool");
            for subjects in [Vec::new(), vec!["subject-a".to_string()]] {
                let baseline = tool
                    .authority_requirements(&NO_RECORDED_PARAMS, &subjects)
                    .map_err(|e| e.to_string());
                for probe in &probes {
                    let probed = tool
                        .authority_requirements(probe, &subjects)
                        .map_err(|e| e.to_string());
                    assert_eq!(
                        format!("{probed:?}"),
                        format!("{baseline:?}"),
                        "`{name}` derives authority requirements from its invocation params, which \
                         the event log does not record — `flux policy simulate` would classify its \
                         history from input it does not have. Keep it out of `simulation_registry`."
                    );
                }
            }
        }
    }
}
