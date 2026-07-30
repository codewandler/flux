//! Sweep **every** file under `examples/` through the strictest gate its form supports — a real
//! directory enumeration (`std::fs::read_dir`), not a hand-picked list, so a newly added example is
//! guarded by default (L-41). `flows_validate.rs` used to hardcode 4 of the 12 root examples; the
//! other 8 had no CI guard at all, and the value-template drift class has broken checked-in flows
//! twice before without one catching it early.
//!
//! Three tiers, by form — each file is sniffed, not pre-classified:
//!
//! 1. **JSON `DraftAst`** (the file parses as JSON) — deserialize as a `DraftAst`, then run it
//!    through the SAME `lower` gate `flux flow run` applies against [`full_registry`].
//! 2. **Native-text bare flow** (`flow name(...) -> T` / `flow name(...) { ... }`, no module decls)
//!    — [`Module::parse_str`] sniffs it to `Module::Flow`; same `lower` gate against
//!    [`full_registry`]. Exception: [`FLOW_PARSE_ONLY`] below.
//! 3. **Native-text multi-agent Program** (`agent`/`channel`/`trigger`/`journey` decls present,
//!    e.g. `channels-app.flux`) — `Module::parse_str` sniffs it to `Module::Program`. Every
//!    `trigger.run` must resolve to a declared journey or top-level flow (a real, registry-free
//!    structural check — a dangling trigger target is exactly the kind of drift this sweep exists
//!    to catch). The journeys' *bodies* are **not** lowered: they call the orchestration ops
//!    (`emit`/`send`/`ask`/`spawn`), which `flux-app` (L6) registers against a live `Bus` +
//!    `JourneyHost` it constructs at `App` start — there is no in-process registry reachable from
//!    flux-eval's (L3) own dependency set that provides them without a genuine layering violation
//!    (AGENTS.md's layer table: flux-eval may depend on its own layer or lower, never on
//!    flux-app). Parse + structural is the honest, non-bypassing gate here; `flux-app`'s own tests
//!    (`crates/flux-app/tests/`) exercise the full runtime path for program-form examples.
//!
//! [`full_registry`] is the fullest op registry reasonably buildable from flux-eval's own
//! dependency set: `register_builtins` + `register_eval_ops` + the `task` tool + flux-cognition's
//! model-backed ops (`ai.extract`/`ai.rank`/`ai.judge`/`ai.reason`/`synth`/`ai.rewrite`), wired to
//! a key-free [`flux_provider::NullProvider`] — `lower` only inspects each op's typed *signature*,
//! never calls it, so no model is ever reached and no network/API key is needed. One file still
//! calls an op outside even that reach: `advanced-code-review.flux` calls `slack.message.send`, an
//! op the out-of-process `flux-plugin-slack` binary registers only once an operator installs it and
//! the host wires its subprocess — categorically unavailable to an in-process test registry. It is
//! pinned at parse-only via [`FLOW_PARSE_ONLY`], with the reason recorded next to its name. The
//! deterministic `bitcoin-price.flux` example likewise calls `web.fetch`, which is registered by
//! `flux-web` (L5); flux-eval (L3) cannot depend on that outer crate without violating the workspace
//! layering rule, so that file receives the same explicit parse-only treatment.

use std::collections::HashSet;
use std::sync::Arc;

use flux_flow::program::Module;
use flux_runtime::ToolRegistry;

/// Native-text bare flows that call an op outside [`full_registry`]'s reach for a genuine,
/// documented layering reason — pinned at parse-only rather than the full `lower` gate. Keep this
/// list to real cross-layer gaps (checked by hand when added); anything else that fails to lower
/// is drift and must fail the sweep loudly.
const FLOW_PARSE_ONLY: &[(&str, &str)] = &[
    (
        "advanced-code-review.flux",
        "calls slack.message.send — an out-of-process flux-plugin-slack op, registered only once an \
         operator installs the plugin and the host wires its subprocess; no in-process registry \
         reachable from flux-eval provides it",
    ),
    (
        "bitcoin-price.flux",
        "calls web.fetch — a flux-web (L5) op; flux-eval (L3) cannot depend on that outer-layer \
         registry without violating the workspace layering rule",
    ),
];

/// The fullest op registry flows_validate can build from flux-eval's own dependency set: builtins,
/// eval ops, `task`, and the cognition pack. See the module doc for why a `NullProvider` is safe
/// here (no model call ever happens — `lower` only reads each op's declared signature).
fn full_registry() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    flux_tools::register_builtins(&mut reg);
    flux_eval::register_eval_ops(&mut reg);
    reg.register(Arc::new(flux_orchestrate::TaskTool));
    flux_cognition::CognitionPack::new(Arc::new(flux_provider::NullProvider), "examples-validate")
        .register(&mut reg);
    reg
}

/// Lower `ast` against `reg`'s live op set — the same gate `flux flow run` applies — panicking with
/// the diagnostics on failure.
fn lower_or_panic(path: &str, ast: &flux_flow::ast::DraftAst, reg: &ToolRegistry) {
    let ops = flux_flow::registry::OpRegistry::new(reg);
    flux_flow::analyze::lower(ast, &ops, &Default::default()).unwrap_or_else(|diags| {
        panic!(
            "{path} fails the flow-run gate (unknown ops / missing required params / type \
             conflicts): {diags:?}"
        )
    });
}

