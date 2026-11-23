//! The selection's menu commands and the effects the runtime carries out through the same core
//! functions as the CLI. Accounts are named by their id, so a rename in between never acts on the
//! wrong one.

use baton_core::text::span;

use crate::keys::Key;
use crate::pane::Item;
use crate::state::{
    ActionOption, ChoiceOption, Choose, Effect, Modal, ModalKind, Op, Outcome, State, Submit, Tone,
    account_by_id, accounts, config_of, result, result_with, session_by_id, signed_out, toast,
};

/// The commands an item offers: the key and what it does, in a few words.
pub fn item_actions(state: &State, item: &Item) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    match item {
        Item::Account(id) => {
            let Some(a) = account_by_id(state, id) else {
                return out;
            };
            let last = accounts(state).len().saturating_sub(1);
            if a.index > 0 {
                out.push(("K", "up"));
            }
            if a.index < last {
                out.push(("J", "down"));
            }
            out.push(("r", "usage now"));
            out.push(("e", "edit name"));
            out.push(("t", "thresholds"));
            out.push((
                "s",
                if signed_out(state, &a.id) {
                    "sign in"
                } else {
                    "sign in again"
                },
            ));
            out.push(("a", "add"));
            out.push(("d", "remove"));
        }
        Item::Session(id) => {
            if session_by_id(state, id).is_some_and(|x| x.live) {
                out.push(("h", "hop"));
                out.push(("H", "hop to"));
            }
        }
        Item::Overview => {
            out.push(("r", "usage now"));
            out.push(("a", "add account"));
        }
        _ => {}
    }
    out
}

/// A command of an Actions dialog.
fn option(label: &str, command: &str, key: &str) -> ActionOption {
    ActionOption {
        label: label.to_owned(),
        command: command.to_owned(),
        key: key.to_owned(),
    }
}

/// The state with a dialog open.
fn open(state: &State, modal: Modal) -> Outcome {
    let mut next = state.clone();
    next.modal = Some(modal);
    result(next)
}

/// Opens the Actions dialog of an item, when it has one.
pub fn open_actions(state: &State, item: &Item) -> Outcome {
    let Item::Account(id) = item else {
        let (title, options) = match item {
            Item::Overview => ("Overview actions", vec![option("Refresh all accounts", "r", "r")]),
            Item::Session(id) if session_by_id(state, id).is_some_and(|x| x.live) => (
                "Session actions",
                vec![
                    option("Move to best account", "h", "h"),
                    option("Move to an account", "H", "H"),
                ],
            ),
            _ => return result(state.clone()),
        };
        return open(
            state,
            Modal {
                title: title.to_owned(),
                lines: Vec::new(),
                scroll: None,
                kind: ModalKind::Actions {
                    item: item.clone(),
                    index: 0,
                    options,
                },
            },
        );
    };
    let Some(account) = account_by_id(state, id) else {
        return result(state.clone());
    };
    let offered = item_actions(state, item);
    let offers = |key: &str| offered.iter().any(|(k, _)| *k == key);
    let mut options = vec![
        option("Rename", "e", "e"),
        option("Change mark", "mode", "m"),
        option("Usage thresholds", "t", "t"),
        option("Refresh usage", "r", "r"),
        option(
            if signed_out(state, &account.id) {
                "Sign in"
            } else {
                "Sign in again"
            },
            "s",
            "s",
        ),
    ];
    if offers("K") {
        options.push(option("Move up", "K", "K"));
    }
    if offers("J") {
        options.push(option("Move down", "J", "J"));
    }
    options.push(option("Delete account", "d", "d"));
    open(
        state,
        Modal {
            title: "Account actions".to_owned(),
            lines: vec![account.name.clone()],
            scroll: None,
            kind: ModalKind::Actions {
                item: item.clone(),
                index: 0,
                options,
            },
        },
    )
}

/// Runs a command of the open Actions dialog.
pub fn run_action(state: &State, command: &str) -> Outcome {
    let Some(ModalKind::Actions { item, .. }) = state.modal.as_ref().map(|modal| &modal.kind) else {
        return result(state.clone());
    };
    let mut next = state.clone();
    next.modal = None;
    if command != "mode" {
        return act(&next, item, &Key::from_name(command)).unwrap_or_else(|| result(next));
    }
    let account = match item {
        Item::Account(id) => account_by_id(state, id),
        _ => None,
    };
    let Some(account) = account else {
        return result(next);
    };
    let options: Vec<ChoiceOption> = [
        ("Preferred", "preferred"),
        ("Normal", "normal"),
        ("Last resort", "last-resort"),
    ]
    .iter()
    .map(|(label, value)| ChoiceOption {
        label: (*label).to_owned(),
        value: (*value).to_owned(),
    })
    .collect();
    let index = options
        .iter()
        .position(|o| o.value == account.mark.as_str())
        .unwrap_or(0);
    open(
        &next,
        Modal {
            title: "Account mark".to_owned(),
            lines: vec![account.name.clone()],
            scroll: None,
            kind: ModalKind::Choice {
                options,
                index,
                choose: Choose::Mark {
                    id: account.id.clone(),
                    done: format!("Updated the mark of {}.", account.name),
                },
                submit_label: Some("Set mark".to_owned()),
            },
        },
    )
}

