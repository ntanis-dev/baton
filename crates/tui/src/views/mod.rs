//! What each kind of sidebar item shows in the right pane: a title, a note and a body.

pub mod account;
pub mod common;
pub mod overview;
pub mod policy;
pub mod session;
pub mod stats;

use crate::pane::{Item, PaneBox, ViewModel};
use crate::state::State;

/// The pane's content for an item. The state has its data.
pub fn model(state: &State, item: &Item, pane: &PaneBox) -> ViewModel {
    let width = pane.room.cols;
    match item {
        Item::Empty => ViewModel::default(),
        Item::Overview => overview::model(state, width),
        Item::Account(id) => account::model(state, id, width),
        Item::Session(id) => session::model(state, id, width),
        Item::Sessions => session::recent_model(state, width),
        Item::Policy => policy::model(state, width),
        Item::Stats => stats::model(state, width),
    }
}
