//! L-95 — Railflux goldens: a `DraftAst` projected as a 7-bit ASCII dataflow diagram.
//!
//! Three guarantees are pinned here, because they are what a later reader (L-100) will depend on:
//!
//! 1. **The shared fixture.** `docs/designs/flux-notation-workbench.md` names the `triage` flow as
//!    the epic's golden fixture; the first test pins its parallel fan-out, match arms, confirmation
//!    gate, calls, bindings and returns byte-for-byte.
//! 2. **Totality.** Every `Node` variant in the generated node-kind catalog has an explicit
//!    expected rendering here — a new variant fails this file rather than silently rendering as a
//!    headless leaf.
//! 3. **The canonical envelope.** Output is strictly 7-bit ASCII and byte-identical for equal ASTs.

use std::collections::BTreeMap;

use flux_lang::ast::{
    Branch, DraftAst, FallbackBranch, FlowEffect, MatchCase, Node, Param, RouteCase, SagaStep,
    Selector, SymbolName, ThingKind, ThingRef, TypeRef,
};
use flux_lang::program::Module;
use flux_lang::render::render_rail;

/// The epic's shared fixture, in canonical (post-L-93) Flux source. Parsing it here rather than
/// hand-building the AST keeps the golden honest: the diagram below is a projection of what the
/// real parser produces for real source.
const TRIAGE: &str = r#"flow triage(ticket: Ticket) -> Answer
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

fn triage_ast() -> DraftAst {
    match Module::parse_str(TRIAGE).expect("the triage fixture is canonical Flux") {
        Module::Flow(ast) => ast,
        Module::Program(_) => panic!("the triage fixture is a bare flow, not a program"),
    }
}

#[test]
fn the_triage_flow_renders_the_designs_railflux_shape() {
    let expected = "\
[flow triage (ticket: Ticket) -> Answer]
  ticket --> classify(.) --> kind
  [parallel]
    [branch] --> docs
      ticket --> search(query: .)
    [branch] --> hits
      ticket.title --> grep(pattern: .)
  [match kind]
    [case \"bug\"]
      [confirm \"Open issue?\" risk: \"medium\"]
        hits, ticket --> create_issue(hits, ticket) --> issue
        issue --> RETURN
    [default]
      docs --> RETURN
";
    assert_eq!(render_rail(&triage_ast()), expected);
}

#[test]
fn identical_asts_render_byte_identically() {
    let ast = triage_ast();
    let other = triage_ast();
    assert_eq!(ast, other, "the fixture parses deterministically");
    assert_eq!(render_rail(&ast), render_rail(&other));
    // And a second render of the *same* value is stable (no interior mutability / hashing order).
    assert_eq!(render_rail(&ast), render_rail(&ast));
}

#[test]
fn canonical_output_is_strictly_seven_bit_ascii() {
    // Non-ASCII reaches the renderer through every content channel there is: a symbol name, an op
    // name, a string literal, a `fmt` template, a `confirm` message, a thing selector, a type name.
    // None of them may leak a byte above 0x7F — nor a raw control byte, which would break the
    // line-per-statement structure the diagram depends on.
    let ast = DraftAst {
        name: Some("triage—naïve".into()),
        params: vec![Param {
            name: SymbolName("tícket".into()),
            ty: TypeRef::Named("Tîcket".into()),
        }],
        returns: None,
        body: vec![
            Node::Bind {
                name: SymbolName("résumé".into()),
                value: Box::new(Node::Call {
                    op: "señal".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!("→ ünicode ✓ \u{1F600}"),
                    }],
                }),
                ty: None,
                effect: None,
            },
            Node::Confirm {
                message: "Löschen?\nJa".into(),
                risk: Some("hoch".into()),
                body: vec![Node::Fmt {
                    template: "ok — {résumé}".into(),
                }],
            },
            Node::Thing {
                thing: ThingRef {
                    kind: ThingKind::Custom("bücher".into()),
                    selector: Selector::Name("Wörterbuch".into()),
                },
            },
        ],
    };
    let out = render_rail(&ast);
    assert!(out.is_ascii(), "railflux must be 7-bit ASCII, got: {out:?}");
    for ch in out.chars() {
        assert!(
            ch == '\n' || !ch.is_control(),
            "railflux must not emit raw control bytes, got {ch:?} in {out:?}"
        );
    }
    // Nothing is dropped on the way — the escapes carry the content.
    assert!(out.contains("\\u00e9"), "escaped content survives: {out}");
}

