//! `flux export <run> -o run.html` (C-132): render a recorded run into ONE self-contained static
//! HTML file — a shareable artifact for bug reports, PR links, and demos. The read-only sibling of
//! the Time Machine verbs (`replay`/`fork`/`diff`).
//!
//! # Purity
//! Every code path here is a **read**: it opens the event store the same way `flux diff` does
//! ([`open_event_store`] + `EventStore` getters) and never constructs a provider or an agent
//! engine (unlike `flux replay`, which lazily builds one). No event is ever appended.
//!
//! # Plan visuals
//! The plan tree reuses [`flux_lang::render::render_styled_spans`] — the exact substrate
//! `flow_render`/`flux render` uses for the SVG plan-tree view (`crates/flux-tools/src/render.rs`)
//! — mapping each `(text, Role)` span to a CSS class instead of an SVG fill. No second renderer.
//!
//! # Redaction (C-22)
//! `run_trace`/`observations`/`plan_source` are already redacted at record time through the live
//! turn's `Redactor` (confirmed: `flux-flow/src/cassette.rs::RecordScope::record`,
//! `flux-flow/src/engine.rs::flush_observations`, L-38's `plan_source` writers). This command runs
//! every one of those fields through a FRESH `Redactor::new()` again anyway — cheap, idempotent,
//! defense-in-depth.
//!
//! Conversation `Message` text and `TurnSummary.user_input`/`answer` are the one place that
//! guarantee does **not** hold: `flux-flow/src/engine.rs::begin_turn_lifecycle` calls
//! `record_message`/`begin_turn` with the raw prompt, with no redactor in the path. For those
//! fields the fresh, per-export `Redactor` is the ONLY control available — and it is shape-based
//! only (`sk-…`/`ghp_…`/a JWT/…; see `flux_secret::SECRET_PREFIXES`), since a pure read has no way
//! to learn which arbitrary values were live secrets during that historical run. Still, every
//! string this command renders is routed through it — nothing is trusted verbatim.
//!
//! # Sub-agent nesting (A-59 / A-08)
//! A child run is correlated via `EventContext{agent_id: "subagent:<role>", correlation_id: parent
//! session}` ([`EventStore::children_of`]). Children are ordered by their parent's `subagent.trace`
//! observation (the point the `task` tool call actually landed), falling back to `children_of`'s
//! creation order for anything without one (e.g. a cancelled spawn), and rendered recursively —
//! nested `<details>` sections, indented via CSS — so a multi-level sub-agent tree stays readable.

use super::*;

use std::fmt::Write as _;

use flux_lang::ast::RunEvent;
use flux_lang::render::{render_styled_spans, Role};
use flux_secret::Redactor;

/// `flux export <run> -o run.html`. `run_arg` resolves exactly like `flux replay`/`flux fork`/
/// `flux diff` (`last`, or a literal session id). `out` writes to a file (parents created); with
/// none, the HTML goes to stdout — the same convention as `flux render`.
pub(super) fn run_export(run_arg: &str, out: Option<&str>) -> Result<()> {
    let events = open_event_store()?;
    let sid = resolve_run(&events, run_arg)?;
    let pricing = flux_credentials::load_pricing_table();
    let redactor = Redactor::new();
    let html = export_html(&events, &sid, &pricing, &redactor)?;
    match out {
        Some(path) => {
            let path = std::path::Path::new(path);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create {}", parent.display()))?;
                }
            }
            std::fs::write(path, &html).with_context(|| format!("write {}", path.display()))?;
            eprintln!("exported {sid} → {} ({} bytes)", path.display(), html.len());
        }
        None => println!("{html}"),
    }
    Ok(())
}

/// Resolve `<run>` the same way [`run_replay`]/[`run_fork`]/[`run_diff_cmd`] do: `last` is the most
/// recently created session; anything else must already exist.
fn resolve_run(events: &EventStore, arg: &str) -> Result<String> {
    if arg == "last" {
        events
            .latest_session()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .context("no recorded sessions")
    } else {
        events
            .info(arg)
            .with_context(|| format!("unknown session `{arg}`"))?;
        Ok(arg.to_string())
    }
}

// ---------------------------------------------------------------------------
// HTML assembly
// ---------------------------------------------------------------------------

