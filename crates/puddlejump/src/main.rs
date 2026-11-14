//! Command-line frontend for puddlejump: argument parsing and output only.

mod cli;
mod commands;
mod environment;
mod prompt;
mod render;

use std::io;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;
use crate::commands::{Environment, Terminal};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = Environment::from_process().and_then(|environment| {
        let stdout = io::stdout();
        let stderr = io::stderr();
        let stdin = io::stdin();
        let (mut out, mut err, mut input) = (stdout.lock(), stderr.lock(), stdin.lock());
        let mut terminal = Terminal {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        };
        commands::run(&cli, &environment, &mut terminal)
    });
    match result {
        Ok(outcome) => ExitCode::from(outcome.exit_code()),
        Err(error) => {
            eprintln!("error: {}", describe(&error));
            ExitCode::from(2)
        }
    }
}

/// The error chain as one line, leaving out any cause whose text the level
/// above already contains.
///
/// Core's errors put their cause in their own message, so printing the whole
/// chain would say everything twice.
fn describe(error: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in error.chain() {
        let text = cause.to_string();
        if !parts.iter().any(|earlier| earlier.contains(&text)) {
            parts.push(text);
        }
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use std::io;

    #[test]
    fn a_plain_error_is_just_its_message() {
        assert_eq!(describe(&anyhow::anyhow!("boom")), "boom");
    }

    #[test]
    fn context_and_cause_are_joined() {
        let error = Err::<(), _>(io::Error::other("disk full"))
            .context("cannot save")
            .unwrap_err();

        assert_eq!(describe(&error), "cannot save: disk full");
    }

    #[test]
    fn a_cause_already_in_the_message_above_is_not_repeated() {
        /// Like core's errors: puts its cause in its own message and also
        /// exposes it as the source.
        #[derive(Debug)]
        struct Wrapper {
            source: io::Error,
        }
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "cannot read x: {}", self.source)
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.source)
            }
        }
        let error = anyhow::Error::new(Wrapper {
            source: io::Error::other("denied"),
        })
        .context("cannot open the repo");

        assert_eq!(
            describe(&error),
            "cannot open the repo: cannot read x: denied"
        );
    }
}
