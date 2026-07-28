//! Render a `flow.plan` observation (a durable authored or host-built execution DAG) as a styled ratatui block — the
//! same tree the CLI prints, brought to the TUI. We prefer the observation's `plan_ast` so the tree
//! is syntax-highlighted via [`flux_flow::render::render_styled`] + our ANSI palette; if only the
//! pre-rendered `plan` string is present we show that plain.

use ansi_to_tui::IntoText;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::theme::{self, Theme};

/// Render the plan from a `flow.plan` observation's `data`: a `plan · <risk> · N op(s)` header line
/// followed by the highlighted DAG. Empty when neither an AST nor a plain plan string is present.
///
/// A resumed/halted plan (`resumed: true`, A-17) carries per-statement ✓/✗/· status markers in its
/// `plan` text instead of full syntax highlighting — patch-and-continue's granularity is top-level
/// statements only — so that text is rendered directly (marker-colored, one `Line` per statement)
/// rather than reconstructing a fresh, unmarked tree from `plan_ast`.
pub fn render(data: &Value, theme: &Theme) -> Vec<Line<'static>> {
    let risk = data.get("risk").and_then(|v| v.as_str()).unwrap_or("");
    let ops = data.get("ops").and_then(|v| v.as_u64()).unwrap_or(0);
    let historical = data
        .get("historical")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut header = vec![Span::styled(
        "plan",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if historical {
        header.push(Span::styled("  · historical", theme.muted_style()));
    } else if !risk.is_empty() {
        header.push(Span::raw("  "));
        header.push(Span::styled(risk.to_string(), risk_style(risk, theme)));
    }
    if !historical {
        let plural = if ops == 1 { "" } else { "s" };
        header.push(Span::styled(
            format!("  · {ops} op{plural}"),
            theme.muted_style(),
        ));
    }

    let resumed = data
        .get("resumed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if resumed {
        let Some(text) = data.get("plan").and_then(|v| v.as_str()) else {
            return Vec::new();
        };
        let mut out = vec![Line::from(header)];
        for line in text.lines() {
            let style = match line.chars().next() {
                Some('✓') => theme.ok_style(),
                Some('✗') => theme.err_style(),
                Some('·') => theme.muted_style(),
                _ => Style::default(),
            };
            out.push(Line::styled(line.to_string(), style));
        }
        return out;
    }

    let tree_ansi = data
        .get("plan_ast")
        .and_then(|v| serde_json::from_value::<flux_flow::ast::DraftAst>(v.clone()).ok())
        .map(|ast| flux_flow::render::render_styled(&ast, &theme::plan_palette()))
        .or_else(|| {
            data.get("plan")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });

    let Some(tree_ansi) = tree_ansi else {
        return Vec::new();
    };

    let mut out = vec![Line::from(header)];
    match tree_ansi.into_text() {
        Ok(text) => out.extend(text.lines),
        Err(_) => out.extend(tree_ansi.lines().map(|l| Line::raw(l.to_string()))),
    }
    out
}

/// Color a risk summary like the CLI's `risk_badge`: low/no-op green, medium yellow, else red.
/// Shared with the approval sheet's plan badge (C-182) so the two can't drift.
pub(crate) fn risk_style(summary: &str, theme: &Theme) -> Style {
    match summary.split([' ', '·']).next().unwrap_or("").trim() {
        "low" | "no-op" => theme.ok_style(),
        "medium" => theme.warn_style(),
        _ => theme.err_style(),
    }
}

/// Render a gather-plan `flow.plan` observation's `data` as a compact one-liner (A-15): op names
/// (+ a short arg) pulled off the plan's call nodes, joined `·`-separated after a `gathering`
/// label — never the full tree + risk badge [`render`] gives a full execution plan. Mirrors the
/// CLI's `gather_compact_line`.
pub fn render_compact(data: &Value, theme: &Theme) -> Vec<Line<'static>> {
    Vec::from([Line::from(vec![
        Span::styled(
            "gathering",
            theme.muted_style().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {}", ops_summary(data)), theme.muted_style()),
    ])])
}

/// The op-list summary shared by [`render_compact`]: each call node's op + a best-effort short arg
/// (via `toolview::format_call`, the same formatter tool cards use), falling back to a bare op
/// count when the AST can't be walked.
fn ops_summary(data: &Value) -> String {
    let calls = data
        .get("plan_ast")
        .and_then(|v| serde_json::from_value::<flux_flow::ast::DraftAst>(v.clone()).ok())
        .map(|ast| {
            let mut out = Vec::new();
            for n in &ast.body {
                collect_calls(n, &mut out);
            }
            out
        })
        .unwrap_or_default();
    if calls.is_empty() {
        let ops = data.get("ops").and_then(|v| v.as_u64()).unwrap_or(0);
        let plural = if ops == 1 { "" } else { "s" };
        return format!("{ops} op{plural}");
    }
    calls
        .iter()
        .map(|(op, input)| {
            let call = crate::toolview::format_call(op, input);
            if call.arg.is_empty() {
                call.verb
            } else {
                format!("{} {}", call.verb, call.arg)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Walk a gather plan's top-level shape (a `Call`, a `$x = Call(...)` bind, or a `seq` of either)
/// collecting each call's op name + its input (the single literal-object argument a tool call
/// carries, when the plan author wrote one plainly). Mirrors the CLI's `collect_plan_calls`.
fn collect_calls(node: &flux_flow::ast::Node, out: &mut Vec<(String, Value)>) {
    use flux_flow::ast::Node;
    match node {
        Node::Call { op, args } => {
            let input = args
                .first()
                .and_then(|a| match a {
                    Node::Lit { value } => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or(Value::Null);
            out.push((op.clone(), input));
        }
        Node::Bind { value, .. } => collect_calls(value, out),
        Node::Seq { body, .. } => body.iter().for_each(|n| collect_calls(n, out)),
        _ => {}
    }
}
