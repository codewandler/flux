//! L-97 — **Flux Glyph**: the compact indented opcode projection of a `DraftAst`.
//!
//! Four things are pinned here, because they are the story's contract:
//!
//! 1. **The shared fixture.** `docs/designs/flux-notation-workbench.md` names the `triage` flow as
//!    the epic's golden fixture. Its Glyph document parses to *exactly* the AST canonical Flux
//!    parses to, and that AST formats back to the pinned Glyph text (a fixed point).
//! 2. **The vocabulary.** Exactly the fourteen opcodes the design names, plus `@{…}` as the
//!    raw-AST escape — no more, no less.
//! 3. **Native core plus escape totality.** Every `Node` kind in the generated catalog round-trips
//!    (`parse_glyph(format_glyph(ast)) == ast`), natively when Glyph spells it and through the
//!    escape when it does not; a seeded generator pushes the same property over random ASTs.
//! 4. **Fail-closed diagnostics.** Indentation, unknown opcode, arm placement, duplicate branch and
//!    malformed escape each report the offending Glyph line and never guess.

use std::collections::BTreeMap;

use flux_lang::ast::{
    Branch, DraftAst, FallbackBranch, FlowEffect, MatchCase, Node, Param, RouteCase, SagaStep,
    Selector, SymbolName, ThingKind, ThingRef, TypeRef,
};
use flux_lang::glyph::{format_glyph, parse_glyph, OPCODES};
use flux_lang::program::Module;

// ---------------------------------------------------------------------------
// The shared triage fixture
// ---------------------------------------------------------------------------

/// The epic's shared fixture in canonical (post-L-93) Flux source — the same text the Railflux
/// goldens project, so both notations demonstrably meet at one AST.
const TRIAGE_FLUX: &str = r#"flow triage(ticket: Ticket) -> Answer
  kind = classify(ticket)

  parallel
    branch docs
      search(query: ticket)
    branch hits
      grep(pattern: ticket.title)

  match kind
    case "bug"
      confirm "Open issue?" risk medium
        issue = create_issue({ ticket, hits })
        return issue
    default
      return docs
"#;

/// The same flow in Flux Glyph. This is the byte-exact document `format_glyph` emits for the
/// fixture's AST, so the notation is a fixed point: read it, write it, get this back.
const TRIAGE_GLYPH: &str = r#"F triage(ticket:Ticket)>Answer
= kind classify(ticket)
&
  | docs
    search(query: ticket)
  | hits
    grep(pattern: ticket.title)
?= kind
  | "bug"
    !? "Open issue?" medium
      = issue create_issue(hits, ticket)
      ^ issue
  |*
    ^ docs
"#;

fn triage_ast() -> DraftAst {
    match Module::parse_str(TRIAGE_FLUX).expect("the triage fixture is canonical Flux") {
        Module::Flow(ast) => ast,
        Module::Program(_) => panic!("the triage fixture is a bare flow, not a program"),
    }
}

#[test]
fn the_triage_glyph_document_parses_to_the_canonical_flux_ast() {
    let from_glyph = parse_glyph(TRIAGE_GLYPH).expect("the fixture is valid Glyph");
    assert_eq!(
        from_glyph,
        triage_ast(),
        "Glyph and canonical Flux must meet at the same AST"
    );
}

#[test]
fn the_triage_ast_formats_to_the_pinned_glyph_document() {
    assert_eq!(format_glyph(&triage_ast()), TRIAGE_GLYPH);
}

#[test]
fn the_pinned_glyph_document_is_a_fixed_point() {
    let once = format_glyph(&parse_glyph(TRIAGE_GLYPH).unwrap());
    assert_eq!(once, TRIAGE_GLYPH);
    // Deterministic: formatting the same AST twice is byte-identical.
    assert_eq!(format_glyph(&triage_ast()), format_glyph(&triage_ast()));
}

// ---------------------------------------------------------------------------
// The vocabulary
// ---------------------------------------------------------------------------

