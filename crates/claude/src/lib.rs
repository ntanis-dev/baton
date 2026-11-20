//! Running Claude Code for Baton: reading and rebuilding its command line, ending it gently, the
//! one-line key prompt, the questions about settings the accounts differ on, the sign-in check, the usage read and its poller, the lease that makes one
//! baton instance poll at a time, and the session wrapper that moves a conversation from account to
//! account.

pub mod args;
pub mod ask;
pub mod auth;
pub mod countdown;
pub mod lead;
pub mod poll_host;
pub mod poller;
pub mod probe;
mod proc;
pub mod terminate;
