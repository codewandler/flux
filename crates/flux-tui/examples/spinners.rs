//! Showcase of the animated spinner catalog (mirrors the Go library's cmd/demo):
//! every effect side by side, bar width cycling 8→12→16→20→24. Run with
//! `cargo run -p flux-tui --example spinners`; any key quits.

use std::io::{self, Write as _};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::{cursor, terminal};
use flux_tui::spinners::{ansi_line, ALL, FPS_MS};

const WIDTHS: [usize; 5] = [8, 12, 16, 20, 24];
/// Ticks between width switches.
const WIDTH_CYCLE: usize = 32;

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut out = io::stdout();
    crossterm::execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;
    let result = run(&mut out);
    let _ = crossterm::execute!(out, cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    result
}

fn run(out: &mut io::Stdout) -> io::Result<()> {
    let mut tick = 0usize;
    loop {
        let width = WIDTHS[tick / WIDTH_CYCLE % WIDTHS.len()];
        let mut buf = format!("\x1b[H\x1b[2Jspinners — width {width:>2} — any key quits\r\n\r\n");
        for (i, spinner) in ALL.iter().enumerate() {
            buf.push_str(&format!(
                "{:>2}. {:<14} {}\r\n",
                i + 1,
                spinner.name,
                ansi_line(&(spinner.frame)(tick, width))
            ));
        }
        out.write_all(buf.as_bytes())?;
        out.flush()?;
        tick += 1;
        if event::poll(Duration::from_millis(FPS_MS))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    return Ok(());
                }
            }
        }
    }
}
