//! Core library for puddlejump, a dotfiles manager for Linux.
//!
//! This crate holds all logic. It returns data and errors and knows nothing
//! about terminals or windows; frontends decide how to present results.

pub mod config;
pub mod graph;
pub mod host;
pub mod link;
pub mod manifest;
pub mod package;
pub mod plan;
pub mod system;
