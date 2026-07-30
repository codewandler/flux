//! `parse` — read canonical Flux-Lang **text** back into a [`DraftAst`]. The round-trip partner of
//! [`crate::format`]: `parse(&format(&ast)) == ast` for every `DraftAst` (the supported subset natively,
//! everything else via the `@json` escape; the flow *header* is the one documented exception — see
//! [`crate::format`]'s "flow-header exception"). The accepting grammar is the lossless CST in
//! [`crate::parser`]; this module is the strict public wrapper around structured CST lowering.
//!
//! It is **total**: malformed input returns [`FlowError::Parse`], never a panic. Errors carry a
//! `line N:` prefix (1-based source line, counting comment/blank lines) wherever a line is in scope,
//! so the model repair loop can point at the offending statement.
//!
//! # Declared vs. referenced names
//! A *declared* symbol (a bind target, `each` item, `-> $bind` arrow target, `ctx` name, sym-list
//! entry, `parallel` branch name) is a plain identifier: ASCII alphanumerics and `_` only. `.` is
//! deliberately **not** a declared-name character — in expression position `$a.b` is field-access
//! sugar for `jq(".b", $a)`, so a dotted *declared* name would silently change meaning through a
//! format→parse cycle (`$a.b = 1` is a parse error, not a bind named `a.b`).
//!
//! # Surface (see [`crate::format`] for the full grammar)
//! - Header: `flow [<name>][(<param>, …)][ -> <type>]`, body indented 2 (or any consistent step).
//! - `x = <expr>`, `x: T = <expr>` — bind; `$x` stays accepted as a legacy/reserved-word escape.
//! - `<op>(<arg>, …)` — a call whether its result is used or discarded; legacy `do` is accepted.
//! - `pack += a, b` — ctx_append; `ctx p` + indented `purpose`/`budget`/`include`/`exclude`.
//! - `when`/`else`, `unless`, `each x in <src> [-> [flat] c]`, `repeat <n> [-> c]` (`until` first
//!   body line), `seq [-> c]`, `return <expr>`.
//! - `match <subj>`/`route <sel>` (`case <v>` arms + `default`), `fallback [-> $b]` (`branch` arms),
//!   `loop for <ms> every <ms> [-> $b]` (`until` first body line), `timeout <ms> [-> $b]`,
//!   `budget <n> [-> $b]`.
//! - Inline `fmt("<template>")` and `var.path[0]` field/index-access sugar (lowers to `jq`).
//! - `@json <compact-json>` — the wire-format escape for any unsupported node (inline or statement).
//! - A `goal "…"` header line is tolerated and ignored (`DraftAst` has no goal slot).

use crate::ast::DraftAst;
#[cfg(test)]
use crate::ast::{FlowEffect, Node, Param, SymbolName, TypeRef};
use crate::error::{FlowError, Result};
use crate::program::Module;
#[cfg(test)]
use flux_spec::{Effect, Idempotency, Risk};
use rowan::TextRange;

use crate::syntax::SyntaxNode;

/// Parse a single Flux-Lang flow from text into a [`DraftAst`].
///
/// This strict AST entry parses once through the tolerant CST grammar, refuses any recovered
/// lexer/parser error, and lowers the validated tree. Editor callers that need a partial tree call
/// [`crate::parser::parse_cst`] directly.
pub fn parse(src: &str) -> Result<DraftAst> {
    let parsed = crate::parser::parse_cst(src);
    crate::lower_cst::cst_to_draft(&parsed)
        .map(|lowered| lowered.ast)
        .map_err(|errors| lowering_error(errors, &parsed.syntax()))
}

/// Parse a `.flux` **module** from native Flux-Lang text.
///
/// The tolerant CST is the only accepting production grammar. This strict entry point rejects any
/// recovered lexer/parser error before lowering the structured tree into semantic declarations.
pub fn parse_program(src: &str) -> Result<Module> {
    let parsed = crate::parser::parse_cst(src);
    crate::lower_cst::cst_to_module(&parsed)
        .map(|lowered| lowered.module)
        .map_err(|errors| lowering_error(errors, &parsed.syntax()))
}

pub(crate) fn lowering_error(
    errors: Vec<crate::lower_cst::LowerError>,
    root: &SyntaxNode,
) -> FlowError {
    let Some(error) = errors.into_iter().next() else {
        return FlowError::Parse("CST lowering failed without a diagnostic".to_string());
    };
    let message = error
        .message
        .strip_prefix("parse error: ")
        .unwrap_or(&error.message);
    if message.starts_with("line ") {
        return FlowError::Parse(message.to_string());
    }
    match error.range {
        Some(range) => FlowError::Parse(format!("line {}: {message}", line_at_range(root, range))),
        None => FlowError::Parse(message.to_string()),
    }
}

fn line_at_range(root: &SyntaxNode, range: TextRange) -> usize {
    let offset: usize = u32::from(range.start()) as usize;
    let mut bytes = 0usize;
    let mut line = 1usize;
    for token in root
        .descendants_with_tokens()
        .filter_map(|it| it.into_token())
    {
        if bytes >= offset {
            break;
        }
        let take = (offset - bytes).min(token.text().len());
        line += token.text().as_bytes()[..take]
            .iter()
            .filter(|b| **b == b'\n')
            .count();
        bytes += token.text().len();
    }
    line
}

