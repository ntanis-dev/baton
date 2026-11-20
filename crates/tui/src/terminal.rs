//! Where the menu's keys come from: the process's own terminal, read through crossterm, or a stream of
//! the bytes a terminal sends (a test, or anything that stands in for one).
//!
//! Either way a thread reads and sends actions to the thread that owns the store. While Claude Code's
//! sign-in has the terminal the reading stops ([`Keys::pause`]): what is typed then is Claude Code's.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use crate::input::Decoder;
use crate::keys::Key;
use crate::state::{Action, MouseEvent, MouseKind, WHEEL_STEP};
use crate::store::Msg;

/// A source of keys.
pub trait Keys: Send {
    /// Stops reading: what is typed from now on is someone else's. Returns once the reading stopped.
    fn pause(&mut self);

    /// Reads again.
    fn resume(&mut self);

    /// Stops for good.
    fn close(&mut self);
}

/// The key of a crossterm key event, by the menu's names; None for one the menu does not use (and for
/// a key going up).
pub fn key_of(event: &KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    let moving = |name: &str| {
        Some(if ctrl {
            Key::Named(format!("ctrl+{name}"))
        } else if shift {
            Key::Named(format!("shift+{name}"))
        } else {
            Key::from_name(name)
        })
    };
    match event.code {
        KeyCode::Esc => Some(Key::Escape),
        KeyCode::Up => moving("up"),
        KeyCode::Down => moving("down"),
        KeyCode::Left => moving("left"),
        KeyCode::Right => moving("right"),
        KeyCode::PageUp => moving("pageup"),
        KeyCode::PageDown => moving("pagedown"),
        KeyCode::Home => moving("home"),
        KeyCode::End => moving("end"),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Tab if shift => Some(Key::Named("shift+tab".to_owned())),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::BackTab => Some(Key::Named("shift+tab".to_owned())),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Delete => Some(Key::Delete),
        // Ctrl and Alt together is AltGr on Windows: the character it types is just a character.
        KeyCode::Char(c) if ctrl && !alt => Some(Key::Ctrl(c.to_ascii_lowercase())),
        KeyCode::Char(_) if alt && !ctrl => None,
        KeyCode::Char(c) => Key::Char(c).printable().map(Key::Char),
        _ => None,
    }
}

/// The action of a crossterm event; None for one the menu does not use.
pub fn action_of(event: &Event) -> Option<Action> {
    match event {
        Event::Key(key) => key_of(key).map(Action::Key),
        Event::Paste(text) => Some(Action::Paste(text.clone())),
        Event::Resize(cols, rows) => Some(Action::Resize {
            cols: usize::from(*cols),
            rows: usize::from(*rows),
        }),
        Event::Mouse(mouse) => {
            let button = |button: MouseButton| match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
            };
            let (kind, button, delta) = match mouse.kind {
                MouseEventKind::Down(b) => (MouseKind::Press, button(b), 0),
                MouseEventKind::Up(b) => (MouseKind::Release, button(b), 0),
                MouseEventKind::Drag(b) => (MouseKind::Move, button(b), 0),
                MouseEventKind::Moved => (MouseKind::Move, 0, 0),
                MouseEventKind::ScrollDown => (MouseKind::Wheel, 1, WHEEL_STEP),
                MouseEventKind::ScrollUp => (MouseKind::Wheel, 0, -WHEEL_STEP),
                MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => return None,
            };
            Some(Action::Mouse(MouseEvent {
                x: i32::from(mouse.column),
                y: i32::from(mouse.row),
                button,
                kind,
                delta,
            }))
        }
        Event::FocusGained | Event::FocusLost => None,
    }
}

/// The character a key event types, when it types one: a character without Ctrl or Alt, or Enter (a
/// line break, when it is in the middle of text that arrives at once).
fn typed(event: &Event) -> Option<char> {
    let Event::Key(key) = event else {
        return None;
    };
    match key_of(key)? {
        Key::Char(c) => Some(c),
        Key::Enter => Some('\r'),
        _ => None,
    }
}