#[test]
fn the_opcode_vocabulary_is_exactly_the_designs() {
    // `docs/designs/flux-notation-workbench.md`: "`F` flow, `=` bind, `^` return, `?` conditional,
    // `?=` match, `?~` route, `|` case, `|*` default, `&` parallel, `||` race, `??` fallback,
    // `!?` confirm, `!!` assert, and `~=` memo. `@{...}` is the raw-node escape."
    let mut expected = vec![
        "F", "=", "^", "?", "?=", "?~", "|", "|*", "&", "||", "??", "!?", "!!", "~=",
    ];
    expected.sort_unstable();
    let mut actual: Vec<&str> = OPCODES.iter().map(|(op, _)| *op).collect();
    actual.sort_unstable();
    assert_eq!(
        actual, expected,
        "the Glyph vocabulary drifted from the design"
    );
}

#[test]
fn the_escape_carries_a_compact_raw_json_node() {
    // `@{…}` is the one escape: a node with no native Glyph spelling travels as its wire JSON.
    // A bind carrying an `@effect(…)` marker is two canonical lines, so it has no Glyph spelling.
    let ast = DraftAst {
        body: vec![Node::Bind {
            name: SymbolName("x".into()),
            value: Box::new(Node::Lit {
                value: serde_json::json!(1),
            }),
            ty: None,
            effect: Some(FlowEffect::Read),
        }],
        ..Default::default()
    };
    assert_eq!(
        format_glyph(&ast),
        "F\n@{\"kind\":\"bind\",\"name\":\"x\",\"value\":{\"kind\":\"lit\",\"value\":1},\"effect\":\"read\"}\n"
    );
    assert_eq!(parse_glyph(&format_glyph(&ast)).unwrap(), ast);
}

#[test]
fn a_canonical_one_liner_stays_readable_rather_than_escaping() {
    // The escape is the *last* resort: any node whose canonical Flux spelling is a single line is
    // carried through verbatim, so Glyph stays readable instead of degenerating into JSON.
    let ast = DraftAst {
        body: vec![Node::Checkpoint {
            label: "phase-1".into(),
        }],
        ..Default::default()
    };
    assert_eq!(format_glyph(&ast), "F\ncheckpoint \"phase-1\"\n");
    assert_eq!(parse_glyph(&format_glyph(&ast)).unwrap(), ast);
}

// ---------------------------------------------------------------------------
// Native core plus escape — totality over every node kind
// ---------------------------------------------------------------------------

