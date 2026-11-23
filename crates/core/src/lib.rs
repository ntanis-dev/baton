//! The Baton model: where everything lives, the accounts and the policy, usage readings and refusals,
//! per-conversation state, and the text layout the commands and the menu share.

pub mod art;
pub mod charts;
pub mod claude_path;
pub mod clock;
pub mod config;
pub mod env;
pub mod format;
pub mod fsutil;
pub mod hop;
pub mod json;
pub mod lines;
pub mod names;
pub mod oslock;
pub mod paths;
pub mod policy;
pub mod poll;
pub mod prompts;
pub mod retention;
pub mod sessions;
pub mod state;
pub mod stats;
pub mod sys;
pub mod term;
pub mod text;
pub mod theme;
pub mod usage;
pub mod window;
