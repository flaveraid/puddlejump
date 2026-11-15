//! Running external commands, behind a seam so tests can assert exact argv.

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// Which output stream of a command a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Receives each line a command writes, without its trailing newline.
pub type LineCallback<'a> = &'a mut dyn FnMut(Stream, &str);

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

    /// Like [`CommandRunner::run`], but also hands each line of output to
    /// `on_line` while the command is still running, so a caller can show
    /// progress. The full output is still returned.
    ///
    /// The default runs the command to completion and then replays its output
    /// line by line, which keeps runners that cannot stream (such as test
    /// fakes) working; it only loses the live timing.
    ///
    /// Errors if the program cannot be started.
    fn run_streaming(
        &self,
        program: &str,
        args: &[String],
        on_line: LineCallback,
    ) -> io::Result<CommandOutput> {
        let output = self.run(program, args)?;
        replay(&output.stdout, Stream::Stdout, on_line);
        replay(&output.stderr, Stream::Stderr, on_line);
        Ok(output)
    }
}

fn replay(text: &str, stream: Stream, on_line: LineCallback) {
    for line in text.lines() {
        on_line(stream, line);
    }
}

impl<R: CommandRunner + ?Sized> CommandRunner for &R {
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
        (**self).run(program, args)
    }

    fn run_streaming(
        &self,
        program: &str,
        args: &[String],
        on_line: LineCallback,
    ) -> io::Result<CommandOutput> {
        (**self).run_streaming(program, args, on_line)
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

    /// Reads both streams on their own threads and forwards each line to
    /// `on_line` on the calling thread, so the callback needs no `Send`.
    fn run_streaming(
        &self,
        program: &str,
        args: &[String],
        on_line: LineCallback,
    ) -> io::Result<CommandOutput> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            // Both were requested as pipes above, so this cannot happen; report
            // it rather than panic.
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("could not capture the command's output"));
        };
        let (stdout_text, stderr_text) = forward_output(stdout, stderr, on_line);
        let status = child.wait()?;
        Ok(CommandOutput {
            exit_code: status.code(),
            stdout: stdout_text,
            stderr: stderr_text,
        })
    }
}

/// Forwards every line of both streams to `on_line` as it arrives, and returns
/// each stream's text.
fn forward_output(
    stdout: impl Read + Send,
    stderr: impl Read + Send,
    on_line: LineCallback,
) -> (String, String) {
    let (sender, receiver) = mpsc::channel();
    let (mut out_text, mut err_text) = (String::new(), String::new());
    thread::scope(|scope| {
        scope.spawn({
            let sender = sender.clone();
            move || read_lines(stdout, Stream::Stdout, &sender)
        });
        scope.spawn(move || read_lines(stderr, Stream::Stderr, &sender));
        // The loop ends when both reader threads finish and drop their senders.
        for (stream, line) in receiver {
            on_line(stream, &line);
            let text = match stream {
                Stream::Stdout => &mut out_text,
                Stream::Stderr => &mut err_text,
            };
            text.push_str(&line);
            text.push('\n');
        }
    });
    (out_text, err_text)
}

