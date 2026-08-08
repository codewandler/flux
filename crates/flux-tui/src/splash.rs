//! Animated FLUX startup splash: matrix rain → block-logo reveal → pulsing glow.
//!
//! The animation is a pure sequence of 60×12 [`Cell`] frames driven by an embedded
//! PCG32, so the whole run is a deterministic function of the seed. Two drivers
//! consume it: [`play_blocking`] paints raw ANSI on the alternate screen for the
//! plain-terminal REPL, and [`splash_intro`] renders through ratatui for `flux tui`.
//! Any key dismisses; after ~1.9 s of glow the splash auto-dismisses. Decorative
//! only — every driver error is a silent skip, never a startup failure.

use std::io::{self, Write as _};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::{cursor, terminal};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};

use super::{flag_on, Tui};

pub const CANVAS_W: usize = 60;
pub const CANVAS_H: usize = 12;
/// Box = canvas + rounded border + one column of horizontal padding per side.
pub const BOX_W: u16 = CANVAS_W as u16 + 4;
pub const BOX_H: u16 = CANVAS_H as u16 + 2;
/// Terminals smaller than the box skip the splash entirely.
pub const MIN_COLS: u16 = BOX_W;
pub const MIN_ROWS: u16 = BOX_H;

const RAIN_FRAMES: u32 = 35;
const REVEAL_FRAMES: u32 = 50;
/// Sine period of the glow shimmer, in frames.
const GLOW_PERIOD: u32 = 80;
/// Glow frames before auto-dismiss (~1.9 s at [`TICK_MS`]).
const GLOW_FRAMES: u32 = 34;
pub const TICK_MS: u64 = 55;

const TAGLINE: &str = "[ deterministic agent platform ]";
const HINT: &str = " any key to skip";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

const WHITE: Rgb = Rgb(0xff, 0xff, 0xff);
const CYAN0: Rgb = Rgb(0x00, 0xd7, 0xff);
const CYAN1: Rgb = Rgb(0x00, 0xaf, 0xff);
const CYAN2: Rgb = Rgb(0x00, 0x87, 0xd7);
/// Border tint.
const CYAN3: Rgb = Rgb(0x00, 0x5f, 0x87);
const CYAN4: Rgb = Rgb(0x00, 0x3f, 0x5c);
const DIM: Rgb = Rgb(0x00, 0x44, 0x66);
const TAG_DIM: Rgb = Rgb(0x00, 0x66, 0x88);

