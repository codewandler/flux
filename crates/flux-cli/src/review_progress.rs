//! Live progress projection and stderr renderers for the immutable built-in review flow (C-530).

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgressRender {
    Tree,
    Plain,
    Off,
}

impl ProgressRender {
    fn resolve(requested: ReviewProgress) -> Self {
        match requested {
            ReviewProgress::Auto if std::io::stderr().is_terminal() => Self::Tree,
            ReviewProgress::Auto => Self::Plain,
            ReviewProgress::Tree => Self::Tree,
            ReviewProgress::Plain => Self::Plain,
            ReviewProgress::Off => Self::Off,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhaseStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl PhaseStatus {
    fn marker(self) -> &'static str {
        match self {
            Self::Pending => "…",
            Self::Running => "◐",
            Self::Done => "✓",
            Self::Failed => "✗",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// Bounded, privacy-preserving projection for the built-in review flow. Host-authored operation names
/// identify the phases; reviewer detail comes only from `FleetProjection`'s default-deny rows.
#[derive(Debug)]
struct ReviewProgressState {
    gather: PhaseStatus,
    gather_done: usize,
    aggregate: PhaseStatus,
    task_done: usize,
    task_failed: bool,
    fleet: flux_tui::fleet::FleetProjection,
}

impl Default for ReviewProgressState {
    fn default() -> Self {
        Self {
            gather: PhaseStatus::Pending,
            gather_done: 0,
            aggregate: PhaseStatus::Pending,
            task_done: 0,
            task_failed: false,
            fleet: flux_tui::fleet::FleetProjection::new(),
        }
    }
}

impl ReviewProgressState {
    fn tool_call(&mut self, name: &str) {
        match name {
            "git_status" | "git_diff" | "read_many" => self.gather = PhaseStatus::Running,
            "review.aggregate" => self.aggregate = PhaseStatus::Running,
            _ => {}
        }
    }

    fn tool_result(&mut self, name: &str, is_error: bool) {
        match name {
            "git_status" | "git_diff" | "read_many" => {
                if is_error {
                    self.gather = PhaseStatus::Failed;
                } else {
                    self.gather_done += 1;
                    if self.gather_done >= 3 {
                        self.gather = PhaseStatus::Done;
                    }
                }
            }
            "task" => {
                self.task_done += 1;
                self.task_failed |= is_error;
            }
            "review.aggregate" => {
                self.aggregate = if is_error {
                    PhaseStatus::Failed
                } else {
                    PhaseStatus::Done
                };
            }
            _ => {}
        }
    }

    fn reviewers(&self, now: std::time::Instant) -> PhaseStatus {
        let rows = self.fleet.rows(now);
        if self.task_failed
            || rows.iter().any(|row| {
                matches!(
                    row.status,
                    flux_tui::fleet::WorkerStatus::Finished { is_error: true }
                )
            })
        {
            PhaseStatus::Failed
        } else if self.task_done >= 3
            || (rows.len() >= 3
                && rows.iter().all(|row| {
                    matches!(
                        row.status,
                        flux_tui::fleet::WorkerStatus::Finished { is_error: false }
                    )
                }))
        {
            PhaseStatus::Done
        } else if rows.is_empty() && self.task_done == 0 {
            PhaseStatus::Pending
        } else {
            PhaseStatus::Running
        }
    }

    fn finish(&mut self, success: bool) {
        if success {
            self.gather = PhaseStatus::Done;
            self.aggregate = PhaseStatus::Done;
            self.task_done = self.task_done.max(3);
        } else {
            if self.gather != PhaseStatus::Done {
                self.gather = PhaseStatus::Failed;
            }
            if self.aggregate != PhaseStatus::Done {
                self.aggregate = PhaseStatus::Failed;
            }
            self.task_failed = true;
        }
    }

    fn lines(&self, now: std::time::Instant) -> Vec<String> {
        let reviewers = self.reviewers(now);
        let rows = self.fleet.rows(now);
        let complete = rows
            .iter()
            .filter(|row| matches!(row.status, flux_tui::fleet::WorkerStatus::Finished { .. }))
            .count()
            .max(self.task_done.min(3));
        let mut lines = vec!["Review".to_string()];
        lines.push(format!(
            "├─ {} Gather context · {}",
            self.gather.marker(),
            self.gather.label()
        ));
        lines.push(format!(
            "├─ {} Specialized reviewers · {complete}/3 · {}",
            reviewers.marker(),
            reviewers.label()
        ));
        for (index, row) in rows.iter().enumerate() {
            let branch = if index + 1 == rows.len() {
                "└─"
            } else {
                "├─"
            };
            let marker = match row.status {
                flux_tui::fleet::WorkerStatus::Finished { is_error: false } => "✓",
                flux_tui::fleet::WorkerStatus::Finished { is_error: true } => "✗",
                _ if row.stalled => "⚠",
                _ => "◐",
            };
            let operation = row
                .status
                .op()
                .map(|op| format!(" · {op}"))
                .unwrap_or_default();
            lines.push(format!(
                "│  {branch} {marker} {}#{} · {}{operation} · idle {}",
                row.role,
                row.spawn_id,
                row.status.label(),
                style::fmt_elapsed(row.idle),
            ));
        }
        lines.push(format!(
            "└─ {} Aggregate findings · {}",
            self.aggregate.marker(),
            self.aggregate.label()
        ));
        lines
    }

    fn summary(&self, now: std::time::Instant) -> String {
        let rows = self.fleet.rows(now);
        let live = rows
            .iter()
            .filter(|row| !matches!(row.status, flux_tui::fleet::WorkerStatus::Finished { .. }))
            .count();
        format!(
            "review · context {} · reviewers {} ({live} live, {}/3 complete) · aggregate {}",
            self.gather.label(),
            self.reviewers(now).label(),
            self.task_done.min(3),
            self.aggregate.label(),
        )
    }
}

pub(super) struct ReviewProgressSink {
    render: ProgressRender,
    state: ReviewProgressState,
    painted_lines: usize,
    last_plain: String,
}

impl ReviewProgressSink {
    pub(super) fn shared(requested: ReviewProgress) -> Arc<std::sync::Mutex<Self>> {
        Arc::new(std::sync::Mutex::new(Self {
            render: ProgressRender::resolve(requested),
            state: ReviewProgressState::default(),
            painted_lines: 0,
            last_plain: String::new(),
        }))
    }

    pub(super) fn start(&mut self) {
        self.paint();
    }

    pub(super) fn finish(&mut self, success: bool) {
        self.state.finish(success);
        self.paint();
    }

    /// Refresh elapsed/idle/stalled presentation while no new runtime event is arriving.
    pub(super) fn tick(&mut self) {
        self.paint();
    }

    fn paint(&mut self) {
        let now = std::time::Instant::now();
        match self.render {
            ProgressRender::Off => {}
            ProgressRender::Plain => {
                let summary = self.state.summary(now);
                if summary != self.last_plain {
                    eprintln!("{summary}");
                    self.last_plain = summary;
                }
            }
            ProgressRender::Tree => {
                let lines = self.state.lines(now);
                let mut stderr = std::io::stderr().lock();
                if self.painted_lines > 0 {
                    let _ = write!(stderr, "\x1b[{}A\r\x1b[J", self.painted_lines);
                }
                for line in &lines {
                    let _ = writeln!(stderr, "\r\x1b[2K{line}");
                }
                let _ = stderr.flush();
                self.painted_lines = lines.len();
            }
        }
    }
}

impl AgentSink for ReviewProgressSink {
    fn tool_call(&mut self, _dispatch: DispatchId, name: &str, _input: &Value) {
        self.state.tool_call(name);
        self.paint();
    }

    fn tool_result(&mut self, _dispatch: DispatchId, name: &str, result: &ToolResult) {
        self.state.tool_result(name, result.is_error);
        self.paint();
    }

    fn observation(&mut self, observation: &flux_evidence::Observation) {
        if let Some(activity) = flux_runtime::SpawnActivity::from_observation(observation) {
            self.state.fleet.apply(&activity, std::time::Instant::now());
            self.paint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_runtime::{SpawnActivity, SpawnActivityEvent};

    fn activity(spawn_id: u64, role: &str, event: SpawnActivityEvent) -> SpawnActivity {
        SpawnActivity {
            spawn_id,
            role: role.into(),
            child_session_id: format!("s_{spawn_id}"),
            parent_session: Some("review".into()),
            depth: 1,
            event,
        }
    }

    /// C-530 failing-first: all concurrent roles must be visible as correlated, closed-status rows.
    #[test]
    fn review_progress_tree_tracks_three_correlated_reviewers_and_phases() {
        let now = std::time::Instant::now();
        let mut state = ReviewProgressState::default();
        for op in ["git_status", "git_diff", "read_many"] {
            state.tool_call(op);
            state.tool_result(op, false);
        }
        for (id, role) in [
            (1, "review-security"),
            (2, "review-correctness"),
            (3, "review-maintainability"),
        ] {
            state.fleet.apply(
                &activity(id, role, SpawnActivityEvent::Planning { active: true }),
                now,
            );
        }

        let running = state.lines(now).join("\n");
        assert!(running.contains("✓ Gather context · done"));
        assert!(running.contains("Specialized reviewers · 0/3 · running"));
        assert!(running.contains("review-security#1 · planning"));
        assert!(running.contains("review-correctness#2 · planning"));
        assert!(running.contains("review-maintainability#3 · planning"));

        for (id, role) in [
            (1, "review-security"),
            (2, "review-correctness"),
            (3, "review-maintainability"),
        ] {
            state.fleet.apply(
                &activity(
                    id,
                    role,
                    SpawnActivityEvent::Finished {
                        usage: None,
                        is_error: false,
                    },
                ),
                now,
            );
            state.tool_result("task", false);
        }
        state.tool_call("review.aggregate");
        state.tool_result("review.aggregate", false);
        let done = state.lines(now).join("\n");
        assert!(done.contains("Specialized reviewers · 3/3 · done"));
        assert!(done.contains("✓ Aggregate findings · done"));
        assert!(!state.summary(now).contains('\u{1b}'));
    }
}
