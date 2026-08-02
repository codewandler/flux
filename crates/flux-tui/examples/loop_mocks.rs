//! A-144 — look at five loop-view layouts drawing the same run, and pick one.
//!
//! ```text
//! cargo run -p flux-tui --example loop_mocks            # interactive explorer
//! cargo run -p flux-tui --example loop_mocks -- --dump  # every mock × case × width, as text
//! ```
//!
//! Interactive keys: `1`–`5` mock · `t l d f` load case · `w n m` viewport ·
//! `←`/`→` cycle mocks · `?` the recommendation · `q` quit.
//!
//! **A-146 — the composed view and its three axes:** `x` switches between the five fixed mocks and
//! the composed view · `[`/`]` the depth limit · `c` condense-completed · `p` the detail pane ·
//! `0` back to the recommended defaults. The three are independent, so every combination is
//! reachable; the header of every drawing says which point in the space it is.
//!
//! Nothing here is wired to a running agent; see `crates/flux-tui/src/loopmock/mod.rs`.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal;
use flux_tui::loopmock::{self, Axes, Viewport, LOAD_CASES, MOCKS};
use flux_tui::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport as TuiViewport};

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--dump") {
        print!("{}", loopmock::snapshot_document());
        return Ok(());
    }
    run()
}

/// What the explorer is currently showing.
struct Pick {
    mock: usize,
    case: usize,
    vp: Viewport,
    help: bool,
    /// A-146: show the composed view rather than one of the five fixed mocks.
    composed: bool,
    /// The point in the axis space the composed view is drawn at.
    axes: Axes,
}

fn run() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut term = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: TuiViewport::Fullscreen,
        },
    )?;
    crossterm::execute!(io::stdout(), terminal::EnterAlternateScreen)?;
    let result = explore(&mut term);
    let _ = crossterm::execute!(io::stdout(), terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    result
}

fn explore(term: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let theme = Theme::default();
    let mut pick = Pick {
        mock: 0,
        case: 0,
        vp: loopmock::WIDE,
        help: false,
        composed: false,
        axes: Axes::DEFAULT,
    };
    loop {
        term.draw(|f| draw(f, &pick, &theme))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('?') => pick.help = !pick.help,
            KeyCode::Char(c @ '1'..='5') => {
                pick.mock = c as usize - '1' as usize;
                pick.composed = false;
            }
            // A-146: the composed view, and one key per axis. Each is settable on its own.
            KeyCode::Char('x') => pick.composed = !pick.composed,
            KeyCode::Char('0') => {
                pick.composed = true;
                pick.axes = Axes::DEFAULT;
            }
            KeyCode::Char(']') => {
                pick.composed = true;
                pick.axes.depth = pick.axes.depth.deeper();
            }
            KeyCode::Char('[') => {
                pick.composed = true;
                pick.axes.depth = pick.axes.depth.shallower();
            }
            KeyCode::Char('c') => {
                pick.composed = true;
                pick.axes.condense = !pick.axes.condense;
            }
            KeyCode::Char('p') => {
                pick.composed = true;
                pick.axes.pane = !pick.axes.pane;
            }
            KeyCode::Right => pick.mock = (pick.mock + 1) % MOCKS.len(),
            KeyCode::Left => pick.mock = (pick.mock + MOCKS.len() - 1) % MOCKS.len(),
            KeyCode::Char('t') => pick.case = 0,
            KeyCode::Char('l') => pick.case = 1,
            KeyCode::Char('d') => pick.case = 2,
            KeyCode::Char('f') => pick.case = 3,
            KeyCode::Char('w') => pick.vp = loopmock::WIDE,
            KeyCode::Char('n') => pick.vp = loopmock::NARROW,
            KeyCode::Char('m') => pick.vp = loopmock::FLOOR,
            _ => {}
        }
    }
}

fn draw(frame: &mut ratatui::Frame, pick: &Pick, theme: &Theme) {
    let mock = MOCKS[pick.mock];
    let case = LOAD_CASES[pick.case];
    let spec = mock.spec();
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(pick.vp.rows as u16 + 2),
            Constraint::Min(3),
        ])
        .split(area);

    let keys = Line::from(vec![
        Span::styled("1-5", theme.accent_style()),
        Span::styled(" mock  ", theme.muted_style()),
        Span::styled("x", theme.accent_style()),
        Span::styled(" axes  ", theme.muted_style()),
        Span::styled("[ ]", theme.accent_style()),
        Span::styled(" depth  ", theme.muted_style()),
        Span::styled("c", theme.accent_style()),
        Span::styled(" condense  ", theme.muted_style()),
        Span::styled("p", theme.accent_style()),
        Span::styled(" pane  ", theme.muted_style()),
        Span::styled("t l d f", theme.accent_style()),
        Span::styled(" case  ", theme.muted_style()),
        Span::styled("w n m", theme.accent_style()),
        Span::styled(" width  ", theme.muted_style()),
        Span::styled("?", theme.accent_style()),
        Span::styled(" rec  ", theme.muted_style()),
        Span::styled("q", theme.accent_style()),
        Span::styled(" quit", theme.muted_style()),
    ]);
    frame.render_widget(Paragraph::new(keys), chunks[0]);

    if pick.help {
        frame.render_widget(
            Paragraph::new(loopmock::RECOMMENDATION)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(" A-144 ")),
            area,
        );
        return;
    }

    let (render, title, mut notes) = if pick.composed {
        let (cols, rows) = pick.axes.floor();
        (
            loopmock::render_axes(pick.axes, case, pick.vp, theme),
            format!(
                " composed · {} · {} · {}×{} ",
                pick.axes.label(),
                case.name(),
                pick.vp.cols,
                pick.vp.rows
            ),
            vec![
                Line::from(vec![
                    Span::styled("axes           ", theme.muted_style()),
                    Span::raw(pick.axes.label()),
                ]),
                Line::from(vec![
                    Span::styled("floor          ", theme.muted_style()),
                    Span::raw(format!(
                        "{cols}×{rows} — the pane's floor travels with the pane, not the layout",
                    )),
                ]),
            ],
        )
    } else {
        (
            loopmock::render(mock, case, pick.vp, theme),
            format!(
                " {} · {} · {}×{} ",
                spec.name,
                case.name(),
                pick.vp.cols,
                pick.vp.rows
            ),
            vec![
                Line::from(vec![
                    Span::styled("optimizes for  ", theme.muted_style()),
                    Span::raw(spec.optimizes_for),
                ]),
                Line::from(vec![
                    Span::styled("gives up       ", theme.muted_style()),
                    Span::raw(spec.gives_up),
                ]),
                Line::from(vec![
                    Span::styled("A-140 pause    ", theme.muted_style()),
                    Span::raw(spec.pause_affordance),
                ]),
                Line::from(vec![
                    Span::styled("A-142 inspect  ", theme.muted_style()),
                    Span::raw(spec.inspection_pane),
                ]),
            ],
        )
    };
    frame.render_widget(
        Paragraph::new(render.lines.clone())
            .block(Block::default().borders(Borders::ALL).title(title)),
        chunks[1],
    );
    for elision in &render.elisions {
        notes.push(Line::from(Span::styled(
            format!("withheld       {} {}", elision.hidden, elision.what),
            theme.warn_style(),
        )));
    }
    notes.push(Line::from(Span::styled(
        format!(
            "drew           {} of {} steps",
            render.steps_drawn(),
            loopmock::fixture(case).step_count()
        ),
        Style::default().add_modifier(Modifier::DIM),
    )));
    frame.render_widget(
        Paragraph::new(notes)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" trade-off ")),
        chunks[2],
    );
}