pub(crate) fn lerp(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let mix = |x: u8, y: u8| (f64::from(x) + t * (f64::from(y) - f64::from(x))) as u8;
    Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

// 5-row × 9-col block glyphs, same font family as the ancestor CODER splash.
const LETTER_F: [&str; 5] = [
    "███████╗ ",
    "██╔════╝ ",
    "█████╗   ",
    "██╔══╝   ",
    "██║      ",
];
const LETTER_L: [&str; 5] = [
    "██╗      ",
    "██║      ",
    "██║      ",
    "██║      ",
    "███████╗ ",
];
const LETTER_U: [&str; 5] = [
    "██╗   ██╗",
    "██║   ██║",
    "██║   ██║",
    "██║   ██║",
    "╚██████╔╝",
];
const LETTER_X: [&str; 5] = [
    "██╗  ██╗ ",
    "╚██╗██╔╝ ",
    " ╚███╔╝  ",
    " ██╔██╗  ",
    "██╔╝ ██╗ ",
];
const LETTERS: [[&str; 5]; 4] = [LETTER_F, LETTER_L, LETTER_U, LETTER_X];
const LOGO_H: usize = 5;

fn logo_rows() -> [Vec<char>; 5] {
    std::array::from_fn(|row| LETTERS.iter().flat_map(|l| l[row].chars()).collect())
}

const MATRIX_CHARS: &str = "ﾊﾐﾋｰｳｼﾅﾓﾆｻﾜﾂｵﾘｱﾎﾃﾏｹﾒｴｶｷﾑﾕﾗｾﾈｽﾀﾇﾍ01";

// ── PCG32 ────────────────────────────────────────────────────────────────────
// Hand-rolled (PCG-XSH-RR) so the animation stays dependency-free and every
// frame is reproducible from the seed.

pub(crate) struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    fn with_stream(seed: u64, seq: u64) -> Self {
        let mut rng = Pcg32 {
            state: 0,
            inc: (seq << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    pub(crate) fn new(seed: u64) -> Self {
        Self::with_stream(seed, 0xda3e_39cb_94b9_5bdb)
    }

    pub(crate) fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        xorshifted.rotate_right((old >> 59) as u32)
    }

    pub(crate) fn gen_range(&mut self, n: usize) -> usize {
        ((u64::from(self.next_u32()) * n as u64) >> 32) as usize
    }

    pub(crate) fn gen_f64(&mut self) -> f64 {
        f64::from(self.next_u32()) / 4_294_967_296.0
    }
}

// ── Animation core ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bold: bool,
}

const BLANK: Cell = Cell {
    ch: ' ',
    fg: Rgb(0, 0, 0),
    bold: false,
};

/// One composed 60×12 frame; background is always black.
pub struct Frame(pub [[Cell; CANVAS_W]; CANVAS_H]);

impl Frame {
    fn set(&mut self, x: isize, y: isize, ch: char, fg: Rgb, bold: bool) {
        if (0..CANVAS_W as isize).contains(&x) && (0..CANVAS_H as isize).contains(&y) {
            self.0[y as usize][x as usize] = Cell { ch, fg, bold };
        }
    }
}

struct Drop {
    x: usize,
    y: f64,
    speed: f64,
    chars: Vec<char>,
}

impl Drop {
    fn new(rng: &mut Pcg32, matrix: &[char], x: usize, max_y: usize) -> Self {
        let len = 4 + rng.gen_range(7);
        let chars = (0..len)
            .map(|_| matrix[rng.gen_range(matrix.len())])
            .collect();
        Drop {
            x,
            y: -(rng.gen_range(max_y + 1) as f64),
            speed: 0.25 + rng.gen_f64() * 0.45,
            chars,
        }
    }

    fn advance(&mut self, rng: &mut Pcg32, matrix: &[char], max_y: usize) {
        self.y += self.speed;
        if rng.gen_range(4) == 0 {
            let i = rng.gen_range(self.chars.len());
            self.chars[i] = matrix[rng.gen_range(matrix.len())];
        }
        if self.y as isize - self.chars.len() as isize > max_y as isize {
            *self = Drop::new(rng, matrix, self.x, max_y);
        }
    }

    fn draw(&self, rng: &mut Pcg32, frame: &mut Frame, alpha: f64) {
        if alpha <= 0.0 {
            return;
        }
        let head = self.y as isize;
        let len = self.chars.len();
        for k in 0..len {
            let row = head - k as isize;
            if !(0..CANVAS_H as isize).contains(&row) {
                continue;
            }
            // Stochastic fade: cells drop out as alpha shrinks during the reveal.
            if alpha < 1.0 && rng.gen_f64() > alpha {
                continue;
            }
            let fg = match k {
                0 => WHITE,
                1 => CYAN0,
                _ if k < len / 2 => CYAN2,
                _ => CYAN4,
            };
            frame.set(self.x as isize, row, self.chars[k], fg, false);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Rain,
    Reveal,
    Glow,
}

pub struct Splash {
    rng: Pcg32,
    matrix: Vec<char>,
    logo: [Vec<char>; 5],
    drops: Vec<Drop>,
    phase: Phase,
    frame: u32,
}

impl Splash {
    pub fn new(seed: u64) -> Self {
        let mut rng = Pcg32::new(seed);
        let matrix: Vec<char> = MATRIX_CHARS.chars().collect();
        let drops = (0..CANVAS_W / 2)
            .map(|i| Drop::new(&mut rng, &matrix, i * 2, CANVAS_H))
            .collect();
        Splash {
            rng,
            matrix,
            logo: logo_rows(),
            drops,
            phase: Phase::Rain,
            frame: 0,
        }
    }

    /// A seed for production callers; tests pass fixed seeds instead.
    pub fn clock_seed() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5eed)
    }

    /// True once the glow ran its course; drivers then dismiss on their own.
    pub fn finished(&self) -> bool {
        self.phase == Phase::Glow && self.frame >= GLOW_FRAMES
    }

    /// Advance one tick and compose the frame.
    pub fn next_frame(&mut self) -> Frame {
        self.frame += 1;
        match self.phase {
            Phase::Rain if self.frame >= RAIN_FRAMES => {
                self.phase = Phase::Reveal;
                self.frame = 0;
            }
            Phase::Reveal if self.frame >= REVEAL_FRAMES => {
                self.phase = Phase::Glow;
                self.frame = 0;
            }
            _ => {}
        }

        let mut f = Frame([[BLANK; CANVAS_W]; CANVAS_H]);
        let (progress, rain_alpha) = match self.phase {
            Phase::Rain => (0.0, 1.0),
            Phase::Reveal => {
                let p = f64::from(self.frame) / f64::from(REVEAL_FRAMES);
                (p, (1.0 - p * 1.6).max(0.0))
            }
            Phase::Glow => (1.0, 0.0),
        };

        let rng = &mut self.rng;
        for d in &mut self.drops {
            d.advance(rng, &self.matrix, CANVAS_H);
            d.draw(rng, &mut f, rain_alpha);
        }

        let logo_w = self.logo[0].len();
        let start_x = (CANVAS_W - logo_w) / 2;
        let start_y = (CANVAS_H - LOGO_H) / 2;
        let t = f64::from(self.frame) / f64::from(GLOW_PERIOD) * std::f64::consts::TAU;

        for li in 0..LOGO_H {
            for ci in 0..logo_w {
                let ch = self.logo[li][ci];
                if ch == ' ' {
                    continue;
                }
                let (lx, ly) = ((start_x + ci) as isize, (start_y + li) as isize);
                match self.phase {
                    Phase::Rain => {}
                    Phase::Reveal => {
                        // Left-to-right stochastic wipe; unrevealed cells flicker
                        // as dark katakana, "scrambling into shape".
                        let threshold = (progress * 1.4 - ci as f64 / logo_w as f64 * 0.5).max(0.0);
                        if rng.gen_f64() > threshold {
                            if rng.gen_f64() < 0.35 {
                                let g = self.matrix[rng.gen_range(self.matrix.len())];
                                f.set(lx, ly, g, CYAN4, false);
                            }
                            continue;
                        }
                        let wave = ((t + ci as f64 * 0.25 + li as f64 * 0.4).sin() + 1.0) / 2.0;
                        f.set(lx, ly, ch, lerp(CYAN2, CYAN0, wave), true);
                    }
                    Phase::Glow => {
                        let wave =
                            ((t * 2.0 + ci as f64 * 0.22 + li as f64 * 0.5).sin() + 1.0) / 2.0;
                        let mut fg = lerp(CYAN1, CYAN0, wave);
                        if rng.gen_f64() < 0.012 {
                            fg = WHITE;
                        }
                        f.set(lx, ly, ch, fg, true);
                    }
                }
            }
        }

        if self.phase == Phase::Glow {
            let tag_len = TAGLINE.chars().count();
            let tag_y = (start_y + LOGO_H + 1) as isize;
            let tag_x = (CANVAS_W - tag_len) / 2;
            let tag_progress = (f64::from(self.frame) / f64::from(GLOW_PERIOD) * 4.0).min(1.0);
            for (i, ch) in TAGLINE.chars().enumerate() {
                if rng.gen_f64() > tag_progress {
                    continue;
                }
                let wave = ((t * 1.5 + i as f64 * 0.18).sin() + 1.0) / 2.0;
                f.set(
                    (tag_x + i) as isize,
                    tag_y,
                    ch,
                    lerp(TAG_DIM, CYAN2, wave),
                    false,
                );
            }
            let cursor_x = tag_x + tag_len + 1;
            if (self.frame / 9).is_multiple_of(2) && cursor_x < CANVAS_W {
                f.set(cursor_x as isize, tag_y, '█', CYAN0, false);
            }
            for (i, ch) in HINT.chars().enumerate() {
                f.set(i as isize, CANVAS_H as isize - 1, ch, DIM, false);
            }
        }

        f
    }
}

// ── Raw ANSI driver (plain-terminal REPL) ────────────────────────────────────

/// Raw mode + alternate screen + hidden cursor, restored unconditionally on drop
/// (panic-safe), mirroring `terminal_io::TerminalGuard`.
struct RawGuard {
    raw: bool,
    alt: bool,
    hidden: bool,
}

impl RawGuard {
    fn enter(out: &mut io::Stdout) -> io::Result<Self> {
        let mut guard = RawGuard {
            raw: false,
            alt: false,
            hidden: false,
        };
        terminal::enable_raw_mode()?;
        guard.raw = true;
        crossterm::execute!(out, terminal::EnterAlternateScreen)?;
        guard.alt = true;
        crossterm::execute!(out, cursor::Hide)?;
        guard.hidden = true;
        Ok(guard)
    }
}

impl std::ops::Drop for RawGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        if self.hidden {
            let _ = crossterm::execute!(out, cursor::Show);
        }
        if self.alt {
            let _ = crossterm::execute!(out, terminal::LeaveAlternateScreen);
        }
        if self.raw {
            let _ = terminal::disable_raw_mode();
        }
    }
}