fn read_lines(reader: impl Read, stream: Stream, sender: &mpsc::Sender<(Stream, String)>) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        // Tools can write anything, so invalid UTF-8 is replaced, not fatal.
        let line = String::from_utf8_lossy(&buffer).into_owned();
        if sender.send((stream, line)).is_err() {
            return;
        }
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
        streamed: RefCell<Vec<bool>>,
    }

    impl FakeCommandRunner {
        /// A runner whose commands all succeed with no output.
        pub fn new() -> Self {
            Self {
                commands: RefCell::new(Vec::new()),
                replies: RefCell::new(VecDeque::new()),
                streamed: RefCell::new(Vec::new()),
            }
        }

        /// For each command run so far, whether it went through
        /// `run_streaming` (true) or plain `run` (false).
        pub fn streamed(&self) -> Vec<bool> {
            self.streamed.borrow().clone()
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
        fn run_streaming(
            &self,
            program: &str,
            args: &[String],
            on_line: LineCallback,
        ) -> io::Result<CommandOutput> {
            // The trait default runs `run` and replays its output; `run` logs
            // this call as buffered, so correct the record afterwards.
            let result = CommandRunner::run(self, program, args).inspect(|output| {
                replay(&output.stdout, Stream::Stdout, on_line);
                replay(&output.stderr, Stream::Stderr, on_line);
            });
            if let Some(last) = self.streamed.borrow_mut().last_mut() {
                *last = true;
            }
            result
        }

        fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
            self.streamed.borrow_mut().push(false);
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

    /// Collects every line a streaming command writes.
    fn stream(program: &str, script: &str) -> (io::Result<CommandOutput>, Vec<(Stream, String)>) {
        let mut lines = Vec::new();
        let result = SystemCommandRunner.run_streaming(
            program,
            &args(&["-c", script]),
            &mut |stream, line| lines.push((stream, line.to_string())),
        );
        (result, lines)
    }

    #[test]
    fn streaming_gives_each_line_to_the_callback_as_it_is_written() {
        let (result, lines) = stream("sh", "echo one; echo two");

        assert!(result.unwrap().succeeded());
        assert_eq!(
            lines,
            [
                (Stream::Stdout, "one".to_string()),
                (Stream::Stdout, "two".to_string())
            ]
        );
    }

    #[test]
    fn streaming_tells_stdout_and_stderr_apart() {
        let (_, lines) = stream("sh", "echo out; echo err >&2");

        assert!(lines.contains(&(Stream::Stdout, "out".to_string())));
        assert!(lines.contains(&(Stream::Stderr, "err".to_string())));
    }

    #[test]
    fn streaming_still_captures_the_output_for_the_caller() {
        let (result, _) = stream("sh", "echo out; echo err >&2");
        let output = result.unwrap();

        assert_eq!(output.stdout, "out\n");
        assert_eq!(output.stderr, "err\n");
    }

    #[test]
    fn streaming_reports_a_failing_exit_code() {
        let (result, lines) = stream("sh", "echo boom >&2; exit 4");

        assert_eq!(result.unwrap().exit_code, Some(4));
        assert_eq!(lines, [(Stream::Stderr, "boom".to_string())]);
    }

    #[test]
    fn streaming_delivers_a_final_line_with_no_newline() {
        let (_, lines) = stream("sh", "printf partial");

        assert_eq!(lines, [(Stream::Stdout, "partial".to_string())]);
    }

    #[test]
    fn streaming_replaces_invalid_utf8_instead_of_failing() {
        let (result, lines) = stream("sh", "printf 'a\\377b\\n'");

        assert!(result.unwrap().succeeded());
        assert_eq!(lines, [(Stream::Stdout, "a\u{fffd}b".to_string())]);
    }

    #[test]
    fn streaming_a_program_that_does_not_exist_is_an_error() {
        let (result, lines) = stream("puddlejump-no-such-program", "");

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(lines.is_empty());
    }

    #[test]
    fn streaming_closes_stdin_so_a_command_that_reads_it_sees_end_of_input() {
        let (_, lines) = stream("sh", "cat; echo done");

        assert_eq!(lines, [(Stream::Stdout, "done".to_string())]);
    }

    #[test]
    fn the_default_streaming_run_replays_a_buffered_run_to_the_callback() {
        // A runner that only implements `run` still streams, after the fact.
        struct Buffered;
        impl CommandRunner for Buffered {
            fn run(&self, _: &str, _: &[String]) -> io::Result<CommandOutput> {
                Ok(CommandOutput {
                    exit_code: Some(0),
                    stdout: "a\nb\n".to_string(),
                    stderr: "c\n".to_string(),
                })
            }
        }
        let mut lines = Vec::new();

        Buffered
            .run_streaming("x", &[], &mut |stream, line| {
                lines.push((stream, line.to_string()))
            })
            .unwrap();

        assert_eq!(
            lines,
            [
                (Stream::Stdout, "a".to_string()),
                (Stream::Stdout, "b".to_string()),
                (Stream::Stderr, "c".to_string()),
            ]
        );
    }

    #[test]
    fn stdin_is_closed_so_a_command_that_reads_it_sees_end_of_input() {
        let output = SystemCommandRunner
            .run("sh", &args(&["-c", "cat; echo done"]))
            .unwrap();

        assert_eq!(output.stdout, "done\n");
    }
}
