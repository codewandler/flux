//! `flux ops --explore` — a standalone full-screen browser over the operation catalog.
//!
//! **The DTO is the point.** `flux-tui` depends on neither `flux-tools`, `flux-web` nor `flux-cli`
//! (crate charter, C-518), so this module renders a caller-supplied `Vec<`[`OpRow`]`>` and knows
//! nothing about registries, categories or doc indexes. `flux-cli`'s `ops_cmd` assembles the rows.
//! That seam is what lets iteration 2 stream plugin ops and iteration 4 add connector ops without
//! this file learning where an op comes from.
//!
//! Start state is search-first, like a search engine's home page: a small animated node
//! constellation ([`crate::pictogram`]) above a centered input. The first keystroke moves to a
//! results split — ranked list left, selected op's detail right.
//!
//! State and render are pure functions over [`ExplorerState`], so `TestBackend` drives the whole
//! surface headlessly; [`run_ops_explorer`] is only the terminal plumbing around them.

use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::pictogram::{Pictogram, PICTO_H, PICTO_W};
use crate::theme::Theme;

/// One input parameter of an operation, as the explorer displays it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamRow {
    pub name: String,
    /// JSON Schema type name (`string`, `integer`, …). Empty when the schema does not say.
    pub ty: String,
    pub description: String,
    pub required: bool,
}

/// One operation, flattened for display. Assembled by the caller; see the module docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpRow {
    pub name: String,
    pub description: String,
    pub params: Vec<ParamRow>,
    pub effects: Vec<flux_spec::Effect>,
    pub risk: flux_spec::Risk,
    pub idempotency: flux_spec::Idempotency,
    /// The evidence-gated tool group, when the op has one. `None` reads as *core*.
    pub group: Option<String>,
    /// Derived display category — the filter facet.
    pub category: String,
    /// Provenance label from the registry (which registrar contributed the op).
    pub source: String,
    pub doc_public_url: String,
    /// The `flux docs` loopback URL. Labelled as needing that server, because it does.
    pub doc_local_url: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpsExplorerOptions {
    pub theme: Theme,
    /// Pictogram animation seed. Fixed in tests; clock-derived in the real command.
    pub seed: u64,
}

/// Which of the two stages the surface is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Start,
    Results,
}

/// Keyboard focus. Typing *always* edits the query, so command keys need their own focus rather
/// than stealing letters from the search box — `q` is a perfectly good query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Input,
    Command,
}

/// What a key press asked the driver to do. Returned rather than performed so the state machine
/// stays pure and testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Quit,
    /// Copy this text to the host clipboard (OSC 52).
    Copy(String),
}

/// The category filter's "no filter" sentinel.
pub(crate) const ALL_CATEGORIES: &str = "all";

pub(crate) struct ExplorerState {
    pub(crate) rows: Vec<OpRow>,
    pub(crate) query: String,
    pub(crate) stage: Stage,
    pub(crate) focus: Focus,
    pub(crate) selected: usize,
    /// Index into [`ExplorerState::categories`]; 0 is [`ALL_CATEGORIES`].
    pub(crate) category: usize,
    pub(crate) categories: Vec<String>,
    pub(crate) theme: Theme,
    pub(crate) pictogram: Pictogram,
    pub(crate) help_open: bool,
    /// Set when a copy happened, so the footer can acknowledge it.
    pub(crate) notice: Option<String>,
}

impl ExplorerState {
    pub(crate) fn new(rows: Vec<OpRow>, options: OpsExplorerOptions) -> Self {
        // Derived from the rows themselves — the explorer never invents a category vocabulary.
        let mut categories: Vec<String> = rows.iter().map(|r| r.category.clone()).collect();
        categories.sort();
        categories.dedup();
        categories.insert(0, ALL_CATEGORIES.to_string());
        ExplorerState {
            rows,
            query: String::new(),
            stage: Stage::Start,
            focus: Focus::Input,
            selected: 0,
            category: 0,
            categories,
            theme: options.theme,
            pictogram: Pictogram::new(options.seed),
            help_open: false,
            notice: None,
        }
    }

