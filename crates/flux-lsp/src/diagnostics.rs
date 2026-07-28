//! `diagnostics` — what the editor squiggles, and how loudly (L-89).
//!
//! Two sources feed one list. **Parse errors** come from the tolerant CST parse and always have a
//! real span. **Analyzer findings** come from `flux_lang::analyze::lower` run per declaration
//! against the [`crate::catalog`], with each finding's rendered node path resolved back to a source
//! range through the L-59 side-map.
//!
//! Round 2 fixed two ways this surface lied. It now analyzes against the *workspace* catalog, so a
//! call to a composite living in `.flux/flows` is no longer reported as an unknown operation while
//! `flux flow run` executes it happily. And every finding carries a **stable code** plus a severity
//! that reflects what the finding means: reaching `analyze::lower`'s error path means the
//! declaration does not lower, so it cannot run — that is an `ERROR`, not the blanket `WARNING`
//! every finding used to get. [`ADVISORY`] is the (currently empty) escape hatch for findings that
//! do not block; a new soft check adds its code there rather than downgrading the whole surface.

use std::collections::HashSet;

use flux_lang::lower_cst::LoweredModule;
use flux_lang::parser::Parse;
use flux_lang::program::{CompositeOpDecl, Module, Program};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};

use crate::convert::{source_range, LineIndex};

/// Codes whose findings are hints rather than failures. Everything else makes the declaration
/// un-runnable and is reported as an `ERROR`.
const ADVISORY: &[&str] = &[];

/// The stable, client-filterable code for an analyzer message.
fn code_for(message: &str) -> &'static str {
    const TABLE: &[(&str, &str)] = &[
        ("unknown operation", "unknown-operation"),
        ("unbound symbol", "unbound-symbol"),
        ("missing required parameter", "missing-parameter"),
        ("recursive composite op cycle", "composite-cycle"),
        ("duplicate composite op", "duplicate-op"),
        ("duplicate `parallel` branch name", "duplicate-binding"),
        ("duplicate `race` branch name", "duplicate-binding"),
        ("duplicate `route` case label", "duplicate-binding"),
        ("expected", "type-mismatch"),
    ];
    TABLE
        .iter()
        .find(|(needle, _)| message.contains(needle))
        .map(|(_, code)| *code)
        .unwrap_or("analyzer-finding")
}

fn diagnostic(
    range: Range,
    message: String,
    code: &str,
    severity: DiagnosticSeverity,
) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(code.into())),
        source: Some("flux-lsp".into()),
        message,
        ..Default::default()
    }
}

/// An analyzer finding, coded and severity-classified.
fn finding(range: Range, message: String) -> Diagnostic {
    let code = code_for(&message);
    let severity = if ADVISORY.contains(&code) {
        DiagnosticSeverity::WARNING
    } else {
        DiagnosticSeverity::ERROR
    };
    diagnostic(range, message, code, severity)
}

/// Tolerant-parse errors from an already-built CST, as positioned LSP diagnostics.
pub fn parse_errors(parsed: &Parse, text: &str, index: &LineIndex) -> Vec<Diagnostic> {
    parsed
        .errors
        .iter()
        .map(|e| {
            diagnostic(
                source_range(e.range, text, index),
                e.message.clone(),
                "parse-error",
                DiagnosticSeverity::ERROR,
            )
        })
        .collect()
}

/// The full diagnostic list for a document: parse errors, or — on a cleanly parsing buffer — the
/// analyzer findings for every declaration.
pub fn diagnostics(
    registry: &flux_runtime::ToolRegistry,
    workspace: &[CompositeOpDecl],
    parsed: &Parse,
    text: &str,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    let syntax = parse_errors(parsed, text, index);
    if !syntax.is_empty() {
        return syntax;
    }
    analyzer_findings(registry, workspace, parsed, text, index)
}

fn analyzer_findings(
    registry: &flux_runtime::ToolRegistry,
    workspace: &[CompositeOpDecl],
    parsed: &Parse,
    text: &str,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    let lowered = match flux_lang::lower_cst::cst_to_module(parsed) {
        Ok(lowered) => lowered,
        Err(errors) => {
            return errors
                .into_iter()
                .map(|error| {
                    let range = error
                        .range
                        .map(|range| source_range(range, text, index))
                        .unwrap_or_default();
                    finding(range, error.message)
                })
                .collect();
        }
    };
    match &lowered.module {
        Module::Flow(ast) => {
            let catalog = flux_flow::registry::OpRegistry::new(registry).with_composites(workspace);
            declaration_findings(ast, &catalog, lowered.flows.first(), text, index)
        }
        Module::Program(program) => {
            program_diagnostics(registry, workspace, program, &lowered, text, index)
        }
    }
}

