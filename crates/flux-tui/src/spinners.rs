//! Full-width animated spinner effects (a Rust port of the codewandler/spinners
//! Go catalog, curated to eight). Each effect is a pure `fn(tick, width) -> Vec<Cell>`
//! returning exactly `width.max(2)` single-column cells, so frames are deterministic
//! and renderer-agnostic: [`ansi_line`] emits truecolor SGR for the plain CLI,
//! [`cells_to_spans`] feeds the ratatui footer.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::splash::{Cell, Rgb};

/// All effects run at 16 fps in the source catalog.
pub const FPS_MS: u64 = 62;

pub struct Spinner {
    pub name: &'static str,
    pub frame: fn(tick: usize, width: usize) -> Vec<Cell>,
}

/// Ordered catalog; [`by_round`] cycles through it starting here.
pub const ALL: &[Spinner] = &[
    Spinner {
        name: "Knight Rider",
        frame: knight_rider,
    },
    Spinner {
        name: "Comet",
        frame: comet,
    },
    Spinner {
        name: "Tidal Wave",
        frame: tidal_wave,
    },
    Spinner {
        name: "Matrix",
        frame: matrix,
    },
    Spinner {
        name: "Equalizer",
        frame: equalizer,
    },
    Spinner {
        name: "Aurora",
        frame: aurora,
    },
    Spinner {
        name: "Thunderstrike",
        frame: thunderstrike,
    },
    Spinner {
        name: "Binary Rain",
        frame: binary_rain,
    },
];

/// The effect for the `n`-th round — flai-style cycling: every model round-trip
/// walks one step through the catalog.
pub fn by_round(n: usize) -> &'static Spinner {
    &ALL[n % ALL.len()]
}