// ===========================================================================
// Tests — the correctness gate: parse(format(ast)) == ast for every ast.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Branch, FallbackBranch, MatchCase, RouteCase, Selector, ThingKind, ThingRef};
    use crate::format::format;

    /// The headline invariant. Every curated AST round-trips exactly through the text surface.
    fn assert_round_trips(ast: &DraftAst) {
        let text = format(ast);
        match parse(&text) {
            Ok(back) => assert_eq!(&back, ast, "round-trip mismatch.\n--- text ---\n{text}"),
            Err(e) => panic!("parse failed: {e}\n--- text ---\n{text}"),
        }
    }

    fn lit(v: serde_json::Value) -> Node {
        Node::Lit { value: v }
    }
    fn var(name: &str) -> Node {
        Node::Var { name: name.into() }
    }
    fn call(op: &str, args: Vec<Node>) -> Node {
        Node::Call {
            op: op.into(),
            args,
        }
    }
    fn bind(name: &str, value: Node) -> Node {
        Node::Bind {
            name: name.into(),
            value: Box::new(value),
            ty: None,
            effect: None,
        }
    }
    fn jq(path: &str, input: Node) -> Node {
        Node::Jq {
            optional: false,
            path: path.into(),
            input: Box::new(input),
        }
    }
    fn s(v: &str) -> serde_json::Value {
        serde_json::Value::String(v.into())
    }

    #[test]
    fn durability_nodes_round_trip_natively() {
        // memo / once / checkpoint / await — formerly @json-only (L-60).
        let ast = DraftAst {
            body: vec![
                Node::Memo {
                    name: "x".into(),
                    value: Box::new(var("y")),
                    ty: None,
                    effect: None,
                },
                Node::Memo {
                    name: "n".into(),
                    value: Box::new(lit(s("hi"))),
                    ty: Some(TypeRef::String),
                    effect: Some(FlowEffect::Read),
                },
                Node::Once {
                    label: "charge".into(),
                    body: vec![call("pay", vec![])],
                    bind: Some("receipt".into()),
                },
                Node::Checkpoint {
                    label: "phase-1".into(),
                },
                Node::Await {
                    binding: Some("reply".into()),
                    source: "user_input".into(),
                    as_type: None,
                    condition: Some(Box::new(var("decision_needed"))),
                },
                Node::Await {
                    binding: None,
                    source: "webhook".into(),
                    as_type: None,
                    condition: None,
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "durability nodes should render natively:\n{text}"
        );
        assert!(text.contains("memo x = y"), "{text}");
        assert!(text.contains("@effect(read)"), "{text}");
        assert!(text.contains("memo n: String = \"hi\""), "{text}");
        assert!(text.contains("once \"charge\" -> receipt"), "{text}");
        assert!(text.contains("checkpoint \"phase-1\""), "{text}");
        assert!(
            text.contains("await reply = \"user_input\", when: decision_needed"),
            "{text}"
        );
        assert!(text.contains("await \"webhook\""), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn guardrail_and_sugar_nodes_round_trip_natively() {
        // confirm / throttle / debounce / verify + peek / parse — formerly @json-only (L-61).
        let ast = DraftAst {
            body: vec![
                Node::Confirm {
                    message: "delete prod?".into(),
                    risk: Some("high".into()),
                    body: vec![call("delete", vec![])],
                },
                Node::Confirm {
                    message: "ok?".into(),
                    risk: None,
                    body: vec![],
                },
                Node::Throttle {
                    name: "api".into(),
                    max: 5,
                    window_ms: 1000,
                    body: vec![call("fetch", vec![])],
                },
                Node::Debounce {
                    name: "save".into(),
                    wait_ms: 250,
                    body: vec![call("persist", vec![])],
                },
                Node::Verify {
                    cmd: Box::new(call("bash", vec![lit(s("echo hi"))])),
                    expect: Box::new(lit(s("hi"))),
                    message: Some("must greet".into()),
                },
                bind("v", Node::Peek { name: "x".into() }),
                bind(
                    "n",
                    Node::Parse {
                        value: Box::new(jq(".price", var("raw"))),
                        as_type: "f64".into(),
                    },
                ),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "batch-2 nodes should render natively:\n{text}"
        );
        assert!(
            text.contains("confirm \"delete prod?\", risk: high"),
            "{text}"
        );
        assert!(text.contains("throttle \"api\", max: 5, per: 1s"), "{text}");
        assert!(text.contains("debounce \"save\", wait: 250ms"), "{text}");
        assert!(
            text.contains("verify bash(\"echo hi\") contains \"hi\": \"must greet\""),
            "{text}"
        );
        assert!(text.contains("v = peek x"), "{text}");
        assert!(text.contains("n = parse(raw.price, as: \"f64\")"), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn arm_body_control_flow_round_trips_natively() {
        // try / race / scope / saga / pipe — formerly @json-only (L-62).
        let ast = DraftAst {
            body: vec![
                Node::Try {
                    body: vec![call("risky", vec![])],
                    catch: Some("err".into()),
                    handler: vec![call("log", vec![var("err")])],
                },
                Node::Race {
                    timeout_ms: 5000,
                    branches: vec![
                        crate::ast::Branch {
                            name: "fast".into(),
                            body: vec![call("cheap", vec![])],
                        },
                        crate::ast::Branch {
                            name: "slow".into(),
                            body: vec![call("expensive", vec![])],
                        },
                    ],
                    bind: Some("winner".into()),
                },
                Node::Scope {
                    acquire: Some(Box::new(call("lock", vec![]))),
                    bind: Some("h".into()),
                    body: vec![call("use_it", vec![var("h")])],
                    finally: vec![call("release", vec![var("h")])],
                },
                Node::Saga {
                    steps: vec![
                        crate::ast::SagaStep {
                            body: vec![call("charge", vec![])],
                            undo: vec![call("refund", vec![])],
                        },
                        crate::ast::SagaStep {
                            body: vec![call("ship", vec![])],
                            undo: vec![],
                        },
                    ],
                },
                Node::Pipe {
                    steps: vec![call("a", vec![]), call("b", vec![])],
                    bind: Some("out".into()),
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "batch-3 nodes should render natively:\n{text}"
        );
        assert!(text.contains("catch err"), "{text}");
        assert!(text.contains("race 5s -> winner"), "{text}");
        assert!(text.contains("branch fast"), "{text}");
        assert!(text.contains("scope h = lock()"), "{text}");
        assert!(text.contains("finally"), "{text}");
        assert!(
            text.contains("saga") && text.contains("step") && text.contains("undo"),
            "{text}"
        );
        assert!(text.contains("pipe -> out"), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn thing_references_round_trip_natively() {
        use crate::ast::{Selector, ThingKind, ThingRef};
        let mk = |kind: ThingKind, selector: Selector| Node::Thing {
            thing: ThingRef { kind, selector },
        };
        let ast = DraftAst {
            body: vec![
                bind("f", mk(ThingKind::File, Selector::Path("src/x.rs".into()))),
                bind("p", mk(ThingKind::Person, Selector::Name("john".into()))),
                bind("u", mk(ThingKind::Url, Selector::Id("https://x".into()))),
                bind(
                    "c",
                    mk(
                        ThingKind::Custom("widget".into()),
                        Selector::Key("w-1".into()),
                    ),
                ),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "thing refs should render natively:\n{text}"
        );
        assert!(text.contains("f = thing file path \"src/x.rs\""), "{text}");
        assert!(text.contains("p = thing person name \"john\""), "{text}");
        assert!(text.contains("u = thing url id \"https://x\""), "{text}");
        assert!(
            text.contains("c = thing custom \"widget\" key \"w-1\""),
            "{text}"
        );
        assert_round_trips(&ast);
    }

    // ---- P6: new native text forms ----

    #[test]
    fn relaxed_bind_forms_are_native_and_round_trip() {
        // The bind grammar now accepts `$b = $a` (var alias) and `$x = <literal>` directly. The text
        // surface already produced these shapes; this pins that they stay native (no `@json`) and exact.
        let ast = DraftAst {
            body: vec![
                bind("b", var("a")),
                bind("n", lit(serde_json::json!(5))),
                bind("greeting", lit(s("hi"))),
                bind("xs", lit(serde_json::json!([1, 2, 3]))),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("b = a"), "var alias: {text}");
        assert!(text.contains("n = 5"), "number lit: {text}");
        assert!(!text.contains("@json"), "no json fallback: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn all_literal_and_empty_templates_use_json_while_lit_json_stays_native() {
        // The round-trip disjointness rule, pinned from both sides: an all-literal or empty `Obj`/`List`
        // has no native spelling and uses `@json` (so its text can never collide with a `Lit`'s
        // `{…}`/`[…]`), while a `Lit` holding a JSON object/array still renders as compact JSON.
        let all_lit: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj", "fields": { "ok": {"kind": "lit", "value": true} }
        }))
        .unwrap();
        let empty_obj: Node = serde_json::from_value(serde_json::json!({"kind": "obj"})).unwrap();
        let empty_list: Node = serde_json::from_value(serde_json::json!({"kind": "list"})).unwrap();
        for node in [all_lit, empty_obj, empty_list] {
            let ast = DraftAst {
                body: vec![bind("r", node)],
                ..Default::default()
            };
            assert!(
                format(&ast).contains("@json"),
                "all-literal/empty template should use @json"
            );
            assert_round_trips(&ast);
        }

        // `Lit` JSON objects/arrays stay native compact JSON (NOT @json) and round-trip as `Lit`.
        let lit_ast = DraftAst {
            body: vec![
                bind("o", lit(serde_json::json!({"a": 1}))),
                bind("xs", lit(serde_json::json!([1, 2, 3]))),
            ],
            ..Default::default()
        };
        let text = format(&lit_ast);
        assert!(!text.contains("@json"), "Lit JSON stays native: {text}");
        assert!(text.contains(r#"o = {"a":1}"#), "{text}");
        assert_round_trips(&lit_ast);
    }

    #[test]
    fn obj_and_list_templates_are_native_when_dynamic() {
        // A template with a dynamic ($var/expr) leaf spells natively as `{ k: expr }` / `[ … ]` and
        // round-trips: field-access leaves, nested templates, mixed static/dynamic, and a
        // non-identifier (quoted) key.
        let template: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj",
            "fields": {
                "intent": {"kind": "jq", "path": ".intent", "input": {"kind": "var", "name": "x"}},
                "ok": {"kind": "lit", "value": true},
                "items": {"kind": "list", "items": [
                    {"kind": "var", "name": "a"},
                    {"kind": "lit", "value": 1}
                ]},
                "nested": {"kind": "obj", "fields": { "deep": {"kind": "var", "name": "d"} }},
                "a-b": {"kind": "var", "name": "q"}
            }
        }))
        .unwrap();
        let ast = DraftAst {
            body: vec![Node::Return {
                value: Box::new(template),
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "dynamic template should be native: {text}"
        );
        assert!(text.contains("x.intent"), "field-access leaf: {text}");
        assert!(
            text.contains("ok: true"),
            "literal leaf rendered inline: {text}"
        );
        assert!(
            text.contains(r#""a-b": q"#),
            "non-ident key is quoted: {text}"
        );
        assert_round_trips(&ast);
    }

    #[test]
    fn assert_round_trips_natively() {
        // `assert <cond> [, "<message>"]`; a comma inside `op(a,b)` belongs to the cond, not the message.
        let ast = DraftAst {
            body: vec![
                Node::Assert {
                    cond: Box::new(var("hits")),
                    message: Some("grep returned nothing".into()),
                },
                Node::Assert {
                    cond: Box::new(var("gate")),
                    message: None,
                },
                Node::Assert {
                    cond: Box::new(call("ok", vec![var("a"), var("b")])),
                    message: Some("two-arg cond".into()),
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            text.contains("assert hits, \"grep returned nothing\""),
            "{text}"
        );
        assert!(text.contains("assert gate\n"), "no-message form: {text}");
        assert!(!text.contains("@json"), "assert is native: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn retry_round_trips_natively_with_a_json_guard_fallback() {
        let full = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: Some("exponential".into()),
                delay_ms: Some(500),
                body: vec![call("flaky", vec![])],
                bind: Some("out".into()),
            }],
            ..Default::default()
        };
        let text = format(&full);
        assert!(
            text.contains("retry 3, backoff: exponential, delay: 500ms -> out"),
            "{text}"
        );
        assert!(!text.contains("@json"), "native: {text}");
        assert_round_trips(&full);

        let minimal = DraftAst {
            body: vec![Node::Retry {
                max: 1,
                backoff: None,
                delay_ms: None,
                body: vec![call("once", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        assert!(
            format(&minimal).contains("retry 1\n"),
            "minimal: {}",
            format(&minimal)
        );
        assert_round_trips(&minimal);

        // A `backoff` with whitespace can't be spelled natively → falls back to `@json` (still exact).
        let weird = DraftAst {
            body: vec![Node::Retry {
                max: 2,
                backoff: Some("a b".into()),
                delay_ms: None,
                body: vec![call("x", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        assert!(
            format(&weird).contains("@json"),
            "guard fallback: {}",
            format(&weird)
        );
        assert_round_trips(&weird);
    }

    #[test]
    fn parallel_round_trips_natively() {
        let ast = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: "readme".into(),
                        body: vec![call("read", vec![lit(s("README.md"))])],
                    },
                    Branch {
                        name: "todos".into(),
                        body: vec![call("grep", vec![lit(s("TODO"))])],
                    },
                ],
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("parallel\n"), "{text}");
        assert!(text.contains("branch readme"), "{text}");
        assert!(text.contains("branch todos"), "{text}");
        assert!(!text.contains("@json"), "native: {text}");
        assert_round_trips(&ast);

        // Single-branch and empty `parallel` also round-trip.
        let single = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![Branch {
                    name: "only".into(),
                    body: vec![call("go", vec![])],
                }],
            }],
            ..Default::default()
        };
        assert_round_trips(&single);
        let empty = DraftAst {
            body: vec![Node::Parallel { branches: vec![] }],
            ..Default::default()
        };
        assert_round_trips(&empty);
    }

    #[test]
    fn field_access_sugar_round_trips_and_is_native() {
        // `$plan.kind` <-> jq(".kind", $plan); nested `$o.a.b` too.
        let ast = DraftAst {
            body: vec![
                bind("k", jq(".kind", var("plan"))),
                bind("d", jq(".a.b", var("o"))),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("k = plan.kind"), "field sugar: {text}");
        assert!(text.contains("d = o.a.b"), "nested field sugar: {text}");
        assert!(!text.contains("@json"), "no json fallback: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn fmt_inline_round_trips_and_is_native() {
        let ast = DraftAst {
            body: vec![bind(
                "msg",
                Node::Fmt {
                    template: "hi {name}".into(),
                },
            )],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            text.contains(r#"msg = fmt("hi {name}")"#),
            "fmt inline: {text}"
        );
        assert!(!text.contains("@json"), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn jq_falls_back_to_json_when_unspellable() {
        // Non-Var input → @json (still round-trips).
        let over_call = DraftAst {
            body: vec![bind("y", jq(".kind", call("get_plan", vec![])))],
            ..Default::default()
        };
        assert!(format(&over_call).contains("@json"), "non-var jq → json");
        assert_round_trips(&over_call);
        // Bracket path → @json (the `$var.path` surface only spells simple dotted paths).
        let bracket = DraftAst {
            body: vec![bind("z", jq(".items[0]", var("o")))],
            ..Default::default()
        };
        assert!(format(&bracket).contains("@json"), "bracket path → json");
        assert_round_trips(&bracket);
    }

    #[test]
    fn match_and_route_round_trip_natively() {
        let m = DraftAst {
            body: vec![Node::Match {
                subject: Box::new(jq(".kind", var("plan"))),
                cases: vec![
                    MatchCase {
                        value: lit(s("chat")),
                        body: vec![bind("a", jq(".text", var("plan")))],
                    },
                    MatchCase {
                        value: lit(s("error")),
                        body: vec![call("echo", vec![lit(s("err"))])],
                    },
                ],
                default: vec![bind("r", call("present_results", vec![var("plan")]))],
            }],
            ..Default::default()
        };
        let text = format(&m);
        assert!(text.contains("match plan.kind"), "{text}");
        assert!(text.contains("case \"chat\""), "{text}");
        assert!(text.contains("default"), "{text}");
        assert!(!text.contains("@json"), "match native: {text}");
        assert_round_trips(&m);

        let r = DraftAst {
            body: vec![Node::Route {
                selector: Box::new(call("classify", vec![var("x")])),
                cases: vec![
                    RouteCase {
                        label: "bug".into(),
                        body: vec![call("echo", vec![lit(s("b"))])],
                    },
                    RouteCase {
                        label: "feat".into(),
                        body: vec![call("echo", vec![lit(s("f"))])],
                    },
                ],
                default: vec![call("echo", vec![lit(s("x"))])],
            }],
            ..Default::default()
        };
        let rt = format(&r);
        assert!(rt.contains("route classify(x)"), "{rt}");
        assert!(rt.contains("case \"bug\""), "{rt}");
        assert!(!rt.contains("@json"), "route native: {rt}");
        assert_round_trips(&r);
    }

    #[test]
    fn fallback_loop_timeout_budget_round_trip_natively() {
        let ast = DraftAst {
            body: vec![
                Node::Fallback {
                    branches: vec![
                        FallbackBranch {
                            body: vec![call("a", vec![])],
                        },
                        FallbackBranch {
                            body: vec![call("b", vec![])],
                        },
                    ],
                    bind: Some("win".into()),
                },
                Node::Loop {
                    for_ms: 1000,
                    every_ms: 100,
                    until: Some(Box::new(var("done"))),
                    body: vec![call("poll", vec![])],
                    bind: Some("ticks".into()),
                },
                Node::Timeout {
                    ms: 5000,
                    body: vec![call("slow", vec![])],
                    bind: None,
                },
                Node::Budget {
                    limit: 10,
                    body: vec![call("spend", vec![])],
                    bind: Some("used".into()),
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("fallback -> win"), "{text}");
        assert!(text.contains("branch"), "{text}");
        assert!(
            text.contains("loop for 1s, every: 100ms, until: done -> ticks"),
            "{text}"
        );
        assert!(text.contains("timeout 5s"), "{text}");
        assert!(text.contains("budget 10 -> used"), "{text}");
        assert!(!text.contains("@json"), "all native: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn with_tools_round_trips_natively() {
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["read_many".into(), "git_status".into()],
                body: vec![call("read_many", vec![])],
                bind: Some("scoped".into()),
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            text.contains(r#"with_tools ["read_many","git_status"] -> scoped"#),
            "{text}"
        );
        assert!(!text.contains("@json"), "with_tools native: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn with_tools_empty_allowlist_round_trips() {
        // `with_tools []` — the strictest scope (no tool at all).
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec![],
                body: vec![call("noop", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("with_tools []"), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn empty_flow_round_trips() {
        assert_round_trips(&DraftAst::default());
        assert_round_trips(&DraftAst {
            name: Some("noop".into()),
            ..Default::default()
        });
    }

    #[test]
    fn header_with_params_returns_round_trips() {
        assert_round_trips(&DraftAst {
            name: Some("route-call".into()),
            params: vec![
                Param {
                    name: "utterance".into(),
                    ty: TypeRef::String,
                },
                Param {
                    name: "count".into(),
                    ty: TypeRef::Number,
                },
                Param {
                    name: "tickets".into(),
                    ty: TypeRef::List(Box::new(TypeRef::Named("Ticket".into()))),
                },
            ],
            returns: Some(TypeRef::Named("RouteResult".into())),
            body: vec![Node::Return {
                value: Box::new(var("utterance")),
            }],
        });
        // Anonymous flow with params only.
        assert_round_trips(&DraftAst {
            name: None,
            params: vec![Param {
                name: "x".into(),
                ty: TypeRef::Bool,
            }],
            returns: Some(TypeRef::Any),
            body: Vec::new(),
        });
    }

    #[test]
    fn binds_calls_vars_lits_returns_round_trip() {
        assert_round_trips(&DraftAst {
            body: vec![
                // plain bind of a call
                Node::Bind {
                    name: "content".into(),
                    value: Box::new(call("read", vec![lit(serde_json::json!("README.md"))])),
                    ty: None,
                    effect: None,
                },
                // typed bind
                Node::Bind {
                    name: "draft".into(),
                    value: Box::new(var("content")),
                    ty: Some(TypeRef::Named("Draft".into())),
                    effect: None,
                },
                // bind with effect + type (annotation line)
                Node::Bind {
                    name: "sent".into(),
                    value: Box::new(call("email.send", vec![var("draft")])),
                    ty: Some(TypeRef::Bool),
                    effect: Some(FlowEffect::SendExternal),
                },
                // bare call statement (do form)
                call("git_stage", vec![lit(serde_json::json!(["."]))]),
                // bare var + bare literal statements
                var("content"),
                lit(serde_json::json!({"k": [1.0, true, null], "s": "v"})),
                // return
                Node::Return {
                    value: Box::new(lit(serde_json::json!("done"))),
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn control_flow_round_trips() {
        assert_round_trips(&DraftAst {
            body: vec![
                Node::When {
                    cond: Box::new(call("ready", vec![var("url")])),
                    then: vec![call("bash", vec![lit(serde_json::json!("echo yes"))])],
                    otherwise: vec![call("bash", vec![lit(serde_json::json!("echo no"))])],
                },
                // when with empty then but a populated else
                Node::When {
                    cond: Box::new(var("flag")),
                    then: Vec::new(),
                    otherwise: vec![Node::Return {
                        value: Box::new(lit(serde_json::json!(false))),
                    }],
                },
                Node::Unless {
                    cond: Box::new(var("already_built")),
                    body: vec![call("bash", vec![lit(serde_json::json!("cargo build"))])],
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn loops_and_seq_round_trip() {
        assert_round_trips(&DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(var("files")),
                    item: "f".into(),
                    body: vec![Node::Bind {
                        name: "text".into(),
                        value: Box::new(call("read", vec![var("f")])),
                        ty: None,
                        effect: None,
                    }],
                    collect: Some("contents".into()),
                    flat: false,
                },
                Node::Each {
                    source: Box::new(var("dirs")),
                    item: "d".into(),
                    body: vec![call("glob", vec![var("d")])],
                    collect: Some("all".into()),
                    flat: true,
                },
                Node::Each {
                    source: Box::new(lit(serde_json::json!(["a", "b"]))),
                    item: "x".into(),
                    body: Vec::new(),
                    collect: None,
                    flat: false,
                },
                Node::Repeat {
                    max: 10,
                    until: Some(Box::new(var("done"))),
                    body: vec![Node::Bind {
                        name: "done".into(),
                        value: Box::new(call("poll", Vec::new())),
                        ty: None,
                        effect: None,
                    }],
                    collect: Some("rounds".into()),
                },
                Node::Repeat {
                    max: 3,
                    until: None,
                    body: vec![call("tick", Vec::new())],
                    collect: None,
                },
                Node::Seq {
                    body: vec![call("a", Vec::new()), call("b", Vec::new())],
                    bind: Some("result".into()),
                },
                Node::Seq {
                    body: Vec::new(),
                    bind: None,
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn ctx_and_ctx_append_round_trip() {
        assert_round_trips(&DraftAst {
            body: vec![
                Node::Ctx {
                    name: "pack".into(),
                    purpose: Some("review the diff".into()),
                    include: vec!["diff".into(), "summary".into()],
                    exclude: vec!["secrets".into()],
                    budget: Some(4000),
                },
                Node::CtxAppend {
                    ctx: "pack".into(),
                    add: vec!["extra".into(), "notes".into()],
                },
                // minimal ctx (no attributes) + empty append
                Node::Ctx {
                    name: "bare".into(),
                    purpose: None,
                    include: Vec::new(),
                    exclude: Vec::new(),
                    budget: None,
                },
                Node::CtxAppend {
                    ctx: "bare".into(),
                    add: Vec::new(),
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn nested_blocks_round_trip() {
        assert_round_trips(&DraftAst {
            name: Some("nested".into()),
            body: vec![Node::Each {
                source: Box::new(var("items")),
                item: "it".into(),
                body: vec![Node::When {
                    cond: Box::new(var("it")),
                    then: vec![Node::When {
                        cond: Box::new(var("inner")),
                        then: vec![call("bash", vec![lit(serde_json::json!("both"))])],
                        otherwise: vec![call("bash", vec![lit(serde_json::json!("only outer"))])],
                    }],
                    otherwise: vec![Node::Seq {
                        body: vec![call("cleanup", Vec::new())],
                        bind: None,
                    }],
                }],
                collect: None,
                flat: false,
            }],
            ..Default::default()
        });
    }

    #[test]
    fn json_fallback_round_trips_statement_and_inline() {
        let ast = DraftAst {
            body: vec![
                // Permanently-@json shapes at statement position: unspellable (dotted) names can
                // never be spelled natively, so they exercise the statement-position escape
                // regardless of which node kinds gain native syntax (memo/once/checkpoint/await/…).
                Node::Var { name: "a.b".into() },
                Node::Bind {
                    name: "c.d".into(),
                    value: Box::new(lit(s("x"))),
                    ty: None,
                    effect: None,
                },
                // `jq(".bitcoin.usd", $raw)` inline is *native* field-access sugar; a bracket path
                // is not, so it uses the inline @json escape.
                Node::Bind {
                    name: "price".into(),
                    value: Box::new(Node::Jq {
                        path: ".prices[0]".into(),
                        input: Box::new(var("raw")),
                        optional: false,
                    }),
                    ty: None,
                    effect: None,
                },
                // A thing reference (unsupported) inline as a call arg.
                Node::Bind {
                    name: "p".into(),
                    value: Box::new(call(
                        "notify",
                        vec![Node::Thing {
                            thing: ThingRef {
                                kind: ThingKind::Person,
                                selector: Selector::Name("john".into()),
                            },
                        }],
                    )),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        // Statement-position `@json` lines are really present (explicit escape coverage).
        assert!(
            text.lines()
                .filter(|l| l.trim_start().starts_with("@json "))
                .count()
                >= 2,
            "expected statement-position @json lines: {text}"
        );
        assert_round_trips(&ast);
    }

    // ----- F21: name guards — the round-trip is total for unspellable names -----

    /// Confirmed counterexample 1 (F8/F21): `Var{name:"a.b"}` in expression position used to format
    /// as `$a.b`, which reparses as `Jq{".b", $a}` — a *different program*, silently.
    #[test]
    fn dotted_var_name_round_trips_via_json_fallback() {
        let expr_pos = DraftAst {
            body: vec![bind("x", var("a.b"))],
            ..Default::default()
        };
        let text = format(&expr_pos);
        assert!(text.contains("@json"), "expression position: {text}");
        assert_round_trips(&expr_pos);

        // Statement position: the parser no longer admits `.` in declared names, so the formatter
        // must fall back here too (it used to round-trip only by charset luck).
        let stmt_pos = DraftAst {
            body: vec![var("a.b")],
            ..Default::default()
        };
        let text = format(&stmt_pos);
        assert!(text.contains("@json"), "statement position: {text}");
        assert_round_trips(&stmt_pos);
    }

    /// Confirmed counterexample 2 (F21): symbol/op names with spaces used to produce *unparseable*
    /// text (loud failure). They now fall back to `@json` and round-trip exactly.
    #[test]
    fn space_names_round_trip_via_json_fallback() {
        let ast = DraftAst {
            body: vec![
                bind("a b", lit(serde_json::json!(1))),
                call("git status", vec![lit(serde_json::json!("."))]),
                var("a b"),
                bind("y", call("git status", vec![])), // inline op position
            ],
            ..Default::default()
        };
        assert_round_trips(&ast);
    }

    /// Every declared-name position falls back to `@json` when the name is unspellable — one node
    /// per position, each with a name the surface cannot spell.
    #[test]
    fn every_name_position_guards_unspellable_names() {
        let bad: SymbolName = "a b".into();
        let some_bad = || Some(SymbolName::from("a b"));
        let nodes: Vec<(&str, Node)> = vec![
            (
                "bind name",
                Node::Bind {
                    name: bad.clone(),
                    value: Box::new(lit(serde_json::json!(1))),
                    ty: None,
                    effect: None,
                },
            ),
            (
                "bind ty (builtin-colliding Named)",
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(lit(serde_json::json!(1))),
                    ty: Some(TypeRef::Named("Bool".into())),
                    effect: None,
                },
            ),
            (
                "memo name",
                Node::Memo {
                    name: bad.clone(),
                    value: Box::new(lit(serde_json::json!(1))),
                    ty: None,
                    effect: None,
                },
            ),
            (
                "each item",
                Node::Each {
                    source: Box::new(var("xs")),
                    item: bad.clone(),
                    body: vec![],
                    collect: None,
                    flat: false,
                },
            ),
            (
                "each collect",
                Node::Each {
                    source: Box::new(var("xs")),
                    item: "x".into(),
                    body: vec![],
                    collect: some_bad(),
                    flat: false,
                },
            ),
            (
                "repeat collect",
                Node::Repeat {
                    max: 2,
                    until: None,
                    body: vec![],
                    collect: some_bad(),
                },
            ),
            (
                "seq bind",
                Node::Seq {
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "ctx name",
                Node::Ctx {
                    name: bad.clone(),
                    purpose: None,
                    include: vec![],
                    exclude: vec![],
                    budget: None,
                },
            ),
            (
                "ctx include sym",
                Node::Ctx {
                    name: "p".into(),
                    purpose: None,
                    include: vec![bad.clone()],
                    exclude: vec![],
                    budget: None,
                },
            ),
            (
                "ctx exclude sym",
                Node::Ctx {
                    name: "p".into(),
                    purpose: None,
                    include: vec![],
                    exclude: vec![bad.clone()],
                    budget: None,
                },
            ),
            (
                "ctx_append ctx",
                Node::CtxAppend {
                    ctx: bad.clone(),
                    add: vec![],
                },
            ),
            (
                "ctx_append sym",
                Node::CtxAppend {
                    ctx: "p".into(),
                    add: vec![bad.clone()],
                },
            ),
            (
                "fallback bind",
                Node::Fallback {
                    branches: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "loop bind",
                Node::Loop {
                    for_ms: 1,
                    every_ms: 1,
                    until: None,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "timeout bind",
                Node::Timeout {
                    ms: 1,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "budget bind",
                Node::Budget {
                    limit: 1,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "with_tools bind",
                Node::CapScope {
                    tools: vec![],
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "retry bind",
                Node::Retry {
                    max: 1,
                    backoff: None,
                    delay_ms: None,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "parallel branch name",
                Node::Parallel {
                    branches: vec![Branch {
                        name: bad.clone(),
                        body: vec![],
                    }],
                },
            ),
            ("call op (statement)", call("not an op", vec![])),
            ("call op (inline)", bind("x", call("not an op", vec![]))),
            (
                "call op literally `fmt` (inline collides with the Fmt node)",
                bind("x", call("fmt", vec![lit(serde_json::json!("hi"))])),
            ),
            ("jq input var name", bind("x", jq(".kind", var("a b")))),
            ("var (statement)", var("a b")),
            ("var (expression)", bind("x", var("a b"))),
        ];
        for (what, node) in nodes {
            let ast = DraftAst {
                body: vec![node],
                ..Default::default()
            };
            let text = format(&ast);
            assert!(text.contains("@json"), "{what} must @json-fallback: {text}");
            assert_round_trips(&ast);
        }
    }

    // ----- F8 text side: `.` is no longer a declared-name character -----

    #[test]
    fn dotted_declared_names_are_parse_errors() {
        for bad in [
            "flow x\n  $a.b = 1",
            "flow x\n  $a.b: Number = 1",
            "flow x\n  $pack.x += $a",
            "flow x\n  each $it.x in $xs",
            "flow x\n  seq -> $out.y",
            "flow x\n  repeat 2 -> $c.d",
            "flow x\n  ctx $p.q",
            "flow x\n  ctx $p\n    include $a.b",
            "flow x\n  parallel\n    branch $l.r\n      do go",
        ] {
            assert!(parse(bad).is_err(), "expected Err for: {bad:?}");
        }
        // Expression position is untouched: `$plan.kind` stays jq field-access sugar.
        let ast = parse("flow x\n  $k = $plan.kind\n").unwrap();
        assert_eq!(ast.body, vec![bind("k", jq(".kind", var("plan")))]);
        // …and a bare `$a` statement still parses as a var reference.
        let ast = parse("flow x\n  $a\n").unwrap();
        assert_eq!(ast.body, vec![var("a")]);
    }

    /// The documented flow-header exception: the header has no `@json` escape, so an unspellable
    /// flow name cannot round-trip — but it must fail **loudly** (a parse error), never silently
    /// reparse as a different program. The analyzer rejects such names before they ever format.
    #[test]
    fn unspellable_flow_header_name_is_a_loud_parse_error() {
        let ast = DraftAst {
            name: Some("a b".into()),
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            parse(&text).is_err(),
            "space flow name must fail loudly, got Ok for: {text}"
        );
    }

    // ----- F22: parse errors carry 1-based source line numbers -----

    #[test]
    fn parse_errors_carry_line_numbers() {
        // Comment and blank lines still count toward the source line number.
        let err = parse("flow x\n\n  # a comment\n  $a =").unwrap_err();
        assert!(err.to_string().contains("line 4:"), "{err}");
        // The innermost statement line wins, not the enclosing block header.
        let err = parse("flow x\n  when $ok\n    do\n").unwrap_err();
        assert!(err.to_string().contains("line 3:"), "{err}");
        // Header errors point at the header line.
        let err = parse("not a flow").unwrap_err();
        assert!(err.to_string().contains("line 1:"), "{err}");
        // Module declarations locate their lines too.
        let err = parse_program("agent a\n  model \"m\"\n\nwidget w\n").unwrap_err();
        assert!(err.to_string().contains("line 4:"), "{err}");
    }

    // ----- Hand-written text fixtures: pin the surface (not just self-consistency) -----

    #[test]
    fn fixture_basic_flow() {
        let src = "\
flow greet(name: String) -> String
  # bind then return
  $msg = greet_op($name)
  return $msg
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast,
            DraftAst {
                name: Some("greet".into()),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::String,
                }],
                returns: Some(TypeRef::String),
                body: vec![
                    Node::Bind {
                        name: "msg".into(),
                        value: Box::new(call("greet_op", vec![var("name")])),
                        ty: None,
                        effect: None,
                    },
                    Node::Return {
                        value: Box::new(var("msg")),
                    },
                ],
            }
        );
    }

    #[test]
    fn fixture_paren_form_call_and_when_else() {
        // A bare call written in paren form (not `do`) must parse to the same Call node.
        let src = "\
flow check
  read(\"log.txt\")
  when $ok
    do bash \"echo yes\"
  else
    do bash \"echo no\"
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![
                call("read", vec![lit(serde_json::json!("log.txt"))]),
                Node::When {
                    cond: Box::new(var("ok")),
                    then: vec![call("bash", vec![lit(serde_json::json!("echo yes"))])],
                    otherwise: vec![call("bash", vec![lit(serde_json::json!("echo no"))])],
                },
            ]
        );
    }

    #[test]
    fn fixture_goal_line_is_tolerated_and_ignored() {
        let src = "\
flow withgoal
goal \"do the thing\"
  return true
";
        let ast = parse(src).unwrap();
        assert_eq!(ast.name.as_deref(), Some("withgoal"));
        assert_eq!(
            ast.body,
            vec![Node::Return {
                value: Box::new(lit(serde_json::json!(true))),
            }]
        );
    }

    #[test]
    fn fixture_ctx_block() {
        let src = "\
flow pack-it
  ctx $pack
    purpose \"review\"
    budget 1500
    include $a, $b
    exclude $c
  $pack += $d
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![
                Node::Ctx {
                    name: "pack".into(),
                    purpose: Some("review".into()),
                    include: vec!["a".into(), "b".into()],
                    exclude: vec!["c".into()],
                    budget: Some(1500),
                },
                Node::CtxAppend {
                    ctx: "pack".into(),
                    add: vec!["d".into()],
                },
            ]
        );
    }

    // ----- Multi-line string literals (L-39): `"""…"""`, verbatim, delimiter-terminated -----

    #[test]
    fn multiline_string_literal_parses_verbatim_across_physical_lines() {
        let src = "\
flow
  $x = \"\"\"first line
second line
third line\"\"\"
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![bind("x", lit(s("first line\nsecond line\nthird line")))]
        );
    }

    #[test]
    fn multiline_string_content_is_taken_literally_no_comment_no_escape_processing() {
        // `#` and backslashes inside the block are ordinary characters, not a comment start or an
        // escape sequence — the whole point of the spelling is "no interpretation between the
        // delimiters".
        let src = "\
flow
  $x = \"\"\"line one # not a comment
back\\slash and a \"single\" quote are literal\"\"\"
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![bind(
                "x",
                lit(s(
                    "line one # not a comment\nback\\slash and a \"single\" quote are literal"
                ))
            )]
        );
    }

    #[test]
    fn multiline_string_works_as_a_call_arg_and_inside_an_object_template() {
        let src = "\
flow
  do write \"out.txt\", \"\"\"payload
line 2\"\"\"
  $t = { path: \"out.txt\", content: \"\"\"templated
value\"\"\" }
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![
                call("write", vec![lit(s("out.txt")), lit(s("payload\nline 2"))]),
                bind(
                    "t",
                    Node::Obj {
                        fields: {
                            let mut m = std::collections::BTreeMap::new();
                            m.insert("path".to_string(), Box::new(lit(s("out.txt"))));
                            m.insert("content".to_string(), Box::new(lit(s("templated\nvalue"))));
                            m
                        },
                    }
                ),
            ]
        );
    }

    #[test]
    fn empty_multiline_string_parses_as_empty_string() {
        let src = "flow\n  $x = \"\"\"\"\"\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body, vec![bind("x", lit(s("")))]);
    }

    #[test]
    fn unterminated_multiline_string_is_a_located_parse_error() {
        let src = "flow\n  $x = \"\"\"never closed\n";
        let err = parse(src).unwrap_err();
        assert!(
            err.to_string().contains("line 2:"),
            "expected the opening line in the error, got: {err}"
        );
        assert!(
            err.to_string().to_lowercase().contains("unterminated"),
            "expected an 'unterminated' diagnostic, got: {err}"
        );
    }

    #[test]
    fn multiline_block_inside_a_pure_json_object_stays_a_lit_not_a_template() {
        // The dominant corpus case (`do edit {"path":...,"content":"""..."""}`): quoted keys make
        // this valid JSON, so it must desugar to a plain escaped string BEFORE the parser's
        // try-JSON-then-template split runs, staying a `Lit`, not falling through to the `Obj`
        // value-template reader (bareword-key path).
        let src = "\
flow
  do edit {\"path\": \"f.txt\", \"content\": \"\"\"line1
line2\"\"\"}
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![call(
                "edit",
                vec![lit(
                    serde_json::json!({"path": "f.txt", "content": "line1\nline2"})
                )]
            )]
        );
    }

    #[test]
    fn escaped_triple_quotes_inside_a_normal_string_are_not_mistaken_for_a_block() {
        // `\"\"\"` (three ESCAPED quotes) inside an ordinary `"…"` string must stay ordinary string
        // content — the in-string escape tracking keeps the multi-line-block detector from firing
        // while a normal string is open.
        let src = "flow\n  $x = \"a\\\"\\\"\\\"b\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body, vec![bind("x", lit(s("a\"\"\"b")))]);
    }

    #[test]
    fn two_multiline_blocks_in_one_statement() {
        let src = "flow\n  do op \"\"\"x\ny\"\"\", \"\"\"a\nb\"\"\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![call("op", vec![lit(s("x\ny")), lit(s("a\nb"))])]
        );
    }

    #[test]
    fn multiline_content_preserves_blank_lines_indentation_and_statement_look_alikes() {
        // `#`, blank lines, indentation, and a line that looks like its own statement are all just
        // literal characters inside the block — nothing about it is interpreted.
        let src = "flow\n  $x = \"\"\"# not a comment\n\n  indented\n$y = 1\"\"\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![bind("x", lit(s("# not a comment\n\n  indented\n$y = 1")))]
        );
    }

    // ----- Totality: malformed input errors, never panics -----

    #[test]
    fn malformed_input_returns_parse_error() {
        // Inputs with a located line: the error must carry a `line N:` prefix (F22).
        for (bad, line) in [
            ("not a flow", 1),
            ("flow x\n\telse", 2),            // tab indentation
            ("flow x\n  $ = 1", 2),           // empty symbol
            ("flow x\n  $a = ", 2),           // missing expression
            ("flow x\n  do", 2),              // do without op
            ("flow x\n  each $f $xs", 2),     // each without `in`
            ("flow x\n  repeat\n", 2),        // repeat without count
            ("flow x\n  when", 2),            // when without condition
            ("flow x\n  $a = read(\"x\"", 2), // unbalanced parens
            ("flow x\n  else", 2),            // dangling else
            ("flow x\n  @json {oops}", 2),    // invalid json
        ] {
            let err = parse(bad).unwrap_err();
            assert!(
                err.to_string().contains(&format!("line {line}:")),
                "expected `line {line}:` for {bad:?}, got: {err}"
            );
        }
        // Empty input has no line to point at.
        assert!(parse("").is_err());
    }

    // -----------------------------------------------------------------------
    // Program / module layer
    // -----------------------------------------------------------------------

    const CANONICAL_APP: &str = "\
# app.flux — the whole app in native flux-lang
agent assistant
  model \"claude-sonnet-4-6\"
  tools [search, send]
  datasources [docs]
  description \"answers from the docs\"

channel slack
  bot_token secret \"SLACK_BOT_TOKEN\"
  app_token secret \"SLACK_APP_TOKEN\"

datasource docs
  kind \"markdown\"
  path \"./docs\"

op repo_health(path: String, prior: Ctx) -> Health
  description \"Check git state and summarize failures\"
  risk \"medium\"
  idempotency \"idempotent\"
  effects [read, process, model]
  limits {dispatches: 20, timeout_ms: 120000, context_chars: 8000}
  expose true

  $status = git_status()
  $tests = cargo_test({args: [\"--workspace\"]})
  ctx $pack
    purpose \"repo-health\"
    budget 8000
    include $prior, $status, $tests
  $summary = ai.reason({ask: \"Summarize repo health\", ctx: $pack})
  return {status: $status, tests: $tests, summary: $summary}

trigger on_msg
  on \"slack\"
  run greet
  agent assistant

journey greet
  agent assistant
  flow
    $r = complete($text)
    return $r
";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FrozenErrorClass {
        Layout,
        OrphanClause,
        InvalidLiteral,
        UnknownDeclaration,
        MissingOperand,
    }

    fn frozen_error_class(error: &FlowError) -> FrozenErrorClass {
        let message = error.to_string().to_ascii_lowercase();
        if message.contains("tab") || message.contains("indent") {
            FrozenErrorClass::Layout
        } else if message.contains("else") {
            FrozenErrorClass::OrphanClause
        } else if message.contains("json") {
            FrozenErrorClass::InvalidLiteral
        } else if message.contains("top-level") || message.contains("declaration") {
            FrozenErrorClass::UnknownDeclaration
        } else {
            FrozenErrorClass::MissingOperand
        }
    }

    fn error_line(error: &FlowError) -> Option<usize> {
        error
            .to_string()
            .strip_prefix("parse error: line ")?
            .split_once(':')?
            .0
            .parse()
            .ok()
    }

    fn assert_frozen_flow(source: &str, expected_json: &str) {
        let expected: serde_json::Value =
            serde_json::from_str(expected_json).expect("valid frozen AST fixture");
        let actual =
            serde_json::to_value(parse(source).expect("CST accepts frozen source fixture"))
                .expect("serialize parsed AST");
        assert_eq!(actual, expected, "semantic drift for:\n{source}");
    }

    /// Expected AST JSON and error classes were captured before the CST cutover. They stay plain
    /// data rather than executable parser code, so this guard cannot accidentally exercise the new
    /// implementation twice or preserve a second accepting grammar in production.
    #[test]
    fn structured_cst_matches_frozen_pre_cutover_fixtures() {
        assert_frozen_flow(
            "flow greet(name: String) -> String\n  $msg = greet_op($name)\n  return $msg\n",
            r#"{
                "name":"greet",
                "params":[{"name":"name","ty":"string"}],
                "returns":"string",
                "body":[
                    {"kind":"bind","name":"msg","value":{"kind":"call","op":"greet_op","args":[{"kind":"var","name":"name"}]}},
                    {"kind":"return","value":{"kind":"var","name":"msg"}}
                ]
            }"#,
        );
        assert_frozen_flow(
            "flow choose(input: Json) -> Json\n  $kind = $input.kind\n  when $kind\n    return $input\n  else\n    return null\n",
            r#"{
                "name":"choose",
                "params":[{"name":"input","ty":{"named":"Json"}}],
                "returns":{"named":"Json"},
                "body":[
                    {"kind":"bind","name":"kind","value":{"kind":"jq","path":".kind","input":{"kind":"var","name":"input"},"optional":false}},
                    {"kind":"when","cond":{"kind":"var","name":"kind"},"then":[{"kind":"return","value":{"kind":"var","name":"input"}}],"otherwise":[{"kind":"return","value":{"kind":"lit","value":null}}]}
                ]
            }"#,
        );
        assert_frozen_flow(
            "flow notes\n  $body = \"\"\"alpha\n# not a comment\nomega\"\"\"\n  return $body\n",
            r#"{
                "name":"notes",
                "params":[],
                "body":[
                    {"kind":"bind","name":"body","value":{"kind":"lit","value":"alpha\n# not a comment\nomega"}},
                    {"kind":"return","value":{"kind":"var","name":"body"}}
                ]
            }"#,
        );

        // `CANONICAL_APP` is the frozen whole-module source fixture. The detailed assertions in
        // `parse_program_reads_the_full_typed_module_surface` pin every declaration below.
        assert!(matches!(
            parse_program(CANONICAL_APP),
            Ok(Module::Program(_))
        ));

        let errors = [
            (
                "flow x\n\tdo work\n",
                false,
                FrozenErrorClass::Layout,
                Some(2),
            ),
            (
                "flow x\n  else\n",
                false,
                FrozenErrorClass::OrphanClause,
                Some(2),
            ),
            (
                "flow x\n  @json {oops}\n",
                false,
                FrozenErrorClass::InvalidLiteral,
                Some(2),
            ),
            (
                "widget unknown\n",
                true,
                FrozenErrorClass::UnknownDeclaration,
                Some(1),
            ),
            (
                "flow x\n  do\n",
                false,
                FrozenErrorClass::MissingOperand,
                Some(2),
            ),
        ];
        for (source, module, class, line) in errors {
            let actual = if module {
                parse_program(source).unwrap_err()
            } else {
                parse(source).unwrap_err()
            };
            assert_eq!(
                frozen_error_class(&actual),
                class,
                "CST: {source:?}: {actual}"
            );
            assert_eq!(error_line(&actual), line, "CST: {source:?}");
        }
    }

    #[test]
    fn parse_program_reads_the_full_typed_module_surface() {
        let Module::Program(p) = parse_program(CANONICAL_APP).unwrap() else {
            panic!("module declarations must sniff as a program");
        };

        // agent
        assert_eq!(p.agents.len(), 1);
        let a = &p.agents[0];
        assert_eq!(a.name, "assistant");
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(a.tools, vec!["search", "send"]);
        assert_eq!(a.datasources, vec!["docs"]);
        assert_eq!(a.description.as_deref(), Some("answers from the docs"));

        // channel — kind defaults to the name; secrets are markers, never plaintext
        assert_eq!(p.channels.len(), 1);
        assert_eq!(p.channels[0].kind, "slack");
        assert_eq!(
            p.channels[0].settings["bot_token"],
            serde_json::json!({ "$secret": "SLACK_BOT_TOKEN" })
        );

        // datasource
        assert_eq!(p.datasources.len(), 1);
        assert_eq!(p.datasources[0].kind, "markdown");
        assert_eq!(p.datasources[0].path.as_deref(), Some("./docs"));

        // composite op
        assert_eq!(p.ops.len(), 1);
        let op = &p.ops[0];
        assert_eq!(op.name, "repo_health");
        assert_eq!(op.params.len(), 2);
        assert_eq!(
            op.returns.as_ref().map(TypeRef::label).as_deref(),
            Some("Health")
        );
        assert_eq!(op.meta.risk, Risk::Medium);
        assert_eq!(op.meta.idempotency, Idempotency::Idempotent);
        assert_eq!(
            op.meta.effects,
            vec![Effect::Read, Effect::Process, Effect::Network]
        );
        assert_eq!(op.meta.limits.dispatches, Some(20));
        assert!(op.meta.expose);
        assert!(matches!(op.body.body.last(), Some(Node::Return { .. })));

        // trigger
        assert_eq!(p.triggers[0].on, "slack");
        assert_eq!(p.triggers[0].run, "greet");
        assert_eq!(p.triggers[0].agent.as_deref(), Some("assistant"));

        // journey + its inline flow body (defaulted to the journey name)
        let flow = p.flow_named("greet").expect("journey resolves by name");
        assert_eq!(flow.name.as_deref(), Some("greet"));
        assert_eq!(p.journeys[0].agent.as_deref(), Some("assistant"));
        assert!(
            matches!(flow.body.last(), Some(Node::Return { .. })),
            "the flow body parsed as native statements"
        );
    }

    #[test]
    fn program_permissions_and_agent_narrowing_parse() {
        let src = r#"permissions
  allow [search, "ai.reason", send]
  deny [write, bash]

agent guide
  tools [search]
  datasources [handbook]
  allow [search, "ai.reason", send]
  deny [bash]

trigger questions
  on "user_input"
  run answer

journey answer
  agent guide
  flow
    return ""
"#;
        let Module::Program(program) = parse_program(src).expect("parse program") else {
            panic!("expected program")
        };
        let app = program.permissions.expect("top-level permissions");
        assert_eq!(
            app.allow.as_deref(),
            Some(&["search".into(), "ai.reason".into(), "send".into()][..])
        );
        assert_eq!(app.deny, ["write", "bash"]);
        let agent = program.agents.first().expect("guide");
        let permissions = agent.permissions.as_ref().expect("agent permissions");
        assert_eq!(
            permissions.allow.as_deref(),
            Some(&["search".into(), "ai.reason".into(), "send".into()][..])
        );
        assert_eq!(permissions.deny, ["bash"]);
        assert_eq!(program.journeys[0].agent.as_deref(), Some("guide"));
    }

    #[test]
    fn permission_allow_absent_inherits_but_explicit_empty_denies_all() {
        let Module::Program(inherited) = parse_program("permissions\n  deny [bash]\n").unwrap()
        else {
            panic!("program")
        };
        assert_eq!(inherited.permissions.unwrap().allow, None);

        let Module::Program(empty) = parse_program("permissions\n  allow []\n").unwrap() else {
            panic!("program")
        };
        assert_eq!(empty.permissions.unwrap().allow, Some(Vec::new()));
    }

    #[test]
    fn duplicate_or_unknown_permission_declarations_are_rejected() {
        for (src, needle) in [
            ("permissions\n\npermissions\n", "only once"),
            (
                "permissions\n  allow [read]\n  allow [search]\n",
                "duplicate permissions attribute `allow`",
            ),
            (
                "permissions\n  maybe [read]\n",
                "unknown permissions attribute",
            ),
            (
                "agent guide\n  allow [read]\n  allow [search]\n",
                "duplicate agent attribute `allow`",
            ),
        ] {
            let err = parse_program(src).unwrap_err().to_string();
            assert!(err.contains(needle), "`{src}`: {err}");
        }
    }

    #[test]
    fn agent_bound_trigger_needs_no_run_journey() {
        // An agent-bound trigger routes to a model turn (`run_agent`), so a `run` journey name is
        // optional — the runtime never reads it. This is the shape the support-bot example uses.
        let src = "trigger on_msg\n  on \"slack\"\n  agent assistant\n";
        let Module::Program(p) = parse_program(src).unwrap() else {
            panic!("program")
        };
        assert_eq!(p.triggers[0].on, "slack");
        assert_eq!(p.triggers[0].agent.as_deref(), Some("assistant"));
        assert!(p.triggers[0].run.is_empty(), "no journey to run");
    }

    #[test]
    fn trigger_with_neither_run_nor_agent_is_an_error() {
        let src = "trigger on_msg\n  on \"slack\"\n";
        let err = parse_program(src).unwrap_err().to_string();
        assert!(
            err.contains("`run`") && err.contains("`agent`"),
            "error names both routes: {err}"
        );
    }

    #[test]
    fn channel_kind_can_be_overridden() {
        let src = "channel myslack\n  kind \"slack\"\n";
        let Module::Program(p) = parse_program(src).unwrap() else {
            panic!("program")
        };
        assert_eq!(p.channels[0].name, "myslack");
        assert_eq!(p.channels[0].kind, "slack");
    }

    #[test]
    fn a_lone_flow_is_a_bare_flow_not_a_program() {
        let m = parse_program("flow greet\n  return null").unwrap();
        assert!(matches!(m, Module::Flow(_)));
    }

    #[test]
    fn an_unknown_top_level_decl_is_a_clean_error() {
        let err = parse_program("widget foo\n  x 1\n").unwrap_err();
        assert!(format!("{err}").contains("unknown top-level declaration"));
    }

    #[test]
    fn parse_when_native_comparison() {
        let src = "flow test\n  when $count > 3\n    return true\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::When { cond, .. } = &ast.body[0] else {
            panic!("expected When node");
        };
        match cond.as_ref() {
            Node::Expr { formula, vars } => {
                assert_eq!(formula, "count > 3");
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("count"));
                match vars.get("count").unwrap().as_ref() {
                    Node::Var { name } => assert_eq!(name.0, "count"),
                    _ => panic!("expected Var node"),
                }
            }
            _ => panic!("expected Expr node, got {:?}", cond),
        }
    }

    #[test]
    fn parse_until_native_call_predicate() {
        let src = "flow test\n  repeat 10\n    until len($queue) == 0\n    return null\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::Repeat { until, .. } = &ast.body[0] else {
            panic!("expected Repeat node");
        };
        let Some(until_node) = until else {
            panic!("expected until condition");
        };
        match until_node.as_ref() {
            Node::Expr { formula, vars } => {
                assert!(formula.contains("len(queue)"));
                assert!(formula.contains("== 0"));
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("queue"));
            }
            _ => panic!("expected Expr node, got {:?}", until_node),
        }
    }

    #[test]
    fn parse_bind_rhs_native_expr() {
        let src = "flow test\n  $ok = $score >= 0.8\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::Bind { name, value, .. } = &ast.body[0] else {
            panic!("expected Bind node");
        };
        assert_eq!(name.0, "ok");
        match value.as_ref() {
            Node::Expr { formula, vars } => {
                assert_eq!(formula, "score >= 0.8");
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("score"));
            }
            _ => panic!("expected Expr node, got {:?}", value),
        }
    }

    #[test]
    fn parse_assert_native_expr_with_message() {
        let src = "flow test\n  assert $n > 0, \"must be positive\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::Assert { cond, message } = &ast.body[0] else {
            panic!("expected Assert node");
        };
        match cond.as_ref() {
            Node::Expr { formula, vars } => {
                assert_eq!(formula, "n > 0");
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("n"));
            }
            _ => panic!("expected Expr node, got {:?}", cond),
        }
        assert_eq!(message.as_ref().unwrap(), "must be positive");
    }

    #[test]
    fn roundtrip_native_expr_preserves_json() {
        use crate::format::format;

        let test_cases = vec![
            "flow test\n  when $count > 3\n    return true\n",
            "flow test\n  $ok = $score >= 0.8\n",
            "flow test\n  unless $flag == false\n    return null\n",
            "flow test\n  assert $n > 0, \"positive\"\n",
        ];

        for src in test_cases {
            let ast = parse(src).unwrap();
            let formatted = format(&ast);
            let reparsed = parse(&formatted).unwrap();
            assert_eq!(ast, reparsed, "roundtrip failed for: {}", src);
        }
    }

    #[test]
    fn native_expr_fallback_does_not_collide_with_existing_valid_forms() {
        let src = r#"flow test
  $plain = $score
  $call = len($queue)
  when $ready
    return $plain
  assert ok($plain)
  $ok = $score >= 0.8
"#;
        let ast = parse(src).unwrap();

        let Node::Bind { value, .. } = &ast.body[0] else {
            panic!("expected first bind");
        };
        assert!(matches!(value.as_ref(), Node::Var { .. }));

        let Node::Bind { value, .. } = &ast.body[1] else {
            panic!("expected second bind");
        };
        assert!(matches!(value.as_ref(), Node::Call { op, .. } if op == "len"));

        let Node::When { cond, .. } = &ast.body[2] else {
            panic!("expected when");
        };
        assert!(matches!(cond.as_ref(), Node::Var { .. }));

        let Node::Assert { cond, .. } = &ast.body[3] else {
            panic!("expected assert");
        };
        assert!(matches!(cond.as_ref(), Node::Call { op, .. } if op == "ok"));

        let Node::Bind { value, .. } = &ast.body[4] else {
            panic!("expected native expr bind");
        };
        assert!(matches!(value.as_ref(), Node::Expr { formula, .. } if formula == "score >= 0.8"));
    }

    #[test]
    fn parse_when_with_dotted_access() {
        let src = "flow test\n  when $issue.state == 'opened'\n    return true\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::When { cond, .. } = &ast.body[0] else {
            panic!("expected When node");
        };
        match cond.as_ref() {
            Node::Expr { formula, vars } => {
                // The formula should have issue.state (without $)
                assert!(formula.contains("issue.state"));
                assert!(formula.contains("== 'opened'"));
                // Only the root name should be in vars
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("issue"));
            }
            _ => panic!("expected Expr node, got {:?}", cond),
        }
    }

    #[test]
    fn format_native_expr_with_dollar() {
        use crate::format::format;
        let src = "flow test\n  when $count > 3\n    return true\n";
        let ast = parse(src).unwrap();
        let formatted = format(&ast);
        // The formatted output should contain the native expr with $
        assert!(
            formatted.contains("when $count > 3"),
            "formatted: {}",
            formatted
        );
        // Should not fall back to @json
        assert!(!formatted.contains("@json"), "formatted: {}", formatted);
    }

    #[test]
    fn comprehensive_native_expr_integration() {
        use crate::format::format;

        let src = r#"flow test
  $count = 10
  $score = 0.85

  when $count > 5
    $high = true

  $ok = $score >= 0.8

  unless $count == 0
    return true

  assert $score > 0.5, "score too low"

  repeat 3
    until $count == 0
    $count = 1

  return false
"#;

        let ast = parse(src).unwrap();
        let formatted = format(&ast);

        // All native expressions should be preserved with $
        assert!(formatted.contains("when $count > 5"));
        assert!(formatted.contains("ok = $score >= 0.8"));
        assert!(formatted.contains("unless $count == 0"));
        assert!(formatted.contains("assert $score > 0.5"));
        assert!(formatted.contains(", until: $count == 0"));

        // Roundtrip test
        let reparsed = parse(&formatted).unwrap();
        assert_eq!(ast, reparsed, "Roundtrip failed");
    }
}
