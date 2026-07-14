//! `flow_render`: Flux-Lang source/plan → syntax-highlighted SVG.
//!
//! The portable-image counterpart to the editor highlighting stories — for the surfaces that
//! can't run a grammar (a GitHub README, Slack, docs, chat panels). Two views over one theme:
//! `Source` colours the code via [`flux_lang::highlight`] (CST-classified, total — malformed
//! source still renders), `Tree` colours the execution-path tree via
//! [`flux_lang::render::render_styled_spans`]. [`render_flux_svg`] is the pure core (no
//! [`ToolContext`]) shared by the model-facing tool, the `flux render` CLI (L-77), and doc-image
//! regeneration. [`ToolResult`] is text-only, so the tool stays read-only and SVG-only.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_lang::highlight::{highlight, HighlightClass};
use flux_lang::program::{CompositeOpDecl, Module};
use flux_lang::render::{render_statement, render_styled_spans, Palette, Role};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};

// Theme — ported verbatim from `flux-tree-sitter/scripts/render-example.mjs` (One Dark), so the
// source view matches the README image that script used to generate.
const BG: &str = "#282c34";
const FG: &str = "#abb2bf";
const DOTS: [&str; 3] = ["#e06c75", "#e5c07b", "#98c379"];
const KEYWORD: &str = "#c678dd";
const OP: &str = "#61afef";
const VAR: &str = "#e06c75";
const ANNOTATION: &str = "#56b6c2";
const STRING: &str = "#98c379";
const NUMBER: &str = "#d19a66";
const COMMENT: &str = "#7f848e";
const TYPE: &str = "#e5c07b";
const PUNCT: &str = "#828997";
const BRACKET: &str = "#abb2bf";
const CONNECTOR: &str = "#5c6370";

// Layout — the same numbers as the Node script. `CHAR_ADVANCE` is a padded monospace advance
// estimate used for canvas sizing only; tspans flow at the font's real advance.
const FONT_SIZE: usize = 15;
const CHAR_ADVANCE: f64 = 9.4;
const LINE_HEIGHT: usize = 22;
const PAD: usize = 22;
const HEADER: usize = 44;
const BOTTOM: usize = 18;
const FONT: &str =
    "ui-monospace, 'SF Mono', 'JetBrains Mono', Menlo, Consolas, 'Liberation Mono', monospace";

/// Which rendering [`render_flux_svg`] emits: the highlighted source, or the execution-path tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Source,
    Tree,
}

/// Render Flux-Lang `src` as a self-contained SVG. `Source` is total (malformed source still
/// renders — the CST is error-recovering); `Tree` needs parseable source and errors otherwise.
pub fn render_flux_svg(src: &str, view: View) -> Result<String> {
    render(src, view).map(|r| r.svg)
}

/// A rendered SVG plus the layout facts the tool's summary line reports.
struct Rendered {
    svg: String,
    width: usize,
    height: usize,
    lines: usize,
}

/// One line of the canvas: coloured text fragments.
type ColoredLine = Vec<(String, &'static str)>;

fn render(src: &str, view: View) -> Result<Rendered> {
    let lines = match view {
        View::Source => source_lines(src),
        View::Tree => tree_lines(src)?,
    };
    Ok(svg_of(&lines))
}

/// The token's fill colour. Brackets keep the foreground colour (the JS theme's
/// `punctuation.bracket`); every other punctuation/operator token dims.
fn class_color(class: HighlightClass, token: &str) -> &'static str {
    match class {
        HighlightClass::Keyword => KEYWORD,
        HighlightClass::Op => OP,
        HighlightClass::Var => VAR,
        HighlightClass::Annotation => ANNOTATION,
        HighlightClass::String => STRING,
        HighlightClass::Number => NUMBER,
        HighlightClass::Comment => COMMENT,
        HighlightClass::Type => TYPE,
        HighlightClass::Punct => match token {
            "(" | ")" | "[" | "]" | "{" | "}" => BRACKET,
            _ => PUNCT,
        },
        HighlightClass::Error => FG,
    }
}

/// The tree-role's fill colour ([`Role::Text`] is the structural glue — plain foreground).
fn role_color(role: Role) -> &'static str {
    match role {
        Role::Text => FG,
        Role::Keyword => KEYWORD,
        Role::Op => OP,
        Role::Symbol => VAR,
        Role::String => STRING,
        Role::Lit => NUMBER,
        Role::Effect => ANNOTATION,
        Role::Connector => CONNECTOR,
        Role::Thing => TYPE,
    }
}