/// Events as actions, the characters that arrive at once (a paste on a console without paste marks,
/// or fast typing) as one piece of text. `more` gives the next event when one is there already.
pub fn actions_of(first: Event, more: &mut dyn FnMut() -> Option<Event>) -> Vec<Action> {
    let mut out = Vec::new();
    let mut next = Some(first);
    while let Some(event) = next.take() {
        let Some(ch) = typed(&event) else {
            out.extend(action_of(&event));
            next = more();
            continue;
        };
        if ch == '\r' {
            // Enter that starts no text is the Enter key.
            out.push(Action::Key(Key::Enter));
            next = more();
            continue;
        }
        let mut text = String::from(ch);
        // A line break counts as text only when more text follows it at once.
        let mut breaks = String::new();
        loop {
            next = more();
            match next.as_ref().and_then(typed) {
                Some('\r') => breaks.push('\r'),
                Some(ch) => {
                    text.push_str(&breaks);
                    breaks.clear();
                    text.push(ch);
                }
                None => break,
            }
        }
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(only), None) => out.push(Action::Key(Key::Char(only))),
            _ => out.push(Action::Text(text)),
        }
        out.extend(breaks.chars().map(|_| Action::Key(Key::Enter)));
    }
    out
}

/// What the reading thread and its owner share.
#[derive(Debug, Default)]
struct Switch {
    /// The owner asked the reading to stop for now.
    paused: AtomicBool,
    /// The reading stopped.
    idle: AtomicBool,
    /// The owner asked the reading to stop for good.
    closed: AtomicBool,
}

impl Switch {
    /// Waits (a moment at most) until the reading thread says it stopped.
    fn wait_idle(&self) {
        let started = Instant::now();
        while !self.idle.load(Ordering::SeqCst) && started.elapsed() < Duration::from_millis(500) {
            thread::sleep(Duration::from_millis(2));
        }
    }
}

/// The process's own terminal, read through crossterm.
#[derive(Debug)]
pub struct TerminalKeys {
    /// What is shared with the reading thread.
    switch: Arc<Switch>,
}

impl TerminalKeys {
    /// Starts reading the terminal's events and sending them to `to`. `mouse`: whether mouse events
    /// are passed on.
    pub fn start(to: Sender<Msg>, mouse: bool) -> TerminalKeys {
        let switch = Arc::new(Switch::default());
        let shared = Arc::clone(&switch);
        thread::spawn(move || {
            let send = |action: Action| {
                if !mouse && matches!(action, Action::Mouse(_)) {
                    return true;
                }
                to.send(Msg::Action(action)).is_ok()
            };
            loop {
                if shared.closed.load(Ordering::SeqCst) {
                    break;
                }
                if shared.paused.load(Ordering::SeqCst) {
                    shared.idle.store(true, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                shared.idle.store(false, Ordering::SeqCst);
                match event::poll(Duration::from_millis(40)) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => {
                        let _ = to.send(Msg::InputEnded);
                        break;
                    }
                }
                let Ok(first) = event::read() else {
                    let _ = to.send(Msg::InputEnded);
                    break;
                };
                let mut more = || match event::poll(Duration::ZERO) {
                    Ok(true) => event::read().ok(),
                    _ => None,
                };
                if !actions_of(first, &mut more).into_iter().all(send) {
                    break;
                }
            }
            shared.idle.store(true, Ordering::SeqCst);
        });
        TerminalKeys { switch }
    }
}

impl Keys for TerminalKeys {
    fn pause(&mut self) {
        self.switch.paused.store(true, Ordering::SeqCst);
        self.switch.wait_idle();
    }

    fn resume(&mut self) {
        self.switch.idle.store(false, Ordering::SeqCst);
        self.switch.paused.store(false, Ordering::SeqCst);
    }

    fn close(&mut self) {
        self.switch.closed.store(true, Ordering::SeqCst);
        self.switch.wait_idle();
    }
}

impl Drop for TerminalKeys {
    fn drop(&mut self) {
        self.switch.closed.store(true, Ordering::SeqCst);
    }
}

/// What the thread that reads the bytes tells the one that makes actions of them.
enum Raw {
    /// The next bytes.
    Bytes(Vec<u8>),
    /// The stream ended.
    End,
    /// Something changed (the reading resumed, or closed): look again.
    Wake,
}

/// A stream of the bytes a terminal sends.
pub struct ByteKeys {
    /// What is shared with the threads.
    switch: Arc<Switch>,
    /// Wakes the thread that makes actions.
    wake: Sender<Raw>,
}

impl std::fmt::Debug for ByteKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ByteKeys")
            .field("switch", &self.switch)
            .finish_non_exhaustive()
    }
}