/// One sample per `Node` variant, with the exact diagram it must produce. The `kind` string is the
/// serde tag, checked below against the generated node-kind catalog so a new variant cannot land
/// without a rendering decision.
fn variant_samples() -> Vec<(&'static str, Node, &'static str)> {
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
            "  --> read(\"README.md\")\n",
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
            "  path --> read(.) --> doc: String !read\n",
        ),
        (
            "when",
            Node::When {
                cond: Box::new(read("ok")),
                then: body(),
                otherwise: vec![call("other")],
            },
            "  [when ok]\n    [then]\n      --> step()\n    [else]\n      --> other()\n",
        ),
        (
            "repeat",
            Node::Repeat {
                max: 3,
                until: Some(Box::new(read("done"))),
                body: body(),
                collect: Some(SymbolName("all".into())),
            },
            "  [repeat 3 until: done collect: all]\n    --> step()\n",
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
            "  [each f in files collect: all flat: true]\n    --> step()\n",
        ),
        (
            "assert",
            Node::Assert {
                cond: Box::new(read("ok")),
                message: Some("must hold".into()),
            },
            "  [assert ok message: \"must hold\"]\n",
        ),
        (
            "pipe",
            Node::Pipe {
                steps: body(),
                bind: Some(SymbolName("out".into())),
            },
            "  [pipe] --> out\n    --> step()\n",
        ),
        (
            "seq",
            Node::Seq {
                body: body(),
                bind: None,
            },
            "  [seq]\n    --> step()\n",
        ),
        (
            "memo",
            Node::Memo {
                name: SymbolName("survey".into()),
                value: Box::new(call("scan")),
                ty: None,
                effect: Some(FlowEffect::Read),
            },
            "  --> scan() --> memo survey !read\n",
        ),
        (
            "parallel",
            Node::Parallel {
                branches: vec![Branch {
                    name: SymbolName("left".into()),
                    body: body(),
                }],
            },
            "  [parallel]\n    [branch] --> left\n      --> step()\n",
        ),
        (
            "await",
            Node::Await {
                binding: Some(SymbolName("reply".into())),
                source: "inbox".into(),
                as_type: Some(TypeRef::String),
                condition: Some(Box::new(read("needed"))),
            },
            "  [await \"inbox\" as: String when: needed] --> reply\n",
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
            "  [retry 3 backoff: \"linear\" delay: 250ms] --> out\n    --> step()\n",
        ),
        (
            "try",
            Node::Try {
                body: body(),
                catch: Some(SymbolName("err".into())),
                handler: vec![call("recover")],
            },
            "  [try]\n    [do]\n      --> step()\n    [catch err]\n      --> recover()\n",
        ),
        (
            "confirm",
            Node::Confirm {
                message: "Proceed?".into(),
                risk: None,
                body: body(),
            },
            "  [confirm \"Proceed?\"]\n    --> step()\n",
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
            "  [loop 1m every: 1s until: done]\n    --> step()\n",
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
            "  [race 5s] --> winner\n    [branch] --> fast\n      --> step()\n",
        ),
        (
            "throttle",
            Node::Throttle {
                name: "sends".into(),
                max: 5,
                window_ms: 60_000,
                body: body(),
            },
            "  [throttle \"sends\" max: 5 window: 1m]\n    --> step()\n",
        ),
        (
            "debounce",
            Node::Debounce {
                name: "saves".into(),
                wait_ms: 200,
                body: body(),
            },
            "  [debounce \"saves\" wait: 200ms]\n    --> step()\n",
        ),
        (
            "unless",
            Node::Unless {
                cond: Box::new(read("skip")),
                body: body(),
            },
            "  [unless skip]\n    --> step()\n",
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
            "  [verify build() contains \"ok\" message: \"build must pass\"]\n",
        ),
        (
            "return",
            Node::Return {
                value: Box::new(read("answer")),
            },
            "  answer --> RETURN\n",
        ),
        (
            "peek",
            Node::Peek {
                name: SymbolName("draft".into()),
            },
            "  draft --> [peek draft]\n",
        ),
        ("var", read("draft"), "  draft --> [.]\n"),
        (
            "lit",
            Node::Lit {
                value: serde_json::json!({ "n": 1 }),
            },
            "  --> [{\"n\":1}]\n",
        ),
        (
            "thing",
            Node::Thing {
                thing: ThingRef {
                    kind: ThingKind::Person,
                    selector: Selector::Name("John".into()),
                },
            },
            "  --> [thing person name \"John\"]\n",
        ),
        (
            "expr",
            Node::Expr {
                formula: "price * 2".into(),
                vars: BTreeMap::from([("price".to_string(), Box::new(read("btc")))]),
            },
            "  btc --> [expr(\"price * 2\", price: .)]\n",
        ),
        (
            "fmt",
            Node::Fmt {
                template: "BTC: {price}".into(),
            },
            "  --> [fmt(\"BTC: {price}\")]\n",
        ),
        (
            "jq",
            Node::Jq {
                // A path with no field-access spelling keeps the explicit form, traversal flag
                // and all. The readable `ticket.title` chain form is pinned by the triage golden.
                path: ".items[]".into(),
                input: Box::new(read("raw")),
                optional: true,
            },
            "  raw --> [jq(\".items[]\", ., optional: true)]\n",
        ),
        (
            "parse",
            Node::Parse {
                value: Box::new(read("raw")),
                as_type: "f64".into(),
            },
            "  raw --> [parse(., as: \"f64\")]\n",
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
            "  [ctx purpose: \"triage\" include: [a, b] exclude: [c] budget: 2000] --> pack\n",
        ),
        (
            "ctx_append",
            Node::CtxAppend {
                ctx: SymbolName("pack".into()),
                add: vec![SymbolName("d".into())],
            },
            "  [ctx_append add: [d]] --> pack\n",
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
            "  [match kind]\n    [case \"bug\"]\n      --> step()\n    [default]\n      --> other()\n",
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
            "  [route pick()]\n    [case \"bug\"]\n      --> step()\n",
        ),
        (
            "fallback",
            Node::Fallback {
                branches: vec![FallbackBranch { body: body() }],
                bind: Some(SymbolName("out".into())),
            },
            "  [fallback] --> out\n    [branch]\n      --> step()\n",
        ),
        (
            "timeout",
            Node::Timeout {
                ms: 5_000,
                body: body(),
                bind: None,
            },
            "  [timeout 5s]\n    --> step()\n",
        ),
        (
            "budget",
            Node::Budget {
                limit: 10,
                body: body(),
                bind: None,
            },
            "  [budget 10]\n    --> step()\n",
        ),
        (
            "cap_scope",
            Node::CapScope {
                tools: vec!["read".into(), "grep".into()],
                body: body(),
                bind: None,
            },
            "  [with_tools [\"read\", \"grep\"]]\n    --> step()\n",
        ),
        (
            "scope",
            Node::Scope {
                acquire: Some(Box::new(call("lock"))),
                bind: Some(SymbolName("handle".into())),
                body: body(),
                finally: vec![call("unlock")],
            },
            "  [scope] --> handle\n    [acquire]\n      --> lock()\n    [do]\n      --> step()\n    [finally]\n      --> unlock()\n",
        ),
        (
            "saga",
            Node::Saga {
                steps: vec![SagaStep {
                    body: body(),
                    undo: vec![call("compensate")],
                }],
            },
            "  [saga]\n    [step]\n      [do]\n        --> step()\n      [undo]\n        --> compensate()\n",
        ),
        (
            "once",
            Node::Once {
                label: "charge".into(),
                body: body(),
                bind: Some(SymbolName("receipt".into())),
            },
            "  [once \"charge\"] --> receipt\n    --> step()\n",
        ),
        (
            "checkpoint",
            Node::Checkpoint {
                label: "phase-1".into(),
            },
            "  [checkpoint \"phase-1\"]\n",
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
            "  count --> [{ n: ., ok: true }]\n",
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
            "  a --> [[ ., 3 ]]\n",
        ),
    ]
}

