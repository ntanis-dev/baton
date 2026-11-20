//! The terminal the menu runs in: where it draws and what it is told its size is. The process's own
//! terminal is [`StdConsole`]; a test hands the menu a screen of its own.

use std::io::{self, IsTerminal, Write};

use crate::paint::{MOUSE_OFF, MOUSE_ON, PASTE_OFF, PASTE_ON};

/// A terminal to draw on.
pub trait Console: Send {
    /// Whether it is a terminal at all (and not a pipe or a file).
    fn is_tty(&self) -> bool;

    /// Its size now, columns and rows; None when it cannot say.
    fn size(&self) -> Option<(usize, usize)>;

    /// Writes text, escape sequences and all.
    fn write(&mut self, text: &str) -> io::Result<()>;

    /// Turns raw input on or off: keys one by one, Ctrl+C as a key. Only the process's own terminal
    /// has such a mode.
    fn raw(&mut self, _on: bool) {}

    /// Turns what the menu asks a terminal to report on or off: mouse presses and the wheel (when
    /// `mouse`), and pastes between marks.
    fn reports(&mut self, mouse: bool, on: bool) -> io::Result<()> {
        if on {
            self.write(&format!("{}{PASTE_ON}", if mouse { MOUSE_ON } else { "" }))
        } else {
            self.write(&format!("{PASTE_OFF}{MOUSE_OFF}"))
        }
    }
}

/// The process's own terminal: its standard output, and the console's modes.
#[derive(Debug, Default)]
pub struct StdConsole {
    /// Whether raw input was turned on here (and has to be turned off again).
    raw: bool,
    /// Whether mouse reports were turned on here.
    mouse: bool,
}

impl StdConsole {
    /// The process's terminal.
    pub fn new() -> StdConsole {
        // The classic Windows console understands escape sequences only once it is asked to.
        #[cfg(windows)]
        let _ = crossterm::ansi_support::supports_ansi();
        StdConsole::default()
    }
}

impl Console for StdConsole {
    fn is_tty(&self) -> bool {
        io::stdout().is_terminal()
    }

    fn size(&self) -> Option<(usize, usize)> {
        let (cols, rows) = crossterm::terminal::size().ok()?;
        (cols > 0 && rows > 0).then(|| (usize::from(cols), usize::from(rows)))
    }

    fn write(&mut self, text: &str) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(text.as_bytes())?;
        out.flush()
    }

    fn raw(&mut self, on: bool) {
        if on && !self.raw {
            self.raw = crossterm::terminal::enable_raw_mode().is_ok();
        } else if !on && self.raw {
            let _ = crossterm::terminal::disable_raw_mode();
            self.raw = false;
        }
    }

    #[cfg(windows)]
    fn reports(&mut self, mouse: bool, on: bool) -> io::Result<()> {
        // The Windows console reports the mouse as input records once its input mode asks for them,
        // and has no marks around a paste: escape sequences for either would arrive as typed keys.
        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        if on && mouse && !self.mouse {
            crossterm::execute!(io::stdout(), EnableMouseCapture)?;
            self.mouse = true;
        } else if !on && self.mouse {
            self.mouse = false;
            crossterm::execute!(io::stdout(), DisableMouseCapture)?;
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn reports(&mut self, mouse: bool, on: bool) -> io::Result<()> {
        if on {
            self.mouse = mouse;
            self.write(&format!("{}{PASTE_ON}", if mouse { MOUSE_ON } else { "" }))
        } else {
            self.mouse = false;
            self.write(&format!("{PASTE_OFF}{MOUSE_OFF}"))
        }
    }
}

impl Drop for StdConsole {
    fn drop(&mut self) {
        self.raw(false);
        if self.mouse {
            let _ = self.reports(true, false);
        }
    }
}

/// Gives the process's terminal back as it was, whatever the menu had changed of it: for whoever ends
/// the process without the menu's help (a signal handler, a panic hook). Safe to call at any time;
/// nothing is written where the standard output is no terminal.
pub fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    if !io::stdout().is_terminal() {
        return;
    }
    #[cfg(windows)]
    let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture);
    let mut out = io::stdout().lock();
    let _ = out.write_all(crate::paint::RESTORE.as_bytes());
    let _ = out.flush();
}