fn export_html(
    events: &EventStore,
    sid: &str,
    pricing: &flux_core::PricingTable,
    redactor: &Redactor,
) -> Result<String> {
    let body = render_session(events, sid, pricing, redactor)?;
    Ok(format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>flux run {sid_esc}</title>\n\
         <style>{CSS}</style>\n\
         </head>\n\
         <body>\n\
         <header>\n\
         <h1>flux run <code>{sid_esc}</code></h1>\n\
         <p class=\"muted\">Exported by <code>flux export</code> (flux {version}) — a static, \
         offline snapshot. No network requests, no external assets, no scripts.</p>\n\
         </header>\n\
         <main>\n{body}\n</main>\n\
         </body>\n\
         </html>\n",
        sid_esc = esc(sid),
        version = env!("CARGO_PKG_VERSION"),
    ))
}

/// Render one session (and, recursively, its sub-agent children) as a `<details>` section. Nesting
/// depth is conveyed purely by DOM structure + the `.session .session` CSS rule (indentation,
/// border) — there is no separate depth counter to thread through.
fn render_session(
    events: &EventStore,
    sid: &str,
    pricing: &flux_core::PricingTable,
    redactor: &Redactor,
) -> Result<String> {
    let info = events.info(sid).map_err(|e| anyhow::anyhow!("{e}"))?;
    let turns = events.turns(sid).map_err(|e| anyhow::anyhow!("{e}"))?;
    let trace = events.run_trace(sid).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cost = events
        .cost_summary(sid, pricing)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let observations = events
        .observations(sid)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let children = events
        .children_of(sid)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut out = String::new();
    let _ = write!(out, "<details class=\"session\" open>\n<summary>");
    let _ = write!(out, "<span class=\"session-id\">{}</span>", esc(sid));
    if let Some(role) = info.context.agent_id.as_deref() {
        let _ = write!(
            out,
            " <span class=\"badge badge-role\">{}</span>",
            esc(role)
        );
    }
    let _ = write!(
        out,
        " <span class=\"muted\">model {} · created {} · updated {}</span>",
        esc(&info.model),
        fmt_ts(info.created_at_ms),
        fmt_ts(info.updated_at_ms),
    );
    out.push_str("</summary>\n<div class=\"session-body\">\n");

    out.push_str("<h4>Timeline</h4>\n");
    out.push_str(&render_turns(redactor, &turns));

    out.push_str("<h4>Operations</h4>\n");
    out.push_str(&render_run_trace(redactor, &trace));

    out.push_str("<h4>Cost</h4>\n");
    out.push_str(&render_cost(&cost));

    if !children.is_empty() {
        let ordered = order_children(&children, &observations);
        let _ = writeln!(out, "<h4>Sub-agents ({})</h4>", ordered.len());
        for child in &ordered {
            match render_session(events, child, pricing, redactor) {
                Ok(child_html) => out.push_str(&child_html),
                Err(e) => {
                    let _ = writeln!(
                        out,
                        "<p class=\"muted\">sub-agent {}: {}</p>",
                        esc(child),
                        esc(&e.to_string())
                    );
                }
            }
        }
    }

    out.push_str("</div>\n</details>\n");
    Ok(out)
}

/// Order a session's sub-agent children by the point their `subagent.trace` observation landed on
/// the parent's evidence trail (A-59/A-08: `data.session` is the child's stream id) — the exact
/// spawn anchor, not a timestamp guess. Anything in `children_of` with no matching observation
/// (e.g. a cancelled spawn) is appended afterward in `children_of`'s own (creation) order, so no
/// child is ever dropped.
fn order_children(children: &[String], observations: &[flux_evidence::Observation]) -> Vec<String> {
    let mut ordered: Vec<String> = Vec::new();
    for obs in observations {
        if obs.kind != "subagent.trace" {
            continue;
        }
        let Some(child) = obs.data.get("session").and_then(|v| v.as_str()) else {
            continue;
        };
        if children.iter().any(|c| c == child) && !ordered.iter().any(|c| c == child) {
            ordered.push(child.to_string());
        }
    }
    for c in children {
        if !ordered.iter().any(|o| o == c) {
            ordered.push(c.clone());
        }
    }
    ordered
}