#[test]
fn every_node_kind_has_an_explicit_railflux_rendering() {
    let mut covered: Vec<&str> = variant_samples().iter().map(|(kind, ..)| *kind).collect();
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
        "every Node kind needs a Railflux rendering sample; missing: {missing:?}"
    );
    assert_eq!(
        covered.len(),
        catalog.len(),
        "the sample table carries an unknown kind: {covered:?} vs {catalog:?}"
    );
}

#[test]
fn each_node_kind_renders_its_pinned_diagram() {
    let mut drifted = Vec::new();
    for (kind, node, expected) in variant_samples() {
        let ast = DraftAst {
            body: vec![node],
            ..Default::default()
        };
        let out = render_rail(&ast);
        assert!(out.is_ascii(), "{kind} must render as 7-bit ASCII: {out:?}");
        let body = out
            .strip_prefix("[flow]\n")
            .unwrap_or_else(|| panic!("{kind}: unexpected header in {out:?}"));
        if body != expected {
            drifted.push(format!(
                "{kind}\n  expected: {expected:?}\n  actual:   {body:?}"
            ));
        }
    }
    assert!(
        drifted.is_empty(),
        "railflux rendering drifted for {} kind(s):\n{}",
        drifted.len(),
        drifted.join("\n")
    );
}