/// Colour the source by its highlight spans: per line, runs of same-coloured `char`s. The grid is
/// char-indexed, not byte-indexed — sigils and interpolation glyphs are multi-byte — and a
/// multi-line span (a `"""…"""` string) colours every line it covers, because each char looks up
/// its own span.
fn source_lines(src: &str) -> Vec<ColoredLine> {
    let spans = highlight(src);
    let body = src.strip_suffix('\n').unwrap_or(src);
    let mut out = Vec::new();
    let mut si = 0; // sweep index into the ordered, non-overlapping spans
    let mut offset = 0usize; // byte offset of the current char in `src`
    for line in body.split('\n') {
        let mut frags: ColoredLine = Vec::new();
        for ch in line.chars() {
            while si < spans.len() && usize::from(spans[si].0.end()) <= offset {
                si += 1;
            }
            let color = match spans.get(si) {
                Some((r, class)) if usize::from(r.start()) <= offset => {
                    class_color(*class, &src[*r])
                }
                _ => FG, // between spans (whitespace)
            };
            match frags.last_mut() {
                Some((text, c)) if *c == color => text.push(ch),
                _ => frags.push((ch.to_string(), color)),
            }
            offset += ch.len_utf8();
        }
        offset += 1; // the '\n'
        out.push(frags);
    }
    out
}

/// The tree view — flow-first: flows render as [`render_styled_spans`] trees; composite ops get a
/// best-effort statement-head list (they have no tree renderer); a module with neither falls back
/// to the source view. A hard parse error propagates (the tool surfaces it as an error result).
fn tree_lines(src: &str) -> Result<Vec<ColoredLine>> {
    let module = Module::parse_str(src).map_err(|e| {
        Error::Other(format!(
            "flow_render: tree view needs parseable source: {e}"
        ))
    })?;
    let mut lines: Vec<Vec<(String, Role)>> = Vec::new();
    match module {
        Module::Flow(ast) => lines.extend(render_styled_spans(&ast)),
        Module::Program(program) => {
            for f in &program.flows {
                if !lines.is_empty() {
                    lines.push(Vec::new());
                }
                lines.extend(render_styled_spans(f));
            }
            for op in &program.ops {
                if !lines.is_empty() {
                    lines.push(Vec::new());
                }
                lines.extend(op_head_lines(op));
            }
            if lines.is_empty() {
                // Agents/channels/… only: nothing has a tree — show the source instead.
                return Ok(source_lines(src));
            }
        }
    }
    Ok(lines
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|(text, role)| (text, role_color(role)))
                .collect()
        })
        .collect())
}

/// Best-effort tree for a composite op: the op header plus one connector-prefixed
/// [`render_statement`] head per body statement.
fn op_head_lines(op: &CompositeOpDecl) -> Vec<Vec<(String, Role)>> {
    let mut lines = vec![vec![
        ("op".to_string(), Role::Keyword),
        (format!(" {}", op.name), Role::Text),
    ]];
    let n = op.body.body.len();
    for (i, node) in op.body.body.iter().enumerate() {
        let connector = if i + 1 == n { "└─ " } else { "├─ " };
        lines.push(vec![
            (connector.to_string(), Role::Connector),
            (render_statement(node, &Palette::PLAIN), Role::Text),
        ]);
    }
    lines
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Emit the terminal-window SVG around `lines`: rounded dark canvas, three window dots, one
/// `<text>` per line. Width tracks the longest line (in chars); height the line count.
fn svg_of(lines: &[ColoredLine]) -> Rendered {
    let max_cols = lines
        .iter()
        .map(|l| l.iter().map(|(t, _)| t.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0)
        .max(1);
    let width = (PAD as f64 * 2.0 + max_cols as f64 * CHAR_ADVANCE).ceil() as usize;
    let height = HEADER + lines.len() * LINE_HEIGHT + BOTTOM;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" font-family=\"{FONT}\" font-size=\"{FONT_SIZE}\">\n  \
         <rect width=\"{width}\" height=\"{height}\" rx=\"10\" fill=\"{BG}\"/>\n"
    );
    for (i, c) in DOTS.iter().enumerate() {
        let cx = 20 + i * 20;
        let cy = HEADER / 2;
        svg.push_str(&format!(
            "  <circle cx=\"{cx}\" cy=\"{cy}\" r=\"6\" fill=\"{c}\"/>\n"
        ));
    }
    for (i, frags) in lines.iter().enumerate() {
        let y = HEADER + 16 + i * LINE_HEIGHT;
        let tspans: String = frags
            .iter()
            .map(|(t, c)| format!("<tspan fill=\"{c}\">{}</tspan>", esc(t)))
            .collect();
        svg.push_str(&format!(
            "  <text x=\"{PAD}\" y=\"{y}\" xml:space=\"preserve\">{tspans}</text>\n"
        ));
    }
    svg.push_str("</svg>\n");
    Rendered {
        svg,
        width,
        height,
        lines: lines.len(),
    }
}

/// Arguments for `flow_render` (mirrors `flow_run`: stored flows resolve through the same dirs).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FlowRenderInput {
    /// Inline Flux-Lang source to render. Pass exactly one of `source` / `name`.
    #[serde(default)]
    source: Option<String>,
    /// A stored flow to render: a filename stem under .flux/flows (or ~/.flux/flows) or a declared
    /// flow name. Pass exactly one of `source` / `name`.
    #[serde(default)]
    name: Option<String>,
    /// "source" (highlighted source, the default) or "tree" (the execution-path plan tree).
    #[serde(default)]
    view: Option<String>,
}

