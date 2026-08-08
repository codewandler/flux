use super::*;

/// Whether tool output is shown in full (set by `-v`/`--verbose`, which exports `FLUX_VERBOSE`).
pub(super) fn verbose() -> bool {
    flux_system::env_truthy("FLUX_VERBOSE")
}

pub(super) fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// A preview of a tool result for the CLI: continuation lines indented under the header, with a
/// trailing note when lines were elided. `full` (from `-v`/`FLUX_VERBOSE`) disables the caps and shows
/// everything. This affects only what the user sees — the model always receives the full result.
pub(super) fn tool_preview(s: &str, full: bool) -> String {
    // C-539: the caps are declared beside the TUI's in `toolview::budget` — the surfaces budget
    // differently on purpose (no expand affordance here), but never drift silently.
    const MAX_LINES: usize = flux_tui::toolview::budget::CLI_PREVIEW_LINES;
    const MAX_LINE_CHARS: usize = flux_tui::toolview::budget::CLI_PREVIEW_LINE_CHARS;
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= 1 {
        return if full {
            s.to_string()
        } else {
            truncate(s, MAX_LINE_CHARS)
        };
    }
    let shown = if full {
        lines.len()
    } else {
        lines.len().min(MAX_LINES)
    };
    let mut out = String::new();
    for (i, line) in lines.iter().take(shown).enumerate() {
        if i > 0 {
            out.push_str("\n  ");
        }
        let line = line.trim_end();
        out.push_str(&if full {
            line.to_string()
        } else {
            truncate(line, MAX_LINE_CHARS)
        });
    }
    let extra = lines.len() - shown;
    if extra > 0 {
        out.push_str(&format!(
            "\n  … (+{extra} more line{}; -v for full)",
            if extra == 1 { "" } else { "s" }
        ));
    }
    out
}

/// Below this many columns of room the animated bar falls back to the braille glyph.
const MIN_BAR_WIDTH: usize = 8;
/// The animated bar never grows past this, keeping the label adjacent and readable.
const MAX_BAR_WIDTH: usize = 24;

/// Columns available for the animated spinner bar: terminal width minus the label,
/// a fixed elapsed reserve (so `59s → 1m 0s` doesn't jitter the bar), and separators.
fn spinner_bar_width(term_cols: u16, label_width: usize) -> usize {
    const ELAPSED_RESERVE: usize = 8;
    (term_cols as usize)
        .saturating_sub(label_width + ELAPSED_RESERVE + 3)
        .min(MAX_BAR_WIDTH)
}

/// Visible columns of a label that may carry SGR escapes (e.g. a dimmed phase label).
fn visible_width(s: &str) -> usize {
    let mut in_escape = false;
    s.chars()
        .filter(|&c| {
            if in_escape {
                if c == 'm' {
                    in_escape = false;
                }
                false
            } else if c == '\x1b' {
                in_escape = true;
                false
            } else {
                true
            }
        })
        .count()
}

/// Shared between [`CliSink`] and its spinner ticker task.
pub(super) struct SpinnerState {
    pub(super) active: bool,
    pub(super) label: String,
    pub(super) frame: usize,
}

/// Coordinates exclusive ownership of the current stderr line between the animated spinner ticker
/// and interactive prompts (approvals, confirms). While a prompt holds the gate the ticker must not
/// repaint and `stop_spinner` must not clear — the prompt owns the line; without this, the 80 ms
/// `\r\x1b[K` repaint (or a `planning(false)` drained mid-approval) erases the prompt within one
/// tick, leaving a spinner that looks hung while `y` still answers. Process-global because stderr
/// is process-global: the approver sits behind `Arc<dyn Approver>` and sinks are built per turn,
/// so no shared construction scope exists to thread an instance through.
pub(super) struct PromptGate {
    state: std::sync::Mutex<GateState>,
    serial: Arc<tokio::sync::Mutex<()>>,
}

struct GateState {
    /// Prompt-hold depth. The async serial lock makes this 0/1; a count keeps painter bookkeeping
    /// defensive if a future prompt acquires through a nested helper.
    holders: usize,
    /// Live spinner tickers (0 or 1 in practice) — lets `acquire` know whether there is a painted
    /// line to clear, so piped stderr stays free of control bytes.
    painters: usize,
}

/// Held by the ticker for the duration of one frame draw. Holding it keeps the gate locked, so
/// `acquire` cannot interleave a clear mid-paint — once `acquire` returns, no paint lands after it.
pub(super) struct PaintPermit<'a>(#[allow(dead_code)] std::sync::MutexGuard<'a, GateState>);

/// Releases the prompt's hold on drop — including when the turn future is cancelled mid-approval.
pub(super) struct PromptGuard {
    gate: Arc<PromptGate>,
    _serial: tokio::sync::OwnedMutexGuard<()>,
}