/// Play the splash on the alternate screen, blocking until a key press, an
/// undersized resize, or the glow finishing. The caller gates on TTY/color/size;
/// this re-checks size defensively and treats every error as "skip".
pub fn play_blocking(seed: u64) -> io::Result<()> {
    let (mut cols, mut rows) = terminal::size()?;
    if cols < MIN_COLS || rows < MIN_ROWS {
        return Ok(());
    }
    let mut out = io::stdout();
    let guard = RawGuard::enter(&mut out)?;
    crossterm::execute!(out, terminal::Clear(terminal::ClearType::All))?;

    let mut splash = Splash::new(seed);
    let tick = Duration::from_millis(TICK_MS);
    let mut next_tick = Instant::now();
    'animation: loop {
        if Instant::now() >= next_tick {
            let frame = splash.next_frame();
            paint(&mut out, &frame, cols, rows)?;
            if splash.finished() {
                break;
            }
            next_tick += tick;
            let now = Instant::now();
            if next_tick < now {
                next_tick = now;
            }
        }
        let timeout = next_tick.saturating_duration_since(Instant::now());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => break 'animation,
                Event::Resize(w, h) => {
                    if w < MIN_COLS || h < MIN_ROWS {
                        break 'animation;
                    }
                    cols = w;
                    rows = h;
                    crossterm::execute!(out, terminal::Clear(terminal::ClearType::All))?;
                }
                _ => {}
            }
        }
    }

    // Drain whatever the skip keystroke buffered so nothing leaks into reedline.
    while event::poll(Duration::ZERO)? {
        let _ = event::read()?;
    }
    drop(guard);
    Ok(())
}

