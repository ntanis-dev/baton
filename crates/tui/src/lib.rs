//! The Baton menu: a full-screen terminal app over the accounts, sessions, policy and stats.
//!
//! The menu itself is pure: a typed [`state::State`], a reducer over it, and functions that draw it as
//! rows of spans. Around it, the runtime ([`run`]) owns the state and the terminal: it paints those rows
//! ([`paint`]), feeds keys, pastes, mouse events, resizes and ticks into the reducer ([`terminal`],
//! [`input`]), and does what the reducer asks for through the core functions ([`store`]). Claude Code's
//! sign-in gets the terminal with Baton's screen around it ([`signin`]).
//!
//! The two ways in are [`run_menu`] (`baton view`) and [`sign_in`] (`baton login <account>`).

pub mod actions;
pub mod console;
pub mod controls;
pub mod data;
pub mod fuzzy;
pub mod input;
pub mod keys;
pub mod layout;
pub mod legend;
pub mod modal;
pub mod mouse;
pub mod paint;
pub mod pane;
pub mod reducer;
pub mod run;
pub mod signin;
pub mod state;
pub mod store;
pub mod terminal;
pub mod tree;
pub mod ui;
pub mod views;
pub mod watch;

pub use run::{MenuOptions, run_menu};
pub use signin::{LoginStdio, SignInOptions, sign_in};
