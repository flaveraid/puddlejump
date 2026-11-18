//! Core library for puddlejump, a dotfiles manager for Linux.
//!
//! This crate holds all logic. It returns data and errors and knows nothing
//! about terminals or windows; frontends decide how to present results.

pub mod adopt;
pub mod apply;
pub mod config;
pub mod execute;
pub mod graph;
pub mod hook;
pub mod hook_state;
pub mod host;
pub mod init;
pub mod link;
pub mod manifest;
pub mod package;
pub mod plan;
pub mod session;
pub mod state;
pub mod status;
pub mod system;
pub mod user_config;

#[cfg(test)]
mod test_scripts;