/// The turn-by-turn narrative: prompt, plan tree(s), answer, timing — [`flux_events::TurnSummary`].
fn render_turns(redactor: &Redactor, turns: &[flux_events::TurnSummary]) -> String {
    if turns.is_empty() {
        return "<p class=\"muted\">no turns recorded</p>\n".to_string();
    }
    let mut out = String::new();
    for t in turns {
        let _ = writeln!(
            out,
            "<div class=\"turn\">\n<div class=\"turn-head\">turn {} · {} · <span class=\"badge\">{}</span></div>",
            t.turn_id,
            fmt_ts(t.started_at_ms),
            esc(&t.outcome),
        );
        let _ = writeln!(
            out,
            "<div class=\"turn-field\"><span class=\"label\">prompt</span>{}</div>",
            redact_esc(redactor, &t.user_input),
        );
        for pa in &t.plan_attempts {
            if pa.outcome == "accepted" {
                if let Some(src) = &pa.plan_source {
                    out.push_str(&render_plan_source(redactor, src));
                }
            } else {
                let _ = writeln!(
                    out,
                    "<div class=\"turn-field plan-attempt-{}\"><span class=\"label\">attempt (step {})</span>{}{}</div>",
                    esc(&pa.outcome),
                    pa.step,
                    esc(&pa.outcome),
                    pa.error
                        .as_deref()
                        .map(|e| format!(": {}", redact_esc(redactor, e)))
                        .unwrap_or_default(),
                );
            }
        }
        if let Some(answer) = &t.answer {
            let _ = writeln!(
                out,
                "<div class=\"turn-field\"><span class=\"label\">answer</span>{}</div>",
                redact_esc(redactor, answer),
            );
        }
        let ended = t
            .ended_at_ms
            .map(fmt_ts)
            .unwrap_or_else(|| "(unfinished)".to_string());
        let _ = writeln!(
            out,
            "<div class=\"turn-meta muted\">ended {ended} · {} call(s) · {} iteration(s)</div>\n</div>",
            t.calls, t.iterations,
        );
    }
    out
}

/// Parse+render `plan_source` (L-38: always parses when present) through the SAME
/// `render_styled_spans` substrate `flow_render`'s SVG tree view uses. A source that somehow fails
/// to parse falls back to a plain (unstyled) redacted block rather than dropping the plan.
fn render_plan_source(redactor: &Redactor, src: &str) -> String {
    match flux_lang::parse::parse(src) {
        Ok(ast) => {
            let mut out = String::from("<pre class=\"plan-tree\">");
            for line in render_styled_spans(&ast) {
                for (text, role) in line {
                    if text.is_empty() {
                        continue;
                    }
                    let redacted = redactor.redact(&text);
                    match role_class(role) {
                        Some(class) => {
                            let _ =
                                write!(out, "<span class=\"{class}\">{}</span>", esc(&redacted));
                        }
                        None => out.push_str(&esc(&redacted)),
                    }
                }
                out.push('\n');
            }
            out.push_str("</pre>\n");
            out
        }
        Err(_) => format!(
            "<pre class=\"plan-tree plan-tree-plain\">{}</pre>\n",
            redact_esc(redactor, src)
        ),
    }
}

fn role_class(role: Role) -> Option<&'static str> {
    match role {
        Role::Text => None,
        Role::Keyword => Some("tok-kw"),
        Role::Op => Some("tok-op"),
        Role::Symbol => Some("tok-sym"),
        Role::String => Some("tok-str"),
        Role::Lit => Some("tok-lit"),
        Role::Effect => Some("tok-eff"),
        Role::Connector => Some("tok-conn"),
        Role::Thing => Some("tok-thing"),
    }
}

