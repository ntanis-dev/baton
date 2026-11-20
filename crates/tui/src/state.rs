//! The menu's state and what the sidebar and the panes read from it. Pure: the reducer changes state,
//! the drawing code draws it.

use std::collections::BTreeMap;
use std::sync::Arc;

use baton_core::clock::Ms;
use baton_core::config::{Account, Mark};
use baton_core::policy::AccountState;
use baton_core::theme::GlyphSet;
use serde_json::{Value, json};

use crate::data::{MenuData, SessionInfo};
use crate::keys::Key;
use crate::layout::Size;
use crate::pane::Item;
use crate::views::policy::Form;

/// Where the keyboard is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Focus {
    /// The list.
    Side,
    /// The pinned search control.
    Search,
    /// The pinned Add account control.
    Add,
    /// The Policy editor.
    Pane,
}

/// An account's sign-in, from `claude auth status` (method only, never the email).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Auth {
    /// Whether it is signed in; None when Claude Code did not say.
    pub logged_in: Option<bool>,
    /// How it signed in: `claude.ai`, ...
    pub auth_method: Option<String>,
    /// Why the status could not be read.
    pub error: Option<String>,
}

/// How a toast reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Something to know.
    Info,
    /// Something done.
    Ok,
    /// Something that went wrong.
    Error,
}

impl Tone {
    /// How long a toast of this tone stays, in milliseconds.
    pub fn ttl(self) -> Ms {
        match self {
            Tone::Error => 10_000,
            Tone::Ok => 5_000,
            Tone::Info => 4_000,
        }
    }

    /// The tone's name: `info`, `ok`, `error`.
    pub fn as_str(self) -> &'static str {
        match self {
            Tone::Info => "info",
            Tone::Ok => "ok",
            Tone::Error => "error",
        }
    }
}

/// A short message that floats over the pane for a while.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    /// Its number, for dismissing it.
    pub id: u64,
    /// What it says.
    pub text: String,
    /// How it reads.
    pub tone: Tone,
    /// When it appeared.
    pub at: Ms,
}

/// A core operation the menu asks the runtime to carry out. Accounts are named by their id, so a
/// rename in between never acts on the wrong one.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Sets an account's own thresholds; None follows the policy.
    Thresholds {
        /// The account's id.
        id: String,
        /// Its 5-hour threshold.
        five_hour: Option<f64>,
        /// Its weekly threshold.
        weekly: Option<f64>,
    },
    /// Adds an account; its sign-in follows.
    Add {
        /// The new account's name.
        name: String,
    },
    /// Renames an account.
    Rename {
        /// The account's id.
        id: String,
        /// Its new name.
        name: String,
    },
    /// Sets an account's mark.
    Mark {
        /// The account's id.
        id: String,
        /// Its new mark.
        mark: Mark,
    },
    /// Moves an account in the order.
    Move {
        /// The account's id.
        id: String,
        /// Its new place, from 1.
        position: usize,
    },
    /// Removes an account.
    Remove {
        /// The account's id.
        id: String,
    },
    /// Changes policy settings.
    Policy {
        /// The settings that changed, by name, in the form's order.
        changes: Vec<(String, Value)>,
    },
    /// Asks a session's wrapper to move it.
    Hop {
        /// The session's id.
        session: String,
        /// The account to move to, by id; None for the best one.
        to: Option<String>,
    },
}

impl Op {
    /// The operation's name: `thresholds`, `add`, `rename`, `mark`, `move`, `remove`, `policy`, `hop`.
    pub fn name(&self) -> &'static str {
        match self {
            Op::Thresholds { .. } => "thresholds",
            Op::Add { .. } => "add",
            Op::Rename { .. } => "rename",
            Op::Mark { .. } => "mark",
            Op::Move { .. } => "move",
            Op::Remove { .. } => "remove",
            Op::Policy { .. } => "policy",
            Op::Hop { .. } => "hop",
        }
    }

    /// The operation's arguments as JSON, by the names the operations take them under.
    pub fn args(&self) -> Value {
        use baton_core::json::opt_num;
        match self {
            Op::Thresholds {
                id,
                five_hour,
                weekly,
            } => {
                json!({ "id": id, "fiveHour": opt_num(*five_hour), "weekly": opt_num(*weekly) })
            }
            Op::Add { name } => json!({ "name": name }),
            Op::Rename { id, name } => json!({ "id": id, "name": name }),
            Op::Mark { id, mark } => json!({ "id": id, "mark": mark.as_str() }),
            Op::Move { id, position } => json!({ "id": id, "position": position }),
            Op::Remove { id } => json!({ "id": id }),
            Op::Policy { changes } => {
                let changes: serde_json::Map<String, Value> = changes.iter().cloned().collect();
                json!({ "changes": changes })
            }
            Op::Hop { session, to } => json!({ "session": session, "to": to }),
        }
    }
}

