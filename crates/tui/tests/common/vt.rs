//! A terminal for the menu's tests: it keeps what was written and plays it on a screen of cells (the
//! cursor moves, line and screen erases and text; colours and modes are noted, not drawn). Like xterm
//! and Windows Terminal, it starts with the cursor under a shell prompt, and switching to the
//! alternate screen clears it but leaves the cursor where it was.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use baton_core::text::char_width;
use baton_tui::console::Console;

struct Screen {
    columns: usize,
    rows: usize,
    chunks: Vec<String>,
    modes: BTreeMap<String, bool>,
    grid: Vec<Vec<String>>,
    x: usize,
    y: usize,
    primary: Option<(Vec<Vec<String>>, usize, usize)>,
}

impl Screen {
    fn blank(&self) -> Vec<Vec<String>> {
        vec![vec![" ".to_owned(); self.columns]; self.rows]
    }

    fn newline(&mut self) {
        self.x = 0;
        if self.y + 1 < self.rows {
            self.y += 1;
        } else {
            self.grid.remove(0);
            self.grid.push(vec![" ".to_owned(); self.columns]);
        }
    }

    fn csi(&mut self, params: &[&str], last: char, mark: &str) {
        let n = |i: usize, fallback: usize| -> usize {
            match params.get(i).and_then(|p| p.parse::<usize>().ok()) {
                Some(0) if params.get(i) == Some(&"0") => 0,
                Some(0) | None => fallback,
                Some(value) => value,
            }
        };
        match last {
            'A' => self.y = self.y.saturating_sub(n(0, 1)),
            'B' => self.y = (self.y + n(0, 1)).min(self.rows - 1),
            'C' => self.x = (self.x + n(0, 1)).min(self.columns - 1),
            'D' => self.x = self.x.saturating_sub(n(0, 1)),
            'E' => {
                self.y = (self.y + n(0, 1)).min(self.rows - 1);
                self.x = 0;
            }
            'F' => {
                self.y = self.y.saturating_sub(n(0, 1));
                self.x = 0;
            }
            'G' => self.x = n(0, 1).saturating_sub(1).min(self.columns - 1),
            'H' | 'f' => {
                self.y = n(0, 1).saturating_sub(1).min(self.rows - 1);
                self.x = n(1, 1).saturating_sub(1).min(self.columns - 1);
            }
            'J' => {
                let how = n(0, 0);
                if how == 2 || how == 3 {
                    self.grid = self.blank();
                } else {
                    let (x, y) = (self.x, self.y);
                    self.grid[y][x..].fill(" ".to_owned());
                    for row in self.grid.iter_mut().skip(y + 1) {
                        row.fill(" ".to_owned());
                    }
                }
            }
            'K' => {
                let how = n(0, 0);
                let (x, y) = (self.x, self.y);
                let row = &mut self.grid[y];
                match how {
                    2 => row.fill(" ".to_owned()),
                    1 => row[..=x].fill(" ".to_owned()),
                    _ => row[x..].fill(" ".to_owned()),
                }
            }
            'h' | 'l' if mark == "?" => {
                for p in params {
                    if *p == "1049" && last == 'h' && self.primary.is_none() {
                        // The alternate screen: blank, the cursor where it was.
                        let blank = self.blank();
                        self.primary = Some((std::mem::replace(&mut self.grid, blank), self.x, self.y));
                    } else if *p == "1049"
                        && last == 'l'
                        && let Some((grid, x, y)) = self.primary.take()
                    {
                        (self.grid, self.x, self.y) = (grid, x, y);
                    }
                    self.modes.insert((*p).to_owned(), last == 'h');
                }
            }
            // Colours (m) and anything else change no cell.
            _ => {}
        }
    }