/// Base style for every content run: reset, then black background.
const BASE: &str = "\x1b[0m\x1b[48;2;0;0;0m";

fn sgr(fg: Rgb, bold: bool) -> String {
    format!(
        "{BASE}{}\x1b[38;2;{};{};{}m",
        if bold { "\x1b[1m" } else { "" },
        fg.0,
        fg.1,
        fg.2
    )
}

fn push_move(buf: &mut String, col: u16, row: u16) {
    use std::fmt::Write as _;
    let _ = write!(buf, "\x1b[{};{}H", row + 1, col + 1);
}

/// One full repaint of the bordered box, centered; same-style cell runs share one
/// SGR sequence. Returns the ANSI string via `out` (extracted for testability).
fn compose(frame: &Frame, cols: u16, rows: u16) -> String {
    let left = (cols - BOX_W) / 2;
    let top = (rows - BOX_H) / 2;
    let mut buf = String::with_capacity(16 * 1024);
    let border = sgr(CYAN3, false);

    push_move(&mut buf, left, top);
    buf.push_str(&border);
    buf.push('╭');
    for _ in 0..BOX_W - 2 {
        buf.push('─');
    }
    buf.push('╮');

    for (y, row) in frame.0.iter().enumerate() {
        push_move(&mut buf, left, top + 1 + y as u16);
        buf.push_str(&border);
        buf.push('│');
        buf.push_str(BASE);
        buf.push(' ');
        let mut last: Option<(Rgb, bool)> = None;
        for cell in row {
            if cell.ch == ' ' {
                if last.is_some() {
                    buf.push_str(BASE);
                    last = None;
                }
                buf.push(' ');
            } else {
                let key = Some((cell.fg, cell.bold));
                if key != last {
                    buf.push_str(&sgr(cell.fg, cell.bold));
                    last = key;
                }
                buf.push(cell.ch);
            }
        }
        if last.is_some() {
            buf.push_str(BASE);
        }
        buf.push(' ');
        buf.push_str(&border);
        buf.push('│');
    }

    push_move(&mut buf, left, top + 1 + CANVAS_H as u16);
    buf.push_str(&border);
    buf.push('╰');
    for _ in 0..BOX_W - 2 {
        buf.push('─');
    }
    buf.push('╯');
    buf.push_str("\x1b[0m");
    buf
}