impl PromptGate {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(PromptGate {
            state: std::sync::Mutex::new(GateState {
                holders: 0,
                painters: 0,
            }),
            serial: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// The process-wide instance shared by every sink and prompt. Tests use [`PromptGate::new`]
    /// for isolated instances.
    pub(super) fn global() -> Arc<Self> {
        static GATE: std::sync::OnceLock<Arc<PromptGate>> = std::sync::OnceLock::new();
        GATE.get_or_init(PromptGate::new).clone()
    }

    /// Ticker entry point: paint only while the returned permit is alive. `None` while a prompt
    /// holds the gate — skip the frame (the ticker idles and resumes on release).
    pub(super) fn begin_paint(&self) -> Option<PaintPermit<'_>> {
        let st = self.state.lock().unwrap();
        (st.holders == 0).then_some(PaintPermit(st))
    }

    /// Bookkeeping from `start_spinner`.
    pub(super) fn painter_started(&self) {
        self.state.lock().unwrap().painters += 1;
    }

    /// Bookkeeping from `stop_spinner`. Returns whether the caller should clear the line — false
    /// while a prompt holds the gate (the acquire already cleared it; wiping now would erase the
    /// prompt).
    pub(super) fn painter_stopped(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        st.painters = st.painters.saturating_sub(1);
        st.holders == 0
    }

    /// Take the one interactive-input slot and stderr line. Approval and typed-question readers
    /// share this lock, so two concurrent operations cannot compete for stdin. Once acquired, clear
    /// a live spinner line and block repainting/clearing until the guard drops.
    pub(super) async fn acquire(self: &Arc<Self>) -> PromptGuard {
        let serial = self.serial.clone().lock_owned().await;
        let mut st = self.state.lock().unwrap();
        st.holders += 1;
        if st.painters > 0 {
            eprint!("\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
        drop(st);
        PromptGuard {
            gate: Arc::clone(self),
            _serial: serial,
        }
    }
}

impl Drop for PromptGuard {
    fn drop(&mut self) {
        let mut st = self.gate.state.lock().unwrap();
        st.holders = st.holders.saturating_sub(1);
    }
}

/// Render an op call as a concise, colored *semantic* label: the cyan op name padded to a gutter, then
/// a readable argument — `bash → $ cargo test`, `read → foo.rs:100-180`, `grep → "needle" in src/`. The
/// arg is capped unless `-v`; the full plan is always shown separately (the `flow.plan` tree).
/// Render the session's evidence log for `/evidence`: a one-line summary plus one line per
/// observation (phase, kind, compact data), flagging `tool_error` rows. Returns the empty-state
/// message when nothing has been recorded yet. Reads the same shared log the `observe`/`evidence`/
/// grading ops write.
pub(super) fn format_evidence(log: &flux_evidence::EvidenceLog) -> String {
    let obs = log.all();
    if obs.is_empty() {
        return "no evidence recorded yet — run a turn first".to_string();
    }
    let errors = obs.iter().filter(|o| o.kind == "tool_error").count();
    let iters = obs.iter().filter(|o| o.kind == "turn.iteration").count();
    let mut out = format!(
        "evidence: {} observation{}, {iters} iteration{}, {errors} error{}",
        obs.len(),
        if obs.len() == 1 { "" } else { "s" },
        if iters == 1 { "" } else { "s" },
        if errors == 1 { "" } else { "s" },
    );
    for o in obs {
        // Pad before coloring — `{:<N}` counts ANSI bytes, so styling a padded column would break
        // alignment.
        let phase = format!("{:<9}", format!("{:?}", o.phase).to_lowercase());
        let mark = if o.kind == "tool_error" {
            style::red("!")
        } else {
            " ".to_string()
        };
        let data = if o.data.is_null() {
            String::new()
        } else {
            truncate(&o.data.to_string(), 100)
        };
        out.push_str(&format!(
            "\n  {mark} {} {:<16} {}",
            style::dim(&phase),
            o.kind,
            style::dim(&data)
        ));
    }
    out
}

/// A compact, readable label for an authored-loop operation shown when
/// `--show-loop` reveals the loop. Returns `None` for ordinary ops (which fall through to the normal
/// label path). These ops carry large inputs, so the label deliberately omits the payload.
pub(super) fn loop_machinery_label(name: &str, input: &Value) -> Option<String> {
    let (verb, note) = match name {
        "detect_intent" => ("intent", "classify the request"),
        "explore" => ("explore", "gather / propose actions"),
        "approve_batch" => ("approve", "freeze the action batch"),
        "execute_batch" => ("execute", "run approved actions"),
        "present_results" => ("present", "render the result"),
        "ai_segment" => ("AI segment", "bounded model stage"),
        "observe" => {
            let kind = input.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            return Some(format!("{}  {}", style::cyan("observe"), style::dim(kind)));
        }
        "evidence" => ("evidence", "read the audit trail"),
        "metrics" => ("metrics", "calls / errors / iterations"),
        "grade" => ("grade", "check a criterion"),
        _ => return None,
    };
    Some(format!("{}  {}", style::cyan(verb), style::dim(note)))
}

pub(super) fn render_call_label(name: &str, input: &Value, verbose: bool) -> String {
    // Column width: wide enough for the longest built-in op name (`web.fetch` = 9).
    const GUTTER: usize = 10;
    const ARG_CAP: usize = 120;
    // The loop machinery (revealed by `--show-loop`) may carry large typed state values.
    // Give those a compact, readable label so the stream reads as loop iterations, not a payload dump.
    if let Some(label) = loop_machinery_label(name, input) {
        return label;
    }
    let call = flux_tui::toolview::format_call(name, input);
    let verb = style::cyan(&call.verb);
    if call.arg.is_empty() {
        return verb;
    }
    let arg = if verbose {
        call.arg
    } else {
        truncate(&call.arg, ARG_CAP)
    };
    let pad = GUTTER.saturating_sub(call.verb.chars().count()).max(1);
    format!("{verb}{}{arg}", " ".repeat(pad))
}

/// A concise result summary for the execution stream: `done` for empty output, the line(s) for a
/// small result, or a tool-aware summary for larger results. `-v` shows everything.
///
/// For `grep` and `glob` results the first few matches are shown rather than a bare line count;
/// for `bash` the last non-empty line is used as a quick exit hint. Pass `tool` as `""` for the
/// generic (tool-unaware) path.
pub(super) fn result_summary_for(content: &str, tool: &str, verbose: bool) -> String {
    let content = content.trim();
    if content.is_empty() {
        return "done".to_string();
    }
    if verbose {
        return tool_preview(content, true);
    }
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();

    // Tool-aware previews. Head counts live in `toolview::budget` (C-539).
    const READ_HEAD: usize = flux_tui::toolview::budget::CLI_READ_HEAD_LINES;
    const GREP_HEAD: usize = flux_tui::toolview::budget::CLI_GREP_HEAD_LINES;
    const GLOB_HEAD: usize = flux_tui::toolview::budget::CLI_GLOB_HEAD_LINES;
    match tool {
        "read" | "read_many" => {
            // Never dump raw file contents — show a digest: the head lines + count.
            if n <= READ_HEAD {
                return lines
                    .iter()
                    .map(|l| truncate(l.trim_end(), 120))
                    .collect::<Vec<_>>()
                    .join("\n    ");
            }
            let head = lines[..READ_HEAD]
                .iter()
                .map(|l| truncate(l.trim_end(), 120))
                .collect::<Vec<_>>()
                .join("\n    ");
            return format!("{head}\n    … ({} more lines; -v for full)", n - READ_HEAD);
        }
        "grep" if n > GREP_HEAD => {
            let head = lines[..GREP_HEAD]
                .iter()
                .map(|l| truncate(l.trim_end(), 120))
                .collect::<Vec<_>>()
                .join("\n    ");
            return format!(
                "{head}\n    … (+{} more match{}; -v for full)",
                n - GREP_HEAD,
                if n - GREP_HEAD == 1 { "" } else { "es" }
            );
        }
        "glob" if n > GLOB_HEAD => {
            let head = lines[..GLOB_HEAD]
                .iter()
                .map(|l| truncate(l.trim_end(), 120))
                .collect::<Vec<_>>()
                .join("\n    ");
            return format!("{head}\n    … (+{} more; -v for full)", n - GLOB_HEAD);
        }
        "bash" if n > 1 => {
            // Show the last non-empty line as a quick exit hint.
            let last = lines
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(&lines[n - 1]);
            let last = truncate(last.trim_end(), 160);
            return format!("{n} lines · last: {last}  (-v for full)");
        }
        _ => {}
    }

    match n {
        0 => "done".to_string(),
        1 => truncate(content, 200),
        _ if n <= 6 => lines
            .iter()
            .map(|l| truncate(l.trim_end(), 200))
            .collect::<Vec<_>>()
            .join("\n    "),
        _ => format!("{n} lines · -v for full"),
    }
}

/// Color a risk summary by its leading level (`low` green, `medium` yellow, else red).
pub(super) fn risk_badge(summary: &str) -> String {
    match summary.split([' ', '·']).next().unwrap_or("").trim() {
        "low" | "no-op" => style::green(summary),
        "medium" => style::yellow(summary),
        _ => style::red(summary),
    }
}

pub(super) fn format_operation_timing(timing: flux_core::OperationTiming) -> String {
    let fmt = |micros| style::fmt_elapsed(std::time::Duration::from_micros(micros));
    match (timing.execution_us, timing.approval_wait_us) {
        (Some(execution), Some(approval)) => {
            format!("exec {} + approval {}", fmt(execution), fmt(approval))
        }
        (Some(execution), None) => format!("exec {}", fmt(execution)),
        (None, Some(approval)) => format!("approval {}", fmt(approval)),
        (None, None) => format!("dispatch {}", fmt(timing.total_us)),
    }
}

pub(super) fn format_model_call(o: &flux_evidence::Observation) -> String {
    let stage = o
        .data
        .get("stage")
        .and_then(Value::as_str)
        .unwrap_or("model");
    let round = o.data.get("round").and_then(Value::as_u64).unwrap_or(0);
    let duration = o
        .data
        .get("duration_us")
        .and_then(Value::as_u64)
        .map(std::time::Duration::from_micros)
        .map(style::fmt_elapsed)
        .unwrap_or_else(|| "?".into());
    let ttft = o
        .data
        .get("ttft_us")
        .and_then(Value::as_u64)
        .map(std::time::Duration::from_micros)
        .map(style::fmt_elapsed)
        .unwrap_or_else(|| "n/a".into());
    let operations = o
        .data
        .get("operations")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let schema_bytes = o
        .data
        .get("schema_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let schema = if schema_bytes >= 1024 {
        format!("{:.1} KiB", schema_bytes as f64 / 1024.0)
    } else {
        format!("{schema_bytes} B")
    };
    format!(
        "◇ model {stage} #{round} · {duration} · ttft {ttft} · {operations} op{} · {schema} schema",
        if operations == 1 { "" } else { "s" }
    )
}

/// One stderr line describing every worker a delegated run has live (C-246), or `None` when nothing
/// is delegated. Each segment names the worker by its A-79 spawn id — the only key that separates
/// two concurrent children of the same role — its status from the projection's closed label set, the
/// operation it is in, and **how long it has been quiet**. That last field is the point: a working
/// worker's idle age stays small while a hung one's grows, so the two are told apart on the surface
/// without any per-op log to read.
///
/// Nothing here reads a worker's tool input or observation data; the projection never exposes them.
pub(super) fn fleet_status_line(
    rows: &[flux_tui::fleet::WorkerRow],
    width: usize,
) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    // Once a worker is live again, the finished rows have already had their own line and would only
    // crowd the live ones out of the width — and the live ones carry the idle age that matters.
    // With nothing live, the finished rows ARE the line: that is how the wave's outcome is shown.
    let live: Vec<&flux_tui::fleet::WorkerRow> = rows
        .iter()
        .filter(|r| !matches!(r.status, flux_tui::fleet::WorkerStatus::Finished { .. }))
        .collect();
    let shown: Vec<&flux_tui::fleet::WorkerRow> = if live.is_empty() {
        rows.iter().collect()
    } else {
        live.clone()
    };
    let live = live.len();
    let segments = shown
        .iter()
        .map(|row| {
            let mut segment = format!("{}#{} {}", row.role, row.spawn_id, row.status.label());
            if let Some(op) = row.status.op() {
                segment.push(' ');
                segment.push_str(op);
            }
            if row.errors > 0 {
                segment.push_str(&format!(" ({} err)", row.errors));
            }
            segment.push_str(&format!(" · idle {}", style::fmt_elapsed(row.idle)));
            if row.stalled {
                segment.push_str(" ⚠ stalled");
            }
            segment
        })
        .collect::<Vec<_>>()
        .join(" | ");
    Some(truncate(
        &format!("⚇ fleet · {live} live · {segments}"),
        width.max(40),
    ))
}

/// What a fleet line *says*, with the ages excluded. A burst of timing/observation events refreshes
/// a worker's liveness without changing this, so the same sentence is not reprinted for every one.
pub(super) fn fleet_signature(rows: &[flux_tui::fleet::WorkerRow]) -> String {
    rows.iter()
        .map(|row| {
            format!(
                "{}:{}:{}:{}",
                row.spawn_id,
                row.status.label(),
                row.status.op().unwrap_or(""),
                row.errors
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Renders streaming assistant text to stdout as live-rendered Markdown, and tool activity to stderr,
/// in the "Refined" style: a syntax-highlighted plan, colored `→`/`✓`/`✗` markers, a live spinner while
/// each op runs, and a completion rule with timing. All color is tty/`NO_COLOR`/`--color`-aware.
pub(super) struct CliSink {
    pub(super) live: flux_markdown::render::LiveRenderer,
    /// Show tool output in full (no truncation) — from `-v`/`FLUX_VERBOSE`.
    pub(super) verbose: bool,
    pub(super) width: usize,
    pub(super) stderr_tty: bool,
    pub(super) steps: usize,
    pub(super) turn_start: Option<std::time::Instant>,
    /// The current op's `(label, start)`, set on `tool_call` and finalized on `tool_result`.
    pub(super) pending: Option<(String, std::time::Instant)>,
    /// Dispatcher-attributed phases for the pending op, delivered immediately before its result.
    pub(super) pending_timing: Option<flux_core::OperationTiming>,
    pub(super) spinner: Option<(
        Arc<std::sync::Mutex<SpinnerState>>,
        tokio::task::JoinHandle<()>,
    )>,
    /// Per-call prompt-cache accounting for the turn in progress (C-139). Folded from the engine's
    /// `model.call` observations, because the turn-end `Usage` carries `Usage::accumulate`'s
    /// occupancy snapshot — the last round only — and a hit rate read off that reports the turn's
    /// worst round. Reset at each turn boundary.
    pub(super) turn_cache: flux_core::CacheEfficiency,
    /// Iteration counter: how many tool round-trips have completed this turn.
    pub(super) iter: usize,
    /// Max iterations cap (threaded from `Agent::max_iterations` for display).
    pub(super) max_iter: usize,
    /// The resolved model spec (e.g. `codex/gpt-5.5`) + pricing table for the per-turn cost
    /// annotation. `None` when the sink wasn't given a spec (sub-paths that don't show cost).
    pub(super) model_spec: Option<String>,
    pub(super) pricing: Option<flux_core::PricingTable>,
    /// The phase of the most recent `loop.phase` observation this turn. Current adaptive stages use
    /// `intent`/`explore`; historical sessions may still project `orient`/`gather`/`execute`.
    /// Drives the spinner label via `phase_spinner_label`.
    pub(super) phase: Option<String>,
    /// How many `execute`-phase `loop.phase` observations have landed this turn — the first is the
    /// turn's actual execution planning, every one after it means the prior round didn't finish
    /// (a revision), so the spinner reads "revising…" once this exceeds 1. A plain counter over
    /// observations already reaching the sink; no new flux-flow signal needed.
    pub(super) execute_rounds: usize,
    /// Whether the NEXT `flow.plan` observation is a bounded, read-only gather round rather than
    /// the full execution plan — set on a `gather`-phase `loop.phase` or a `flow.brief` (a brief
    /// only ever accompanies a `gather: true` plan), cleared on `orient`/`execute`. `flow.plan`
    /// itself carries no `gather` flag (that lives on `Compiled`/the host, not the observation), so
    /// this is the cheapest surface-side derivation available without new flux-flow plumbing.
    pub(super) gather_mode: bool,
    /// Stderr-line ownership coordinator shared with interactive prompts — see [`PromptGate`].
    pub(super) gate: Arc<PromptGate>,
    /// Model round-trips started this turn; cycles the truecolor thinking-bar effect
    /// (`flux_tui::spinners::by_round`) so long turns walk through the catalog.
    pub(super) spin_round: usize,
    /// C-246: the shared surface-side fold of A-79's correlated sub-agent activity stream. This
    /// sink is the surface a delegated run — a fleet coordinator (`flux flow run`) included —
    /// actually runs on, and it used to drop every `subagent.activity` observation, so a long wave
    /// of workers read as silence. Same projection the TUI pane will render (`flux_tui::fleet`);
    /// there is one activity path, not two.
    pub(super) fleet: flux_tui::fleet::FleetProjection,
    /// What the last printed fleet line *said*, ages excluded — see [`fleet_signature`].
    pub(super) fleet_sig: String,
    /// When the last fleet line was printed, for the age-refresh reprint.
    pub(super) fleet_printed: Option<std::time::Instant>,
}

/// How long a fleet line may stand before it is reprinted with refreshed ages, even though no
/// worker changed what it is doing. This is what makes a *hung* worker visible while its peers keep
/// working: its idle age keeps growing on the surface instead of freezing at the last status change.
const FLEET_REPRINT_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

impl CliSink {
    pub(super) fn new(max_iter: usize) -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.parse::<usize>().ok())
            .filter(|&w| w >= 20)
            .unwrap_or(80);
        CliSink {
            live: flux_markdown::render::LiveRenderer::new(
                flux_markdown::render::Theme::auto(),
                width,
                stdout_tty,
            ),
            verbose: verbose(),
            width,
            stderr_tty: std::io::stderr().is_terminal(),
            steps: 0,
            turn_start: None,
            pending: None,
            pending_timing: None,
            spinner: None,
            turn_cache: flux_core::CacheEfficiency::default(),
            iter: 0,
            max_iter,
            model_spec: None,
            pricing: None,
            phase: None,
            execute_rounds: 0,
            gather_mode: false,
            gate: PromptGate::global(),
            spin_round: 0,
            fleet: flux_tui::fleet::FleetProjection::new(),
            fleet_sig: String::new(),
            fleet_printed: None,
        }
    }

    /// Attach a model spec + pricing table so the per-turn annotation appends a dollar cost. The
    /// spec is the full `provider/model` (e.g. `codex/gpt-5.5`) so subscription spend is detected
    /// from the provider prefix; the table is the loaded overlay-on-builtin (`load_pricing_table`).
    pub(super) fn with_cost(
        mut self,
        model_spec: String,
        pricing: flux_core::PricingTable,
    ) -> Self {
        self.model_spec = Some(model_spec);
        self.pricing = Some(pricing);
        self
    }

    /// The per-turn dollar-cost suffix for the annotation, when a model spec + pricing table are
    /// attached and the turn reported usage — see [`cost_suffix`] for the full rendering rules
    /// (incl. the C-30 `$? (unpriced)` marker for un-tabled metered cloud models).
    pub(super) fn cost_inline(&self, usage: Option<&Usage>) -> String {
        cost_suffix(self.model_spec.as_deref(), self.pricing.as_ref(), usage)
    }

    /// Fold one sub-agent activity event into the fleet projection and print the resulting line
    /// (C-246). Printed when a worker changed what it is *doing*, or when the standing line is older
    /// than [`FLEET_REPRINT_AFTER`] — the second case is what keeps a hung worker's idle age moving
    /// on the surface while its peers work, rather than frozen at its last status change.
    pub(super) fn render_fleet(&mut self, activity: &flux_runtime::SpawnActivity) {
        let now = std::time::Instant::now();
        if !self.fleet.apply(activity, now) {
            return;
        }
        let rows = self.fleet.rows(now);
        let signature = fleet_signature(&rows);
        let stale = self
            .fleet_printed
            .is_none_or(|printed| now.duration_since(printed) >= FLEET_REPRINT_AFTER);
        if signature == self.fleet_sig && !stale {
            return;
        }
        if let Some(line) = fleet_status_line(&rows, self.width) {
            eprintln!("{}", style::dim(&line));
            self.fleet_sig = signature;
            self.fleet_printed = Some(now);
        }
    }

    /// Commit any in-progress assistant render so subsequent stderr lines appear below it.
    fn commit(&mut self) {
        if self.live.is_active() {
            let mut out = std::io::stdout().lock();
            let _ = self.live.finish(&mut out);
        }
    }

    fn use_spinner(&self) -> bool {
        self.stderr_tty && style::enabled()
    }

    /// Start an animated spinner on the op's line (a background ticker rewriting it via `\r`).
    /// With an `effect`, the leading glyph becomes a full-width truecolor animated bar
    /// (`flux_tui::spinners`); without one it stays the braille glyph.
    fn start_spinner(
        &mut self,
        label: String,
        effect: Option<&'static flux_tui::spinners::Spinner>,
    ) {
        let state = Arc::new(std::sync::Mutex::new(SpinnerState {
            active: true,
            label,
            frame: 0,
        }));
        let s = state.clone();
        let start = std::time::Instant::now();
        self.gate.painter_started();
        let gate = self.gate.clone();
        let period = std::time::Duration::from_millis(match effect {
            Some(_) => flux_tui::spinners::FPS_MS,
            None => 80,
        });
        let task = tokio::spawn(async move {
            const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            loop {
                {
                    // Hold the lock while drawing so `stop_spinner` can't interleave.
                    let mut st = s.lock().unwrap();
                    if !st.active {
                        break;
                    }
                    // Skip the frame while a prompt owns the stderr line (lock order is
                    // spinner-state → gate everywhere; `acquire` takes only the gate).
                    if let Some(_line) = gate.begin_paint() {
                        let tick = st.frame;
                        st.frame += 1;
                        // The bar is re-sized every frame so a resize mid-wait can't wrap
                        // the line (wrap would defeat the `\r\x1b[K` clear).
                        let cols = crossterm::terminal::size().map(|(c, _)| c).unwrap_or(80);
                        let bar_w = effect
                            .map(|_| spinner_bar_width(cols, visible_width(&st.label)))
                            .unwrap_or(0);
                        let lead = match effect {
                            Some(sp) if bar_w >= MIN_BAR_WIDTH => {
                                flux_tui::spinners::ansi_line(&(sp.frame)(tick, bar_w))
                            }
                            _ => style::cyan(&FRAMES[tick % FRAMES.len()].to_string()),
                        };
                        let elapsed = style::fmt_elapsed(start.elapsed());
                        eprint!("\r\x1b[K{} {}  {}", lead, st.label, style::dim(&elapsed));
                        let _ = std::io::stderr().flush();
                    }
                }
                tokio::time::sleep(period).await;
            }
        });
        self.spinner = Some((state, task));
    }

    /// Stop a running spinner and clear its line. Returns true if one was active. The clear is
    /// skipped while a prompt holds the [`PromptGate`] — this call may be a `planning(false)` or
    /// `turn_end` drained during an approval wait, and the line it would wipe is the prompt.
    fn stop_spinner(&mut self) -> bool {
        if let Some((state, task)) = self.spinner.take() {
            state.lock().unwrap().active = false;
            if self.gate.painter_stopped() {
                eprint!("\r\x1b[K");
                std::io::stderr().flush().ok();
            }
            task.abort();
            true
        } else {
            false
        }
    }
}

impl AgentSink for CliSink {
    fn text_delta(&mut self, t: &str) {
        let mut out = std::io::stdout().lock();
        let _ = self.live.push(t, &mut out);
    }
    fn thinking_delta(&mut self, t: &str) {
        // Stream extended-thinking tokens dimmed on stderr so reasoning is observable in the REPL.
        eprint!("{}", style::dim(t));
        std::io::stderr().flush().ok();
    }
    fn planning(&mut self, active: bool) {
        // Fill an otherwise-silent provider wait with a phase-aware spinner. The intent/exploration
        // observation replaces it once the typed model stage completes.
        if active {
            self.turn_start.get_or_insert_with(std::time::Instant::now);
            self.commit();
            let label = phase_spinner_label(self.phase.as_deref(), self.execute_rounds);
            if self.use_spinner() {
                // Truecolor terminals get the animated bar, cycling one effect per model
                // round-trip (flai-style); others keep the braille glyph.
                let effect =
                    style::truecolor().then(|| flux_tui::spinners::by_round(self.spin_round));
                self.spin_round += 1;
                self.start_spinner(style::dim(&label), effect);
            } else if matches!(self.phase.as_deref(), Some("intent" | "explore")) {
                // Redirected runs have no animated line to rewrite. Preserve one stable
                // phase marker per provider consultation so logs and CI output do not reproduce
                // the otherwise-silent wait that A-72 closes for interactive terminals.
                eprintln!("{}", style::dim(&label));
            }
        } else {
            self.stop_spinner();
        }
    }
    fn tool_call(&mut self, _dispatch: DispatchId, name: &str, input: &Value) {
        self.commit();
        self.steps += 1;
        self.iter += 1;
        if self.turn_start.is_none() {
            self.turn_start = Some(std::time::Instant::now());
        }
        let base_label = render_call_label(name, input, self.verbose);
        // Prefix with [N/max] iteration counter when a cap is known.
        let label = if self.max_iter > 0 {
            format!("[{}/{}] {base_label}", self.iter, self.max_iter)
        } else {
            base_label
        };
        if self.use_spinner() {
            // Tool lines keep the braille glyph — their labels are long and a bar would crowd them.
            self.start_spinner(label.clone(), None);
        } else {
            eprintln!("\n{} {label}", style::blue("→"));
        }
        self.pending = Some((label, std::time::Instant::now()));
        self.pending_timing = None;
    }
    fn tool_timing(
        &mut self,
        _dispatch: DispatchId,
        _name: &str,
        timing: &flux_core::OperationTiming,
    ) {
        self.pending_timing = Some(*timing);
    }
    fn tool_result(&mut self, _dispatch: DispatchId, name: &str, result: &ToolResult) {
        let (label, start) = self
            .pending
            .take()
            .unwrap_or_else(|| (String::new(), std::time::Instant::now()));
        // If a spinner ran, its line is cleared — reprint the call line so it stays in the scrollback.
        if self.stop_spinner() {
            eprintln!("\n{} {label}", style::blue("→"));
        }
        let elapsed = self
            .pending_timing
            .take()
            .map(format_operation_timing)
            .unwrap_or_else(|| style::fmt_elapsed(start.elapsed()));
        let elapsed = style::dim(&format!("· {elapsed}"));
        let body = flux_tui::toolview::format_result(name, &result.content, result.is_error)
            .unwrap_or_else(|| result_summary_for(&result.content, name, self.verbose));
        let mark = if result.is_error {
            style::red("✗")
        } else {
            style::green("✓")
        };
        eprintln!("  {mark} {body}  {elapsed}");
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.commit();
        // `action_batch.proposed` / `approval.requested` are deliberately unrendered here: sink
        // events are drained only when the turn future yields, which during an approval is AFTER
        // the prompt line is already open — printing them would garble it. The approval prompt
        // itself (`plan_prompt`, built from the same risk data) carries the batch content with
        // correct ordering.
        if o.kind == "model.call" {
            if let Some(usage) = o
                .data
                .get("usage")
                .and_then(|value| serde_json::from_value::<Usage>(value.clone()).ok())
            {
                self.turn_cache.add(&usage);
            }
            // A-39: the per-call trace line lives on this same observation. It must be printed
            // *here* — a later `else if o.kind == "model.call"` arm can never be reached once this
            // one matches, which silently emptied `--trace-loop` when the C-139 fold was added.
            if flux_flow::engine::show_loop() {
                eprintln!("{}", style::dim(&format_model_call(o)));
            }
        } else if let Some(activity) = flux_runtime::SpawnActivity::from_observation(o) {
            // C-246: A-79's correlated child-activity stream reached this sink and died here, so a
            // delegated run — and a whole fleet of workers — was invisible on the CLI surface. Fold
            // it into the shared projection and print the per-worker line. `input`/`observation`
            // payloads are default-denied by the projection itself, not by this call site.
            self.render_fleet(&activity);
        } else if o.kind == flux_evidence::KIND_DESTRUCTIVE {
            eprintln!(
                "{}",
                style::yellow("⚠ destructive operation — approval required")
            );
        } else if o.kind == "skill.activated" {
            if let Some(name) = o.data.get("skill").and_then(|v| v.as_str()) {
                eprintln!("{}", style::dim(&format!("✦ skill: {name}")));
            }
        } else if o.kind == "context.compacted" {
            let from = o
                .data
                .get("from_messages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let to = o
                .data
                .get("to_messages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            eprintln!(
                "{}",
                style::dim(&format!("⊙ context compacted ({from} → {to} messages)"))
            );
        } else if o.kind == "context.shrunk" {
            // A-63 / F-011: a context pack dropped members to fit its budget — surface it once so a
            // plain run shows the eviction (the model-facing transcript line alone never did).
            let dropped = o.data.get("dropped").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = o.data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
            eprintln!(
                "{}",
                style::dim(&format!("⊙ context: dropped {dropped} of {total} members"))
            );
        } else if o.kind == flux_evidence::KIND_BUDGET_PROJECTION {
            // C-542: the enforcing ledger publishes spent-versus-declared on every charge. This
            // surface is scrollback, not a live header (that is the TUI's budget segment), so it
            // prints the crossings — one visible warning per target, then the hard-limit stop —
            // instead of a line per model call. The figures are the ledger's own.
            if let Some((line, stop)) = budget_crossing_line(&o.data) {
                eprintln!(
                    "{}",
                    if stop {
                        style::red(&line)
                    } else {
                        style::yellow(&line)
                    }
                );
            }
        } else if o.kind == "turn.cancelled" {
            eprintln!("{}", style::dim("⊘ turn cancelled"));
        } else if o.kind == "loop.phase" {
            self.record_phase(o);
        } else if o.kind == flux_evidence::KIND_TURN_INTENT
            && o.data.get("intent").and_then(|v| v.as_str()).is_some()
        {
            self.render_intent(o);
        } else if o.kind == "loop.round" {
            // A-39 (`--trace-loop`/`FLUX_TRACE_LOOP`): one dim line per outer-loop round.
            let round = o.data.get("round").and_then(|v| v.as_u64()).unwrap_or(0);
            let max = o.data.get("max").and_then(|v| v.as_u64()).unwrap_or(0);
            eprintln!("{}", style::dim(&format!("⟳ round {round}/{max}")));
        } else if o.kind == "loop.node" {
            // A-39: one dim line per structural AST node the outer loop executes.
            eprintln!("{}", style::dim(&trace_node_line(&o.data)));
        } else if o.kind == "flow.brief" {
            // A brief only ever accompanies a `gather: true` plan (`compile.rs`'s `parse_brief`
            // call site) — its arrival marks gather mode even when the phase alone (`orient`) is
            // ambiguous between a gather round and a full plan emitted directly.
            self.gather_mode = true;
            self.render_brief(o);
        } else if o.kind == "flow.plan" {
            // A-17 (closes the A-15 residual): `flow.plan` now carries its own `gather` flag,
            // computed host-side from the plan's own `settled` signal — prefer it directly over the
            // surface's `loop.phase`/`flow.brief`-order inference, which couldn't tell an
            // orient-phase gather plan apart from orient emitting the full plan directly when the
            // model's `brief` was unusable. Falls back to the tracked state for a phase-less caller
            // that predates the field (e.g. a stale override still on the pre-A-17 wire shape).
            let gather = o
                .data
                .get("gather")
                .and_then(|v| v.as_bool())
                .unwrap_or(self.gather_mode);
            if gather {
                self.render_gather_compact(o);
            } else {
                self.render_plan(o);
            }
        } else if o.kind == "flow.halt" {
            self.render_halt(o);
        }
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.commit();
        self.stop_spinner();
        let elapsed = self
            .turn_start
            .map(|t| style::fmt_elapsed(t.elapsed()))
            .unwrap_or_default();
        // The right-hand token annotation: context-window occupancy, generated tokens, cache + hit-rate.
        // `ctx` comes from the turn usage (occupancy); the cache tiers come from the per-call fold,
        // so the hit rate is the turn's, not its last round's (C-139).
        // The per-call fold is the better source (C-139) — but only surfaces on paths that emit
        // `model.call` observations. `flux flow run`'s `ai_segment` doesn't, so fall back to the
        // turn snapshot rather than dropping the cache segment entirely on those surfaces.
        let token_inline = usage
            .as_ref()
            .map(|u| {
                if self.turn_cache.is_empty() {
                    usage_annotation(u)
                } else {
                    usage_annotation_with_cache(u, &self.turn_cache)
                }
            })
            .unwrap_or_default();
        // The dollar cost of this turn's tokens, when a model spec + pricing table were attached.
        let cost_inline = self.cost_inline(usage.as_ref());
        // Always print a rule so the turn boundary is visible even for prose-only replies.
        let summary = if self.steps > 0 {
            let plural = if self.steps == 1 { "" } else { "s" };
            format!(
                "{} step{plural} · {elapsed}{token_inline}{cost_inline}",
                self.steps
            )
        } else {
            // Prose-only turn: a minimal rule with elapsed + token stats.
            format!("· {elapsed}{token_inline}{cost_inline}")
        };
        let rule_len = self.width.saturating_sub(summary.chars().count() + 2);
        eprintln!("{} {}", style::rule(rule_len), style::dim(&summary));
        self.turn_cache = flux_core::CacheEfficiency::default();
    }
}

/// The compact token annotation appended to a turn-end rule (and the prose `/goal` footer): the
/// context-window occupancy (the final prompt size), the tokens generated, cache tiers (read AND
/// write — C-06 added the write side, which used to be silently dropped), and reasoning tokens when
/// the provider reported any. Cost itself is a separate suffix ([`cost_annotation`], appended by the
/// caller via `CliSink::cost_inline`) — this function is only the token breakdown. Empty when nothing
/// was billed (e.g. an offline `-m mock` turn).
pub(super) fn usage_annotation(u: &Usage) -> String {
    // No per-call fold available (a caller outside the turn loop, or a session that recorded none):
    // fall back to the turn snapshot, which is what this function always used.
    let mut fallback = flux_core::CacheEfficiency::default();
    fallback.add(u);
    usage_annotation_with_cache(u, &fallback)
}

/// [`usage_annotation`] with the cache tiers supplied separately.
///
/// C-139: `ctx` must stay the turn's context-window occupancy — `Usage::accumulate`'s replace
/// semantics are exactly right for that — but the cache tiers must be the per-call sum, or the
/// rendered hit rate is the turn's LAST round: the round with the longest message tail and so the
/// worst ratio of the turn. `cache` is folded from the engine's `model.call` observations and is the
/// same figure `flux usage` computes offline from the `CallUsage` event log.
pub(super) fn usage_annotation_with_cache(u: &Usage, cache: &flux_core::CacheEfficiency) -> String {
    let context = u.context_tokens();
    if context == 0 && u.output_tokens == 0 {
        return String::new();
    }
    let mut s = format!(
        " · ctx {} · out {}",
        style::fmt_tokens(context),
        style::fmt_tokens(u.output_tokens)
    );
    // One self-describing segment rather than two: `ctx` is the LAST round's occupancy while the
    // cache tiers are summed across the turn's calls, so rendering them as peers (`ctx 26.1k …
    // cache write 44.4k`) reads as a contradiction. The percentage is the turn's hit rate over its
    // own prompt total, and the glyphs match the TUI header so the two surfaces agree on sight.
    if cache.read > 0 || cache.write > 0 {
        s.push_str(&format!(
            " · cache {:.0}% ↺{} ✎{}",
            cache.hit_rate() * 100.0,
            style::fmt_tokens(cache.read),
            style::fmt_tokens(cache.write)
        ));
    }
    if u.reasoning_tokens > 0 {
        s.push_str(&format!(
            " · reasoning {}",
            style::fmt_tokens(u.reasoning_tokens)
        ));
    }
    s
}

/// The dollar-cost suffix for the turn-end annotation. Subscription spend (claude/codex) is shown
/// as an *equivalent metered cost* prefixed with `~` and tagged `(sub)` — it bills against a flat
/// subscription, not the API, so the figure is illustrative, not a charge. Metered spend shows the
/// raw dollar amount. Returns an empty string for a zero-cost turn (e.g. a cached/no-op call).
pub(super) fn cost_annotation(money: &flux_core::Money) -> String {
    if money.usd <= 0.0 {
        return String::new();
    }
    let usd = format!("${:.4}", money.usd);
    if money.subscription {
        format!(" · ~{usd} (sub)")
    } else {
        format!(" · {usd}")
    }
}

/// The complete turn-line cost suffix (shared by every sink): the dollar amount when the table
/// prices the spec; the C-30 ` · $? (unpriced)` marker when a **metered cloud** model has no
/// pricing row (real dollars are being spent invisibly — the marker says so and the once-per-run
/// note points at the `~/.flux/pricing.toml` override); empty when usage/spec/table are absent,
/// when the priced cost is zero, or for local/unknown specs (`ollama*`, `mock`, ad-hoc providers —
/// nothing is billed, and hermetic e2e output must stay byte-identical).
pub(super) fn cost_suffix(
    spec: Option<&str>,
    table: Option<&flux_core::PricingTable>,
    usage: Option<&Usage>,
) -> String {
    let (Some(u), Some(spec), Some(table)) = (usage, spec, table) else {
        return String::new();
    };
    match table.cost(u, spec) {
        Some(money) => cost_annotation(&money),
        None if unpriced_marker_applies(spec) => {
            note_unpriced_once(spec);
            " · $? (unpriced)".to_string()
        }
        None => String::new(),
    }
}

/// The `$?` marker fires only for known metered **cloud** providers — a table miss there hides
/// real spend. Local `ollama*` and unknown/mock providers stay silent. Thin delegate onto
/// `flux_core::is_metered_cloud_spec` (C-33) — the TUI's header uses the same predicate, so the
/// rule has one definition.
pub(super) fn unpriced_marker_applies(spec: &str) -> bool {
    flux_core::is_metered_cloud_spec(spec)
}

/// One-time (per process) plain-stderr hint explaining the `$?` marker and how to price the model.
pub(super) fn note_unpriced_once(spec: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "note: no pricing entry for `{spec}` — add one to ~/.flux/pricing.toml to see $ costs"
        );
    });
}

impl CliSink {
    /// Render a `flow.plan` observation: the syntax-highlighted plan tree + a risk badge header. A
    /// resumed/halted plan (`resumed: true`, A-17) carries per-statement ✓/✗/· status markers in its
    /// `plan` text instead of full syntax highlighting — patch-and-continue's granularity is
    /// top-level statements only — so that text is rendered (marker-colored) directly rather than
    /// reconstructing a fresh, unmarked tree from `plan_ast`.
    fn render_plan(&self, o: &flux_evidence::Observation) {
        let resumed = o
            .data
            .get("resumed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let rendered = if resumed {
            o.data
                .get("plan")
                .and_then(|v| v.as_str())
                .map(style_marked_plan)
        } else {
            o.data
                .get("plan_ast")
                .and_then(|v| serde_json::from_value::<flux_flow::ast::DraftAst>(v.clone()).ok())
                .map(|ast| flux_flow::render::render_styled(&ast, &style::plan_palette()))
                .or_else(|| {
                    o.data
                        .get("plan")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
        };
        let Some(rendered) = rendered else { return };
        let risk = o.data.get("risk").and_then(|v| v.as_str()).unwrap_or("");
        let ops = o.data.get("ops").and_then(|v| v.as_u64()).unwrap_or(0);
        eprintln!(
            "\n{}  {}{}",
            style::bold("plan"),
            risk_badge(risk),
            style::dim(&format!(" · {ops} op(s)"))
        );
        eprintln!("{rendered}");
    }

    /// Render a `flow.halt` observation: a red one-liner marking exactly where guarded execution
    /// halted before the execution report returns to the native stage for correction.
    fn render_halt(&self, o: &flux_evidence::Observation) {
        eprintln!("{}", style::red(&halt_line(&o.data)));
    }

    /// Track a `loop.phase` observation so the spinner names the current typed stage. Historical
    /// gather/execute values remain readable when old sessions are projected.
    fn record_phase(&mut self, o: &flux_evidence::Observation) {
        let phase = o
            .data
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match phase.as_str() {
            "execute" => {
                self.execute_rounds += 1;
                self.gather_mode = false;
            }
            "gather" => self.gather_mode = true,
            "orient" | "intent" | "explore" => self.gather_mode = false,
            _ => {}
        }
        self.phase = Some(phase);
    }

    /// Render A-72's accepted staged intent from the already-durable `turn.intent` observation.
    /// Keyword-derived turn signals use the same observation kind but carry only `signal`, so the
    /// caller filters those out. Normal output stays compact; `-v` adds the exact selected ops.
    fn render_intent(&self, o: &flux_evidence::Observation) {
        for (index, line) in intent_lines(&o.data, self.verbose, self.width)
            .into_iter()
            .enumerate()
        {
            if index == 0 {
                eprintln!("{}", style::cyan(&line));
            } else {
                eprintln!("{}", style::dim(&line));
            }
        }
    }

    /// Render a `flow.brief` observation the moment the grounding artifact is accepted (design
    /// Part 1's "feedback within seconds"): `◆ goal: …` plus a dim `needs: …` line when present.
    fn render_brief(&self, o: &flux_evidence::Observation) {
        let mut lines = brief_lines(&o.data).into_iter();
        if let Some(goal_line) = lines.next() {
            eprintln!("{}", style::cyan(&goal_line));
        }
        for line in lines {
            eprintln!("{}", style::dim(&line));
        }
    }

    /// Render a gather-plan `flow.plan` observation as a compact one-liner (op names, not the full
    /// tree + risk badge a full execution plan gets — those are for the small, read-only,
    /// approval-free collect rounds design Part 1 bounds to ~12 call nodes).
    fn render_gather_compact(&self, o: &flux_evidence::Observation) {
        eprintln!("{}", style::dim(&gather_compact_line(&o.data)));
    }
}

/// The planning spinner's label (A-15): phase-derived so it reads "orienting…"/"gathering…" for
/// the collect passes and "planning…" for the execute pass's first round. "revising…" only once
/// the execute phase has already produced a round THIS turn — a plain counter over the
/// `loop.phase` observations already reaching the sink, not a new flux-flow signal. The halt-aware
/// "✗ step N/M — revising…" line is a separate, real-time render (`render_halt`/`halt_line`, A-17)
/// fired the moment an execution flow halts, distinct from this spinner label. A phase-less caller
/// falls back to "working…".
pub(super) fn phase_spinner_label(phase: Option<&str>, execute_rounds: usize) -> String {
    match phase {
        Some("intent") => "routing intent…".to_string(),
        Some("explore") => "exploring…".to_string(),
        Some("orient") => "orienting…".to_string(),
        Some("gather") => "gathering…".to_string(),
        Some("execute") => {
            if execute_rounds > 1 {
                "revising…".to_string()
            } else {
                "planning…".to_string()
            }
        }
        _ => "working…".to_string(),
    }
}

/// Format the accepted staged intent as bounded, stable plain lines. The intent is model-authored,
/// so whitespace is collapsed before display; families and operation names are host-validated.
pub(super) fn intent_lines(data: &Value, verbose: bool, width: usize) -> Vec<String> {
    let raw_intent = data
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sanitized: String = raw_intent
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let intent = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    let intent_cap = width.saturating_sub(12).clamp(24, 160);
    let families = data
        .get("families")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let operations = data
        .get("operations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let capabilities = if families.is_empty() {
        "none".to_string()
    } else {
        families.join(", ")
    };
    let plural = if operations.len() == 1 {
        "operation"
    } else {
        "operations"
    };
    let mut lines = vec![
        format!("◆ intent: {}", truncate(&intent, intent_cap)),
        format!(
            "  capabilities: {capabilities} · {} {plural}",
            operations.len()
        ),
    ];
    if verbose && !operations.is_empty() {
        lines.push(format!("  operations: {}", operations.join(", ")));
    }
    lines
}

/// Format a `flow.brief` observation's `data` as plain lines (no color, so it's directly testable):
/// `◆ goal: …` then, when present, a `needs: …` list line.
pub(super) fn brief_lines(data: &Value) -> Vec<String> {
    let goal = data.get("goal").and_then(|v| v.as_str()).unwrap_or("");
    let mut lines = vec![format!("◆ goal: {goal}")];
    if let Some(needs) = data.get("needs").and_then(|v| v.as_array()) {
        let items: Vec<&str> = needs.iter().filter_map(|v| v.as_str()).collect();
        if !items.is_empty() {
            lines.push(format!("  needs: {}", items.join(", ")));
        }
    }
    lines
}

/// Format a `flow.halt` observation's `data` (A-17) as a plain line: `✗ step N/M <op> failed —
/// revising…` — or, when the op isn't directly derivable from the failing statement (a composite/
/// control-flow node), `✗ step N/M failed — revising…`. Emitted once per mid-plan halt, right where
/// the action execution report is built — a real-time cue distinct from the per-tool ✓/✗ markers.
pub(super) fn halt_line(data: &Value) -> String {
    let step = data.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let of = data.get("of").and_then(|v| v.as_u64()).unwrap_or(0);
    match data.get("op").and_then(|v| v.as_str()) {
        Some(op) => format!("✗ step {step}/{of} {op} failed — revising…"),
        None => format!("✗ step {step}/{of} failed — revising…"),
    }
}

/// Format a `loop.node` observation's `data` (A-39, `--trace-loop`/`FLUX_TRACE_LOOP`) as a plain,
/// colorless line — one per structural AST node the outer agent loop executes. Falls back to the
/// raw JSON for any `node` kind this hasn't been taught (defensive: the interpreter's trace helper
/// is meant to grow new emission sites without this formatter going stale/panicking).
pub(super) fn trace_node_line(data: &Value) -> String {
    let label = |key: &str| data.get(key).and_then(|v| v.as_str());
    match data.get("node").and_then(|v| v.as_str()) {
        Some("call") => {
            let op = label("op").unwrap_or("?");
            match label("bind") {
                Some(bind) => format!("· {op} → ${bind}"),
                None => format!("· {op}"),
            }
        }
        Some("when") => {
            let branch = label("branch").unwrap_or("?");
            match label("cond") {
                Some(cond) => format!("· when {cond} → {branch}"),
                None => format!("· when → {branch}"),
            }
        }
        Some("unless") => {
            let entered = data
                .get("entered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let word = if entered { "enter" } else { "skip" };
            match label("cond") {
                Some(cond) => format!("· unless {cond} → {word}"),
                None => format!("· unless → {word}"),
            }
        }
        Some("match") => {
            let value = label("value").unwrap_or("");
            let arm = label("arm").unwrap_or("?");
            match label("subject") {
                Some(subject) => format!("· match {subject} = {value} → {arm}"),
                None => format!("· match {value} → {arm}"),
            }
        }
        Some("return") => match label("value") {
            Some(v) => format!("· return {v}"),
            None => "· return".to_string(),
        },
        Some("repeat") => {
            let rounds = data.get("rounds").and_then(|v| v.as_u64()).unwrap_or(0);
            let max = data.get("max").and_then(|v| v.as_u64()).unwrap_or(0);
            format!("· until hit — exit after {rounds}/{max}")
        }
        Some("parallel.branch") => {
            let name = label("name").unwrap_or("?");
            format!("· parallel branch ${name}")
        }
        _ => format!("· {data}"),
    }
}

/// Color each line of a marker-prefixed plan render (A-17): `✓` done lines green, `✗` the failed
/// statement red, `·` not-yet-run lines dim — the per-statement status text a resumed/halted plan's
/// `flow.plan` observation carries (`render_marked_plan` in `flux-flow`) instead of a fresh full
/// tree. Any line that doesn't start with one of those three markers passes through unstyled.
pub(super) fn style_marked_plan(text: &str) -> String {
    text.lines()
        .map(|line| match line.chars().next() {
            Some('✓') => style::green(line),
            Some('✗') => style::red(line),
            Some('·') => style::dim(line),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format a gather-plan `flow.plan` observation's `data` as a compact one-liner: `gathering ·
/// <op> <arg> · <op> <arg> …`, pulling call nodes off `plan_ast` and reusing the same
/// `format_call` the tool-call stream uses (so `read Cargo.toml`/`grep "needle"` etc. read
/// identically to a real op line). Falls back to a bare op count when the AST can't be walked.
pub(super) fn gather_compact_line(data: &Value) -> String {
    const ARG_CAP: usize = 60;
    let calls = data
        .get("plan_ast")
        .and_then(|v| serde_json::from_value::<flux_flow::ast::DraftAst>(v.clone()).ok())
        .map(|ast| {
            let mut out = Vec::new();
            for n in &ast.body {
                collect_plan_calls(n, &mut out);
            }
            out
        })
        .unwrap_or_default();
    let summary = if calls.is_empty() {
        let ops = data.get("ops").and_then(|v| v.as_u64()).unwrap_or(0);
        let plural = if ops == 1 { "" } else { "s" };
        format!("{ops} op{plural}")
    } else {
        calls
            .iter()
            .map(|(op, input)| {
                let call = flux_tui::toolview::format_call(op, input);
                if call.arg.is_empty() {
                    call.verb
                } else {
                    format!("{} {}", call.verb, truncate(&call.arg, ARG_CAP))
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };
    format!("gathering · {summary}")
}

/// Walk a gather plan's top-level shape (a `Call`, a `$x = Call(...)` bind, or a `seq` of either)
/// collecting each call's op name + its input (the single literal-object argument a tool call
/// carries, when the plan author wrote one plainly — a computed/templated argument falls back to
/// an empty input, which `format_call` renders as just the bare verb).
pub(super) fn collect_plan_calls(node: &flux_flow::ast::Node, out: &mut Vec<(String, Value)>) {
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
        Node::Bind { value, .. } => collect_plan_calls(value, out),
        Node::Seq { body, .. } => body.iter().for_each(|n| collect_plan_calls(n, out)),
        _ => {}
    }
}

/// Like [`CliSink`] but also accumulates the assistant text (so `/goal`'s evaluator can read it).
#[derive(Default)]
pub(super) struct GoalSink {
    pub(super) text: String,
    /// `(model spec, pricing table)` for the per-turn cost suffix (C-30); `None` in tests.
    pub(super) cost: Option<(String, flux_core::PricingTable)>,
}

impl AgentSink for GoalSink {
    fn text_delta(&mut self, t: &str) {
        print!("{t}");
        std::io::stdout().flush().ok();
        self.text.push_str(t);
    }
    fn tool_call(&mut self, _dispatch: DispatchId, name: &str, input: &Value) {
        eprintln!(
            "\n{} {}",
            style::blue("→"),
            render_call_label(name, input, verbose())
        );
    }
    fn tool_result(&mut self, _dispatch: DispatchId, name: &str, result: &ToolResult) {
        let mark = if result.is_error {
            style::red("✗")
        } else {
            style::green("✓")
        };
        let body = flux_tui::toolview::format_result(name, &result.content, result.is_error)
            .unwrap_or_else(|| result_summary_for(&result.content, name, verbose()));
        eprintln!("  {mark} {body}");
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        println!();
        if let Some(u) = usage {
            // Same figures as the main rule (tokens + C-30 cost suffix), without the leading separator.
            let (spec, table) = match &self.cost {
                Some((s, t)) => (Some(s.as_str()), Some(t)),
                None => (None, None),
            };
            let stats = format!(
                "{}{}",
                usage_annotation(&u),
                cost_suffix(spec, table, Some(&u))
            );
            let stats = stats.trim_start_matches(" · ");
            if !stats.is_empty() {
                eprintln!("{}", style::dim(stats));
            }
        }
    }
}

/// C-542: one budget figure, humanized the way the rest of the CLI humanizes that quantity — elapsed
/// wall time, a bare call count, compact token counts.
fn budget_amount(dimension: flux_core::BudgetDimension, value: u64) -> String {
    match dimension {
        flux_core::BudgetDimension::WallTime => {
            style::fmt_elapsed(std::time::Duration::from_millis(value))
        }
        flux_core::BudgetDimension::ModelCalls => value.to_string(),
        _ => style::fmt_tokens(value),
    }
}

/// C-542: the CLI's projection of a crossed budget line — `(line, stops_the_run)`, or `None` when the
/// published projection crosses nothing.
///
/// `data` is the enforcing ledger's `budget.projection` payload, and every figure printed here is
/// read straight off it: this surface adds nothing up, so the number it shows is the number that
/// actually stops the run. The distinction the vocabulary turns on stays legible — a crossed target
/// warns and execution continues (`false`), a crossed hard limit is the stop line (`true`) — and the
/// one-warning-per-dimension rule is the ledger's, so a later charge past the same target prints
/// nothing again.
pub(super) fn budget_crossing_line(data: &Value) -> Option<(String, bool)> {
    let projection: flux_core::BudgetProjection =
        serde_json::from_value(data.get("projection")?.clone()).ok()?;
    let breach = |key: &str| -> Option<flux_core::BudgetBreach> {
        serde_json::from_value(data.get(key)?.clone()).ok()
    };
    let (breach, stop) = match breach("exhausted") {
        Some(breach) => (breach, true),
        None => (breach("warning")?, false),
    };
    // A warning names the target it crossed, so say what the hard ceiling still is when one is
    // declared — an undeclared dimension renders nothing rather than a reassuring zero.
    let headroom = match projection.limit.get(breach.dimension) {
        Some(limit) if !stop => {
            format!(" (hard limit {})", budget_amount(breach.dimension, limit))
        }
        _ => String::new(),
    };
    let (label, tail) = if stop {
        ("budget limit reached", "stopping at the next safe boundary")
    } else {
        ("budget target crossed", "execution continues")
    };
    Some((
        format!(
            "⚠ {label} — {} {} {} of {}{headroom} · {tail}",
            breach.scope,
            breach.dimension,
            budget_amount(breach.dimension, breach.spent),
            budget_amount(breach.dimension, breach.limit)
        ),
        stop,
    ))
}

#[cfg(test)]
mod budget_projection_tests {
    use super::*;

    /// One measured model call in the shared budget vocabulary (C-542). The real
    /// [`flux_core::BudgetLedger`] produces every figure asserted below, so a CLI line can never be a
    /// hand-summed total that disagrees with the stop that actually fires.
    fn call(event_id: &str, total_tokens: u64) -> flux_core::BudgetUsageEvent {
        flux_core::BudgetUsageEvent {
            event_id: event_id.into(),
            scope: flux_core::BudgetScope::Segment,
            attribution: flux_core::BudgetAttribution {
                run_id: "run-1".into(),
                session_id: Some("s-1".into()),
                turn_id: Some(1),
                segment: Some("explore".into()),
            },
            spend: flux_core::BudgetSpend {
                model_calls: 1,
                total_tokens,
                ..flux_core::BudgetSpend::default()
            },
            rollup: false,
        }
    }

    /// Exactly the payload the enforcing ledger publishes on its `budget.projection` observation
    /// (`EngineLoopHost::publish_budget`) — the single contract every surface reads.
    fn published(ledger: &flux_core::BudgetLedger, outcome: &flux_core::BudgetOutcome) -> Value {
        let mut data = serde_json::json!({ "projection": ledger.projection() });
        if let Some(warning) = outcome.warning {
            data["warning"] = serde_json::json!(warning);
        }
        if let Some(breach) = outcome.exhausted {
            data["exhausted"] = serde_json::json!(breach);
        }
        data
    }

    /// C-542: the CLI projects the enforcing ledger's published budget contract instead of dropping
    /// it. A crossed target is visible and does not stop the run; a crossed hard limit is the stop
    /// line. Every figure is the ledger's own, so this surface and the stop cannot drift apart.
    #[test]
    fn cli_projects_target_warning_and_hard_stop_from_the_published_budget() {
        let mut ledger = flux_core::BudgetLedger::new(flux_core::BudgetEnvelope {
            scope: flux_core::BudgetScope::Run,
            target: flux_core::BudgetLimits::with_total_tokens(1_000),
            limit: flux_core::BudgetLimits::with_total_tokens(4_000),
        });

        let outcome = ledger.record(&call("call-1", 400));
        assert!(
            budget_crossing_line(&published(&ledger, &outcome)).is_none(),
            "spend under every declared line crosses nothing"
        );

        let outcome = ledger.record(&call("call-2", 1_200));
        let (line, stop) = budget_crossing_line(&published(&ledger, &outcome))
            .expect("a crossed target must be visible");
        assert!(line.contains("budget target crossed"), "{line}");
        assert!(line.contains("run total_tokens 1.6k of 1.0k"), "{line}");
        assert!(line.contains("hard limit 4.0k"), "{line}");
        assert!(!stop, "a target never stops execution: {line}");

        let outcome = ledger.record(&call("call-3", 3_000));
        let (line, stop) = budget_crossing_line(&published(&ledger, &outcome))
            .expect("a crossed hard limit must be visible");
        assert!(line.contains("budget limit reached"), "{line}");
        assert!(line.contains("run total_tokens 4.6k of 4.0k"), "{line}");
        assert!(stop, "a hard limit is the stop line: {line}");
    }

    /// C-542: the one-warning rule belongs to the ledger, and the CLI inherits it rather than
    /// re-deriving it — a target with no hard limit warns once and never reports a stop, however far
    /// past it the run spends.
    #[test]
    fn a_target_without_a_hard_limit_warns_once_and_never_reports_a_stop() {
        let mut ledger = flux_core::BudgetLedger::new(flux_core::BudgetEnvelope {
            scope: flux_core::BudgetScope::Run,
            target: flux_core::BudgetLimits::with_total_tokens(1_000),
            limit: flux_core::BudgetLimits::default(),
        });

        let outcome = ledger.record(&call("call-1", 1_200));
        let (line, stop) =
            budget_crossing_line(&published(&ledger, &outcome)).expect("the crossed target warns");
        assert!(line.contains("budget target crossed"), "{line}");
        assert!(
            !line.contains("hard limit"),
            "nothing hard is declared: {line}"
        );
        assert!(!stop, "{line}");

        for round in 0..3 {
            let outcome = ledger.record(&call(&format!("later-{round}"), 1_200));
            assert!(
                budget_crossing_line(&published(&ledger, &outcome)).is_none(),
                "the target warns once, not once per call: round {round}"
            );
        }
    }
}