    fn play(&mut self, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '\u{1b}' {
                match chars.get(i + 1) {
                    Some('[') => {
                        let mut j = i + 2;
                        let mut mark = String::new();
                        if let Some(m) = chars.get(j).filter(|c| "?>=<".contains(**c)) {
                            mark.push(*m);
                            j += 1;
                        }
                        let mut body = String::new();
                        while j < chars.len() && !('@'..='~').contains(&chars[j]) {
                            body.push(chars[j]);
                            j += 1;
                        }
                        if let Some(last) = chars.get(j) {
                            let params: Vec<&str> = body.split(';').collect();
                            self.csi(&params, *last, &mark);
                        }
                        i = j + 1;
                    }
                    Some(']') => {
                        let mut j = i + 2;
                        while j < chars.len()
                            && chars[j] != '\u{7}'
                            && !(chars[j] == '\u{1b}' && chars.get(j + 1) == Some(&'\\'))
                        {
                            j += 1;
                        }
                        i = if chars.get(j) == Some(&'\u{7}') {
                            j + 1
                        } else {
                            j + 2
                        };
                    }
                    _ => i += 2,
                }
                continue;
            }
            match ch {
                '\r' => self.x = 0,
                '\n' => self.newline(),
                '\u{8}' => self.x = self.x.saturating_sub(1),
                _ => {
                    let w = char_width(ch);
                    if w > 0 && self.x < self.columns {
                        self.grid[self.y][self.x] = ch.to_string();
                        if w == 2 && self.x + 1 < self.columns {
                            self.grid[self.y][self.x + 1] = String::new();
                        }
                        // Line wrap is off: the last column takes every character.
                        self.x = (self.x + w).min(self.columns - 1);
                    }
                }
            }
            i += 1;
        }
    }
}

/// A screen the menu draws on and a test looks at.
#[derive(Clone)]
pub struct VirtualTerminal {
    screen: Arc<Mutex<Screen>>,
}

impl VirtualTerminal {
    pub fn new(columns: usize, rows: usize) -> VirtualTerminal {
        let cursor_row = rows / 2;
        let mut screen = Screen {
            columns,
            rows,
            chunks: Vec::new(),
            modes: BTreeMap::new(),
            grid: Vec::new(),
            x: 0,
            y: cursor_row.min(rows - 1),
            primary: None,
        };
        screen.grid = screen.blank();
        for row in screen.grid.iter_mut().take(cursor_row) {
            for (x, ch) in "PS E:\\> x".chars().enumerate() {
                row[x] = ch.to_string();
            }
        }
        VirtualTerminal {
            screen: Arc::new(Mutex::new(screen)),
        }
    }

    /// What the screen shows, a line per row, without trailing spaces.
    pub fn text(&self) -> String {
        let screen = self.screen.lock().unwrap();
        screen
            .grid
            .iter()
            .map(|row| row.concat().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether a private mode (`1049`, `1000`, ...) is on; None when it was never set.
    pub fn mode(&self, name: &str) -> Option<bool> {
        self.screen.lock().unwrap().modes.get(name).copied()
    }

    /// Everything that was written, in order.
    pub fn written(&self) -> String {
        self.screen.lock().unwrap().chunks.concat()
    }

    /// The terminal changes size; what was on it stays where it was.
    pub fn resize(&self, columns: usize, rows: usize) {
        let mut screen = self.screen.lock().unwrap();
        let old = std::mem::take(&mut screen.grid);
        screen.columns = columns;
        screen.rows = rows;
        screen.grid = (0..rows)
            .map(|y| {
                (0..columns)
                    .map(|x| {
                        old.get(y)
                            .and_then(|row| row.get(x))
                            .cloned()
                            .unwrap_or_else(|| " ".to_owned())
                    })
                    .collect()
            })
            .collect();
        screen.x = screen.x.min(columns - 1);
        screen.y = screen.y.min(rows - 1);
    }
}

impl Console for VirtualTerminal {
    fn is_tty(&self) -> bool {
        true
    }

    fn size(&self) -> Option<(usize, usize)> {
        let screen = self.screen.lock().unwrap();
        Some((screen.columns, screen.rows))
    }

    fn write(&mut self, text: &str) -> io::Result<()> {
        let mut screen = self.screen.lock().unwrap();
        screen.chunks.push(text.to_owned());
        screen.play(text);
        Ok(())
    }
}