/// Every trigger in a program must name a target that exists. Registry-free (no op resolution
/// involved) — catches a dangling trigger target regardless of layering.
///
/// The rule itself is **not** restated here: it is `Program::validate_trigger_targets`, the same L0
/// function `flux_app::Engine::validate` calls, so this sweep cannot be stricter or looser than the
/// runtime it stands in for. It used to be a hand-written copy that asserted
/// `flow_named(&t.run).is_some()` for every trigger, which rejected the legitimate **agent-bound**
/// shape (`agent = "..."`, empty `run`) that the runtime accepts — so an agent-triggered Program
/// could not ship as an example at all (C-232).
fn validate_program_structure(path: &str, program: &flux_flow::program::Program) {
    program
        .validate_trigger_targets()
        .unwrap_or_else(|err| panic!("{path}: {err}"));
}

#[test]
fn every_example_validates_against_its_form_appropriate_gate() {
    let examples_dir = std::env::current_dir()
        .unwrap()
        .join("../../examples")
        .canonicalize()
        .expect("examples/ must exist at the repo root");

    let mut paths: Vec<_> = std::fs::read_dir(&examples_dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", examples_dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "flux"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "expected to find checked-in *.flux examples under {}",
        examples_dir.display()
    );

    let reg = full_registry();
    let mut seen: HashSet<String> = HashSet::new();

    for path in &paths {
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        seen.insert(filename.clone());
        let display = format!("examples/{filename}");
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {display}: {e}"));

        // The `.flux` extension is reserved for native Flux-Lang text. JSON DraftAst values are
        // valid API/wire values, but accepting them here would let mislabeled fixtures regress.
        let module = Module::parse_str(&src)
            .unwrap_or_else(|e| panic!("parse {display} as native flux-lang text: {e}"));
        match module {
            Module::Flow(ast) => {
                if let Some((_, reason)) = FLOW_PARSE_ONLY.iter().find(|(f, _)| *f == filename) {
                    // Documented cross-layer gap: parse succeeded (asserted above), which is the
                    // full gate this file gets. `reason` exists so a reviewer can see why.
                    let _ = reason;
                } else {
                    lower_or_panic(&display, &ast, &reg);
                }
            }
            Module::Program(program) => validate_program_structure(&display, &program),
        }
    }

    // Every filename named in FLOW_PARSE_ONLY must still exist and be swept above — an exception
    // list entry for a file that no longer exists (or was renamed) would silently stop meaning
    // anything.
    for (name, _) in FLOW_PARSE_ONLY {
        assert!(
            seen.contains(*name),
            "FLOW_PARSE_ONLY names `{name}`, which was not found under examples/ — update or \
             remove the entry"
        );
    }
}

/// Parse `src` as a program-form module, panicking if it sniffs as a bare flow.
fn program_or_panic(src: &str) -> flux_flow::program::Program {
    match Module::parse_str(src).expect("fixture must parse as native flux-lang text") {
        Module::Program(program) => program,
        Module::Flow(_) => panic!("fixture must sniff as a program, not a bare flow"),
    }
}

/// An **agent-bound** trigger (`agent = "..."`, no `run`) is what the fleet coordinator needs, and
/// the runtime accepts it — so the sweep must too. Guards C-232: the sweep used to assert
/// `flow_named(&t.run).is_some()` unconditionally, which rejects this valid shape.
#[test]
fn the_sweep_accepts_an_agent_bound_trigger() {
    let program = program_or_panic(
        "\
agent coordinator
  model \"mock\"

trigger fanout
  on \"a2a_request\"
  agent \"coordinator\"
",
    );
    assert!(
        program.triggers[0].run.is_empty(),
        "an agent-bound trigger parses with an empty `run` — that is the shape under test"
    );
    validate_program_structure("fixture/agent-bound-trigger.flux", &program);
}

/// The other direction of C-232: relaxing the sweep for agent-bound triggers must not make it
/// *looser* than the runtime. A trigger whose `run` names nothing declared still fails.
#[test]
#[should_panic(expected = "trigger `dangling` names unknown journey/flow `nope`")]
fn the_sweep_still_rejects_a_trigger_naming_no_declared_flow() {
    let program = program_or_panic(
        "\
trigger dangling
  on \"user_input\"
  run nope

journey handle
  flow
    return null
",
    );
    validate_program_structure("fixture/dangling-trigger.flux", &program);
}

/// And a trigger naming an agent that was never declared must fail too — the sweep inherits that
/// arm of the runtime's rule for free by sharing it.
#[test]
#[should_panic(expected = "trigger `fanout` names unknown agent `ghost`")]
fn the_sweep_rejects_a_trigger_naming_an_undeclared_agent() {
    let program = program_or_panic(
        "\
agent coordinator
  model \"mock\"

trigger fanout
  on \"a2a_request\"
  agent \"ghost\"
",
    );
    validate_program_structure("fixture/undeclared-agent-trigger.flux", &program);
}