fn program_diagnostics(
    registry: &flux_runtime::ToolRegistry,
    workspace: &[CompositeOpDecl],
    program: &Program,
    lowered: &LoweredModule,
    text: &str,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    // The buffer's own composites shadow same-named workspace ones — the same precedence the host
    // applies when a project flow overrides a global one.
    let local: HashSet<&str> = program.ops.iter().map(|op| op.name.as_str()).collect();
    let mut visible = program.ops.clone();
    visible.extend(
        workspace
            .iter()
            .filter(|op| !local.contains(op.name.as_str()))
            .cloned(),
    );
    let catalog = flux_flow::registry::OpRegistry::new(registry).with_composites(&visible);
    let mut diagnostics = Vec::new();

    // Body diagnostics are analyzed declaration-by-declaration below for precise ranges. Keep only
    // module-level composite findings here (duplicates, cycles, metadata surface, await).
    if let Err(findings) = flux_flow::registry::analyze_composites(&program.ops, registry) {
        diagnostics.extend(
            findings
                .into_iter()
                .filter(|f| !f.message.contains("(at `body"))
                // A workspace composite the buffer calls is resolvable at run time; only the
                // buffer's own declarations are checked for structural problems here.
                .filter(|f| !unknown_op_covered_by(&f.message, workspace))
                .map(|f| {
                    let op_index = composite_index_for_message(program, &f.message);
                    let range = op_index
                        .and_then(|i| lowered.ops.get(i))
                        .map(|ranges| source_range(ranges.declaration, text, index))
                        .unwrap_or_default();
                    finding(range, f.message)
                }),
        );
    }

    for (i, op) in program.ops.iter().enumerate() {
        diagnostics.extend(declaration_findings(
            &op.body,
            &catalog,
            lowered.ops.get(i),
            text,
            index,
        ));
    }
    for (i, flow) in program.flows.iter().enumerate() {
        diagnostics.extend(declaration_findings(
            flow,
            &catalog,
            lowered.flows.get(i),
            text,
            index,
        ));
    }
    diagnostics
}

/// Does this "unknown operation" finding name a composite the workspace provides? `analyze_composites`
/// only sees the buffer's declarations, so it cannot know about `.flux/flows`.
fn unknown_op_covered_by(message: &str, workspace: &[CompositeOpDecl]) -> bool {
    message.contains("unknown operation")
        && workspace
            .iter()
            .any(|op| message.contains(&format!("`{}`", op.name)))
}

fn declaration_findings(
    ast: &flux_lang::ast::DraftAst,
    catalog: &dyn flux_lang::opspec::OpCatalog,
    ranges: Option<&flux_lang::lower_cst::DeclarationRanges>,
    text: &str,
    index: &LineIndex,
) -> Vec<Diagnostic> {
    let Err(findings) = flux_lang::analyze::lower(ast, catalog, &HashSet::new()) else {
        return Vec::new();
    };
    findings
        .into_iter()
        .map(|f| {
            let precise = ranges.and_then(|ranges| ranges.body.resolve_diagnostic(&f.message));
            let range = precise
                .map(|range| source_range(range, text, index))
                .or_else(|| ranges.map(|ranges| source_range(ranges.declaration, text, index)))
                .unwrap_or_default();
            let message = if precise.is_none() && f.message.contains("(at `") {
                format!(
                    "{} (declaration range — body range map incomplete)",
                    f.message
                )
            } else {
                f.message
            };
            finding(range, message)
        })
        .collect()
}