    pub(crate) fn active_category(&self) -> &str {
        self.categories
            .get(self.category)
            .map(String::as_str)
            .unwrap_or(ALL_CATEGORIES)
    }

    /// Row indices matching the category filter and the fuzzy query, best first.
    ///
    /// The empty query keeps `specs()` order (name-sorted) rather than falling back to an arbitrary
    /// one, so an operator who clears the box lands somewhere predictable.
    pub(crate) fn filtered(&self) -> Vec<usize> {
        let category = self.active_category();
        let candidates: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| category == ALL_CATEGORIES || r.category == category)
            .map(|(i, _)| i)
            .collect();
        if self.query.is_empty() {
            return candidates;
        }
        let names: Vec<String> = candidates
            .iter()
            .map(|&i| self.rows[i].name.clone())
            .collect();
        crate::fuzzy_rank_indices(&names, &self.query)
            .into_iter()
            .map(|i| candidates[i])
            .collect()
    }

    pub(crate) fn selected_row(&self) -> Option<&OpRow> {
        self.filtered().get(self.selected).map(|&i| &self.rows[i])
    }

    fn clamp_selection(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.filtered().len();
        if len == 0 {
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, len as isize - 1) as usize;
    }

    fn cycle_category(&mut self, delta: isize) {
        let n = self.categories.len() as isize;
        self.category = ((self.category as isize + delta).rem_euclid(n)) as usize;
        self.selected = 0;
    }

    /// The whole key table. Pure: it mutates state and reports what the driver should do.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        self.notice = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Ctrl-C always quits and Ctrl-Y always copies — neither depends on focus, because a user
        // who wants out should not first have to work out which mode they are in.
        if ctrl && matches!(key.code, KeyCode::Char('c')) {
            return Action::Quit;
        }
        if ctrl && matches!(key.code, KeyCode::Char('y')) {
            return self.copy_selected();
        }

        if self.help_open {
            self.help_open = false;
            return Action::None;
        }

        match key.code {
            // Category cycling is focus-independent: it is a filter, not text.
            KeyCode::Tab | KeyCode::Right => {
                self.cycle_category(1);
                return Action::None;
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.cycle_category(-1);
                return Action::None;
            }
            KeyCode::Esc => {
                return match (self.focus, self.stage) {
                    // Esc walks outward one step at a time, and only quits from the start screen.
                    (Focus::Command, _) => {
                        self.focus = Focus::Input;
                        Action::None
                    }
                    (Focus::Input, Stage::Results) => {
                        self.query.clear();
                        self.stage = Stage::Start;
                        self.selected = 0;
                        Action::None
                    }
                    (Focus::Input, Stage::Start) => Action::Quit,
                };
            }
            KeyCode::Up => {
                self.move_selection(-1);
                return Action::None;
            }
            KeyCode::Down => {
                self.move_selection(1);
                return Action::None;
            }
            KeyCode::PageUp => {
                self.move_selection(-10);
                return Action::None;
            }
            KeyCode::PageDown => {
                self.move_selection(10);
                return Action::None;
            }
            _ => {}
        }

        match self.focus {
            Focus::Input => match key.code {
                KeyCode::Enter => {
                    // Enter is the *only* way into command focus, which is what keeps every letter
                    // available to the query.
                    if self.stage == Stage::Results {
                        self.focus = Focus::Command;
                    }
                    Action::None
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    if self.query.is_empty() && self.stage == Stage::Results {
                        self.stage = Stage::Start;
                    }
                    self.clamp_selection();
                    Action::None
                }
                KeyCode::Char(c) => {
                    self.query.push(c);
                    self.stage = Stage::Results;
                    self.selected = 0;
                    Action::None
                }
                _ => Action::None,
            },
            Focus::Command => match key.code {
                KeyCode::Char('j') => {
                    self.move_selection(1);
                    Action::None
                }
                KeyCode::Char('k') => {
                    self.move_selection(-1);
                    Action::None
                }
                KeyCode::Char('y') => self.copy_selected(),
                KeyCode::Char('?') => {
                    self.help_open = true;
                    Action::None
                }
                KeyCode::Char('q') => Action::Quit,
                _ => Action::None,
            },
        }
    }

    /// Bracketed paste appends to the query wholesale — a pasted op name should land in the box,
    /// not be replayed as N keystrokes that each re-sort the list.
    pub(crate) fn on_paste(&mut self, text: &str) {
        self.query.push_str(text.trim());
        if !self.query.is_empty() {
            self.stage = Stage::Results;
        }
        self.selected = 0;
    }

    fn copy_selected(&mut self) -> Action {
        match self.selected_row() {
            Some(row) => {
                let text = row.doc_public_url.clone();
                self.notice = Some(format!("copied {text}"));
                Action::Copy(text)
            }
            None => Action::None,
        }
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Below this width the results split collapses to the list alone; the detail pane needs real
/// columns to be worth its space.
pub(crate) const SPLIT_MIN_COLS: u16 = 70;
/// Below this the surface says so instead of drawing an unreadable smear.
const FLOOR_COLS: u16 = 24;
const FLOOR_ROWS: u16 = 6;
/// Description lines the detail pane shows before deferring to the docs link. See [`render_detail`].
const DETAIL_DESC_LINES: usize = 6;

pub(crate) fn render(f: &mut Frame, state: &mut ExplorerState) {
    let area = f.area();
    let t = state.theme;
    if area.width < FLOOR_COLS || area.height < FLOOR_ROWS {
        // 1×1 has to be safe: `Paragraph` clips, and every helper below assumes room for chrome.
        f.render_widget(
            Paragraph::new("terminal too small").style(t.muted_style()),
            area,
        );
        return;
    }
    match state.stage {
        Stage::Start => render_start(f, area, state),
        Stage::Results => render_results(f, area, state),
    }
}

fn render_start(f: &mut Frame, area: Rect, state: &mut ExplorerState) {
    let t = state.theme;
    // A colorless theme gets a still frame: the shape carries the meaning, and an animation with
    // no color is just flicker.
    let mono = t.accent == ratatui::style::Color::Reset;
    let frame = if mono {
        state.pictogram.static_frame()
    } else {
        state.pictogram.next_frame()
    };

    let mut lines: Vec<Line> = Vec::new();
    let pad = area.width.saturating_sub(PICTO_W as u16) / 2;
    if area.height as usize > PICTO_H + 6 {
        for row in frame.0.iter() {
            let mut spans = vec![Span::raw(" ".repeat(pad as usize))];
            for cell in row.iter() {
                let style = if mono {
                    Style::default()
                } else {
                    Style::default().fg(ratatui::style::Color::Rgb(cell.fg.0, cell.fg.1, cell.fg.2))
                };
                let style = if cell.bold && !mono {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                };
                spans.push(Span::styled(cell.ch.to_string(), style));
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::raw(""));
    }

    let box_w = area.width.min(48);
    let box_pad = area.width.saturating_sub(box_w) / 2;
    let cursor = if mono { "_" } else { "▏" };
    lines.push(Line::from(vec![
        Span::raw(" ".repeat(box_pad as usize)),
        Span::styled("  ", t.muted_style()),
        Span::styled(state.query.clone(), t.text_style()),
        Span::styled(cursor, t.accent_style()),
    ]));
    lines.push(Line::from(vec![
        Span::raw(" ".repeat(box_pad as usize)),
        Span::styled(
            "─".repeat(box_w.saturating_sub(2).max(1) as usize),
            t.muted_style(),
        ),
    ]));
    lines.push(Line::raw(""));
    let hint = format!("type to search {} operations · Esc quits", state.rows.len());
    let hint_pad = area.width.saturating_sub(hint.chars().count() as u16) / 2;
    lines.push(Line::from(vec![
        Span::raw(" ".repeat(hint_pad as usize)),
        Span::styled(hint, t.muted_style()),
    ]));

    // Vertically center what we built, when there is room to.
    let top = area.height.saturating_sub(lines.len() as u16) / 2;
    let mut padded: Vec<Line> = (0..top).map(|_| Line::raw("")).collect();
    padded.extend(lines);
    f.render_widget(Paragraph::new(padded).style(t.base_style()), area);
}

fn render_results(f: &mut Frame, area: Rect, state: &mut ExplorerState) {
    let t = state.theme;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    render_query_bar(f, chunks[0], state);

    let filtered = state.filtered();
    if area.width < SPLIT_MIN_COLS {
        // Single pane: the list alone. The detail is one Esc and a wider terminal away.
        render_list(f, chunks[1], state, &filtered, true);
    } else {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(chunks[1]);
        render_list(f, split[0], state, &filtered, false);
        render_detail(f, split[1], state);
    }
    render_footer(f, chunks[2], state, filtered.len());

    if state.help_open {
        render_help(f, area, t);
    }
}

fn render_query_bar(f: &mut Frame, area: Rect, state: &ExplorerState) {
    let t = state.theme;
    let focus_mark = match state.focus {
        Focus::Input => "search",
        Focus::Command => "command",
    };
    let mut spans = vec![
        Span::styled(" ", t.accent_style()),
        Span::styled(state.query.clone(), t.text_style()),
    ];
    if state.focus == Focus::Input {
        spans.push(Span::styled("▏", t.accent_style()));
    }
    spans.push(Span::styled(format!("  [{focus_mark}]"), t.muted_style()));
    if state.active_category() != ALL_CATEGORIES {
        spans.push(Span::styled(
            format!(" [{}]", state.active_category()),
            t.accent_style(),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(t.base_style()),
        area,
    );
}

/// Risk, as one glyph. A destructive op should be visible in a scanning glance down the list.
fn risk_glyph(risk: flux_spec::Risk) -> &'static str {
    match risk {
        flux_spec::Risk::Low => "·",
        flux_spec::Risk::Medium => "◦",
        flux_spec::Risk::High => "●",
        flux_spec::Risk::Destructive => "✖",
    }
}

fn risk_style(t: &Theme, risk: flux_spec::Risk) -> Style {
    match risk {
        flux_spec::Risk::Low => t.muted_style(),
        flux_spec::Risk::Medium => t.tool_style(),
        flux_spec::Risk::High => t.warn_style(),
        flux_spec::Risk::Destructive => t.err_style(),
    }
}

/// First sentence of a description, for the list's one-line summary.
fn first_sentence(text: &str) -> String {
    let flat = text.replace('\n', " ");
    let trimmed = flat.trim();
    match trimmed.find(". ") {
        Some(i) => trimmed[..=i].to_string(),
        None => trimmed.trim_end_matches('.').to_string(),
    }
}

/// Hand-built lines rather than a ratatui `List`: rows mix four independently styled runs and a
/// width-dependent tail, which a `List` would force through one style per item.
fn render_list(
    f: &mut Frame,
    area: Rect,
    state: &ExplorerState,
    filtered: &[usize],
    single_pane: bool,
) {
    let t = state.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.muted_style())
        .title(Span::styled(" operations ", t.muted_style()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Scroll the window so the selection stays visible without a scrollbar widget.
    let height = inner.height as usize;
    let top = state.selected.saturating_sub(height.saturating_sub(1) / 2);
    let top = top.min(filtered.len().saturating_sub(height));

    let show_category = state.active_category() == ALL_CATEGORIES;
    let mut lines: Vec<Line> = Vec::new();
    for (offset, &idx) in filtered.iter().skip(top).take(height).enumerate() {
        let row = &state.rows[idx];
        let selected = top + offset == state.selected;
        let mut spans = vec![
            Span::styled(if selected { "▸" } else { " " }, t.accent_style()),
            Span::styled(risk_glyph(row.risk), risk_style(&t, row.risk)),
            Span::raw(" "),
            Span::styled(
                row.name.clone(),
                if selected {
                    t.text_style().add_modifier(Modifier::BOLD)
                } else {
                    t.text_style()
                },
            ),
        ];
        // Budget the tail against the actual width so a narrow pane drops the summary rather than
        // wrapping a row that is supposed to be one line.
        let used: usize = 3 + row.name.chars().count();
        let mut left = (inner.width as usize).saturating_sub(used);
        if show_category && left > row.category.chars().count() + 3 {
            spans.push(Span::styled(
                format!("  [{}]", row.category),
                t.muted_style(),
            ));
            left -= row.category.chars().count() + 4;
        }
        if left > 4 {
            let summary = first_sentence(&row.description);
            let clipped: String = summary.chars().take(left.saturating_sub(2)).collect();
            if !clipped.is_empty() {
                spans.push(Span::styled(format!("  {clipped}"), t.muted_style()));
            }
        }
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        lines.push(Line::styled("  no match", t.muted_style()));
    }
    if single_pane {
        // Nothing else will show provenance in this layout, so fold it into the selected row.
        if let Some(row) = state.selected_row() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!(
                    "  {} · {}",
                    row.group.as_deref().unwrap_or("core"),
                    row.source
                ),
                t.muted_style(),
            ));
        }
    }
    f.render_widget(Paragraph::new(lines).style(t.base_style()), inner);
}

fn render_detail(f: &mut Frame, area: Rect, state: &ExplorerState) {
    let t = state.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.muted_style());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some(row) = state.selected_row() else {
        f.render_widget(
            Paragraph::new(Line::styled("  no operation selected", t.muted_style())),
            inner,
        );
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            row.name.clone(),
            t.accent_style().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", row.category), t.muted_style()),
    ]));
    lines.push(Line::raw(""));

    // Pre-wrapped by the markdown renderer, so the Paragraph must NOT wrap again — double wrapping
    // at two different widths is how a code block ends up shredded.
    //
    // Bounded, because several built-in ops carry multi-paragraph descriptions: unbounded, they
    // push risk/effects/source and the doc links off the bottom of an ordinary 24-row terminal,
    // where nothing can scroll them back. The full text is one `flux docs` link away, and iteration
    // 3 renders the documentation section here properly.
    let rendered = crate::markdown::render(&row.description, inner.width);
    let total = rendered.lines.len();
    for line in rendered.lines.into_iter().take(DETAIL_DESC_LINES) {
        lines.push(line);
    }
    if total > DETAIL_DESC_LINES {
        lines.push(Line::styled("  … (see docs)", t.muted_style()));
    }
    lines.push(Line::raw(""));

    if !row.params.is_empty() {
        lines.push(Line::styled("parameters", t.muted_style()));
        // Required first: that is the order someone writing the call needs them in.
        let (mut req, opt): (Vec<_>, Vec<_>) = row.params.iter().partition(|p| p.required);
        req.extend(opt);
        for p in req {
            let mut spans = vec![Span::styled(format!("  {}", p.name), t.text_style())];
            if !p.ty.is_empty() {
                spans.push(Span::styled(format!(" {}", p.ty), t.tool_style()));
            }
            if p.required {
                spans.push(Span::styled(" required", t.warn_style()));
            }
            lines.push(Line::from(spans));
            if !p.description.is_empty() {
                let budget = (inner.width as usize).saturating_sub(6).max(8);
                let desc: String = first_sentence(&p.description)
                    .chars()
                    .take(budget)
                    .collect();
                lines.push(Line::styled(format!("    {desc}"), t.muted_style()));
            }
        }
        lines.push(Line::raw(""));
    }

    let effects = if row.effects.is_empty() {
        "none".to_string()
    } else {
        row.effects
            .iter()
            .map(|e| format!("{e:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let facts = [
        ("risk", format!("{:?}", row.risk).to_lowercase()),
        (
            "idempotency",
            format!("{:?}", row.idempotency).to_lowercase(),
        ),
        ("effects", effects),
        ("group", row.group.clone().unwrap_or_else(|| "core".into())),
        ("source", row.source.clone()),
    ];
    for (k, v) in facts {
        lines.push(Line::from(vec![
            Span::styled(format!("{k:<12}"), t.muted_style()),
            Span::styled(v, t.text_style()),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("docs", t.muted_style()));
    lines.push(Line::styled(
        format!("  {}", row.doc_public_url),
        t.tool_style(),
    ));
    lines.push(Line::styled(
        format!("  {}", row.doc_local_url),
        t.tool_style(),
    ));
    // Its own line, not a trailing tag: a long op name pushes the URL to the pane edge, and a
    // clipped caveat is worse than no caveat — it reads as if the local link just works.
    lines.push(Line::styled("  (needs `flux docs`)", t.muted_style()));
    lines.push(Line::styled("  Ctrl-Y copies the link", t.muted_style()));

    f.render_widget(Paragraph::new(lines).style(t.base_style()), inner);
}

fn render_footer(f: &mut Frame, area: Rect, state: &ExplorerState, matches: usize) {
    let t = state.theme;
    if let Some(notice) = &state.notice {
        f.render_widget(
            Paragraph::new(Line::styled(format!(" {notice}"), t.ok_style())),
            area,
        );
        return;
    }
    let keys = match state.focus {
        Focus::Input => "Enter commands · Tab category · Esc back",
        Focus::Command => "j/k move · y copy · ? help · q quit",
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {matches} match "), t.muted_style()),
            Span::styled(format!("· {keys}"), t.muted_style()),
        ]))
        .style(t.base_style()),
        area,
    );
}

fn render_help(f: &mut Frame, area: Rect, t: Theme) {
    let w = area.width.min(52);
    let h = area.height.min(12);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.accent_style())
        .title(Span::styled(" keys ", t.accent_style()));
    let inner = block.inner(rect);
    f.render_widget(ratatui::widgets::Clear, rect);
    f.render_widget(block, rect);
    let lines = vec![
        Line::styled("  typing always edits the query", t.text_style()),
        Line::styled("  Enter        command focus", t.text_style()),
        Line::styled("  j / k        move selection", t.text_style()),
        Line::styled("  Tab / ← →    cycle category", t.text_style()),
        Line::styled("  y / Ctrl-Y   copy doc link", t.text_style()),
        Line::styled("  Esc          command → search → start", t.text_style()),
        Line::styled("  Ctrl-C       quit from anywhere", t.text_style()),
    ];
    f.render_widget(Paragraph::new(lines).style(t.base_style()), inner);
}