impl ByteKeys {
    /// Starts reading `source` and sending what it holds to `to` as actions. `mouse`: whether mouse
    /// reports are passed on (they are taken out of the keys either way).
    pub fn start(mut source: Box<dyn Read + Send>, to: Sender<Msg>, mouse: bool) -> ByteKeys {
        let switch = Arc::new(Switch::default());
        let (raw, bytes) = mpsc::channel::<Raw>();
        let reader = raw.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match source.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if reader.send(Raw::Bytes(buffer[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = reader.send(Raw::End);
        });
        let shared = Arc::clone(&switch);
        thread::spawn(move || {
            let mut decoder = Decoder::new();
            // What arrived while the reading was paused.
            let mut waiting: Vec<Vec<u8>> = Vec::new();
            let send = |actions: Vec<Action>| {
                actions
                    .into_iter()
                    .filter(|action| mouse || !matches!(action, Action::Mouse(_)))
                    .all(|action| to.send(Msg::Action(action)).is_ok())
            };
            loop {
                if shared.closed.load(Ordering::SeqCst) {
                    break;
                }
                let paused = shared.paused.load(Ordering::SeqCst);
                shared.idle.store(paused, Ordering::SeqCst);
                if !paused {
                    let ok = std::mem::take(&mut waiting)
                        .into_iter()
                        .all(|chunk| send(decoder.push(&chunk)));
                    if !ok {
                        break;
                    }
                }
                let held = if paused { None } else { decoder.wait() };
                let got = match held {
                    Some(wait) => bytes.recv_timeout(wait),
                    None => bytes.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                match got {
                    Ok(Raw::Bytes(chunk)) => waiting.push(chunk),
                    Ok(Raw::Wake) => {}
                    Ok(Raw::End) | Err(RecvTimeoutError::Disconnected) => {
                        let _ = send(decoder.flush());
                        let _ = to.send(Msg::InputEnded);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if !send(decoder.flush()) {
                            break;
                        }
                    }
                }
            }
            shared.idle.store(true, Ordering::SeqCst);
        });
        ByteKeys { switch, wake: raw }
    }
}

impl Keys for ByteKeys {
    fn pause(&mut self) {
        self.switch.paused.store(true, Ordering::SeqCst);
        let _ = self.wake.send(Raw::Wake);
        self.switch.wait_idle();
    }

    fn resume(&mut self) {
        self.switch.paused.store(false, Ordering::SeqCst);
        let _ = self.wake.send(Raw::Wake);
    }

    fn close(&mut self) {
        self.switch.closed.store(true, Ordering::SeqCst);
        let _ = self.wake.send(Raw::Wake);
        self.switch.wait_idle();
    }
}

impl Drop for ByteKeys {
    fn drop(&mut self) {
        self.switch.closed.store(true, Ordering::SeqCst);
        let _ = self.wake.send(Raw::Wake);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventState, MouseEvent as TermMouse};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn named(code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        key_of(&key(code, modifiers)).map(|key| key.name())
    }

    #[test]
    fn keys_by_the_menus_names() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            named(KeyCode::Up, KeyModifiers::SHIFT).as_deref(),
            Some("shift+up")
        );
        assert_eq!(
            named(KeyCode::Right, KeyModifiers::CONTROL).as_deref(),
            Some("ctrl+right")
        );
        assert_eq!(
            named(KeyCode::Tab, KeyModifiers::SHIFT).as_deref(),
            Some("shift+tab")
        );
        assert_eq!(
            named(KeyCode::BackTab, KeyModifiers::SHIFT).as_deref(),
            Some("shift+tab")
        );
        assert_eq!(named(KeyCode::Tab, none).as_deref(), Some("tab"));
        assert_eq!(
            named(KeyCode::Char('J'), KeyModifiers::SHIFT).as_deref(),
            Some("J")
        );
        assert_eq!(
            named(KeyCode::Char('c'), KeyModifiers::CONTROL).as_deref(),
            Some("ctrl+c")
        );
        assert_eq!(
            named(KeyCode::Char('U'), KeyModifiers::CONTROL).as_deref(),
            Some("ctrl+u")
        );
        assert_eq!(
            named(KeyCode::Char(' '), KeyModifiers::CONTROL).as_deref(),
            Some("ctrl+space")
        );
        assert_eq!(named(KeyCode::Char('a'), KeyModifiers::ALT), None);
        assert_eq!(
            named(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT).as_deref(),
            Some("@"),
            "AltGr types a character"
        );
        for (code, name) in [
            (KeyCode::Esc, "escape"),
            (KeyCode::Enter, "enter"),
            (KeyCode::Backspace, "backspace"),
            (KeyCode::Delete, "delete"),
            (KeyCode::PageUp, "pageup"),
            (KeyCode::PageDown, "pagedown"),
            (KeyCode::Home, "home"),
            (KeyCode::End, "end"),
            (KeyCode::Char('/'), "/"),
        ] {
            assert_eq!(named(code, none).as_deref(), Some(name));
        }
        assert_eq!(named(KeyCode::F(5), none), None);
        assert_eq!(named(KeyCode::Insert, none), None);
        let mut up = key(KeyCode::Char('q'), none);
        up.kind = KeyEventKind::Release;
        assert_eq!(key_of(&up), None, "a key going up is no key");
    }

