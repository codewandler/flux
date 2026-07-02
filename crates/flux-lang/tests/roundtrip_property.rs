//! Property test behind the headline invariant: `parse(&format(&ast)) == ast` for **every**
//! `DraftAst` body (L-18 / F21/F22 — round-trip totality).
//!
//! A seeded, hand-rolled generator (deterministic xorshift64*, no external dev-dependency — the
//! workspace has no proptest, and a reproducible seed is all the shrinking we need at this size)
//! builds depth-limited random ASTs covering **all 43 node kinds**, drawing every name/label/string
//! slot from pools that mix spellable identifiers with adversarial strings (dots, spaces, empty,
//! unicode, keywords, quotes, hashes, newlines).
//!
//! **Documented exclusion (the flow-header exception, see `format.rs`):** flow names, parameter
//! names, and header types are generated from *spellable* pools only. The header has no `@json`
//! escape, so an unspellable header name is a loud parse error by design; the analyzer rejects such
//! names (`is_valid_decl_name`) before they can reach a formatted artifact.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use flux_lang::ast::{
    Branch, DraftAst, FallbackBranch, FlowEffect, MatchCase, Node, Param, RouteCase, SagaStep,
    Selector, SymbolName, ThingKind, ThingRef, TypeRef,
};

// ---------------------------------------------------------------------------
// Deterministic PRNG (xorshift64*)
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Splash the seed so small consecutive seeds diverge immediately; state must be nonzero.
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, pool: &'a [T]) -> &'a T {
        &pool[self.below(pool.len())]
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

// ---------------------------------------------------------------------------
// Pools — spellable and adversarial names side by side
// ---------------------------------------------------------------------------

/// Symbol-name slots (bind/each/ctx/collect/arrow/branch names and `$var` references).
const SYMS: &[&str] = &[
    "x", "ok", "a_b", "n0", "9lives", "_", // spellable
    "a.b", "a b", "", "a-b", "tëst", "do", "until", "fmt", "$x", "a\nb", "a#b", "\"q\"", "a,b",
];

/// Op-name slots (`call.op`).
const OPS: &[&str] = &[
    "read",
    "email.send",
    "git-status",
    "_p9",
    "do", // spellable
    "fmt",
    "a b",
    "",
    "9op",
    ".dot",
    "op(x)",
];

/// Free-form string slots (labels, messages, purposes, templates, sources, keys, tools).
const STRINGS: &[&str] = &[
    "",
    "hello",
    "line\nbreak",
    "he said \"hi\"",
    "hash # inside",
    "emoji 🦀",
    "{name} and {{brace}}",
    "a, b]",
];

/// `jq` path slots (only simple dotted paths over a `$var` are native).
const PATHS: &[&str] = &[".a", ".a.b", ".items[0]", ".k", "", "a", ". x"];

/// `retry.backoff` slots (free-form string; only bare words are native).
const BACKOFFS: &[&str] = &["none", "linear", "exponential", "a b", ""];

fn sym(rng: &mut Rng) -> SymbolName {
    SymbolName::from(*rng.pick(SYMS))
}

fn opt_sym(rng: &mut Rng) -> Option<SymbolName> {
    rng.chance(60).then(|| sym(rng))
}

fn string(rng: &mut Rng) -> String {
    (*rng.pick(STRINGS)).to_string()
}

fn opt_string(rng: &mut Rng) -> Option<String> {
    rng.chance(50).then(|| string(rng))
}

fn json_value(rng: &mut Rng) -> serde_json::Value {
    let pool = [
        serde_json::Value::Null,
        serde_json::json!(true),
        serde_json::json!(false),
        serde_json::json!(0),
        serde_json::json!(-3),
        serde_json::json!(2.5),
        serde_json::json!(1e300),
        serde_json::json!("s"),
        serde_json::json!(""),
        serde_json::json!("a\nb"),
        serde_json::json!("do x"),
        serde_json::json!([1, "a", null]),
        serde_json::json!({"k": "v", "n": [true, {"d": 1}]}),
    ];
    pool[rng.below(pool.len())].clone()
}

/// Any type, including unspellable ones (builtin-colliding / charset-violating `Named`) — valid in
/// a `bind` annotation, where the formatter's type guard must send the node to `@json`.
fn any_type(rng: &mut Rng) -> TypeRef {
    match rng.below(12) {
        0 => TypeRef::Any,
        1 => TypeRef::Bool,
        2 => TypeRef::Number,
        3 => TypeRef::String,
        4 => TypeRef::List(Box::new(TypeRef::String)),
        5 => TypeRef::List(Box::new(TypeRef::Named("Ticket".into()))),
        6 => TypeRef::Named("Ticket".into()),
        7 => TypeRef::Named("Bool".into()), // builtin collision
        8 => TypeRef::Named("a=b".into()),  // splits the typed-bind `=`
        9 => TypeRef::Named("a b".into()),  // whitespace
        10 => TypeRef::Named("".into()),    // empty
        _ => TypeRef::Named("List<T>".into()), // sugar collision
    }
}

fn opt_any_type(rng: &mut Rng) -> Option<TypeRef> {
    rng.chance(40).then(|| any_type(rng))
}

/// Header-safe types only (the documented flow-header exclusion).
fn safe_type(rng: &mut Rng) -> TypeRef {
    match rng.below(6) {
        0 => TypeRef::Any,
        1 => TypeRef::Bool,
        2 => TypeRef::Number,
        3 => TypeRef::String,
        4 => TypeRef::List(Box::new(TypeRef::Named("Ticket".into()))),
        _ => TypeRef::Named("Ticket".into()),
    }
}

fn opt_effect(rng: &mut Rng) -> Option<FlowEffect> {
    let pool = [
        None,
        None,
        Some(FlowEffect::Pure),
        Some(FlowEffect::Read),
        Some(FlowEffect::Model),
        Some(FlowEffect::SendExternal),
        Some(FlowEffect::Delete),
    ];
    pool[rng.below(pool.len())]
}

// ---------------------------------------------------------------------------
// The bounded AST generator — all 43 node kinds
// ---------------------------------------------------------------------------

/// Serde `kind` tags, aligned with the constructor indices in [`gen_node`].
const KINDS: &[&str] = &[
    "call",
    "bind",
    "when",
    "repeat",
    "each",
    "assert",
    "pipe",
    "seq",
    "memo",
    "parallel",
    "await",
    "retry",
    "try",
    "confirm",
    "loop",
    "race",
    "throttle",
    "debounce",
    "unless",
    "verify",
    "return",
    "peek",
    "var",
    "lit",
    "thing",
    "expr",
    "fmt",
    "jq",
    "parse",
    "ctx",
    "ctx_append",
    "match",
    "route",
    "fallback",
    "timeout",
    "budget",
    "cap_scope",
    "scope",
    "saga",
    "once",
    "checkpoint",
    "obj",
    "list",
];

/// Leaf kinds usable at depth 0 (no child nodes): indices into [`KINDS`].
const LEAVES: &[usize] = &[22, 23, 26, 21, 40, 24]; // var, lit, fmt, peek, checkpoint, thing

fn gen_body(rng: &mut Rng, depth: usize, seen: &mut BTreeSet<&'static str>) -> Vec<Node> {
    let n = rng.below(3);
    (0..n).map(|_| gen_node(rng, depth, seen)).collect()
}

fn gen_branches(rng: &mut Rng, depth: usize, seen: &mut BTreeSet<&'static str>) -> Vec<Branch> {
    let n = rng.below(3);
    (0..n)
        .map(|_| Branch {
            name: sym(rng),
            body: gen_body(rng, depth, seen),
        })
        .collect()
}

fn gen_node(rng: &mut Rng, depth: usize, seen: &mut BTreeSet<&'static str>) -> Node {
    let idx = if depth == 0 {
        *rng.pick(LEAVES)
    } else {
        rng.below(KINDS.len())
    };
    seen.insert(KINDS[idx]);
    let d = depth.saturating_sub(1);
    match idx {
        0 => Node::Call {
            op: (*rng.pick(OPS)).to_string(),
            args: (0..rng.below(3)).map(|_| gen_node(rng, d, seen)).collect(),
        },
        1 => Node::Bind {
            name: sym(rng),
            value: Box::new(gen_node(rng, d, seen)),
            ty: opt_any_type(rng),
            effect: opt_effect(rng),
        },
        2 => Node::When {
            cond: Box::new(gen_node(rng, d, seen)),
            then: gen_body(rng, d, seen),
            otherwise: gen_body(rng, d, seen),
        },
        3 => Node::Repeat {
            max: rng.below(5) as u32,
            until: rng.chance(50).then(|| Box::new(gen_node(rng, d, seen))),
            body: gen_body(rng, d, seen),
            collect: opt_sym(rng),
        },
        4 => Node::Each {
            source: Box::new(gen_node(rng, d, seen)),
            item: sym(rng),
            body: gen_body(rng, d, seen),
            collect: opt_sym(rng),
            flat: rng.chance(30),
        },
        5 => Node::Assert {
            cond: Box::new(gen_node(rng, d, seen)),
            message: opt_string(rng),
        },
        6 => Node::Pipe {
            steps: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        7 => Node::Seq {
            body: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        8 => Node::Memo {
            name: sym(rng),
            value: Box::new(gen_node(rng, d, seen)),
            ty: opt_any_type(rng),
            effect: opt_effect(rng),
        },
        9 => Node::Parallel {
            branches: gen_branches(rng, d, seen),
        },
        10 => Node::Await {
            binding: opt_sym(rng),
            source: string(rng),
            as_type: opt_any_type(rng),
        },
        11 => Node::Retry {
            max: rng.below(4) as u32,
            backoff: rng.chance(60).then(|| (*rng.pick(BACKOFFS)).to_string()),
            delay_ms: rng.chance(40).then(|| rng.next() % 10_000),
            body: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        12 => Node::Try {
            body: gen_body(rng, d, seen),
            catch: opt_sym(rng),
            handler: gen_body(rng, d, seen),
        },
        13 => Node::Confirm {
            message: string(rng),
            risk: opt_string(rng),
            body: gen_body(rng, d, seen),
        },
        14 => Node::Loop {
            for_ms: rng.next() % 100_000,
            every_ms: rng.next() % 1_000,
            until: rng.chance(50).then(|| Box::new(gen_node(rng, d, seen))),
            body: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        15 => Node::Race {
            timeout_ms: rng.next() % 100_000,
            branches: gen_branches(rng, d, seen),
            bind: opt_sym(rng),
        },
        16 => Node::Throttle {
            name: string(rng),
            max: rng.below(9) as u32,
            window_ms: rng.next() % 100_000,
            body: gen_body(rng, d, seen),
        },
        17 => Node::Debounce {
            name: string(rng),
            wait_ms: rng.next() % 100_000,
            body: gen_body(rng, d, seen),
        },
        18 => Node::Unless {
            cond: Box::new(gen_node(rng, d, seen)),
            body: gen_body(rng, d, seen),
        },
        19 => Node::Verify {
            cmd: Box::new(gen_node(rng, d, seen)),
            expect: Box::new(gen_node(rng, d, seen)),
            message: opt_string(rng),
        },
        20 => Node::Return {
            value: Box::new(gen_node(rng, d, seen)),
        },
        21 => Node::Peek { name: sym(rng) },
        22 => Node::Var { name: sym(rng) },
        23 => Node::Lit {
            value: json_value(rng),
        },
        24 => Node::Thing {
            thing: ThingRef {
                kind: match rng.below(4) {
                    0 => ThingKind::Person,
                    1 => ThingKind::File,
                    2 => ThingKind::Url,
                    _ => ThingKind::Custom(string(rng)),
                },
                selector: match rng.below(3) {
                    0 => Selector::Id(string(rng)),
                    1 => Selector::Name(string(rng)),
                    _ => Selector::Query(string(rng)),
                },
            },
        },
        25 => Node::Expr {
            formula: string(rng),
            vars: {
                let mut m = BTreeMap::new();
                for _ in 0..rng.below(3) {
                    m.insert(string(rng), Box::new(gen_node(rng, 0, seen)));
                }
                m
            },
        },
        26 => Node::Fmt {
            template: string(rng),
        },
        27 => Node::Jq {
            path: (*rng.pick(PATHS)).to_string(),
            input: Box::new(gen_node(rng, d, seen)),
        },
        28 => Node::Parse {
            value: Box::new(gen_node(rng, d, seen)),
            as_type: (*rng.pick(&["f64", "i64", "bool", "json", "string"])).to_string(),
        },
        29 => Node::Ctx {
            name: sym(rng),
            purpose: opt_string(rng),
            include: (0..rng.below(3)).map(|_| sym(rng)).collect(),
            exclude: (0..rng.below(2)).map(|_| sym(rng)).collect(),
            budget: rng.chance(50).then(|| rng.next() % 10_000),
        },
        30 => Node::CtxAppend {
            ctx: sym(rng),
            add: (0..rng.below(3)).map(|_| sym(rng)).collect(),
        },
        31 => Node::Match {
            subject: Box::new(gen_node(rng, d, seen)),
            cases: (0..rng.below(3))
                .map(|_| MatchCase {
                    value: gen_node(rng, d, seen),
                    body: gen_body(rng, d, seen),
                })
                .collect(),
            default: gen_body(rng, d, seen),
        },
        32 => Node::Route {
            selector: Box::new(gen_node(rng, d, seen)),
            cases: (0..rng.below(3))
                .map(|_| RouteCase {
                    label: string(rng),
                    body: gen_body(rng, d, seen),
                })
                .collect(),
            default: gen_body(rng, d, seen),
        },
        33 => Node::Fallback {
            branches: (0..rng.below(3))
                .map(|_| FallbackBranch {
                    body: gen_body(rng, d, seen),
                })
                .collect(),
            bind: opt_sym(rng),
        },
        34 => Node::Timeout {
            ms: rng.next() % 100_000,
            body: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        35 => Node::Budget {
            limit: rng.below(20) as u32,
            body: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        36 => Node::CapScope {
            tools: (0..rng.below(3)).map(|_| string(rng)).collect(),
            body: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        37 => Node::Scope {
            acquire: rng.chance(60).then(|| Box::new(gen_node(rng, d, seen))),
            bind: opt_sym(rng),
            body: gen_body(rng, d, seen),
            finally: gen_body(rng, d, seen),
        },
        38 => Node::Saga {
            steps: (0..rng.below(3))
                .map(|_| SagaStep {
                    body: gen_body(rng, d, seen),
                    undo: gen_body(rng, d, seen),
                })
                .collect(),
        },
        39 => Node::Once {
            label: string(rng),
            body: gen_body(rng, d, seen),
            bind: opt_sym(rng),
        },
        40 => Node::Checkpoint { label: string(rng) },
        41 => Node::Obj {
            fields: {
                let keys = [
                    "k",
                    "a_b",
                    "a-b",
                    "9k",
                    "",
                    "with space",
                    "quo\"te",
                    "läuft",
                ];
                let mut m = BTreeMap::new();
                for _ in 0..rng.below(4) {
                    m.insert(
                        (*rng.pick(&keys)).to_string(),
                        Box::new(gen_node(rng, d, seen)),
                    );
                }
                m
            },
        },
        42 => Node::List {
            items: (0..rng.below(4)).map(|_| gen_node(rng, d, seen)).collect(),
        },
        _ => unreachable!("constructor index out of range"),
    }
}

/// Header slots use spellable pools only — the documented flow-header exception.
fn gen_ast(rng: &mut Rng, seen: &mut BTreeSet<&'static str>) -> DraftAst {
    let names = ["go", "route-call", "a_b", "x9"];
    let params = ["utterance", "count", "x", "arg_1"];
    DraftAst {
        name: rng.chance(70).then(|| (*rng.pick(&names)).to_string()),
        params: (0..rng.below(3))
            .map(|i| Param {
                name: params[i].into(),
                ty: safe_type(rng),
            })
            .collect(),
        returns: rng.chance(50).then(|| safe_type(rng)),
        body: (0..rng.below(5)).map(|_| gen_node(rng, 3, seen)).collect(),
    }
}

// ---------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------

#[test]
fn random_draft_asts_round_trip_exactly() {
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for seed in 1..=1000u64 {
        let mut rng = Rng::new(seed);
        let ast = gen_ast(&mut rng, &mut seen);
        let text = flux_lang::format::format(&ast);
        let back = flux_lang::parse::parse(&text).unwrap_or_else(|e| {
            panic!(
                "seed {seed}: parse failed: {e}\n--- text ---\n{text}\n--- ast ---\n{}",
                serde_json::to_string_pretty(&ast).unwrap()
            )
        });
        assert_eq!(
            back,
            ast,
            "seed {seed}: round-trip changed the AST\n--- text ---\n{text}\n--- emitted ---\n{}\n--- reparsed ---\n{}",
            serde_json::to_string_pretty(&ast).unwrap(),
            serde_json::to_string_pretty(&back).unwrap()
        );
    }
    // The generator really exercised every node kind (guards against pool drift as kinds grow).
    let missing: Vec<_> = KINDS.iter().filter(|k| !seen.contains(**k)).collect();
    assert!(missing.is_empty(), "kinds never generated: {missing:?}");
    assert_eq!(KINDS.len(), 43, "the 43-kind census changed — update KINDS");
}