// ── Driver ───────────────────────────────────────────────────────────────────

/// Poll timeout while the start-screen animation is running fast (just after a keystroke).
const FAST_TICK: Duration = Duration::from_millis(55);
/// The slow shimmer the start screen settles into.
const SLOW_TICK: Duration = Duration::from_millis(400);
/// How long the fast window lasts before settling.
const FAST_WINDOW: Duration = Duration::from_secs(4);
/// Results idle with no animation at all — this is only a resize/keypress wait, so it can be long.
const IDLE_TICK: Duration = Duration::from_secs(30);

/// Open the explorer. Blocking, no agent, no tokio — the whole surface is synchronous.
pub fn run_ops_explorer(rows: Vec<OpRow>, options: OpsExplorerOptions) -> Result<()> {
    use std::io::IsTerminal;
    let out = io::stdout();
    if !io::stdin().is_terminal() || !out.is_terminal() {
        anyhow::bail!("flux ops --explore requires a real terminal on stdin and stdout");
    }

    let mut state = ExplorerState::new(rows, options);
    // The shared guard, not a hand-rolled one: it is what restores raw mode, the alternate screen,
    // mouse capture and bracketed paste unconditionally, including on a panic unwind.
    let (mut terminal, mut guard) = crate::terminal_io::TerminalGuard::enter(out)?;
    let started = Instant::now();
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| render(f, &mut state))?;
            // No idle burn: only the start screen has a deadline, and it decays to a slow shimmer.
            let timeout = match state.stage {
                Stage::Start if started.elapsed() < FAST_WINDOW => FAST_TICK,
                Stage::Start => SLOW_TICK,
                Stage::Results => IDLE_TICK,
            };
            if !event::poll(timeout)? {
                continue;
            }
            match event::read()? {
                Event::Key(key) => match state.on_key(key) {
                    Action::Quit => return Ok(()),
                    Action::Copy(text) => {
                        if let Some(seq) = crate::osc52_copy(&text) {
                            use std::io::Write;
                            let mut o = io::stdout();
                            let _ = o.write_all(seq.as_bytes());
                            let _ = o.flush();
                        }
                    }
                    Action::None => {}
                },
                Event::Paste(text) => state.on_paste(&text),
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollDown => state.move_selection(1),
                    MouseEventKind::ScrollUp => state.move_selection(-1),
                    _ => {}
                },
                _ => {}
            }
        }
    })();
    let restored = guard.restore(terminal.backend_mut());
    result.and(restored)
}

#[cfg(test)]
mod tests;