/// Something the reducer asks the runtime to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Leave the menu.
    Quit,
    /// Read ~/.baton again.
    Reload,
    /// Draw the whole screen again.
    Redraw,
    /// Ask an account's usage now (None: every account's).
    Refresh {
        /// The account's id.
        account: Option<String>,
        /// Without a toast: after a rename or a sign-in. The reducer never asks for this itself.
        quiet: bool,
    },
    /// Run Claude Code's sign-in for an account.
    SignIn {
        /// The account's id.
        account: String,
    },
    /// Ask these accounts' sign-in status (None: every account's). The reducer never asks for this
    /// itself; the runtime does when it starts and after a sign-in.
    Auth {
        /// The accounts' ids.
        accounts: Option<Vec<String>>,
    },
    /// Carry out a core operation, then say `done`.
    Apply {
        /// The operation.
        op: Op,
        /// What to say when it worked; None for "Done." or the operation's own words.
        done: Option<String>,
    },
}

impl Effect {
    /// An operation to carry out.
    pub fn apply(op: Op, done: Option<String>) -> Effect {
        Effect::Apply { op, done }
    }

    /// The effect's kind: `quit`, `reload`, `redraw`, `refresh`, `signin`, `auth`, `apply`.
    pub fn kind(&self) -> &'static str {
        match self {
            Effect::Quit => "quit",
            Effect::Reload => "reload",
            Effect::Redraw => "redraw",
            Effect::Refresh { .. } => "refresh",
            Effect::SignIn { .. } => "signin",
            Effect::Auth { .. } => "auth",
            Effect::Apply { .. } => "apply",
        }
    }

    /// The effect as JSON, for logs and comparisons.
    pub fn to_json(&self) -> Value {
        match self {
            Effect::Quit | Effect::Reload | Effect::Redraw => json!({ "type": self.kind() }),
            Effect::Refresh { account, .. } => json!({ "type": "refresh", "account": account }),
            Effect::SignIn { account } => json!({ "type": "signin", "account": account }),
            Effect::Auth { accounts } => json!({ "type": "auth", "accounts": accounts }),
            Effect::Apply { op, done } => {
                json!({ "type": "apply", "op": op.name(), "args": op.args(), "done": done })
            }
        }
    }
}

/// One command of an Actions dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionOption {
    /// What it says.
    pub label: String,
    /// The command it runs: "e", "mode", "t", ...
    pub command: String,
    /// The key that runs it directly.
    pub key: String,
}

/// One option of a choice dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceOption {
    /// What it says.
    pub label: String,
    /// What picking it sets.
    pub value: String,
}

/// What a choice dialog decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choose {
    /// An account's mark.
    Mark {
        /// The account's id.
        id: String,
        /// What to say when it is set.
        done: String,
    },
    /// Where a session hops to.
    HopTo {
        /// The session's id.
        session: String,
    },
}

/// What an input dialog's text is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submit {
    /// A new account's name.
    Add,
    /// A new name for this account (its id).
    Rename(String),
}

/// What a dialog is.
#[derive(Clone, Debug, PartialEq)]
pub enum ModalKind {
    /// The selection's commands.
    Actions {
        /// What the commands act on.
        item: Item,
        /// The command the keyboard is on.
        index: usize,
        /// The commands.
        options: Vec<ActionOption>,
    },
    /// Yes or no.
    Confirm {
        /// Whether Yes is the default.
        yes: bool,
        /// The button the keyboard is on: Some(true) for Yes, Some(false) for No, None for the default.
        focus: Option<bool>,
        /// What Yes does.
        effect: Effect,
    },
    /// A line of text.
    Input {
        /// The text.
        value: String,
        /// The cursor, in characters.
        cursor: usize,
        /// What the text is for.
        submit: Submit,
    },
    /// Pick one.
    Choice {
        /// The options.
        options: Vec<ChoiceOption>,
        /// The option the keyboard is on.
        index: usize,
        /// What the choice decides.
        choose: Choose,
        /// The label of the button that takes it.
        submit_label: Option<String>,
    },
    /// An account's two thresholds.
    Thresholds {
        /// The account's id.
        account: String,
        /// The account's name.
        account_name: String,
        /// The field the keyboard is on: 0 for 5-hour, 1 for weekly.
        index: usize,
        /// The account's own thresholds (5-hour, weekly); None follows the policy.
        values: [Option<f64>; 2],
        /// The policy's thresholds (5-hour, weekly).
        defaults: [f64; 2],
    },
}