/// `flow_render(source|name, view?) -> SVG` — render Flux-Lang as a highlighted image.
struct FlowRenderTool;

#[async_trait]
impl Tool for FlowRenderTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flow_render".into(),
            description: "Render Flux-Lang as a syntax-highlighted SVG — pass inline `source` or \
                          the `name` of a stored flow (a file under .flux/flows or ~/.flux/flows; \
                          discover names with flow_list). `view: \"source\"` (default) renders the \
                          highlighted source; `view: \"tree\"` renders the execution-path plan \
                          tree. Returns the SVG markup inline — for surfaces that can't highlight \
                          .flux themselves (READMEs, Slack, docs, chat)."
                .into(),
            input_schema: flux_spec::tool_input_schema::<FlowRenderInput>(),
            output_schema: None,
            effects: vec![Effect::Read, Effect::Filesystem],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: FlowRenderInput = crate::parse_params(params, "flow_render")?;
        let view = match args.view.as_deref() {
            None | Some("source") => View::Source,
            Some("tree") => View::Tree,
            Some(v) => {
                return Err(Error::Other(format!(
                    "flow_render: unknown view `{v}` (expected \"source\" or \"tree\")"
                )))
            }
        };
        let (label, source) = match (args.source, args.name) {
            (Some(src), None) => ("inline".to_string(), src),
            (None, Some(name)) => {
                let src = resolve_source(ctx, &name)?;
                (name, src)
            }
            _ => {
                return Err(Error::Other(
                    "flow_render: pass exactly one of `source` or `name`".into(),
                ))
            }
        };
        match render(&source, view) {
            Ok(r) => {
                let view_word = match view {
                    View::Source => "source",
                    View::Tree => "tree",
                };
                Ok(ToolResult::ok_view(
                    r.svg,
                    format!(
                        "rendered {label} ({view_word} view) → SVG {}x{}, {} lines",
                        r.width, r.height, r.lines
                    ),
                ))
            }
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}

/// Resolve through the shared stored-flow catalog, keeping the raw text so source view renders the
/// selected file as written.
fn resolve_source(ctx: &ToolContext, name: &str) -> Result<String> {
    crate::flows::StoredFlowCatalog::load(ctx.system.as_ref())
        .resolve(name)
        .map(|resolved| resolved.source)
        .map_err(|e| Error::Other(format!("flow_render: {e}")))
}

