//! L-59 acceptance: analyzer diagnostics resolve to real source ranges via the CST range
//! side-map. The analyzer stays message-only (its rendered node path is the locator); the
//! [`flux_lang::lower_cst::RangeMap`] turns that path back into a `TextRange`.

use std::collections::HashSet;

use flux_lang::lower_cst::parse_with_ranges;
use flux_lang::opspec::{OpCatalog, OpSignature};
use flux_spec::{Idempotency, Risk};

/// A minimal catalog: `read(path)` and `two(a, b)` are registered, nothing else.
struct MiniCatalog;

impl OpCatalog for MiniCatalog {
    fn lookup(&self, name: &str) -> Option<OpSignature> {
        let (required, description) = match name {
            "read" => (vec!["path".to_string()], "read a file"),
            "two" => (vec!["a".to_string(), "b".to_string()], "two params"),
            _ => return None,
        };
        Some(OpSignature {
            name: name.to_string(),
            description: description.to_string(),
            effects: vec![],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            required_params: required,
            optional_params: vec![],
            param_types: Default::default(),
            semantic_effects: vec![],
        })
    }
}

#[test]
fn analyzer_diagnostics_carry_ranges() {
    // Line 4 (`$y = read($nope)`) references an unbound symbol; line 6 nests it inside `when`.
    let src = "\
flow f(count: Number) -> String
  $x = read(\"a.txt\")
  $y = read($nope)
  when $count > 3
    $z = read($also_missing)
  return $x
";
    let lowered = parse_with_ranges(src).expect("parses");
    let errs = flux_lang::analyze::analyze_flow(&lowered.ast, &MiniCatalog, &HashSet::new())
        .expect_err("unbound symbols must be diagnosed");

    // Every path-carrying diagnostic resolves to a range, and the ranges point at the right lines.
    let mut resolved = 0;
    for d in &errs {
        if let Some(range) = lowered.ranges.resolve_diagnostic(&d.message) {
            resolved += 1;
            let text = &src[range];
            if d.message.contains("$nope") {
                assert!(
                    text.contains("read($nope)"),
                    "diagnostic {:?} resolved to wrong span {text:?}",
                    d.message
                );
            }
            if d.message.contains("$also_missing") {
                assert!(
                    text.contains("read($also_missing)"),
                    "nested diagnostic {:?} resolved to wrong span {text:?}",
                    d.message
                );
            }
        }
    }
    assert!(
        resolved >= 2,
        "expected at least the two unbound-symbol diagnostics to resolve, got {resolved} of {:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn arity_error_resolves_to_the_call_statement() {
    let src = "flow f\n  $x = read(\"a.txt\")\n  two(\"only-one\")\n  return $x\n";
    let lowered = parse_with_ranges(src).expect("parses");
    let errs = flux_lang::analyze::analyze_flow(&lowered.ast, &MiniCatalog, &HashSet::new())
        .expect_err("arity error expected");
    let arity = errs
        .iter()
        .filter_map(|d| lowered.ranges.resolve_diagnostic(&d.message))
        .find(|r| src[*r].contains("two("));
    assert!(
        arity.is_some(),
        "no arity diagnostic resolved to the `two(…)` line: {:?}",
        errs.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}
