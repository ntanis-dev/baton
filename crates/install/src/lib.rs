//! What `baton install` puts into `~/.claude` and what Claude Code then runs: the hooks and the status
//! line, the /hop command, and Baton's registration with Ferry.

pub mod diff;
pub mod ferry;
pub mod hooks;
pub mod install;
mod proc;