/// Register the render pack: `flow_render`, beside `flow_list` / `flow_run`.
pub fn try_register_render(registry: &mut ToolRegistry) -> Result<()> {
    registry.try_register_from("flux-tools flow-render pack", Arc::new(FlowRenderTool))
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_render`].
pub fn register_render(registry: &mut ToolRegistry) {
    try_register_render(registry).expect("flux-tools flow-render pack registration failed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::{System, Workspace};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn source_view_highlights_with_the_one_dark_theme() {
        let svg = render_flux_svg(
            "flow greet(name: String)\n  do notify \"hi\"  # say hi\n",
            View::Source,
        )
        .unwrap();
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(
            svg.contains("<tspan fill=\"#c678dd\">flow</tspan>"),
            "keyword span: {svg}"
        );
        assert!(
            svg.contains("<tspan fill=\"#98c379\">\"hi\"</tspan>"),
            "string span: {svg}"
        );
        assert!(
            svg.contains("<tspan fill=\"#7f848e\"># say hi</tspan>"),
            "comment span: {svg}"
        );
    }

    #[test]
    fn tree_view_colors_connectors() {
        let svg = render_flux_svg("flow f\n  $x = 1\n  return $x\n", View::Tree).unwrap();
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(
            svg.contains("<tspan fill=\"#5c6370\">├─ </tspan>"),
            "mid connector: {svg}"
        );
        assert!(
            svg.contains("<tspan fill=\"#5c6370\">└─ </tspan>"),
            "last connector: {svg}"
        );
    }

    #[test]
    fn canvas_scales_with_longest_line_and_line_count() {
        // "flow f" = 6 chars → width ceil(22·2 + 6·9.4) = 101; one line → height 44 + 22 + 18.
        let svg = render_flux_svg("flow f\n", View::Source).unwrap();
        assert!(svg.contains("width=\"101\" height=\"84\""), "got: {svg}");
        // 18 chars → ceil(44 + 169.2) = 214; two lines → 44 + 2·22 + 18 = 106.
        let bigger = render_flux_svg("flow f_much_longer\n  $x = 1\n", View::Source).unwrap();
        assert!(
            bigger.contains("width=\"214\" height=\"106\""),
            "got: {bigger}"
        );
    }

    #[test]
    fn multi_line_string_colors_every_line() {
        let svg = render_flux_svg(
            "flow f\n  $x = \"\"\"line one\nline two\"\"\"\n  return $x\n",
            View::Source,
        )
        .unwrap();
        assert!(
            svg.contains("<tspan fill=\"#98c379\">\"\"\"line one</tspan>"),
            "first line: {svg}"
        );
        assert!(
            svg.contains("<tspan fill=\"#98c379\">line two\"\"\"</tspan>"),
            "second line: {svg}"
        );
    }

    #[test]
    fn color_grid_indexes_by_char_not_bytes() {
        // The string value is multi-byte; a byte-indexed grid would smear its colour into the
        // trailing comment (or panic slicing mid-char).
        let svg = render_flux_svg(
            "flow f\n  $x = \"héllo—ü\"  # ok\n  return $x\n",
            View::Source,
        )
        .unwrap();
        assert!(
            svg.contains("<tspan fill=\"#98c379\">\"héllo—ü\"</tspan>"),
            "string: {svg}"
        );
        assert!(
            svg.contains("<tspan fill=\"#7f848e\"># ok</tspan>"),
            "comment: {svg}"
        );
    }

    #[test]
    fn malformed_source_still_renders_in_source_view() {
        let svg = render_flux_svg(
            "flow f\n  $a =\n  do read(\n  € oops\n  $b = 2",
            View::Source,
        )
        .unwrap();
        assert!(svg.starts_with("<svg"), "got: {svg}");
    }

    #[test]
    fn tree_view_errors_on_malformed_source() {
        assert!(render_flux_svg("nonsense ???", View::Tree).is_err());
    }

    #[test]
    fn tree_view_renders_program_flows_and_op_heads() {
        let src = "op double(n: Number) -> Number\n  $r = fmt(\"{n}{n}\")\n  return $r\n\n\
                   flow main\n  $d = double(3)\n  return $d\n";
        let svg = render_flux_svg(src, View::Tree).unwrap();
        assert!(
            svg.contains("<tspan fill=\"#c678dd\">flow</tspan>"),
            "flow tree: {svg}"
        );
        assert!(
            svg.contains("<tspan fill=\"#c678dd\">op</tspan>"),
            "op header: {svg}"
        );
        assert!(svg.contains("return $r"), "op statement head: {svg}");
    }

    fn ctx() -> ToolContext {
        // Counter-suffixed (not just PID): this is called from more than one test in this module,
        // and a shared dir let two tests race a concurrent write/read of the same fixture file.
        let n = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-render-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join(".flux/flows")).unwrap();
        std::fs::write(
            dir.join(".flux/flows/greet.flux"),
            "flow greet(name: String)\n  do notify \"hi\"\n  return $name\n",
        )
        .unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    #[tokio::test]
    async fn flow_render_resolves_a_stored_flow_by_name() {
        let c = ctx();
        let r = FlowRenderTool
            .execute(&c, json!({"name": "greet"}))
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(
            r.content.starts_with("<svg"),
            "canonical = SVG markup: {}",
            r.content
        );
        let view = r.view.expect("compact view summary");
        assert!(
            view.starts_with("rendered greet (source view) → SVG "),
            "got: {view}"
        );
        assert!(view.ends_with("3 lines"), "got: {view}");
    }

    #[tokio::test]
    async fn flow_render_requires_exactly_one_input() {
        let c = ctx();
        assert!(FlowRenderTool.execute(&c, json!({})).await.is_err());
        assert!(FlowRenderTool
            .execute(&c, json!({"name": "greet", "source": "flow f\n"}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn flow_render_tree_view_of_inline_source() {
        let c = ctx();
        let r = FlowRenderTool
            .execute(
                &c,
                json!({"source": "flow f\n  $x = 1\n  return $x\n", "view": "tree"}),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("#5c6370"), "connectors: {}", r.content);
        assert!(r.view.unwrap().contains("(tree view)"));
    }

    #[tokio::test]
    async fn flow_render_tree_view_errors_on_unparseable_source() {
        let c = ctx();
        let r = FlowRenderTool
            .execute(&c, json!({"source": "nonsense ???", "view": "tree"}))
            .await
            .unwrap();
        assert!(r.is_error, "tree view of junk must be an error result");
        assert!(r.content.contains("tree view needs parseable source"));
    }
}