/// One representative per `Node` kind. The `kind` string is the serde tag, checked below against the
/// generated node-kind catalog, so a new variant cannot land without a Glyph round-trip decision.
fn variant_samples() -> Vec<(&'static str, Node)> {
    let read = |name: &str| Node::Var {
        name: SymbolName(name.into()),
    };
    let call = |op: &str| Node::Call {
        op: op.into(),
        args: vec![],
    };
    let body = || vec![call("step")];

    vec![
        (
            "call",
            Node::Call {
                op: "read".into(),
                args: vec![Node::Lit {
                    value: serde_json::json!("README.md"),
                }],
            },
        ),
        (
            "bind",
            Node::Bind {
                name: SymbolName("doc".into()),
                value: Box::new(Node::Call {
                    op: "read".into(),
                    args: vec![read("path")],
                }),
                ty: Some(TypeRef::String),
                effect: Some(FlowEffect::Read),
            },
        ),
        (
            "when",
            Node::When {
                cond: Box::new(read("ok")),
                then: body(),
                otherwise: vec![call("other")],
            },
        ),
        (
            "repeat",
            Node::Repeat {
                max: 3,
                until: Some(Box::new(read("done"))),
                body: body(),
                collect: Some(SymbolName("all".into())),
            },
        ),
        (
            "each",
            Node::Each {
                source: Box::new(read("files")),
                item: SymbolName("f".into()),
                body: body(),
                collect: Some(SymbolName("all".into())),
                flat: true,
            },
        ),
        (
            "assert",
            Node::Assert {
                cond: Box::new(read("ok")),
                message: Some("must hold".into()),
            },
        ),
        (
            "pipe",
            Node::Pipe {
                steps: body(),
                bind: Some(SymbolName("out".into())),
            },
        ),
        (
            "seq",
            Node::Seq {
                body: body(),
                bind: None,
            },
        ),
        (
            "memo",
            Node::Memo {
                name: SymbolName("survey".into()),
                value: Box::new(call("scan")),
                ty: None,
                effect: None,
            },
        ),
        (
            "parallel",
            Node::Parallel {
                branches: vec![Branch {
                    name: SymbolName("left".into()),
                    body: body(),
                }],
            },
        ),
        (
            "await",
            Node::Await {
                binding: Some(SymbolName("reply".into())),
                source: "inbox".into(),
                as_type: Some(TypeRef::String),
                condition: Some(Box::new(read("needed"))),
            },
        ),
        (
            "retry",
            Node::Retry {
                max: 3,
                backoff: Some("linear".into()),
                delay_ms: Some(250),
                body: body(),
                bind: Some(SymbolName("out".into())),
            },
        ),
        (
            "try",
            Node::Try {
                body: body(),
                catch: Some(SymbolName("err".into())),
                handler: vec![call("recover")],
            },
        ),
        (
            "confirm",
            Node::Confirm {
                message: "Proceed?".into(),
                risk: None,
                body: body(),
            },
        ),
        (
            "loop",
            Node::Loop {
                for_ms: 60_000,
                every_ms: 1_000,
                until: Some(Box::new(read("done"))),
                body: body(),
                bind: None,
            },
        ),
        (
            "race",
            Node::Race {
                timeout_ms: 5_000,
                branches: vec![Branch {
                    name: SymbolName("fast".into()),
                    body: body(),
                }],
                bind: Some(SymbolName("winner".into())),
            },
        ),
        (
            "throttle",
            Node::Throttle {
                name: "sends".into(),
                max: 5,
                window_ms: 60_000,
                body: body(),
            },
        ),
        (
            "debounce",
            Node::Debounce {
                name: "saves".into(),
                wait_ms: 200,
                body: body(),
            },
        ),
        (
            "unless",
            Node::Unless {
                cond: Box::new(read("skip")),
                body: body(),
            },
        ),
        (
            "verify",
            Node::Verify {
                cmd: Box::new(call("build")),
                expect: Box::new(Node::Lit {
                    value: serde_json::json!("ok"),
                }),
                message: Some("build must pass".into()),
            },
        ),
        (
            "return",
            Node::Return {
                value: Box::new(read("answer")),
            },
        ),
        (
            "peek",
            Node::Peek {
                name: SymbolName("draft".into()),
            },
        ),
        ("var", read("draft")),
        (
            "lit",
            Node::Lit {
                value: serde_json::json!({ "n": 1 }),
            },
        ),
        (
            "thing",
            Node::Thing {
                thing: ThingRef {
                    kind: ThingKind::Person,
                    selector: Selector::Name("John".into()),
                },
            },
        ),
        (
            "expr",
            Node::Expr {
                formula: "price * 2".into(),
                vars: BTreeMap::from([("price".to_string(), Box::new(read("btc")))]),
            },
        ),
        (
            "fmt",
            Node::Fmt {
                template: "BTC: {price}".into(),
            },
        ),
        (
            "jq",
            Node::Jq {
                path: ".items[]".into(),
                input: Box::new(read("raw")),
                optional: true,
            },
        ),
        (
            "parse",
            Node::Parse {
                value: Box::new(read("raw")),
                as_type: "f64".into(),
            },
        ),
        (
            "ctx",
            Node::Ctx {
                name: SymbolName("pack".into()),
                purpose: Some("triage".into()),
                include: vec![SymbolName("a".into()), SymbolName("b".into())],
                exclude: vec![SymbolName("c".into())],
                budget: Some(2000),
            },
        ),
        (
            "ctx_append",
            Node::CtxAppend {
                ctx: SymbolName("pack".into()),
                add: vec![SymbolName("d".into())],
            },
        ),
        (
            "match",
            Node::Match {
                subject: Box::new(read("kind")),
                cases: vec![MatchCase {
                    value: Node::Lit {
                        value: serde_json::json!("bug"),
                    },
                    body: body(),
                }],
                default: vec![call("other")],
            },
        ),
        (
            "route",
            Node::Route {
                selector: Box::new(call("pick")),
                cases: vec![RouteCase {
                    label: "bug".into(),
                    body: body(),
                }],
                default: vec![],
            },
        ),
        (
            "fallback",
            Node::Fallback {
                branches: vec![FallbackBranch { body: body() }],
                bind: Some(SymbolName("out".into())),
            },
        ),
        (
            "timeout",
            Node::Timeout {
                ms: 5_000,
                body: body(),
                bind: None,
            },
        ),
        (
            "budget",
            Node::Budget {
                limit: 10,
                body: body(),
                bind: None,
            },
        ),
        (
            "cap_scope",
            Node::CapScope {
                tools: vec!["read".into(), "grep".into()],
                body: body(),
                bind: None,
            },
        ),
        (
            "scope",
            Node::Scope {
                acquire: Some(Box::new(call("lock"))),
                bind: Some(SymbolName("handle".into())),
                body: body(),
                finally: vec![call("unlock")],
            },
        ),
        (
            "saga",
            Node::Saga {
                steps: vec![SagaStep {
                    body: body(),
                    undo: vec![call("compensate")],
                }],
            },
        ),
        (
            "once",
            Node::Once {
                label: "charge".into(),
                body: body(),
                bind: Some(SymbolName("receipt".into())),
            },
        ),
        (
            "checkpoint",
            Node::Checkpoint {
                label: "phase-1".into(),
            },
        ),
        (
            "obj",
            Node::Obj {
                fields: BTreeMap::from([
                    ("n".to_string(), Box::new(read("count"))),
                    (
                        "ok".to_string(),
                        Box::new(Node::Lit {
                            value: serde_json::json!(true),
                        }),
                    ),
                ]),
            },
        ),
        (
            "list",
            Node::List {
                items: vec![
                    read("a"),
                    Node::Lit {
                        value: serde_json::json!(3),
                    },
                ],
            },
        ),
    ]
}

