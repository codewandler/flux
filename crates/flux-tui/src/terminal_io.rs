//! Crossterm setup and unconditional terminal restoration.

use super::*;

pub(super) type Tui = Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

#[derive(Default)]
pub(super) struct TerminalGuard {
    raw: bool,
    alternate: bool,
    mouse: bool,
    paste: bool,
    cursor_hidden: bool,
}

impl TerminalGuard {
    pub(super) fn enter(mut out: std::io::Stdout) -> anyhow::Result<(Tui, Self)> {
        use crossterm::cursor::Hide;
        use crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
        use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};

        let mut guard = Self::default();
        enable_raw_mode()?;
        guard.raw = true;
        crossterm::execute!(out, EnterAlternateScreen)?;
        guard.alternate = true;
        crossterm::execute!(out, EnableMouseCapture)?;
        guard.mouse = true;
        crossterm::execute!(out, EnableBracketedPaste)?;
        guard.paste = true;
        crossterm::execute!(out, Hide)?;
        guard.cursor_hidden = true;
        let terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(out))?;
        Ok((terminal, guard))
    }

    pub(super) fn restore(
        &mut self,
        backend: &mut ratatui::backend::CrosstermBackend<std::io::Stdout>,
    ) -> anyhow::Result<()> {
        use crossterm::cursor::Show;
        use crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
        use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};

        let mut first_error: Option<anyhow::Error> = None;
        macro_rules! attempt {
            ($condition:expr, $command:expr, $field:ident) => {
                if $condition {
                    if let Err(error) = crossterm::execute!(backend, $command) {
                        first_error.get_or_insert_with(|| error.into());
                    } else {
                        self.$field = false;
                    }
                }
            };
        }
        attempt!(self.cursor_hidden, Show, cursor_hidden);
        attempt!(self.paste, DisableBracketedPaste, paste);
        attempt!(self.mouse, DisableMouseCapture, mouse);
        attempt!(self.alternate, LeaveAlternateScreen, alternate);
        if self.raw {
            if let Err(error) = disable_raw_mode() {
                first_error.get_or_insert_with(|| error.into());
            } else {
                self.raw = false;
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        use crossterm::cursor::Show;
        use crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
        use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
        let mut out = std::io::stdout();
        if self.cursor_hidden {
            let _ = crossterm::execute!(out, Show);
        }
        if self.paste {
            let _ = crossterm::execute!(out, DisableBracketedPaste);
        }
        if self.mouse {
            let _ = crossterm::execute!(out, DisableMouseCapture);
        }
        if self.alternate {
            let _ = crossterm::execute!(out, LeaveAlternateScreen);
        }
        if self.raw {
            let _ = disable_raw_mode();
        }
    }
}