/// A dialog over the view.
#[derive(Clone, Debug, PartialEq)]
pub struct Modal {
    /// Its title.
    pub title: String,
    /// The text under the title.
    pub lines: Vec<String>,
    /// Where the mouse wheel scrolled it to; None follows the keyboard.
    pub scroll: Option<usize>,
    /// What it is.
    pub kind: ModalKind,
}

impl Modal {
    /// The dialog's kind: `actions`, `confirm`, `input`, `choice`, `thresholds`.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            ModalKind::Actions { .. } => "actions",
            ModalKind::Confirm { .. } => "confirm",
            ModalKind::Input { .. } => "input",
            ModalKind::Choice { .. } => "choice",
            ModalKind::Thresholds { .. } => "thresholds",
        }
    }

    /// The option the keyboard is on, for the dialogs that have options.
    pub fn index(&self) -> Option<usize> {
        match &self.kind {
            ModalKind::Actions { index, .. }
            | ModalKind::Choice { index, .. }
            | ModalKind::Thresholds { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// The dialog with the keyboard on option `at`.
    pub fn with_index(&self, at: usize) -> Modal {
        let mut next = self.clone();
        match &mut next.kind {
            ModalKind::Actions { index, .. }
            | ModalKind::Choice { index, .. }
            | ModalKind::Thresholds { index, .. } => *index = at,
            _ => {}
        }
        next
    }
}

/// The menu's state.
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    /// The time now.
    pub now: Ms,
    /// The terminal's size.
    pub size: Size,
    /// Whether colours are drawn.
    pub color: bool,
    /// Which glyph set is drawn.
    pub glyphs: GlyphSet,
    /// Keyboard focus: the list, the pinned search or add control, or the Policy editor. Pinned focus
    /// preserves the open view.
    pub focus: Focus,
    /// The selected row's id.
    pub selected: String,
    /// The selected row's place among the rows that can be selected.
    pub index: usize,
    /// Whether the recent sessions are unfolded.
    pub open_sessions: bool,
    /// The filter typed.
    pub filter: String,
    /// Whether the filter takes the keys.
    pub filtering: bool,
    /// Per pane: the scroll position (its first shown line).
    pub views: BTreeMap<String, usize>,
    /// The policy form while it is open.
    pub form: Option<Form>,
    /// The dialog that is open.
    pub modal: Option<Modal>,
    /// The toasts that show.
    pub toasts: Vec<Toast>,
    /// The number of the last toast.
    pub toast_seq: u64,
    /// What was read from ~/.baton; None until the first read.
    pub data: Option<Arc<MenuData>>,
    /// Whether the splash screen shows.
    pub splash: bool,
    /// Sign-in per account id.
    pub auth: BTreeMap<String, Auth>,
    /// What runs right now (a sign-in), shown in the pane's header.
    pub busy: Option<String>,
}

/// What a fresh state starts from.
#[derive(Clone, Debug, PartialEq)]
pub struct Init {
    /// The terminal's size.
    pub size: Size,
    /// The time now.
    pub now: Ms,
    /// Whether colours are drawn.
    pub color: bool,
    /// Which glyph set is drawn.
    pub glyphs: GlyphSet,
    /// What was read from ~/.baton, when it already was.
    pub data: Option<Arc<MenuData>>,
    /// Whether the splash screen shows first.
    pub splash: bool,
}

impl Default for Init {
    fn default() -> Init {
        Init {
            size: Size { cols: 100, rows: 30 },
            now: 0,
            color: true,
            glyphs: GlyphSet::Rich,
            data: None,
            splash: false,
        }
    }
}

/// A fresh state: the Overview selected, nothing open.
pub fn initial_state(init: Init) -> State {
    State {
        now: init.now,
        size: init.size,
        color: init.color,
        glyphs: init.glyphs,
        focus: Focus::Side,
        selected: "overview".to_owned(),
        index: 0,
        open_sessions: false,
        filter: String::new(),
        filtering: false,
        views: BTreeMap::new(),
        form: None,
        modal: None,
        toasts: Vec::new(),
        toast_seq: 0,
        data: init.data,
        splash: init.splash,
        auth: BTreeMap::new(),
        busy: None,
    }
}

/// The accounts' states, in the operator's order.
pub fn accounts(state: &State) -> &[AccountState] {
    state.data.as_ref().map_or(&[], |data| data.states.as_slice())
}

/// An account's state by its id (a rename changes only the name).
pub fn account_by_id<'a>(state: &'a State, id: &str) -> Option<&'a AccountState> {
    accounts(state).iter().find(|a| a.id == id)
}

