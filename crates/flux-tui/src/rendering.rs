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
    let menu_h = slash.len().max(paths.len()).min(6) as u16;
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
        let rows: Vec<Line> = slash
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
        frame.render_widget(Paragraph::new(rows), menu_area);
    }

    if !paths.is_empty() {
        let theme = &state.theme;
        let sel = state.path_sel.min(paths.len() - 1);
        let start = sel.saturating_sub(5).min(paths.len().saturating_sub(6));
        let rows: Vec<Line> = paths
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
        let visible = queued.len().min(10);
        let height = (visible as u16 + 2).min(frame.area().height);
        let area = centered(frame.area(), frame.area().width.min(76), height);
        frame.render_widget(Clear, area);
        let selected = state.queue_sel.min(queued.len() - 1);
        let start = selected
            .saturating_sub(visible.saturating_sub(1))
            .min(queued.len().saturating_sub(visible));
        let mut rows = vec![Line::styled(
            " queued · Enter edit · Delete remove · Alt-↑/↓ reorder · Esc close ",
            state.theme.accent_style().bg(state.theme.panel_bg),
        )];
        rows.extend(
            queued
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
                }),
        );
        if queued.len() > visible {
            rows.push(Line::styled(
                format!(" {}/{} ", selected + 1, queued.len()),
                state.theme.muted_style().bg(state.theme.panel_bg),
            ));
        }
        frame.render_widget(Paragraph::new(rows).style(state.theme.panel_style()), area);
    }

    if let Some(sessions) = state.session_picker.as_ref() {
        let visible = sessions.len().min(12);
        let height = (visible as u16 + 2).min(frame.area().height);
        let width = frame.area().width.min(76);
        let area = centered(frame.area(), width, height);
        frame.render_widget(Clear, area);
        let selected = state.session_sel.min(sessions.len().saturating_sub(1));
        let start = selected
            .saturating_sub(visible.saturating_sub(1))
            .min(sessions.len().saturating_sub(visible));
        let mut rows = vec![Line::styled(
            " sessions · Enter resume · Esc close ",
            state.theme.accent_style().bg(state.theme.panel_bg),
        )];
        rows.extend(sessions.iter().skip(start).take(visible).enumerate().map(
            |(offset, session)| {
                let index = start + offset;
                let marker = if session.id == state.session_id {
                    "●"
                } else {
                    " "
                };
                let label = format!(
                    " {marker} {}  · {} msg · {}",
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
            },
        ));
        if sessions.len() > visible {
            rows.push(Line::styled(
                format!(" {}/{} ", selected + 1, sessions.len()),
                state.theme.muted_style().bg(state.theme.panel_bg),
            ));
        }
        frame.render_widget(Paragraph::new(rows).style(state.theme.panel_style()), area);
    }

    // C-110: help overlay — keys from HELP_KEYS, commands iterated from the merged built-in +
    // command-file table (no drift).
    if state.help_open {
        let t = &state.theme;
        let width = frame.area().width.min(76);
        let mut rows = vec![Line::styled(
            " help · Esc close ",
            t.accent_style().bg(t.panel_bg),
        )];
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
        let height = (rows.len() as u16).min(frame.area().height);
        let area = centered(frame.area(), width, height);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(rows).style(t.panel_style()), area);
    }

    if let Some(view) = &state.approval {
        let t = &state.theme;
        // C-115: a pending edit/write shows its hunk diff inside the sheet — the decision point
        // where eyes on the content matter most. Windowed to a handful of rows; absent entry or
        // non-diffable tool just means no preview.
        const PREVIEW_CAP: usize = 8;
        let diff = state
            .pending_approval_input(&view.tool)
            .and_then(|input| toolview::format_diff(&view.tool, input));
        let diff_total = diff.as_ref().map(|d| d.len()).unwrap_or(0);
        let reason_row = usize::from(view.reason.is_some());

        // Sheet height: border (2) + question + subjects (windowed) + diff preview + optional
        // reason line + hints, capped at half the screen so long content scrolls instead of
        // swallowing the transcript. When the cap bites, the PREVIEW absorbs the squeeze —
        // question, subjects, and hints always survive.
        let want_preview = diff_total.min(PREVIEW_CAP) + usize::from(diff_total > PREVIEW_CAP); // "… more" marker row
        let max_h = (frame.area().height / 2).max(5);
        let want = 2 + 2 + (view.subjects.len() + want_preview + reason_row) as u16;
        let height = want.min(max_h).min(input_area.y.saturating_sub(1)).max(5);
        let inner_rows = height.saturating_sub(2) as usize;
        let body = inner_rows.saturating_sub(2 + reason_row); // minus question + hints + reason
        let subject_rows = view
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

        let mut rows: Vec<Line> = vec![Line::from(vec![
            Span::styled("approve ", Style::default().fg(t.warn)),
            Span::styled(
                view.tool.clone(),
                t.tool_style().add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(t.warn)),
        ])];
        let start = view
            .scroll
            .min(view.subjects.len().saturating_sub(subject_rows));
        let shown = view.subjects.iter().skip(start).take(subject_rows);
        for subject in shown {
            rows.push(Line::styled(
                format!(
                    "- {}",
                    truncate(subject, area.width.saturating_sub(4) as usize)
                ),
                t.muted_style(),
            ));
        }
        let hidden = view.subjects.len().saturating_sub(start + subject_rows);
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

        let sheet = Paragraph::new(rows).style(state.theme.panel_style()).block(
            Block::bordered()
                .border_style(t.accent_style())
                .title(Span::styled(" approval ", t.accent_style())),
        );
        frame.render_widget(sheet, area);
    }
}
