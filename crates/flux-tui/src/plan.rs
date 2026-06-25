//! Render a `flow.plan` observation (the planner's compiled DAG) as a styled ratatui block — the
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
pub fn render(data: &Value, theme: &Theme) -> Vec<Line<'static>> {
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

    let risk = data.get("risk").and_then(|v| v.as_str()).unwrap_or("");
    let ops = data.get("ops").and_then(|v| v.as_u64()).unwrap_or(0);

    let mut header = vec![Span::styled(
        "plan",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if !risk.is_empty() {
        header.push(Span::raw("  "));
        header.push(Span::styled(risk.to_string(), risk_style(risk, theme)));
    }
    let plural = if ops == 1 { "" } else { "s" };
    header.push(Span::styled(
        format!("  · {ops} op{plural}"),
        theme.muted_style(),
    ));

    let mut out = vec![Line::from(header)];
    match tree_ansi.into_text() {
        Ok(text) => out.extend(text.lines),
        Err(_) => out.extend(tree_ansi.lines().map(|l| Line::raw(l.to_string()))),
    }
    out
}

/// Color a risk summary like the CLI's `risk_badge`: low/no-op green, medium yellow, else red.
fn risk_style(summary: &str, theme: &Theme) -> Style {
    match summary.split([' ', '·']).next().unwrap_or("").trim() {
        "low" | "no-op" => theme.ok_style(),
        "medium" => theme.warn_style(),
        _ => theme.err_style(),
    }
}