/// An account as the config holds it.
pub fn config_of<'a>(state: &'a State, id: &str) -> Option<&'a Account> {
    state.data.as_ref()?.config.accounts.iter().find(|a| a.id == id)
}

/// An account's name; its id when there is no such account.
pub fn name_of(state: &State, id: &str) -> String {
    config_of(state, id).map_or_else(|| id.to_owned(), |a| a.name.clone())
}

/// The sessions: the live ones first.
pub fn sessions(state: &State) -> &[SessionInfo] {
    state.data.as_ref().map_or(&[], |data| data.sessions.as_slice())
}

/// The sessions a wrapper runs now.
pub fn live_sessions(state: &State) -> Vec<&SessionInfo> {
    sessions(state).iter().filter(|x| x.live).collect()
}

/// The sessions seen lately that no wrapper runs.
pub fn recent_sessions(state: &State) -> Vec<&SessionInfo> {
    sessions(state).iter().filter(|x| !x.live).collect()
}

/// A session by its id.
pub fn session_by_id<'a>(state: &'a State, id: &str) -> Option<&'a SessionInfo> {
    sessions(state).iter().find(|x| x.id == id)
}

/// The live sessions on an account.
pub fn sessions_on<'a>(state: &'a State, id: &str) -> Vec<&'a SessionInfo> {
    sessions(state)
        .iter()
        .filter(|x| x.live && x.account.as_deref() == Some(id))
        .collect()
}

/// Whether an account is known to be signed out.
pub fn signed_out(state: &State, id: &str) -> bool {
    state
        .auth
        .get(id)
        .is_some_and(|auth| auth.logged_in == Some(false))
}

/// The state with a toast added; the last three stay.
pub fn toast(state: &State, text: &str, tone: Tone) -> State {
    let id = state.toast_seq + 1;
    let mut next = state.clone();
    next.toast_seq = id;
    next.toasts.push(Toast {
        id,
        text: text.to_owned(),
        tone,
        at: state.now,
    });
    let extra = next.toasts.len().saturating_sub(3);
    next.toasts.drain(..extra);
    next
}

/// The state without the toasts that have been up long enough.
pub fn expire_toasts(state: &State) -> State {
    let mut next = state.clone();
    next.toasts.retain(|t| state.now - t.at < t.tone.ttl());
    next
}

/// What the last toast says.
pub fn last_toast(state: &State) -> Option<&str> {
    state.toasts.last().map(|t| t.text.as_str())
}

/// A mouse event's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseKind {
    /// A button went down.
    Press,
    /// A button came up.
    Release,
    /// The wheel turned.
    Wheel,
    /// The pointer moved.
    Move,
}

/// A mouse event, in cells from the top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    /// The column.
    pub x: i32,
    /// The row.
    pub y: i32,
    /// The button: 0 is the left one, 1 the middle one, 2 the right one.
    pub button: u8,
    /// What happened.
    pub kind: MouseKind,
    /// For the wheel: lines to scroll, down positive.
    pub delta: i32,
}

impl MouseEvent {
    /// A left click at a cell.
    pub fn press(x: i32, y: i32) -> MouseEvent {
        MouseEvent {
            x,
            y,
            button: 0,
            kind: MouseKind::Press,
            delta: 0,
        }
    }