/// Per-op results (and, for `write`/`edit`/`patch`, diffs) from the run trace's
/// [`RunEvent::OpRecorded`] cells — the durable, already-redacted cassette cells C-43/C-22 write.
fn render_run_trace(redactor: &Redactor, trace: &[RunEvent]) -> String {
    let mut out = String::new();
    let mut any = false;
    for ev in trace {
        if let RunEvent::OpRecorded {
            op,
            content,
            view,
            is_error,
            denied,
            redacted,
            truncated,
            ..
        } = ev
        {
            any = true;
            out.push_str(&render_op(
                redactor,
                op,
                content,
                view.as_deref(),
                *is_error,
                *denied,
                *redacted,
                *truncated,
            ));
        }
    }
    if !any {
        return "<p class=\"muted\">no operations recorded</p>\n".to_string();
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn render_op(
    redactor: &Redactor,
    op: &str,
    content: &str,
    view: Option<&str>,
    is_error: bool,
    denied: bool,
    redacted: bool,
    truncated: bool,
) -> String {
    let status_class = if denied {
        "op-denied"
    } else if is_error {
        "op-error"
    } else {
        "op-ok"
    };
    let mut out = String::new();
    let _ = write!(
        out,
        "<div class=\"op {status_class}\">\n<div class=\"op-head\">"
    );
    let _ = write!(out, "<code>{}</code>", esc(op));
    if denied {
        out.push_str(" <span class=\"badge badge-denied\">denied</span>");
    } else if is_error {
        out.push_str(" <span class=\"badge badge-error\">error</span>");
    }
    if redacted {
        out.push_str(" <span class=\"badge badge-redacted\">redacted</span>");
    }
    if truncated {
        out.push_str(" <span class=\"badge badge-truncated\">truncated</span>");
    }
    out.push_str("</div>\n");
    let _ = writeln!(
        out,
        "<div class=\"op-content\">{}</div>",
        redact_esc(redactor, content)
    );
    if let Some(v) = view {
        if v != content {
            if matches!(op, "write" | "edit" | "patch") {
                out.push_str(&render_diff(redactor, v));
            } else {
                let _ = writeln!(
                    out,
                    "<pre class=\"op-view\">{}</pre>",
                    redact_esc(redactor, v)
                );
            }
        }
    }
    out.push_str("</div>\n");
    out
}

/// Style a unified-diff `view` (from `write`/`edit`/`patch` — see `flux_tools::unified_diff` +
/// `edit_result`) line-by-line: `+`/`-`/`@@`/header lines get their own class, everything else is
/// plain context. Purely presentational (a CSS class per already-rendered line) — not a second diff
/// engine.
///
/// Redaction is applied to each line's CONTENT with its `+`/`-` marker split off first, not to the
/// raw line: `flux_secret::Redactor`'s shape heuristic scans for a credential prefix at a *token
/// boundary*, and `+`/`-` are not boundary characters (see `flux_secret::redact_patterns`), so
/// `+sk-ant-…` on an added line would otherwise slip past it — the marker glues onto the secret and
/// the whole thing reads as one non-matching token. Stripping the marker first re-exposes the
/// secret at a real boundary (the start of the string) before it ever reaches the redactor.
fn render_diff(redactor: &Redactor, view: &str) -> String {
    let mut out = String::from("<pre class=\"diff\">");
    for line in view.lines() {
        let (class, marker, rest) = if line.starts_with("+++") || line.starts_with("---") {
            ("diff-hdr", "", line)
        } else if line.starts_with("@@") {
            ("diff-hunk", "", line)
        } else if let Some(rest) = line.strip_prefix('+') {
            ("diff-add", "+", rest)
        } else if let Some(rest) = line.strip_prefix('-') {
            ("diff-del", "-", rest)
        } else {
            ("diff-ctx", "", line)
        };
        let _ = writeln!(
            out,
            "<span class=\"{class}\">{}{}</span>",
            esc(marker),
            redact_esc(redactor, rest)
        );
    }
    out.push_str("</pre>\n");
    out
}

/// The cost rollup — [`flux_events::ModelCost`], one row per model this session (or sub-agent) used.
fn render_cost(rows: &[flux_events::ModelCost]) -> String {
    if rows.is_empty() {
        return "<p class=\"muted\">no metered usage recorded</p>\n".to_string();
    }
    let mut out = String::from(
        "<table class=\"cost\">\n<thead><tr><th>model</th><th>calls</th><th>input</th>\
         <th>output</th><th>cache read</th><th>cost</th></tr></thead>\n<tbody>\n",
    );
    let mut total = 0.0_f64;
    let mut any_cost = false;
    for row in rows {
        let cost_str = match &row.cost {
            Some(m) => {
                total += m.usd;
                any_cost = true;
                format!(
                    "${:.4}{}",
                    m.usd,
                    if m.subscription {
                        " (subscription)"
                    } else {
                        ""
                    }
                )
            }
            None => "—".to_string(),
        };
        let _ = writeln!(
            out,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&row.model),
            row.calls,
            row.usage.input_tokens,
            row.usage.output_tokens,
            row.usage.cache_read_input_tokens,
            esc(&cost_str),
        );
    }
    out.push_str("</tbody>\n</table>\n");
    if any_cost {
        let _ = writeln!(out, "<p class=\"cost-total\">total: ${total:.4}</p>");
    }
    out
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn fmt_ts(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// HTML-escape. The only place raw text becomes markup — every dynamic string in this module
/// passes through this (directly, or via [`redact_esc`]) before it reaches the output buffer.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Redact, THEN escape — every piece of recorded text (conversation, plan, op output) goes through
/// this, never straight through [`esc`] alone. See the module doc's Redaction section for exactly
/// what this does and does not guarantee.
fn redact_esc(redactor: &Redactor, s: &str) -> String {
    esc(&redactor.redact(s))
}

const CSS: &str = r#"
:root {
  --bg: #ffffff;
  --bg-raised: #f6f8fa;
  --fg: #1f2328;
  --muted: #59636e;
  --border: #d1d9e0;
  --accent: #0969da;
  --ok: #1a7f37;
  --error: #cf222e;
  --warn: #9a6700;
  --diff-add-bg: #e6ffec;
  --diff-add-fg: #116329;
  --diff-del-bg: #ffebe9;
  --diff-del-fg: #82071e;
  --diff-hunk-fg: #6639ba;
  --kw: #cf222e;
  --op: #0969da;
  --sym: #953800;
  --str: #0a3069;
  --lit: #8250df;
  --eff: #1a7f37;
  --conn: #6e7781;
  --thing: #8250df;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #0d1117;
    --bg-raised: #161b22;
    --fg: #e6edf3;
    --muted: #8b949e;
    --border: #30363d;
    --accent: #4493f8;
    --ok: #3fb950;
    --error: #f85149;
    --warn: #d29922;
    --diff-add-bg: #033a16;
    --diff-add-fg: #7ee787;
    --diff-del-bg: #67060c;
    --diff-del-fg: #ffa198;
    --diff-hunk-fg: #d2a8ff;
    --kw: #ff7b72;
    --op: #79c0ff;
    --sym: #ffa657;
    --str: #a5d6ff;
    --lit: #d2a8ff;
    --eff: #7ee787;
    --conn: #8b949e;
    --thing: #d2a8ff;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0 auto;
  max-width: 62rem;
  padding: 1.5rem 1.25rem 4rem;
  background: var(--bg);
  color: var(--fg);
  font: 15px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
}
code, pre, .mono { font-family: ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace; }
h1 { font-size: 1.4rem; margin: 0 0 0.25rem; }
h4 { font-size: 0.85rem; text-transform: uppercase; letter-spacing: 0.04em; color: var(--muted);
     margin: 1.25rem 0 0.5rem; }
header { border-bottom: 1px solid var(--border); padding-bottom: 1rem; margin-bottom: 1.25rem; }
.muted { color: var(--muted); font-size: 0.85em; }
code { background: var(--bg-raised); border-radius: 4px; padding: 0.1em 0.35em; }
.session { border: 1px solid var(--border); border-radius: 8px; margin: 0.75rem 0; background: var(--bg); }
.session .session { margin-left: 1.25rem; border-left: 3px solid var(--border); }
.session > summary { cursor: pointer; padding: 0.6rem 0.9rem; background: var(--bg-raised);
  border-radius: 8px; font-weight: 600; }
.session-id { font-family: ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace; }
.session-body { padding: 0 0.9rem 0.9rem; }
.badge { display: inline-block; font-size: 0.75em; padding: 0.05em 0.5em; border-radius: 999px;
  border: 1px solid var(--border); color: var(--muted); }
.badge-role { color: var(--accent); border-color: var(--accent); }
.badge-error { color: var(--error); border-color: var(--error); }
.badge-denied { color: var(--warn); border-color: var(--warn); }
.badge-redacted, .badge-truncated { color: var(--warn); border-color: var(--warn); }
.turn { border: 1px solid var(--border); border-radius: 6px; padding: 0.6rem 0.8rem; margin: 0.6rem 0; }
.turn-head { font-weight: 600; margin-bottom: 0.35rem; }
.turn-field { margin: 0.35rem 0; white-space: pre-wrap; word-break: break-word; }
.turn-field .label { display: block; font-size: 0.72em; text-transform: uppercase; color: var(--muted);
  letter-spacing: 0.03em; margin-bottom: 0.1rem; }
.plan-attempt-compile_error, .plan-attempt-rejected { color: var(--warn); }
.turn-meta { margin-top: 0.35rem; }
.plan-tree { background: var(--bg-raised); border: 1px solid var(--border); border-radius: 6px;
  padding: 0.75rem; overflow-x: auto; white-space: pre; }
.op { border: 1px solid var(--border); border-left-width: 4px; border-radius: 6px; padding: 0.5rem 0.75rem;
  margin: 0.5rem 0; }
.op-ok { border-left-color: var(--ok); }
.op-error { border-left-color: var(--error); }
.op-denied { border-left-color: var(--warn); }
.op-head { font-weight: 600; margin-bottom: 0.25rem; }
.op-content { white-space: pre-wrap; word-break: break-word; }
.op-view, .diff { background: var(--bg-raised); border: 1px solid var(--border); border-radius: 6px;
  padding: 0.6rem; overflow-x: auto; white-space: pre; margin-top: 0.4rem; }
.diff-add { background: var(--diff-add-bg); color: var(--diff-add-fg); display: block; }
.diff-del { background: var(--diff-del-bg); color: var(--diff-del-fg); display: block; }
.diff-hunk { color: var(--diff-hunk-fg); display: block; }
.diff-hdr { color: var(--muted); display: block; }
.diff-ctx { display: block; }
table.cost { border-collapse: collapse; width: 100%; margin: 0.4rem 0; }
table.cost th, table.cost td { text-align: left; padding: 0.3rem 0.6rem; border-bottom: 1px solid var(--border); }
table.cost th { color: var(--muted); font-weight: 600; font-size: 0.8em; }
.cost-total { font-weight: 600; }
.tok-kw { color: var(--kw); font-weight: 600; }
.tok-op { color: var(--op); }
.tok-sym { color: var(--sym); }
.tok-str { color: var(--str); }
.tok-lit { color: var(--lit); }
.tok-eff { color: var(--eff); }
.tok-conn { color: var(--conn); }
.tok-thing { color: var(--thing); }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_session(events: &EventStore, prompt: &str) -> String {
        let sid = events.create_session("mock").unwrap();
        events
            .record_message(&sid, &flux_core::Message::user_text(prompt))
            .unwrap();
        let turn_id = events.begin_turn(&sid, prompt, "mock").unwrap();
        events
            .record_message(&sid, &flux_core::Message::assistant_text("done"))
            .unwrap();
        events
            .end_turn(&sid, turn_id, "chat", 1, "done", None)
            .unwrap();
        sid
    }

    /// Failing-first (C-132 acceptance #2): a run whose CONVERSATION carries a seeded,
    /// credential-shaped secret must export with the secret redacted. Conversation text is the one
    /// field C-22/L-38 never covers (see the module doc) — `record_message` stores it verbatim, so
    /// this genuinely fails until `flux export` routes rendered text through a `Redactor` itself.
    #[test]
    fn seeded_secret_in_conversation_is_redacted_in_the_export() {
        let events = EventStore::in_memory().unwrap();
        let secret = "sk-ant-testsecretseed1234567890";
        let sid = seed_session(&events, &format!("here is my key {secret}, please note it"));

        let pricing = flux_core::PricingTable::builtin();
        let redactor = Redactor::new();
        let html = export_html(&events, &sid, &pricing, &redactor).unwrap();

        assert!(
            !html.contains(secret),
            "raw secret leaked into the export:\n{html}"
        );
        assert!(
            html.contains("[redacted]"),
            "no redaction marker found:\n{html}"
        );
    }

    /// A run with no plan/op activity still exports a well-formed, self-contained document — the
    /// base case the golden test's richer run builds on.
    #[test]
    fn exports_a_minimal_session_as_one_self_contained_document() {
        let events = EventStore::in_memory().unwrap();
        let sid = seed_session(&events, "say hello");

        let pricing = flux_core::PricingTable::builtin();
        let redactor = Redactor::new();
        let html = export_html(&events, &sid, &pricing, &redactor).unwrap();

        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains(&sid));
        assert!(html.contains("say hello"));
        assert!(!html.contains("<script"), "must ship with no JS:\n{html}");
        assert!(
            !html.contains("http://") && !html.contains("https://"),
            "must reference no network resource:\n{html}"
        );
    }

    /// Pure read: rendering the export must never append to the store (no new events beyond the
    /// two seeded messages + turn lifecycle).
    #[test]
    fn export_never_writes_to_the_event_store() {
        let events = EventStore::in_memory().unwrap();
        let sid = seed_session(&events, "say hello");
        let before = events.load_stream(&sid, None).unwrap().len();

        let pricing = flux_core::PricingTable::builtin();
        let redactor = Redactor::new();
        let _ = export_html(&events, &sid, &pricing, &redactor).unwrap();

        let after = events.load_stream(&sid, None).unwrap().len();
        assert_eq!(before, after, "export appended events to the store");
    }

    /// A recorded `write` op's `OpRecorded.view` (a unified diff — see
    /// `flux_tools::unified_diff`/`edit_result`) renders as a styled diff block, added/removed
    /// lines redacted and classed separately, not just a plain `<pre>` dump.
    #[test]
    fn write_op_diff_renders_with_diff_line_styling() {
        let events = EventStore::in_memory().unwrap();
        let sid = seed_session(&events, "add a note");
        let diff_view = "wrote 6 bytes to note.txt\n\n\
             --- a/note.txt\n+++ b/note.txt\n@@ -0,0 +1 @@\n+sk-ant-testsecretseed1234567890\n";
        let cell = RunEvent::OpRecorded {
            seq: 0,
            step: flux_lang::ast::StepId::from("step_write_abc"),
            op: "write".to_string(),
            input_hash: "h".to_string(),
            input_hash_redacted: None,
            input_view: Some("{\"path\":\"note.txt\"}".to_string()),
            input_view_truncated: false,
            content: "wrote 6 bytes to note.txt".to_string(),
            view: Some(diff_view.to_string()),
            is_error: false,
            denied: false,
            redacted: false,
            truncated: false,
        };
        events
            .append(&sid, flux_events::NewEvent::run(cell))
            .unwrap();

        let pricing = flux_core::PricingTable::builtin();
        let redactor = Redactor::new();
        let html = export_html(&events, &sid, &pricing, &redactor).unwrap();

        assert!(html.contains("class=\"diff-add\""), "{html}");
        assert!(html.contains("class=\"diff-hunk\""), "{html}");
        // Even a diff line is routed through the redactor.
        assert!(
            !html.contains("sk-ant-testsecretseed1234567890"),
            "diff content bypassed redaction:\n{html}"
        );
    }

    /// A-59/A-08: a sub-agent child (correlated via `children_of`) is rendered nested under its
    /// parent, not just appended as a second top-level session.
    #[test]
    fn sub_agent_child_is_nested_under_the_parent() {
        let events = EventStore::in_memory().unwrap();
        let parent = seed_session(&events, "delegate this");
        let child_ctx = flux_events::EventContext {
            agent_id: Some("subagent:reviewer".to_string()),
            correlation_id: Some(parent.clone()),
            ..Default::default()
        };
        let child = events
            .create_session_with_context("mock", &child_ctx)
            .unwrap();
        events
            .record_message(&child, &flux_core::Message::user_text("review this"))
            .unwrap();

        let pricing = flux_core::PricingTable::builtin();
        let redactor = Redactor::new();
        let html = export_html(&events, &parent, &pricing, &redactor).unwrap();

        let parent_pos = html.find(&parent).expect("parent session id present");
        let child_pos = html.find(&child).expect("child session id present");
        assert!(
            child_pos > parent_pos,
            "child must be nested AFTER the parent opens:\n{html}"
        );
        assert!(
            html.contains("subagent:reviewer"),
            "child role label missing:\n{html}"
        );
        assert!(
            html.contains("Sub-agents"),
            "no sub-agents section:\n{html}"
        );
    }
}