const fn rgb(hex: u32) -> Rgb {
    Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const fn cell(ch: char, fg: Rgb, bold: bool) -> Cell {
    Cell { ch, fg, bold }
}

/// A transparent gap cell (rendered as plain space, no style run).
const GAP: Cell = cell(' ', Rgb(0, 0, 0), false);

// ── Effects (frame math transliterated 1:1 from catalog.go) ──────────────────

fn knight_rider(tick: usize, width: usize) -> Vec<Cell> {
    const HEAD: Rgb = rgb(0x60A5FA);
    const H1: Rgb = rgb(0x1D4ED8);
    const H2: Rgb = rgb(0x1E3A5F);
    const DARK: Rgb = rgb(0x0F172A);
    let w = width.max(2);
    let total = (w - 1) * 2;
    let mut pos = tick % total;
    if pos >= w {
        pos = total - pos;
    }
    (0..w)
        .map(|i| match i.abs_diff(pos) {
            0 => cell('█', HEAD, true),
            1 => cell('▓', H1, false),
            2 => cell('▒', H2, false),
            _ => cell('░', DARK, false),
        })
        .collect()
}

fn comet(tick: usize, width: usize) -> Vec<Cell> {
    const HEADS: [Rgb; 5] = [
        rgb(0xFFFFFF),
        rgb(0xE0F2FE),
        rgb(0xBAE6FD),
        rgb(0x7DD3FC),
        rgb(0x38BDF8),
    ];
    const TRAIL: [Rgb; 5] = [
        rgb(0x0EA5E9),
        rgb(0x0284C7),
        rgb(0x0369A1),
        rgb(0x075985),
        rgb(0x0C4A6E),
    ];
    const TAIL_CHARS: [char; 6] = ['█', '▓', '▒', '░', '·', ' '];
    let w = width.max(2);
    let tail_len = (w / 3).clamp(2, 6);
    let pos = tick % (w + tail_len);
    let mut cells = vec![GAP; w];
    for j in 0..=tail_len {
        let Some(col) = pos.checked_sub(j).filter(|&c| c < w) else {
            continue;
        };
        let ch = TAIL_CHARS[j.min(TAIL_CHARS.len() - 1)];
        cells[col] = if j == 0 {
            cell(ch, HEADS[tick % HEADS.len()], true)
        } else {
            cell(ch, TRAIL[(j - 1).min(TRAIL.len() - 1)], false)
        };
    }
    cells
}

fn tidal_wave(tick: usize, width: usize) -> Vec<Cell> {
    const STYLES: [Rgb; 8] = [
        rgb(0x0C4A6E),
        rgb(0x075985),
        rgb(0x0369A1),
        rgb(0x0284C7),
        rgb(0x0EA5E9),
        rgb(0x38BDF8),
        rgb(0x7DD3FC),
        rgb(0xBAE6FD),
    ];
    const WAVE: [char; 9] = ['▁', '▂', '▄', '▆', '█', '▆', '▄', '▂', '▁'];
    (0..width.max(2))
        .map(|i| {
            let shift = i as isize - tick as isize;
            let wave_pos = shift.rem_euclid(WAVE.len() as isize) as usize;
            let color_pos = shift.rem_euclid(STYLES.len() as isize) as usize;
            cell(WAVE[wave_pos], STYLES[color_pos], false)
        })
        .collect()
}

fn matrix(tick: usize, width: usize) -> Vec<Cell> {
    const STYLES: [Rgb; 6] = [
        rgb(0x14532D),
        rgb(0x166534),
        rgb(0x15803D),
        rgb(0x16A34A),
        rgb(0x22C55E),
        rgb(0x4ADE80),
    ];
    const CHARS: [char; 12] = ['0', '1', 'ﾊ', 'ﾐ', 'ﾋ', 'ｰ', 'ｳ', 'ｼ', 'ﾅ', 'ﾓ', 'ﾆ', 'ｻ'];
    (0..width.max(2))
        .map(|i| {
            cell(
                CHARS[(tick * 3 + i * 7) % CHARS.len()],
                STYLES[(tick + i) % STYLES.len()],
                false,
            )
        })
        .collect()
}

fn equalizer(tick: usize, width: usize) -> Vec<Cell> {
    const STYLES: [Rgb; 5] = [
        rgb(0x6D28D9),
        rgb(0x7C3AED),
        rgb(0x8B5CF6),
        rgb(0xA78BFA),
        rgb(0xC4B5FD),
    ];
    const HEIGHTS: [char; 4] = ['▁', '▃', '▅', '▇'];
    let nh = HEIGHTS.len();
    (0..width.max(2))
        .map(|i| {
            let phase = (i * 3) % 7;
            let mut h = (tick + phase) % (nh * 2);
            if h >= nh {
                h = nh * 2 - 1 - h;
            }
            cell(HEIGHTS[h], STYLES[i % STYLES.len()], false)
        })
        .collect()
}

fn aurora(tick: usize, width: usize) -> Vec<Cell> {
    const STYLES: [Rgb; 12] = [
        rgb(0x042F2E),
        rgb(0x065F46),
        rgb(0x047857),
        rgb(0x059669),
        rgb(0x34D399),
        rgb(0x6EE7B7),
        rgb(0xA7F3D0),
        rgb(0x6EE7B7),
        rgb(0x34D399),
        rgb(0x059669),
        rgb(0x047857),
        rgb(0x065F46),
    ];
    const CHARS: [char; 7] = ['░', '▒', '▓', '█', '▓', '▒', '░'];
    (0..width.max(2))
        .map(|i| {
            cell(
                CHARS[(tick + i) % CHARS.len()],
                STYLES[(tick + i * 2) % STYLES.len()],
                false,
            )
        })
        .collect()
}

fn thunderstrike(tick: usize, width: usize) -> Vec<Cell> {
    const FLASH_BOLD: Rgb = rgb(0xFFFBEB);
    const FLASH: Rgb = rgb(0xFBBF24);
    const AFTER: [Rgb; 4] = [rgb(0x7C3AED), rgb(0x6D28D9), rgb(0x4C1D95), rgb(0x2E1065)];
    const DARK: Rgb = rgb(0x0F172A);
    const STRIKE_PERIOD: usize = 14;
    (0..width.max(2))
        .map(|i| {
            let offset = (i * 5) % STRIKE_PERIOD;
            let phase = (tick + offset) % STRIKE_PERIOD;
            match phase {
                0..=1 => cell('█', FLASH_BOLD, true),
                2 => cell('▇', FLASH, false),
                3..=6 => cell('▁', AFTER[(phase - 3).min(AFTER.len() - 1)], false),
                _ => cell('░', DARK, false),
            }
        })
        .collect()
}

fn binary_rain(tick: usize, width: usize) -> Vec<Cell> {
    const STYLES: [Rgb; 6] = [
        rgb(0x14532D),
        rgb(0x15803D),
        rgb(0x16A34A),
        rgb(0x22C55E),
        rgb(0x4ADE80),
        rgb(0x86EFAC),
    ];
    const SPEEDS: [usize; 15] = [3, 5, 2, 7, 4, 6, 3, 5, 2, 4, 7, 3, 6, 2, 5];
    (0..width.max(2))
        .map(|i| {
            let speed = SPEEDS[i % SPEEDS.len()];
            let col_tick = tick * speed;
            let digit = if (col_tick + i) % 2 == 0 { '1' } else { '0' };
            cell(digit, STYLES[(col_tick / speed + i) % STYLES.len()], false)
        })
        .collect()
}

// ── Renderers ────────────────────────────────────────────────────────────────

/// Cells → one truecolor ANSI string, exactly `cells.len()` visible columns,
/// consecutive same-style cells sharing one SGR run, single trailing reset.
pub fn ansi_line(cells: &[Cell]) -> String {
    let mut out = String::with_capacity(cells.len() * 16);
    let mut last: Option<(Rgb, bool)> = None;
    for c in cells {
        if c.ch == ' ' {
            if last.is_some() {
                out.push_str("\x1b[0m");
                last = None;
            }
            out.push(' ');
            continue;
        }
        let key = Some((c.fg, c.bold));
        if key != last {
            let Rgb(r, g, b) = c.fg;
            let bold = if c.bold { "1;" } else { "" };
            out.push_str(&format!("\x1b[0m\x1b[{bold}38;2;{r};{g};{b}m"));
            last = key;
        }
        out.push(c.ch);
    }
    out.push_str("\x1b[0m");
    out
}

/// Cells → ratatui spans (same run merging, no background so the footer's own
/// background shows through).
pub fn cells_to_spans(cells: &[Cell]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut last: Option<(Rgb, bool)> = None;
    let flush = |spans: &mut Vec<Span<'static>>, run: &mut String, key: Option<(Rgb, bool)>| {
        if run.is_empty() {
            return;
        }
        let style = match key {
            None => Style::default(),
            Some((Rgb(r, g, b), bold)) => {
                let s = Style::default().fg(Color::Rgb(r, g, b));
                if bold {
                    s.add_modifier(Modifier::BOLD)
                } else {
                    s
                }
            }
        };
        spans.push(Span::styled(std::mem::take(run), style));
    };
    for c in cells {
        let key = (c.ch != ' ').then_some((c.fg, c.bold));
        if key != last {
            flush(&mut spans, &mut run, last);
            last = key;
        }
        run.push(c.ch);
    }
    flush(&mut spans, &mut run, last);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_fills_exactly_the_requested_width() {
        for spinner in ALL {
            for tick in [0usize, 1, 7, 31, 100, 1000] {
                for width in [2usize, 3, 8, 13, 24, 80, 200] {
                    let cells = (spinner.frame)(tick, width);
                    assert_eq!(cells.len(), width, "{} t={tick} w={width}", spinner.name);
                }
                // Sub-minimum widths clamp to 2 instead of panicking.
                assert_eq!((spinner.frame)(tick, 0).len(), 2, "{}", spinner.name);
            }
        }
    }

    #[test]
    fn frames_are_deterministic_and_single_column() {
        use unicode_width::UnicodeWidthChar;
        for spinner in ALL {
            assert_eq!((spinner.frame)(9, 24), (spinner.frame)(9, 24));
            for cells in [(spinner.frame)(0, 40), (spinner.frame)(123, 40)] {
                for c in cells {
                    assert_eq!(c.ch.width(), Some(1), "{} {:?}", spinner.name, c.ch);
                }
            }
        }
    }

    #[test]
    fn knight_rider_bounces_with_halo() {
        // Frame math pinned against catalog.go: w=8, total=14; tick 10 reflects to pos 4.
        let cells = knight_rider(10, 8);
        assert_eq!(
            cells[4],
            Cell {
                ch: '█',
                fg: rgb(0x60A5FA),
                bold: true
            }
        );
        assert_eq!(cells[3].ch, '▓');
        assert_eq!(cells[5].ch, '▓');
        assert_eq!(cells[6].ch, '▒');
        assert_eq!(cells[0].ch, '░');
    }

    #[test]
    fn tidal_wave_scrolls_right() {
        // (i - tick) mod 9 picks the glyph: i=0,tick=5 → rem_euclid(-5,9)=4 → '█'.
        let cells = tidal_wave(5, 12);
        assert_eq!(cells[0].ch, '█');
        assert_eq!(cells[0].fg, rgb(0x0284C7)); // color index rem_euclid(-5, 8) = 3
    }

    #[test]
    fn by_round_cycles_the_catalog() {
        assert_eq!(by_round(0).name, "Knight Rider");
        assert_eq!(by_round(1).name, "Comet");
        assert_eq!(by_round(ALL.len()).name, "Knight Rider");
    }

    #[test]
    fn ansi_line_coalesces_and_resets_once_at_end() {
        let cells = knight_rider(0, 8);
        let line = ansi_line(&cells);
        assert!(line.ends_with("\x1b[0m"));
        // Head is bold blue.
        assert!(line.contains("\x1b[1;38;2;96;165;250m"));
        // The trailing '░' run (5 dark cells at tick 0) must be one SGR run.
        assert_eq!(line.matches("38;2;15;23;42").count(), 1);
        let visible: usize = line
            .split('\x1b')
            .map(|seg| {
                seg.splitn(2, 'm').nth(1).map_or(
                    if seg.is_empty() {
                        0
                    } else {
                        seg.chars().count()
                    },
                    |s| s.chars().count(),
                )
            })
            .sum();
        assert_eq!(visible, 8);
    }

    #[test]
    fn cells_to_spans_merges_runs() {
        let cells = knight_rider(0, 8);
        let spans = cells_to_spans(&cells);
        // tick 0: pos 0 → █ ▓ ▒ ░░░░░ = 4 runs.
        assert_eq!(spans.len(), 4);
        let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, 8);
    }
}
