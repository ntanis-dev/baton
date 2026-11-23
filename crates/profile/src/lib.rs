//! Baton's account profiles: each account is a Claude Code profile in `~/.baton/accounts/<id>` that
//! shares everything but the sign-in with `~/.claude`, and the operations that add, rename, order and
//! remove accounts.

pub mod accounts;
pub mod claude_json;
pub mod merge3;
pub mod profile;
pub mod shared;
pub mod sync;