fn composite_index_for_message(program: &Program, message: &str) -> Option<usize> {
    if message.starts_with("duplicate composite op") {
        return program.ops.iter().enumerate().find_map(|(i, op)| {
            let duplicated = program.ops[..i].iter().any(|prior| prior.name == op.name);
            (duplicated && message.contains(&format!("`{}`", op.name))).then_some(i)
        });
    }
    program.ops.iter().position(|op| {
        message.contains(&format!("`{}`", op.name))
            || (message.starts_with("recursive composite op cycle:")
                && message.split_whitespace().any(|part| {
                    part.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
                        == op.name
                }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::TempWorkspace;
    use crate::catalog::{authoring_registry, workspace_composites};

    fn diagnose(src: &str) -> Vec<Diagnostic> {
        diagnose_with(src, &[])
    }

    fn diagnose_with(src: &str, workspace: &[CompositeOpDecl]) -> Vec<Diagnostic> {
        let parsed = flux_lang::parser::parse_cst(src);
        let index = LineIndex::new(src);
        diagnostics(&authoring_registry(), workspace, &parsed, src, &index)
    }

    fn code(d: &Diagnostic) -> String {
        match &d.code {
            Some(NumberOrString::String(s)) => s.clone(),
            other => panic!("expected a string code, got {other:?}"),
        }
    }

    #[test]
    fn parse_errors_have_positioned_ranges() {
        let diags = diagnose("flow f\n  $a =\n  $b = 1\n");
        assert!(
            !diags.is_empty(),
            "expected a diagnostic for the empty bind RHS"
        );
        assert!(diags.iter().all(|d| d.range.start.line <= 2));
        assert!(diags.iter().all(|d| code(d) == "parse-error"));
    }

    #[test]
    fn stable_host_ops_do_not_report_unknown_operation() {
        let src = r#"flow research
  $response = http.request({url: "https://example.com/api", method: "GET"})
  $page = web.fetch("https://example.com")
  $hits = search({query: "flux", limit: 2})
  $inventory = sources()
  $claims = ai.extract({from: $page, ask: "facts", schema: "Claim[]"})
  $ranked = ai.rank({items: $claims, by: "support"})
  $answer = synth({claims: $ranked, format: "detailed", cite: true})
  return $answer
"#;
        let diagnostics = diagnose(src);
        assert!(
            diagnostics.is_empty(),
            "stable host ops must analyze cleanly: {diagnostics:?}"
        );
    }

    #[test]
    fn an_unknown_operation_is_an_error_with_a_code() {
        let diagnostics = diagnose("flow f\n  made.up()\n");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0]
            .message
            .contains("unknown operation: `made.up`"));
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(code(&diagnostics[0]), "unknown-operation");
    }

    #[test]
    fn an_unbound_symbol_is_an_error_with_a_code() {
        let src = "op broken(value: String) -> String\n  return $missing\n\nflow run\n  return broken(\"x\")\n";
        let diagnostics = diagnose(src);
        let unbound = diagnostics
            .iter()
            .find(|d| d.message.contains("$missing"))
            .expect("unbound diagnostic");
        assert_eq!(unbound.range.start.line, 1);
        assert_eq!(unbound.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(code(unbound), "unbound-symbol");
    }

    #[test]
    fn a_workspace_composite_is_not_reported_as_unknown() {
        let home = TempWorkspace::with_flows(
            "flux-lsp-diagnostics",
            &[
                (
                    "shout.flux",
                    "op shout(text: String) -> String\n  description \"Shout\"\n  risk \"low\"\n  idempotency \"idempotent\"\n  return $text\n",
                ),
                // Unparseable neighbour: skipped, never fatal.
                ("broken.flux", "op ??? not flux at all\n"),
            ],
        );
        let workspace = workspace_composites(home.path());

        let src = "flow f\n  return shout(\"hi\")\n";
        assert!(
            diagnose_with(src, &workspace).is_empty(),
            "a workspace composite resolves: {:?}",
            diagnose_with(src, &workspace)
        );
        // A genuinely undefined op still squiggles, with the workspace catalog loaded.
        let missing = diagnose_with("flow f\n  return definitely_missing()\n", &workspace);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert_eq!(code(&missing[0]), "unknown-operation");
    }

    #[test]
    fn module_resolves_forward_composite_and_ranges_later_flow_error() {
        let src = r#"flow first
  $one = summarize("one")
  return $one

op summarize(text: String) -> String
  description "Summarize text"
  risk "low"
  idempotency "non_idempotent"
  effects [network]
  expose false
  $prompt = fmt("Summarize: {text}")
  $answer = ai.reason($prompt)
  return $answer

flow second
  $bad = definitely_missing()
  return $bad
"#;
        let diagnostics = diagnose(src);
        assert_eq!(
            diagnostics.len(),
            1,
            "only the real unknown op: {diagnostics:?}"
        );
        assert!(diagnostics[0].message.contains("definitely_missing"));
        let expected_line = src[..src.find("$bad =").unwrap()].matches('\n').count() as u32;
        assert_eq!(diagnostics[0].range.start.line, expected_line);
    }

    #[test]
    fn module_reports_composite_cycle_at_a_declaration() {
        let src = "op first() -> String\n  return second()\n\nop second() -> String\n  return first()\n\nflow run\n  return first()\n";
        let diagnostics = diagnose(src);
        let cycle = diagnostics
            .iter()
            .find(|d| d.message.contains("recursive composite op cycle"))
            .expect("cycle diagnostic");
        assert!(cycle.range.start.line == 0 || cycle.range.start.line == 3);
        assert_eq!(code(cycle), "composite-cycle");
        assert_eq!(cycle.severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn module_reports_wrong_composite_arguments_at_call_site() {
        let src = "op echo(value: String) -> String\n  return $value\n\nflow run\n  return echo({wrong: \"x\"})\n";
        let diagnostics = diagnose(src);
        let missing = diagnostics
            .iter()
            .find(|d| d.message.contains("missing required parameter `value`"))
            .expect("arity diagnostic");
        assert_eq!(missing.range.start.line, 4);
        assert_eq!(code(missing), "missing-parameter");
    }
}