/// What a command key does on an item, or None for a key the item does not offer.
pub fn act(state: &State, item: &Item, key: &Key) -> Option<Outcome> {
    let name = key.name();
    let k = name.as_str();
    if !item_actions(state, item).iter().any(|(offered, _)| *offered == k) {
        return None;
    }
    if k == "a" {
        return Some(open(
            state,
            Modal {
                title: "Add an account".to_owned(),
                lines: vec![
                    "Any name you like (an email, words, any script). Baton makes its profile, then Claude Code's sign-in runs in this terminal.".to_owned(),
                ],
                scroll: None,
                kind: ModalKind::Input { value: String::new(), cursor: 0, submit: Submit::Add },
            },
        ));
    }
    // r: the account's usage now (the Overview: every account's).
    if k == "r" {
        let account = match item {
            Item::Account(id) => Some(id.clone()),
            _ => None,
        };
        return Some(result_with(
            state.clone(),
            vec![Effect::Refresh {
                account,
                quiet: false,
            }],
        ));
    }
    match item {
        Item::Account(account) => {
            let a = account_by_id(state, account)?;
            let (id, name) = (a.id.clone(), a.name.clone());
            let apply =
                |op: Op, done: String| Some(result_with(state.clone(), vec![Effect::apply(op, Some(done))]));
            match k {
                "K" => apply(Op::Move { id, position: a.index }, format!("{name} moved up.")),
                "J" => apply(Op::Move { id, position: a.index + 2 }, format!("{name} moved down.")),
                "e" => Some(open(
                    state,
                    Modal {
                        title: format!("Rename {name}"),
                        lines: vec![
                            "Any name you like (an email, words, any script). Its profile, sign-in, sessions and history stay as they are.".to_owned(),
                        ],
                        scroll: None,
                        kind: ModalKind::Input {
                            cursor: name.chars().count(),
                            value: name,
                            submit: Submit::Rename(id),
                        },
                    },
                )),
                "t" => {
                    let config = config_of(state, &id);
                    let policy = &state.data.as_ref()?.policy;
                    Some(open(
                        state,
                        Modal {
                            title: String::new(),
                            lines: Vec::new(),
                            scroll: None,
                            kind: ModalKind::Thresholds {
                                account: id,
                                account_name: name,
                                index: 0,
                                values: [config.and_then(|c| c.five_hour), config.and_then(|c| c.weekly)],
                                defaults: [policy.five_hour, policy.weekly],
                                typing: None,
                            },
                        },
                    ))
                }
                "s" => Some(result_with(state.clone(), vec![Effect::SignIn { account: id }])),
                "d" => {
                    if accounts(state).len() == 1 {
                        let words =
                            format!("{name} is the only account and Baton needs one: add another first.");
                        return Some(result(toast(state, &words, Tone::Error)));
                    }
                    let mut lines = vec![
                        "Its profile folder stays (it holds that account's sign-in); delete it yourself when you no longer need it.".to_owned(),
                    ];
                    let running = state
                        .data
                        .as_ref()?
                        .sessions
                        .iter()
                        .any(|x| x.live && x.account.as_deref() == Some(id.as_str()));
                    if running {
                        lines.push(format!("A session runs on {name} now; it keeps running."));
                    }
                    Some(open(
                        state,
                        Modal {
                            title: format!("Remove {name} from Baton?"),
                            lines,
                            scroll: None,
                            kind: ModalKind::Confirm {
                                yes: false,
                                focus: None,
                                effect: Effect::apply(Op::Remove { id }, Some(format!("Removed {name}."))),
                            },
                        },
                    ))
                }
                _ => None,
            }
        }
        Item::Session(session) => {
            let x = session_by_id(state, session)?;
            if k == "h" {
                return Some(result_with(
                    state.clone(),
                    vec![Effect::apply(
                        Op::Hop {
                            session: x.id.clone(),
                            to: None,
                        },
                        None,
                    )],
                ));
            }
            if k == "H" {
                let options: Vec<ChoiceOption> = accounts(state)
                    .iter()
                    .filter(|a| Some(a.id.as_str()) != x.account.as_deref())
                    .map(|a| a.for_model(x.model.as_deref(), state.now))
                    .map(|a| {
                        let blocked = if a.blocked {
                            let back = match a.available_at.filter(|_| a.available_known) {
                                Some(at) => span(Some(at - state.now)),
                                None => String::new(),
                            };
                            format!("  blocked {back}").trim_end().to_owned()
                        } else {
                            String::new()
                        };
                        ChoiceOption {
                            value: a.id.clone(),
                            label: format!("{}{blocked}", a.name),
                        }
                    })
                    .collect();
                if options.is_empty() {
                    return None;
                }
                return Some(open(
                    state,
                    Modal {
                        title: format!("Move {} at the end of its turn to", x.name),
                        lines: Vec::new(),
                        scroll: None,
                        kind: ModalKind::Choice {
                            options,
                            index: 0,
                            choose: Choose::HopTo {
                                session: x.id.clone(),
                            },
                            submit_label: None,
                        },
                    },
                ));
            }
            None
        }
        _ => None,
    }
}