#[test]
fn every_node_kind_has_a_glyph_round_trip_sample() {
    let mut covered: Vec<&str> = variant_samples().iter().map(|(kind, _)| *kind).collect();
    covered.sort_unstable();
    let mut catalog: Vec<String> = flux_lang::schema::node_kind_rows()
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    catalog.sort();
    let missing: Vec<&String> = catalog
        .iter()
        .filter(|kind| !covered.contains(&kind.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "every Node kind needs a Glyph round-trip sample; missing: {missing:?}"
    );
    assert_eq!(covered.len(), catalog.len());
}

#[test]
fn every_node_kind_round_trips_through_glyph() {
    let mut broken = Vec::new();
    for (kind, node) in variant_samples() {
        // Top level, nested in a match arm, and nested in a parallel branch — the three positions
        // Glyph's own block structure owns.
        let positions: Vec<(&str, DraftAst)> = vec![
            (
                "top level",
                DraftAst {
                    body: vec![node.clone()],
                    ..Default::default()
                },
            ),
            (
                "match case body",
                DraftAst {
                    body: vec![Node::Match {
                        subject: Box::new(Node::Var {
                            name: SymbolName("k".into()),
                        }),
                        cases: vec![MatchCase {
                            value: Node::Lit {
                                value: serde_json::json!("a"),
                            },
                            body: vec![node.clone()],
                        }],
                        default: vec![node.clone()],
                    }],
                    ..Default::default()
                },
            ),
            (
                "parallel branch body",
                DraftAst {
                    body: vec![Node::Parallel {
                        branches: vec![Branch {
                            name: SymbolName("b".into()),
                            body: vec![node.clone()],
                        }],
                    }],
                    ..Default::default()
                },
            ),
        ];
        for (where_, ast) in positions {
            let text = format_glyph(&ast);
            match parse_glyph(&text) {
                Ok(back) if back == ast => {}
                Ok(back) => broken.push(format!(
                    "{kind} ({where_}) changed shape\n  glyph: {text}\n  back:  {back:?}"
                )),
                Err(e) => broken.push(format!("{kind} ({where_}) failed to re-read: {e}\n{text}")),
            }
        }
    }
    assert!(
        broken.is_empty(),
        "{} Glyph round-trip failure(s):\n{}",
        broken.len(),
        broken.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The property: parse_glyph(format_glyph(ast)) == ast
// ---------------------------------------------------------------------------

/// Deterministic xorshift64* — the same shape `roundtrip_property.rs` uses, for the same reason: a
/// reproducible seed is all the shrinking this size of corpus needs, and the workspace has no
/// proptest dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
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

/// Name slots: spellable identifiers beside adversarial ones (dots, spaces, empty, keywords,
/// sigils, newlines) — every one of which must push its node onto the escape rather than corrupt it.
const SYMS: &[&str] = &[
    "x", "ok", "a_b", "n0", "docs", "hits", "a.b", "a b", "", "a-b", "do", "until", "$x", "a\nb",
    "|", "F",
];

/// Free-form string slots (confirm messages, route labels, assert messages).
const STRINGS: &[&str] = &[
    "",
    "hello",
    "line\nbreak",
    "he said \"hi\"",
    "hash # inside",
    "@{\"kind\":\"var\"}",
    "  leading space",
];

/// Bare-word slots for `confirm`'s free-form `risk` field — only a single word is native.
const RISKS: &[&str] = &["low", "medium", "high", "critical", "a b", ""];

fn sym(rng: &mut Rng) -> SymbolName {
    SymbolName::from(*rng.pick(SYMS))
}

fn leaf(rng: &mut Rng, samples: &[(&'static str, Node)]) -> Node {
    samples[rng.below(samples.len())].1.clone()
}

fn gen_body(rng: &mut Rng, depth: usize, samples: &[(&'static str, Node)]) -> Vec<Node> {
    let n = rng.below(3);
    (0..n).map(|_| gen_node(rng, depth, samples)).collect()
}

fn gen_node(rng: &mut Rng, depth: usize, samples: &[(&'static str, Node)]) -> Node {
    if depth == 0 {
        return leaf(rng, samples);
    }
    let d = depth - 1;
    match rng.below(13) {
        0 => Node::Bind {
            name: sym(rng),
            value: Box::new(leaf(rng, samples)),
            ty: rng.chance(30).then_some(TypeRef::Named("Ticket".into())),
            effect: rng.chance(20).then_some(FlowEffect::Read),
        },
        1 => Node::Memo {
            name: sym(rng),
            value: Box::new(leaf(rng, samples)),
            ty: None,
            effect: None,
        },
        2 => Node::Return {
            value: Box::new(leaf(rng, samples)),
        },
        3 => Node::When {
            cond: Box::new(leaf(rng, samples)),
            then: gen_body(rng, d, samples),
            otherwise: if rng.chance(50) {
                gen_body(rng, d, samples)
            } else {
                vec![]
            },
        },
        4 => Node::Match {
            subject: Box::new(leaf(rng, samples)),
            cases: (0..rng.below(3))
                .map(|_| MatchCase {
                    value: leaf(rng, samples),
                    body: gen_body(rng, d, samples),
                })
                .collect(),
            default: if rng.chance(50) {
                gen_body(rng, d, samples)
            } else {
                vec![]
            },
        },
        5 => Node::Route {
            selector: Box::new(leaf(rng, samples)),
            cases: (0..rng.below(3))
                .map(|_| RouteCase {
                    label: (*rng.pick(STRINGS)).to_string(),
                    body: gen_body(rng, d, samples),
                })
                .collect(),
            default: if rng.chance(50) {
                gen_body(rng, d, samples)
            } else {
                vec![]
            },
        },
        6 => Node::Parallel {
            // Deliberately allowed to collide: two branches with the same name have no unambiguous
            // Glyph spelling, so the whole node must take the escape instead.
            branches: (0..rng.below(3))
                .map(|_| Branch {
                    name: sym(rng),
                    body: gen_body(rng, d, samples),
                })
                .collect(),
        },
        7 => Node::Race {
            timeout_ms: *rng.pick(&[0u64, 250, 1_000, 60_000]),
            branches: (0..rng.below(3))
                .map(|_| Branch {
                    name: sym(rng),
                    body: gen_body(rng, d, samples),
                })
                .collect(),
            bind: rng.chance(50).then(|| sym(rng)),
        },
        8 => Node::Fallback {
            branches: (0..rng.below(3))
                .map(|_| FallbackBranch {
                    body: gen_body(rng, d, samples),
                })
                .collect(),
            bind: rng.chance(50).then(|| sym(rng)),
        },
        9 => Node::Confirm {
            message: (*rng.pick(STRINGS)).to_string(),
            risk: rng.chance(70).then(|| (*rng.pick(RISKS)).to_string()),
            body: gen_body(rng, d, samples),
        },
        10 => Node::Assert {
            cond: Box::new(leaf(rng, samples)),
            message: rng.chance(50).then(|| (*rng.pick(STRINGS)).to_string()),
        },
        11 => Node::Seq {
            body: gen_body(rng, d, samples),
            bind: rng.chance(50).then(|| sym(rng)),
        },
        _ => leaf(rng, samples),
    }
}

#[test]
fn random_asts_round_trip_through_glyph() {
    let samples = variant_samples();
    for seed in 0..400u64 {
        let mut rng = Rng::new(seed);
        let ast = DraftAst {
            // The header has no escape (`format`'s documented flow-header exception, inherited),
            // so header names are drawn from spellable pools only.
            name: rng.chance(70).then(|| "triage".to_string()),
            params: (0..rng.below(3))
                .map(|i| Param {
                    name: SymbolName(format!("p{i}")),
                    ty: if rng.chance(50) {
                        TypeRef::Named("Ticket".into())
                    } else {
                        TypeRef::List(Box::new(TypeRef::String))
                    },
                })
                .collect(),
            returns: rng.chance(50).then(|| TypeRef::Named("Answer".into())),
            body: gen_body(&mut rng, 3, &samples),
        };
        let text = format_glyph(&ast);
        let back = parse_glyph(&text)
            .unwrap_or_else(|e| panic!("seed {seed}: Glyph did not re-read: {e}\n{text}"));
        assert_eq!(back, ast, "seed {seed} did not round-trip\n{text}");
    }
}

// ---------------------------------------------------------------------------
// Fail-closed diagnostics
// ---------------------------------------------------------------------------

/// The diagnostic for `src`, which must be rejected.
fn rejected(src: &str) -> String {
    parse_glyph(src)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| panic!("this Glyph must be rejected:\n{src}"))
}

#[test]
fn an_odd_indent_names_its_line() {
    let d = rejected("F\n= a 1\n   ^ a\n");
    assert!(d.contains("line 3"), "{d}");
    assert!(d.contains("two spaces"), "{d}");
}

#[test]
fn an_over_indented_line_names_its_line() {
    let d = rejected("F\n? ok\n      ^ a\n");
    assert!(d.contains("line 3"), "{d}");
    assert!(d.contains("one level"), "{d}");
}

#[test]
fn a_tab_is_never_indentation() {
    let d = rejected("F\n? ok\n\t^ a\n");
    assert!(d.contains("line 3"), "{d}");
    assert!(d.contains("tab"), "{d}");
}

#[test]
fn an_unknown_opcode_names_its_line() {
    let d = rejected("F\n= a 1\n?! ok\n");
    assert!(d.contains("line 3"), "{d}");
    assert!(d.contains("?!"), "{d}");
    assert!(d.contains("opcode"), "{d}");
}

#[test]
fn an_arm_outside_an_arm_taking_construct_names_its_line() {
    let d = rejected("F\n| \"bug\"\n");
    assert!(d.contains("line 2"), "{d}");
    assert!(d.contains("arm"), "{d}");
}

#[test]
fn a_statement_inside_a_match_is_not_an_arm() {
    let d = rejected("F\n?= kind\n  ^ a\n");
    assert!(d.contains("line 3"), "{d}");
    assert!(d.contains("arm"), "{d}");
}

#[test]
fn a_labelled_arm_inside_a_conditional_is_rejected() {
    // `?` takes only the `|*` else arm; a labelled `|` case there would be a guess.
    let d = rejected("F\n? ok\n  | \"bug\"\n    ^ a\n");
    assert!(d.contains("line 3"), "{d}");
    assert!(d.contains("|*"), "{d}");
}

#[test]
fn a_default_arm_inside_a_parallel_is_rejected() {
    let d = rejected("F\n&\n  |*\n    ^ a\n");
    assert!(d.contains("line 3"), "{d}");
    assert!(d.contains("default"), "{d}");
}

#[test]
fn a_duplicate_parallel_branch_name_names_its_line() {
    let d = rejected("F\n&\n  | docs\n    ^ a\n  | docs\n    ^ b\n");
    assert!(d.contains("line 5"), "{d}");
    assert!(d.contains("docs"), "{d}");
    assert!(d.contains("duplicate"), "{d}");
}

#[test]
fn a_duplicate_default_arm_names_its_line() {
    let d = rejected("F\n?= kind\n  |*\n    ^ a\n  |*\n    ^ b\n");
    assert!(d.contains("line 5"), "{d}");
    assert!(d.contains("duplicate"), "{d}");
}

#[test]
fn a_default_arm_before_a_case_names_its_line() {
    let d = rejected("F\n?= kind\n  |*\n    ^ a\n  | \"bug\"\n    ^ b\n");
    assert!(d.contains("line 3") || d.contains("line 5"), "{d}");
    assert!(d.contains("last"), "{d}");
}

#[test]
fn a_malformed_escape_names_its_line() {
    for bad in [
        "F\n@{not json}\n",
        "F\n@{\"kind\":\"nope\"}\n",
        "F\n@\n",
        "F\n@json {\"kind\":\"var\",\"name\":\"a\"}\n",
    ] {
        let d = rejected(bad);
        assert!(d.contains("line 2"), "{bad} -> {d}");
        assert!(d.contains("escape"), "{bad} -> {d}");
    }
}

#[test]
fn a_pass_through_statement_may_not_carry_a_body() {
    // Glyph owns block structure through its opcodes; a canonical one-liner is always a leaf.
    let d = rejected("F\nread(\"a\")\n  ^ a\n");
    assert!(d.contains("line 3"), "{d}");
}

#[test]
fn the_flow_header_may_appear_only_once_and_first() {
    let d = rejected("F a\nF b\n");
    assert!(d.contains("line 2"), "{d}");
    let d = rejected("= a 1\nF b\n");
    assert!(d.contains("line 2"), "{d}");
}

#[test]
fn a_bind_without_a_value_is_rejected_rather_than_guessed() {
    let d = rejected("F\n= a\n");
    assert!(d.contains("line 2"), "{d}");
}

#[test]
fn a_canonical_expression_error_is_reported_at_its_glyph_line() {
    // The expression grammar is canonical Flux's; its diagnostic must still name the *Glyph* line
    // the author can see. Blank and comment lines carry no structure, so the Glyph line number and
    // the canonical one deliberately diverge here — the bad statement is Glyph line 9 but only the
    // fifth canonical line, and the reader must report the former.
    let d = rejected("# the triage flow\n\nF\n\n= a 1\n\n?= kind\n  | \"bug\"\n    = b read(((\n");
    assert!(d.contains("line 9"), "{d}");
    assert!(
        !d.contains("line 5"),
        "the canonical line leaked through: {d}"
    );
}

// ---------------------------------------------------------------------------
// Explicit selection — no sniffing, no change to `.flux`
// ---------------------------------------------------------------------------

#[test]
fn glyph_is_never_sniffed_from_or_into_canonical_flux() {
    // Canonical `.flux` loading is untouched: Glyph is not accepted by the `.flux` entry points…
    assert!(
        flux_lang::parse::parse(TRIAGE_GLYPH).is_err(),
        "the .flux parser must not accept Glyph"
    );
    assert!(
        Module::parse_str(TRIAGE_GLYPH).is_err(),
        "the .flux module loader must not accept Glyph"
    );
    // …and the Glyph reader is not a second canonical parser either: conversion is explicit in
    // both directions.
    assert!(
        parse_glyph(TRIAGE_FLUX).is_err(),
        "the Glyph reader must not accept canonical Flux source"
    );
}

#[test]
fn canonical_flux_still_formats_as_canonical_flux() {
    // Nothing about the canonical surface moved: `format`/`parse` are byte-for-byte unaffected.
    let ast = triage_ast();
    assert_eq!(
        flux_lang::parse::parse(&flux_lang::format::format(&ast)).unwrap(),
        ast
    );
}
