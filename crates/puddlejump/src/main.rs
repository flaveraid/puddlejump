//! Command-line frontend for puddlejump: argument parsing and output only.

use clap::Parser;

/// A dotfiles manager for Linux: Stow-style linking with Make-style dependencies.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {}

fn main() -> anyhow::Result<()> {
    Cli::parse();
    Ok(())
}