fn paint(out: &mut io::Stdout, frame: &Frame, cols: u16, rows: u16) -> io::Result<()> {
    out.write_all(compose(frame, cols, rows).as_bytes())?;
    out.flush()
}

// ── Ratatui driver (`flux tui` opening state) ────────────────────────────────

/// Play the splash through ratatui frames before the chat event loop starts.
/// Assumes `TerminalGuard::enter` is already active (raw mode, alt screen).
/// Skipped under `NO_COLOR`, `FLUX_NO_SPLASH`, or an undersized terminal.
pub(super) fn splash_intro(terminal: &mut Tui) -> anyhow::Result<()> {
    if std::env::var_os("NO_COLOR").is_some()
        || std::env::var("FLUX_NO_SPLASH").is_ok_and(|v| flag_on(&v))
    {
        return Ok(());
    }
    let size = terminal.size()?;
    if size.width < MIN_COLS || size.height < MIN_ROWS {
        return Ok(());
    }

    let mut splash = Splash::new(Splash::clock_seed());
    let tick = Duration::from_millis(TICK_MS);
    let mut next_tick = Instant::now();
    loop {
        if Instant::now() >= next_tick {
            let frame = splash.next_frame();
            terminal.draw(|f| render_splash(f, &frame))?;
            if splash.finished() {
                break;
            }
            next_tick += tick;
            let now = Instant::now();
            if next_tick < now {
                next_tick = now;
            }
        }
        let timeout = next_tick.saturating_duration_since(Instant::now());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => break,
                Event::Resize(w, h) if w < MIN_COLS || h < MIN_ROWS => break,
                _ => {}
            }
        }
    }
    // Force a full first repaint of the chat view.
    terminal.clear()?;
    Ok(())
}

fn render_splash(f: &mut ratatui::Frame, frame: &Frame) {
    let area = f.area();
    let w = MIN_COLS.min(area.width);
    let h = MIN_ROWS.min(area.height);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(
        Paragraph::new(splash_lines(frame)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(rgb_color(CYAN3)).bg(Color::Black))
                .style(Style::default().bg(Color::Black))
                .padding(Padding::horizontal(1)),
        ),
        rect,
    );
}