    #[test]
    fn mouse_pastes_and_sizes() {
        let mouse = |kind| {
            action_of(&Event::Mouse(TermMouse {
                kind,
                column: 7,
                row: 9,
                modifiers: KeyModifiers::NONE,
            }))
        };
        assert_eq!(
            mouse(MouseEventKind::Down(MouseButton::Left)),
            Some(Action::Mouse(MouseEvent::press(7, 9)))
        );
        assert_eq!(
            mouse(MouseEventKind::ScrollDown),
            Some(Action::Mouse(MouseEvent {
                button: 1,
                ..MouseEvent::wheel(7, 9, 1)
            }))
        );
        assert_eq!(
            mouse(MouseEventKind::ScrollUp),
            Some(Action::Mouse(MouseEvent::wheel(7, 9, -1)))
        );
        let Some(Action::Mouse(right)) = mouse(MouseEventKind::Down(MouseButton::Right)) else {
            panic!("a press");
        };
        assert_eq!((right.kind, right.button), (MouseKind::Press, 2));
        let Some(Action::Mouse(up)) = mouse(MouseEventKind::Up(MouseButton::Left)) else {
            panic!("a release");
        };
        assert_eq!(up.kind, MouseKind::Release);
        assert_eq!(mouse(MouseEventKind::ScrollLeft), None);
        assert_eq!(
            action_of(&Event::Paste("two\nlines".to_owned())),
            Some(Action::Paste("two\nlines".to_owned()))
        );
        assert_eq!(
            action_of(&Event::Resize(120, 30)),
            Some(Action::Resize { cols: 120, rows: 30 })
        );
        assert_eq!(action_of(&Event::FocusGained), None);
    }

    #[test]
    fn characters_that_arrive_at_once_are_one_piece_of_text() {
        let events = |text: &str| -> Vec<Event> {
            text.chars()
                .map(|c| {
                    Event::Key(key(
                        if c == '\r' {
                            KeyCode::Enter
                        } else {
                            KeyCode::Char(c)
                        },
                        KeyModifiers::NONE,
                    ))
                })
                .collect()
        };
        let run = |mut list: Vec<Event>| {
            list.reverse();
            let first = list.pop().unwrap();
            actions_of(first, &mut || list.pop())
        };
        assert_eq!(run(events("q")), vec![Action::Key(Key::Char('q'))]);
        assert_eq!(run(events("\r")), vec![Action::Key(Key::Enter)]);
        assert_eq!(run(events("acct5")), vec![Action::Text("acct5".to_owned())]);
        // Enter after text is the Enter key; a line break in the middle of text is part of it.
        assert_eq!(
            run(events("name\r")),
            vec![Action::Text("name".to_owned()), Action::Key(Key::Enter)]
        );
        assert_eq!(
            run(events("one\rtwo\r")),
            vec![Action::Text("one\rtwo".to_owned()), Action::Key(Key::Enter)]
        );
        assert_eq!(
            run(events("\re")),
            vec![Action::Key(Key::Enter), Action::Key(Key::Char('e'))]
        );
        // Anything else ends the text and comes after it.
        let mut mixed = events("ab");
        mixed.push(Event::Key(key(KeyCode::Down, KeyModifiers::NONE)));
        mixed.extend(events("c"));
        assert_eq!(
            run(mixed),
            vec![
                Action::Text("ab".to_owned()),
                Action::Key(Key::Down),
                Action::Key(Key::Char('c'))
            ]
        );
    }
}
