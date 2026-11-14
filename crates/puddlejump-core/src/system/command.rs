//! Running external commands, behind a seam so tests can assert exact argv.

use std::io;
use std::process::{Command, Stdio};

/// What a finished command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// The exit code, or `None` if the command was killed by a signal.
    pub exit_code: Option<i32>,
    /// Everything the command wrote to stdout.
    pub stdout: String,
    /// Everything the command wrote to stderr.
    pub stderr: String,
}

impl CommandOutput {
    /// Whether the command exited with code 0.
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Runs external commands. Everything that shells out goes through this, so
/// tests can substitute a fake and assert the exact program and arguments.
pub trait CommandRunner {
    /// Runs `program` with `args` to completion and captures its output.
    ///
    /// Errors if the program cannot be started (for example, it is not
    /// installed). A program that starts and fails is `Ok` with a non-zero
    /// exit code.
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput>;
}

impl<R: CommandRunner + ?Sized> CommandRunner for &R {
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
        (**self).run(program, args)
    }
}

/// Runs commands for real, with stdin closed so nothing can wait for input.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        Ok(CommandOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// One command a [`FakeCommandRunner`] was asked to run.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RecordedCommand {
        pub program: String,
        pub args: Vec<String>,
    }

    /// A runner that records every command and replies from a queue.
    pub struct FakeCommandRunner {
        pub commands: RefCell<Vec<RecordedCommand>>,
        replies: RefCell<VecDeque<io::Result<CommandOutput>>>,
    }

    impl FakeCommandRunner {
        /// A runner whose commands all succeed with no output.
        pub fn new() -> Self {
            Self {
                commands: RefCell::new(Vec::new()),
                replies: RefCell::new(VecDeque::new()),
            }
        }

        /// Queues the reply to the next command.
        pub fn reply(self, reply: io::Result<CommandOutput>) -> Self {
            self.replies.borrow_mut().push_back(reply);
            self
        }

        /// Queues a reply with this exit code and stderr.
        pub fn exit(self, exit_code: i32, stderr: &str) -> Self {
            self.reply(Ok(CommandOutput {
                exit_code: Some(exit_code),
                stdout: String::new(),
                stderr: stderr.to_string(),
            }))
        }

        /// The programs and arguments run so far, as one line each.
        pub fn lines(&self) -> Vec<String> {
            self.commands
                .borrow()
                .iter()
                .map(|command| {
                    std::iter::once(command.program.as_str())
                        .chain(command.args.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect()
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
            self.commands.borrow_mut().push(RecordedCommand {
                program: program.to_string(),
                args: args.to_vec(),
            });
            self.replies.borrow_mut().pop_front().unwrap_or_else(|| {
                Ok(CommandOutput {
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn a_successful_command_reports_its_output_and_exit_code() {
        let output = SystemCommandRunner
            .run("sh", &args(&["-c", "echo out; echo err >&2"]))
            .unwrap();

        assert!(output.succeeded());
        assert_eq!(output.stdout, "out\n");
        assert_eq!(output.stderr, "err\n");
    }

    #[test]
    fn a_failing_command_reports_its_exit_code_without_erroring() {
        let output = SystemCommandRunner
            .run("sh", &args(&["-c", "exit 3"]))
            .unwrap();

        assert!(!output.succeeded());
        assert_eq!(output.exit_code, Some(3));
    }

    #[test]
    fn a_command_killed_by_a_signal_has_no_exit_code() {
        let output = SystemCommandRunner
            .run("sh", &args(&["-c", "kill -9 $$"]))
            .unwrap();

        assert_eq!(output.exit_code, None);
        assert!(!output.succeeded());
    }

    #[test]
    fn a_program_that_does_not_exist_is_an_error() {
        let error = SystemCommandRunner
            .run("puddlejump-no-such-program", &[])
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn stdin_is_closed_so_a_command_that_reads_it_sees_end_of_input() {
        let output = SystemCommandRunner
            .run("sh", &args(&["-c", "cat; echo done"]))
            .unwrap();

        assert_eq!(output.stdout, "done\n");
    }
}