fn rgb_color(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Cells → ratatui lines, merging same-style runs into one span each.
fn splash_lines(frame: &Frame) -> Vec<Line<'static>> {
    frame
        .0
        .iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut run = String::new();
            let mut last: Option<(Rgb, bool)> = None;
            let flush = |spans: &mut Vec<Span<'static>>, run: &mut String, key| {
                if run.is_empty() {
                    return;
                }
                let style = match key {
                    None => Style::default().bg(Color::Black),
                    Some((fg, bold)) => {
                        let s = Style::default().fg(rgb_color(fg)).bg(Color::Black);
                        if bold {
                            s.add_modifier(Modifier::BOLD)
                        } else {
                            s
                        }
                    }
                };
                spans.push(Span::styled(std::mem::take(run), style));
            };
            for cell in row {
                let key = (cell.ch != ' ').then_some((cell.fg, cell.bold));
                if key != last {
                    flush(&mut spans, &mut run, last);
                    last = key;
                }
                run.push(cell.ch);
            }
            flush(&mut spans, &mut run, last);
            Line::from(spans)
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcg32_matches_reference_sequence() {
        // Known-answer values from the canonical pcg32-demo (seed 42, stream 54).
        let mut rng = Pcg32::with_stream(42, 54);
        let got: Vec<u32> = (0..6).map(|_| rng.next_u32()).collect();
        assert_eq!(
            got,
            vec![
                0xa15c_02b7,
                0x7b47_f409,
                0xba1d_3330,
                0x83d2_f293,
                0xbfa4_784b,
                0xcbed_606e
            ]
        );
    }

    #[test]
    fn glyphs_are_nine_columns_and_logo_fits() {
        for letter in LETTERS {
            for row in letter {
                assert_eq!(row.chars().count(), 9, "glyph row {row:?}");
            }
        }
        let rows = logo_rows();
        for row in &rows {
            assert_eq!(row.len(), 36);
        }
        assert!(rows[0].len() < CANVAS_W);
        const _: () = assert!(LOGO_H + 1 + 1 < CANVAS_H, "tagline row must fit the canvas");
        assert!(TAGLINE.chars().count() < CANVAS_W);
        assert!(HINT.chars().count() < CANVAS_W);
    }

    #[test]
    fn phases_advance_and_finish_deterministically() {
        let mut splash = Splash::new(42);
        // Frames 1..RAIN_FRAMES-1 are rain; RAIN_FRAMES-th tick flips to reveal.
        for _ in 0..RAIN_FRAMES - 1 {
            splash.next_frame();
            assert_eq!(splash.phase, Phase::Rain);
        }
        splash.next_frame();
        assert_eq!(splash.phase, Phase::Reveal);
        for _ in 0..REVEAL_FRAMES {
            assert_eq!(splash.phase, Phase::Reveal);
            splash.next_frame();
        }
        assert_eq!(splash.phase, Phase::Glow);
        assert!(!splash.finished());
        while !splash.finished() {
            splash.next_frame();
        }
        assert_eq!(splash.frame, GLOW_FRAMES);
    }

    #[test]
    fn frames_are_deterministic_for_a_seed() {
        let run = |seed| {
            let mut splash = Splash::new(seed);
            let mut cells = Vec::new();
            for _ in 0..120 {
                let f = splash.next_frame();
                cells.extend(f.0.iter().flatten().copied());
            }
            cells
        };
        assert_eq!(run(7), run(7));
        assert_ne!(run(7), run(8));
    }

    #[test]
    fn glow_frame_shows_full_bold_logo() {
        let mut splash = Splash::new(1);
        while !splash.finished() {
            splash.next_frame();
        }
        let frame = splash.next_frame();
        let rows = logo_rows();
        let start_x = (CANVAS_W - rows[0].len()) / 2;
        let start_y = (CANVAS_H - LOGO_H) / 2;
        for (li, row) in rows.iter().enumerate() {
            for (ci, &ch) in row.iter().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let cell = frame.0[start_y + li][start_x + ci];
                assert_eq!(cell.ch, ch);
                assert!(cell.bold);
                assert!(
                    cell.fg == WHITE || (cell.fg.0 == 0 && cell.fg.2 == 0xff),
                    "glow cell must be white or a cyan blend, got {:?}",
                    cell.fg
                );
            }
        }
    }

    #[test]
    fn compose_coalesces_runs_and_frames_the_box() {
        let mut splash = Splash::new(3);
        let frame = splash.next_frame();
        let ansi = compose(&frame, 80, 24);
        assert!(ansi.contains('╭') && ansi.contains('╯'));
        // Border color present, black background enforced, and a trailing reset.
        assert!(ansi.contains("\x1b[38;2;0;95;135m"));
        assert!(ansi.contains("\x1b[48;2;0;0;0m"));
        assert!(ansi.ends_with("\x1b[0m"));
        // A run of blanks must not repeat the base SGR per cell.
        assert!(!ansi.contains(&format!("{BASE} {BASE} ")));
    }

    #[test]
    fn splash_lines_cover_the_full_canvas_width() {
        let mut splash = Splash::new(9);
        // Deep into glow: logo + tagline + hint all present.
        let mut frame = splash.next_frame();
        for _ in 0..100 {
            frame = splash.next_frame();
        }
        let lines = splash_lines(&frame);
        assert_eq!(lines.len(), CANVAS_H);
        for line in &lines {
            let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(width, CANVAS_W);
        }
    }
}
