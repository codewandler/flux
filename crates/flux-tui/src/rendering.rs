//! Pure ratatui rendering and layout.

use super::*;

/// A centered sub-rect `w`×`h` (clamped to `area`).
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// A `cap`-row scroll window over `total` items such that `selected` (clamped into range) is
/// always inside the returned `(start, count)` — the windowing math the queue, session-picker,
/// and help overlays each used to hand-roll separately (C-152). `count <= cap` and
/// `count <= total`; never panics at `total == 0` or `total == 1`.
fn overlay_window(selected: usize, total: usize, cap: usize) -> (usize, usize) {
    let count = cap.min(total);
    if count == 0 {
        return (0, 0);
    }
    let selected = selected.min(total - 1);
    let start = selected.saturating_sub(count - 1).min(total - count);
    (start, count)
}

/// Shared chrome for the queue, session-picker, and help overlays (C-152): an accent-on-`panel_bg`
/// header row, the caller's already-styled/windowed body rows, and an optional trailing `n/m`
/// overflow counter — one `Clear` + `Paragraph` pane sized exactly to its content instead of three
/// hand-rolled copies. Selection styling and row content stay with the caller; only the shape
/// (header style, no border, exact-fit sizing) is shared.
fn render_overlay_panel(
    frame: &mut Frame,
    theme: &Theme,
    header: impl Into<String>,
    body: Vec<Line<'static>>,
    counter: Option<(usize, usize)>,
    max_width: u16,
) {
    let width = frame.area().width.min(max_width);
    let mut lines = vec![Line::styled(
        header.into(),
        theme.accent_style().bg(theme.panel_bg),
    )];
    lines.extend(body);
    if let Some((position, total)) = counter {
        lines.push(Line::styled(
            format!(" {position}/{total} "),
            theme.muted_style().bg(theme.panel_bg),
        ));
    }
    let height = (lines.len() as u16).min(frame.area().height);
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).style(theme.panel_style()), area);
}

/// Minimum transcript width the empty-state card renders into (C-157) — narrower than this it
/// would need to wrap onto more lines than a short orientation card is worth, so it is skipped
/// rather than added as noise (the same narrow-width posture C-102 established for the
/// header/footer bars).
const EMPTY_CARD_MIN_WIDTH: u16 = 44;

/// A short centered card shown while the transcript has no entries yet (C-157): names the active
/// model, the workspace root, and the primary affordances (`/help`, `/` commands, `@` file
/// completion) so a fresh session answers "where am I / what can I do" instead of leaving the
/// idle footer hint as the only orientation. Drawn straight into `area` with no
/// [`ChatState::transcript_viewport`] involvement — it is not a transcript row and must never be
/// treated like one (no layout cache, no focus, no scroll).
fn render_empty_state_card(frame: &mut Frame, state: &ChatState, area: Rect) {
    if area.width < EMPTY_CARD_MIN_WIDTH || area.height < 3 {
        return;
    }
    let t = &state.theme;
    let model = state.model_spec.as_deref().unwrap_or(&state.model);
    let identity = if state.workspace_root.is_empty() {
        model.to_string()
    } else {
        format!("{model}  ·  {}", state.workspace_root)
    };
    let card_width = area.width.min(60);
    let lines = vec![
        Line::styled("flux", t.accent_style().add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center),
        Line::styled(
            truncate(&identity, card_width.saturating_sub(2) as usize),
            t.muted_style(),
        )
        .alignment(Alignment::Center),
        Line::styled("/help commands  ·  / commands  ·  @ files", t.muted_style())
            .alignment(Alignment::Center),
    ];
    let card = centered(area, card_width, lines.len() as u16);
    frame.render_widget(Paragraph::new(lines), card);
}