    /// A turn of the wheel over a cell.
    pub fn wheel(x: i32, y: i32, delta: i32) -> MouseEvent {
        MouseEvent {
            x,
            y,
            button: 0,
            kind: MouseKind::Wheel,
            delta,
        }
    }
}

/// How far one notch of the wheel goes: one row, in the list and in the pane alike, so that nothing is
/// ever skipped. A fast turn sends many notches.
pub const WHEEL_STEP: i32 = 1;

/// What the reducer is told.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// The splash screen's time is up.
    SplashEnd,
    /// The mouse did something.
    Mouse(MouseEvent),
    /// A key was pressed.
    Key(Key),
    /// Several characters arrived at once (fast typing).
    Text(String),
    /// Text was pasted.
    Paste(String),
    /// Time passed: toasts expire and ~/.baton is read again.
    Tick,
    /// The terminal changed size.
    Resize {
        /// Its columns.
        cols: usize,
        /// Its rows.
        rows: usize,
    },
    /// ~/.baton was read.
    Data(Arc<MenuData>),
    /// An account's sign-in status was read.
    Auth {
        /// The account's id.
        account: String,
        /// Its status.
        status: Auth,
    },
    /// Something started or stopped running (a sign-in).
    Busy(Option<String>),
    /// An operation's outcome.
    Applied {
        /// What to say.
        text: String,
        /// Whether it worked.
        ok: bool,
    },
}

/// What the reducer answers: the new state and what the runtime is to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The new state.
    pub state: State,
    /// What the runtime is to do, in order.
    pub effects: Vec<Effect>,
}

/// An outcome with nothing to do.
pub fn result(state: State) -> Outcome {
    Outcome {
        state,
        effects: Vec::new(),
    }
}

/// An outcome with things to do.
pub fn result_with(state: State, effects: Vec<Effect>) -> Outcome {
    Outcome { state, effects }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_come_and_go() {
        let mut state = initial_state(Init::default());
        assert_eq!(last_toast(&state), None);
        for (i, text) in ["one", "two", "three", "four"].iter().enumerate() {
            state.now = i as Ms * 1000;
            state = toast(&state, text, if i == 3 { Tone::Error } else { Tone::Info });
        }
        assert_eq!(
            state.toasts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["two", "three", "four"]
        );
        assert_eq!(state.toasts[2].id, 4);
        assert_eq!(last_toast(&state), Some("four"));
        state.now = 5500;
        let kept = expire_toasts(&state);
        assert_eq!(
            kept.toasts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["three", "four"]
        );
        state.now = 20_000;
        assert!(expire_toasts(&state).toasts.is_empty());
        assert_eq!(
            expire_toasts(&initial_state(Init::default())),
            initial_state(Init::default())
        );
    }

    #[test]
    fn effects_as_json() {
        assert_eq!(Effect::Quit.to_json(), json!({ "type": "quit" }));
        assert_eq!(
            Effect::apply(
                Op::Thresholds {
                    id: "a".into(),
                    five_hour: Some(92.0),
                    weekly: None
                },
                Some("Saved.".into())
            )
            .to_json(),
            json!({ "type": "apply", "op": "thresholds", "args": { "id": "a", "fiveHour": 92, "weekly": null }, "done": "Saved." })
        );
        assert_eq!(
            Effect::apply(
                Op::Policy {
                    changes: vec![("fiveHour".into(), json!(91))]
                },
                None
            )
            .to_json(),
            json!({ "type": "apply", "op": "policy", "args": { "changes": { "fiveHour": 91 } }, "done": null })
        );
        assert_eq!(
            Effect::Refresh {
                account: None,
                quiet: false
            }
            .to_json(),
            json!({ "type": "refresh", "account": null })
        );
    }

    #[test]
    fn signed_out_is_only_what_claude_said() {
        let mut state = initial_state(Init::default());
        assert!(!signed_out(&state, "a"));
        state.auth.insert(
            "a".into(),
            Auth {
                logged_in: Some(false),
                ..Auth::default()
            },
        );
        state.auth.insert(
            "b".into(),
            Auth {
                error: Some("no claude".into()),
                ..Auth::default()
            },
        );
        assert!(signed_out(&state, "a"));
        assert!(!signed_out(&state, "b"));
        assert_eq!(name_of(&state, "a"), "a");
        assert!(accounts(&state).is_empty() && sessions(&state).is_empty());
    }
}
