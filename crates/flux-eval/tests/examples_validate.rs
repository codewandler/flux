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
//! pinned at parse-only via [`FLOW_PARSE_ONLY`], with the reason recorded next to its name.

use std::collections::HashSet;
use std::sync::Arc;

use flux_flow::program::Module;
use flux_runtime::ToolRegistry;

/// Native-text bare flows that call an op outside [`full_registry`]'s reach for a genuine,
/// documented layering reason — pinned at parse-only rather than the full `lower` gate. Keep this
/// list to real cross-layer gaps (checked by hand when added); anything else that fails to lower
/// is drift and must fail the sweep loudly.
const FLOW_PARSE_ONLY: &[(&str, &str)] = &[(
    "advanced-code-review.flux",
    "calls slack.message.send — an out-of-process flux-plugin-slack op, registered only once an \
     operator installs the plugin and the host wires its subprocess; no in-process registry \
     reachable from flux-eval provides it",
)];

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

/// Every `trigger.run` in a program must name a declared journey or top-level flow. Registry-free
/// (no op resolution involved) — catches a dangling trigger target regardless of layering.
fn validate_program_structure(path: &str, program: &flux_flow::program::Program) {
    for t in &program.triggers {
        assert!(
            program.flow_named(&t.run).is_some(),
            "{path}: trigger `{}` runs `{}`, which names no declared journey or top-level flow",
            t.name,
            t.run
        );
    }
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

        if serde_json::from_str::<serde_json::Value>(&src).is_ok() {
            // Tier 1: JSON DraftAst.
            let ast: flux_flow::ast::DraftAst = serde_json::from_str(&src)
                .unwrap_or_else(|e| panic!("parse {display} as DraftAst: {e}"));
            lower_or_panic(&display, &ast, &reg);
            continue;
        }

        // Tiers 2/3: native flux-lang text.
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