/// The approval sheet's border/title style and title text, tiered by the pending call's
/// risk/effect (C-154): a destructive delete and a read used to render identically (both plain
/// `accent`). Tiers reuse the runtime's own destructive/mutating signal
/// (`ApprovalRequest::destructive`/`mutating`, sourced from `IntentSet`/`PlanApprovalRequest` —
/// no new classification invented). `destructive` additionally carries `BOLD` and `mutating`
/// changes the title text, so `Theme::MONO` (`NO_COLOR`) — whose roles all resolve to the same
/// `Color::Reset` — still tells the three tiers apart without relying on color.
fn approval_tier_style(request: &controller::ApprovalRequest, t: &Theme) -> (Style, &'static str) {
    if request.destructive {
        (
            t.err_style().add_modifier(Modifier::BOLD),
            " approval · destructive ",
        )
    } else if request.mutating {
        (t.warn_style(), " approval · write ")
    } else {
        (t.accent_style(), " approval ")
    }
}

/// Render the chat: scrollable transcript, a status/spinner row, the input box, optional modal.
pub fn render(frame: &mut Frame, state: &ChatState) {
    // Whole-screen fill so a light theme works on terminals with a dark background (C-104);
    // `base_bg == Reset` (the dark themes) keeps the terminal's own background untouched.
    frame.render_widget(
        Block::default().style(state.theme.base_style()),
        frame.area(),
    );
    if frame.area().width < 24 || frame.area().height < 6 {
        frame.render_widget(
            Paragraph::new("terminal too small — resize to continue")
                .style(state.theme.muted_style()),
            frame.area(),
        );
        return;
    }
    let input_h = state.input_rows();
    let slash = state
        .slash_query()
        .map(|q| slash_matches(&q, &state.file_commands))
        .unwrap_or_default();
    // C-112: the `@` path-completion popup shares the slash menu's layout slot (the two are
    // mutually exclusive — an active slash query suppresses `at_token`).
    let paths = if slash.is_empty() {
        state.path_popup_matches()
    } else {
        Vec::new()
    };
    // C-153: reserve one extra row for the `n/m` overflow counter once the candidate list runs
    // past the 6-row window — otherwise a long slash/`@` menu gives no more-below signal at all.
    let menu_len = slash.len().max(paths.len());
    let menu_h = menu_len.min(6) as u16 + if menu_len > 6 { 1 } else { 0 };
    // A point-in-time copy: the engine may drain the shared steering queue mid-render (A-94).
    let queued = state.queue_texts();
    let queue_h = if queued.is_empty() {
        0
    } else {
        queued.len().min(3) as u16
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(queue_h),
        Constraint::Length(menu_h),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let (header_area, transcript_area, queue_area, menu_area, input_area, footer_area) = (
        chunks[0], chunks[1], chunks[2], chunks[3], chunks[4], chunks[5],
    );

    frame.render_widget(
        Paragraph::new(state.header_line(header_area.width)),
        header_area,
    );

    // C-157: an empty session shows a short orientation card instead of running the transcript
    // viewport at all — the card must never populate the layout cache, participate in focus
    // (C-111), or set scroll bookkeeping, so it stays entirely outside `transcript_viewport`.
    if state.entries.is_empty() {
        render_empty_state_card(frame, state, transcript_area);
    } else {
        let visible = state.transcript_viewport(transcript_area.width, transcript_area.height);
        frame.render_widget(Paragraph::new(visible), transcript_area);

        // C-106: a scroll position indicator while detached from follow mode. It overlays the last
        // column only while detached — the transcript width never changes, so the layout cache stays
        // keyed the same across attach/detach.
        if !state.follow && state.last_max_scroll.get() > 0 {
            use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
            let mut bar_state = ScrollbarState::new(state.last_max_scroll.get() as usize)
                .position(state.scroll as usize)
                .viewport_content_length(transcript_area.height as usize);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .style(state.theme.muted_style())
                    .thumb_style(state.theme.accent_style()),
                transcript_area,
                &mut bar_state,
            );
        }
    }

    if !queued.is_empty() {
        let mut rows: Vec<Line> = queued
            .iter()
            .take(3)
            .enumerate()
            .map(|(i, prompt)| {
                Line::from(vec![
                    Span::styled(format!(" {}. ", i + 1), state.theme.accent_style()),
                    Span::styled(
                        truncate(&prompt.replace('\n', " "), 72),
                        state.theme.muted_style(),
                    ),
                ])
            })
            .collect();
        if queued.len() > 3 {
            rows[2] = Line::styled(
                format!(" +{} more queued", queued.len() - 2),
                state.theme.muted_style(),
            );
        }
        frame.render_widget(Paragraph::new(rows), queue_area);
    }

    if !slash.is_empty() {
        let theme = &state.theme;
        let sel = state.slash_sel.min(slash.len() - 1);
        let start = sel.saturating_sub(5).min(slash.len().saturating_sub(6));
        let mut rows: Vec<Line> = slash
            .iter()
            .skip(start)
            .take(6)
            .enumerate()
            .map(|(i, c)| {
                let absolute = start + i;
                let style = if absolute == sel {
                    Style::default().bg(theme.sel_bg).fg(theme.accent)
                } else {
                    theme.muted_style()
                };
                Line::from(vec![
                    Span::styled(if absolute == sel { " ▸ " } else { "   " }, style),
                    Span::styled(format!("/{}", c.name), style.add_modifier(Modifier::BOLD)),
                    Span::styled(format!("   {}", c.desc), style),
                ])
            })
            .collect();
        // C-153: a rendered counter, matching the queue/session overlays' overflow signal.
        if slash.len() > 6 {
            rows.push(Line::styled(
                format!(" {}/{} ", sel + 1, slash.len()),
                theme.muted_style(),
            ));
        }
        frame.render_widget(Paragraph::new(rows), menu_area);
    }

    if !paths.is_empty() {
        let theme = &state.theme;
        let sel = state.path_sel.min(paths.len() - 1);
        let start = sel.saturating_sub(5).min(paths.len().saturating_sub(6));
        let mut rows: Vec<Line> = paths
            .iter()
            .skip(start)
            .take(6)
            .enumerate()
            .map(|(i, path)| {
                let absolute = start + i;
                let style = if absolute == sel {
                    Style::default().bg(theme.sel_bg).fg(theme.accent)
                } else {
                    theme.muted_style()
                };
                Line::from(vec![
                    Span::styled(if absolute == sel { " ▸ " } else { "   " }, style),
                    Span::styled(path.clone(), style),
                ])
            })
            .collect();
        if paths.len() > 6 {
            rows.push(Line::styled(
                format!(" {}/{} ", sel + 1, paths.len()),
                theme.muted_style(),
            ));
        }
        frame.render_widget(Paragraph::new(rows), menu_area);
    }

    frame.render_widget(
        Block::default().style(state.theme.composer_style()),
        input_area,
    );
    let mut input = state.input.clone();
    input.set_style(state.theme.composer_style());
    input.set_placeholder_style(state.theme.muted_style().bg(state.theme.composer_bg));
    input.set_cursor_style(
        state
            .theme
            .composer_style()
            .add_modifier(Modifier::REVERSED),
    );
    frame.render_widget(&input, input_area);

    frame.render_widget(
        Paragraph::new(state.footer_line(footer_area.width)),
        footer_area,
    );

    if state.queue_open && !queued.is_empty() {
        let (start, visible) = overlay_window(state.queue_sel, queued.len(), 10);
        let selected = state.queue_sel.min(queued.len() - 1);
        let rows: Vec<Line> = queued
            .iter()
            .skip(start)
            .take(visible)
            .enumerate()
            .map(|(offset, prompt)| {
                let index = start + offset;
                let style = if index == selected {
                    Style::default()
                        .fg(state.theme.accent)
                        .bg(state.theme.sel_bg)
                } else {
                    state.theme.panel_style()
                };
                Line::styled(
                    format!(
                        " {}  {}",
                        index + 1,
                        truncate(&prompt.replace('\n', " "), 68)
                    ),
                    style,
                )
            })
            .collect();
        let counter = (queued.len() > visible).then_some((selected + 1, queued.len()));
        render_overlay_panel(
            frame,
            &state.theme,
            " queued · Enter edit · Delete remove · Alt-↑/↓ reorder · Esc close ",
            rows,
            counter,
            76,
        );
    }

    if state.session_picker.is_some() {
        // C-153: filtered/ranked through the shared fuzzy matcher, not the raw loaded list.
        let sessions = state.session_picker_matches();
        let width = frame.area().width.min(76);
        let (start, visible) = overlay_window(state.session_sel, sessions.len(), 12);
        let selected = state.session_sel.min(sessions.len().saturating_sub(1));
        let header = if state.session_query.is_empty() {
            " sessions · type to filter · Enter resume · Esc close ".to_string()
        } else {
            format!(
                " sessions · filter: {} · Enter resume · Esc close ",
                state.session_query
            )
        };
        // C-151: current wall time for each row's relative "… ago" — `humanize::fmt_age` itself
        // stays deterministic (it takes `now_ms` as a parameter); only this live call site reads
        // the clock.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let rows: Vec<Line> = sessions
            .iter()
            .skip(start)
            .take(visible)
            .enumerate()
            .map(|(offset, session)| {
                let index = start + offset;
                let marker = if session.id == state.session_id {
                    "●"
                } else {
                    " "
                };
                let age = fmt_age(now_ms, session.updated_at_ms);
                let label = format!(
                    " {marker} {}  · {} msg · {} · {age}",
                    session.id, session.messages, session.model
                );
                let style = if index == selected {
                    Style::default()
                        .fg(state.theme.accent)
                        .bg(state.theme.sel_bg)
                } else {
                    state.theme.panel_style()
                };
                Line::styled(truncate(&label, width as usize), style)
            })
            .collect();
        let counter = (sessions.len() > visible).then_some((selected + 1, sessions.len()));
        render_overlay_panel(frame, &state.theme, header, rows, counter, 76);
    }

    // C-140: `/usage` overlay — the turn in progress, per round, from the same per-call data
    // `flux usage` reads offline. The session header shows cumulative totals; the per-round bars are
    // where a mid-turn cache collapse (tool-set churn, TTL expiry) becomes visible AS it happens.
    if state.usage_open {
        let t = &state.theme;
        let width = frame.area().width.min(56);
        // Body width available for a bar: the frame minus the label gutter and the trailing pct.
        let bar_w = (width as usize).saturating_sub(22).clamp(6, 24);
        let bar = |ratio: f64| -> String {
            let filled = ((ratio.clamp(0.0, 1.0)) * bar_w as f64).round() as usize;
            format!("{}{}", "█".repeat(filled), "░".repeat(bar_w - filled))
        };

        let mut rows = vec![Line::styled(
            format!(" usage · {} ", state.session_id),
            t.accent_style().bg(t.panel_bg),
        )];

        if state.turn_cache.is_empty() && state.cache.is_empty() {
            // Empty state: an offline `-m mock` run, or before the first model call of the session.
            rows.push(Line::styled(
                " no model calls recorded yet",
                t.muted_style().bg(t.panel_bg),
            ));
        } else {
            let head = match state.turn_rounds.last() {
                Some(last) => format!(" {} · round {}", last.model, state.turn_rounds.len()),
                None => format!(" {}", state.model),
            };
            rows.push(Line::styled(head, t.muted_style().bg(t.panel_bg)));

            let turn = &state.turn_cache;
            rows.push(Line::styled(" this turn", t.muted_style().bg(t.panel_bg)));
            rows.push(Line::from(vec![
                Span::styled("   hit  ", t.muted_style().bg(t.panel_bg)),
                Span::styled(bar(turn.hit_rate()), t.accent_style().bg(t.panel_bg)),
                Span::styled(
                    format!("  {:>3.0}%", turn.hit_rate() * 100.0),
                    t.panel_style(),
                ),
            ]));
            rows.push(Line::styled(
                format!(
                    "   read {} · write {} · fresh {}",
                    fmt_count(turn.read),
                    fmt_count(turn.write),
                    fmt_count(turn.fresh)
                ),
                t.panel_style(),
            ));

            if !state.turn_rounds.is_empty() {
                rows.push(Line::styled(" per round", t.muted_style().bg(t.panel_bg)));
                // Newest rounds matter most; when the list outgrows the frame keep the tail and
                // mark how many were elided rather than silently dropping the early ones.
                let budget = (frame.area().height as usize)
                    .saturating_sub(rows.len() + 4)
                    .max(1);
                let skipped = state.turn_rounds.len().saturating_sub(budget);
                if skipped > 0 {
                    rows.push(Line::styled(
                        format!("   … {skipped} earlier"),
                        t.muted_style().bg(t.panel_bg),
                    ));
                }
                for (index, round) in state.turn_rounds.iter().enumerate().skip(skipped) {
                    // The churn marker is DERIVED, not guessed: the engine reports how many
                    // operations were advertised on each call, and a change between rounds is
                    // exactly the tool-set churn that cold-writes the cached prefix (tools render
                    // before system on the Anthropic wire). No change ⇒ no marker.
                    let churned = index
                        .checked_sub(1)
                        .and_then(|prev| state.turn_rounds.get(prev))
                        .is_some_and(|prev| prev.operations != round.operations);
                    let mut spans = vec![
                        Span::styled(
                            format!("  {:>2} ", index + 1),
                            t.muted_style().bg(t.panel_bg),
                        ),
                        Span::styled(bar(round.hit_rate()), t.accent_style().bg(t.panel_bg)),
                        Span::styled(
                            format!("  {:>3.0}%", round.hit_rate() * 100.0),
                            t.panel_style(),
                        ),
                    ];
                    if churned {
                        spans.push(Span::styled(
                            format!(" ← tools {}", round.operations),
                            t.warn_style().bg(t.panel_bg),
                        ));
                    } else if !round.stage.is_empty() {
                        spans.push(Span::styled(
                            format!(" {}", round.stage),
                            t.muted_style().bg(t.panel_bg),
                        ));
                    }
                    rows.push(Line::from(spans));
                }
            }

            let session = &state.cache;
            let cost = match (state.cost_usd, state.cost_unpriced) {
                (Some(usd), true) => format!(" · ${usd:.4}+?"),
                (Some(usd), false) => format!(" · ${usd:.4}"),
                (None, true) => " · $?".to_string(),
                (None, false) => String::new(),
            };
            rows.push(Line::styled(
                format!(
                    " session Σ {} prompt · {:.0}% hit · ↓{}{}",
                    fmt_count(session.prompt_tokens()),
                    session.hit_rate() * 100.0,
                    fmt_count(state.tokens_out),
                    cost
                ),
                t.muted_style().bg(t.panel_bg),
            ));
        }
        rows.push(Line::styled(
            " esc to close ",
            t.muted_style().bg(t.panel_bg),
        ));

        let height = (rows.len() as u16).min(frame.area().height);
        let area = centered(frame.area(), width, height);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(rows).style(t.panel_style()), area);
    }

    // C-110: help overlay — keys from HELP_KEYS, commands iterated from the merged built-in +
    // command-file table (no drift).
    if state.help_open {
        let t = &state.theme;
        let mut rows = Vec::new();
        let key_w = HELP_KEYS
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        for (keys, what) in HELP_KEYS {
            rows.push(Line::from(vec![
                Span::styled(format!(" {keys:key_w$}  "), t.accent_style().bg(t.panel_bg)),
                Span::styled((*what).to_string(), t.panel_style()),
            ]));
        }
        rows.push(Line::styled(" commands", t.muted_style().bg(t.panel_bg)));
        let commands = all_slash_commands(&state.file_commands);
        for chunk in commands.chunks(2) {
            let mut spans = Vec::new();
            for c in chunk {
                spans.push(Span::styled(
                    format!(" /{:<9}", c.name),
                    t.accent_style().bg(t.panel_bg).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(format!("{:<26}", c.desc), t.panel_style()));
            }
            rows.push(Line::from(spans));
        }
        render_overlay_panel(frame, t, " help · Esc close ", rows, None, 76);
    }

    if let Some(view) = &state.approval {
        let t = &state.theme;
        // C-115: a pending edit/write shows its hunk diff inside the sheet — the decision point
        // where eyes on the content matter most. Windowed to a handful of rows; absent entry or
        // non-diffable tool just means no preview.
        const PREVIEW_CAP: usize = 8;
        let diff = state
            .pending_approval_input(&view.request.tool)
            .and_then(|input| toolview::format_diff(&view.request.tool, input));
        let diff_total = diff.as_ref().map(|d| d.len()).unwrap_or(0);
        let reason_row = usize::from(view.reason.is_some());
        // C-182: a plan's destructive disclosure gets its own row above the detail list, where it
        // cannot be scrolled out of sight by a long target list.
        let warn_row = usize::from(view.request.destructive);

        // Sheet height: border (2) + question + subjects (windowed) + diff preview + optional
        // reason line + hints, capped at half the screen so long content scrolls instead of
        // swallowing the transcript. When the cap bites, the PREVIEW absorbs the squeeze —
        // question, subjects, and hints always survive.
        let want_preview = diff_total.min(PREVIEW_CAP) + usize::from(diff_total > PREVIEW_CAP); // "… more" marker row
        let max_h = (frame.area().height / 2).max(5);
        let want =
            2 + 2 + (view.request.subjects.len() + want_preview + reason_row + warn_row) as u16;
        let height = want.min(max_h).min(input_area.y.saturating_sub(1)).max(5);
        let inner_rows = height.saturating_sub(2) as usize;
        // minus question + hints + reason + destructive warning
        let body = inner_rows.saturating_sub(2 + reason_row + warn_row);
        let subject_rows = view
            .request
            .subjects
            .len()
            .min(body.saturating_sub(want_preview).max(1));
        let preview_budget = body.saturating_sub(subject_rows);
        let (preview_rows, preview_more) = if diff_total > preview_budget {
            (preview_budget.saturating_sub(1), true)
        } else {
            (diff_total, false)
        };
        let area = Rect {
            x: frame.area().x,
            y: input_area.y.saturating_sub(height),
            width: frame.area().width,
            height,
        };
        frame.render_widget(Clear, area);

        let mut header = vec![
            Span::styled("approve ", Style::default().fg(t.warn)),
            Span::styled(
                view.request.tool.clone(),
                t.tool_style().add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(t.warn)),
        ];
        // C-182: a plan's aggregate risk rides the header in risk color, so the badge survives
        // however far the detail list is scrolled.
        if let Some(summary) = &view.request.summary {
            header.push(Span::raw("  "));
            header.push(Span::styled(summary.clone(), plan::risk_style(summary, t)));
        }
        let mut rows: Vec<Line> = vec![Line::from(header)];
        if view.request.destructive {
            rows.push(Line::styled(
                "⚠ contains a destructive operation",
                t.warn_style().add_modifier(Modifier::BOLD),
            ));
        }
        let start = view
            .scroll
            .min(view.request.subjects.len().saturating_sub(subject_rows));
        let shown = view.request.subjects.iter().skip(start).take(subject_rows);
        for subject in shown {
            rows.push(Line::styled(
                format!(
                    "- {}",
                    truncate(subject, area.width.saturating_sub(4) as usize)
                ),
                t.muted_style(),
            ));
        }
        let hidden = view
            .request
            .subjects
            .len()
            .saturating_sub(start + subject_rows);
        if let Some(diff) = &diff {
            for row in diff.iter().take(preview_rows) {
                rows.push(diff_row_line(t, row, " "));
            }
            if preview_more {
                rows.push(Line::styled(
                    format!(" … {} more diff lines", diff.len() - preview_rows),
                    t.muted_style(),
                ));
            }
        }
        // C-113: the one-line deny-reason input replaces nothing — it rides above the hints
        // while active, with a cursor to signal it takes the keyboard.
        if let Some(reason) = &view.reason {
            rows.push(Line::from(vec![
                Span::styled("deny reason: ", t.err_style().add_modifier(Modifier::BOLD)),
                Span::styled(reason.clone(), t.err_style()),
                Span::styled("▍", t.accent_style()),
            ]));
        }
        let mut hints = if view.reason.is_some() {
            vec![
                Span::styled("[Enter]", t.err_style().add_modifier(Modifier::BOLD)),
                Span::styled(" deny with reason  ", t.muted_style()),
                Span::styled("[Esc]", t.accent_style().add_modifier(Modifier::BOLD)),
                Span::styled(" back", t.muted_style()),
            ]
        } else {
            vec![
                Span::styled("[y]", t.ok_style().add_modifier(Modifier::BOLD)),
                Span::styled(" allow ", t.muted_style()),
                Span::styled(
                    "[a]",
                    Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" always ", t.muted_style()),
                Span::styled("[n/Esc]", t.err_style().add_modifier(Modifier::BOLD)),
                Span::styled(" deny ", t.muted_style()),
                Span::styled("[d]", t.err_style().add_modifier(Modifier::BOLD)),
                Span::styled(" reason", t.muted_style()),
            ]
        };
        if hidden > 0 || start > 0 {
            hints.push(Span::styled(
                format!("  ↑/↓ +{hidden} more"),
                t.accent_style(),
            ));
        }
        rows.push(Line::from(hints));

        // C-154: border + title tint by risk tier instead of a fixed accent, so a destructive
        // delete no longer looks like a read at a glance; MONO/NO_COLOR still tells the tiers
        // apart via the BOLD modifier + title text (its roles all resolve to the same color).
        let (tier_style, tier_title) = approval_tier_style(&view.request, t);
        let sheet = Paragraph::new(rows).style(state.theme.panel_style()).block(
            Block::bordered()
                .border_style(tier_style)
                .title(Span::styled(tier_title, tier_style)),
        );
        frame.render_widget(sheet, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-152: `selected` (clamped into range) is always inside the returned window, for every
    /// combination of a small `total`/`cap`/`selected` — including `total == 0` and `total == 1`,
    /// which the three overlays' hand-rolled `saturating_sub` math used to have to get right on
    /// their own.
    #[test]
    fn overlay_window_keeps_selection_visible_and_never_panics() {
        for total in 0..=6usize {
            for cap in 0..=6usize {
                for selected in 0..=8usize {
                    let (start, count) = overlay_window(selected, total, cap);
                    assert!(count <= cap, "count <= cap");
                    assert!(count <= total, "count <= total");
                    assert!(start + count <= total, "window stays in bounds");
                    if total > 0 && count > 0 {
                        let clamped = selected.min(total - 1);
                        assert!(
                            clamped >= start && clamped < start + count,
                            "selected {selected} (clamped {clamped}) not in window \
                             [{start}, {}) for total={total} cap={cap}",
                            start + count
                        );
                    }
                }
            }
        }
    }

    /// A window smaller than the total scrolls to keep `selected` as the LAST visible row rather
    /// than clamping to `[0, cap)` and losing it off the bottom.
    #[test]
    fn overlay_window_scrolls_to_keep_a_far_selection_visible() {
        assert_eq!(overlay_window(0, 20, 6), (0, 6));
        assert_eq!(overlay_window(2, 20, 6), (0, 6));
        assert_eq!(overlay_window(10, 20, 6), (5, 6));
        // Near the tail the window pins to the last `cap` items instead of overhanging past it.
        assert_eq!(overlay_window(19, 20, 6), (14, 6));
    }

    #[test]
    fn overlay_window_at_len_zero_and_one_never_panics() {
        assert_eq!(overlay_window(0, 0, 10), (0, 0));
        assert_eq!(overlay_window(5, 0, 10), (0, 0));
        assert_eq!(overlay_window(0, 1, 10), (0, 1));
        assert_eq!(overlay_window(5, 1, 10), (0, 1));
        assert_eq!(overlay_window(0, 1, 0), (0, 0));
    }
}
