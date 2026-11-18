//! Running post-link hooks and reporting what they did.

use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Serialize;

use crate::package::PackageName;
use crate::plan::HookPlan;

/// Which output stream a line of hook output came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HookOutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Receives hook progress as it happens. Core never prints; a frontend
/// implements this to show output live.
pub trait HookObserver {
    /// A hook is about to run.
    fn started(&mut self, hook: &HookPlan);

    /// The running hook wrote a line, without its trailing newline.
    fn output(&mut self, hook: &HookPlan, stream: HookOutputStream, line: &str);

    /// The system package installer wrote a line, without its trailing
    /// newline. Called while the install runs, before any hook starts.
    ///
    /// Does nothing by default, so an observer that only cares about hooks
    /// needs no change.
    fn install_output(&mut self, _stream: HookOutputStream, _line: &str) {}
}

/// An observer that ignores everything, for callers that only want the report.
#[derive(Debug, Clone, Copy, Default)]
pub struct SilentObserver;

impl HookObserver for SilentObserver {
    fn started(&mut self, _hook: &HookPlan) {}

    fn output(&mut self, _hook: &HookPlan, _stream: HookOutputStream, _line: &str) {}
}

/// One line a hook wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputLine {
    /// Which stream it came from.
    pub stream: HookOutputStream,
    /// The line, without its trailing newline.
    pub text: String,
}

/// How a hook turned out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum HookResult {
    /// The script exited with status 0.
    Succeeded,
    /// The script ran and failed.
    Failed {
        /// The exit code, or `None` if it was killed by a signal.
        exit_code: Option<i32>,
    },
    /// The script could not be started.
    NotRunnable {
        /// Why, such as that it does not exist or is not executable.
        reason: String,
    },
}

/// A hook and what happened when it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookOutcome {
    /// The package whose hook this is.
    pub package: PackageName,
    /// How it turned out.
    pub result: HookResult,
    /// Everything it wrote, in the order it was received.
    pub output: Vec<OutputLine>,
}

/// Runs every hook in order, streaming output to `observer`.
///
/// A hook that fails does not stop the ones after it. Each hook runs as the
/// current user, in its package directory, with `PUDDLEJUMP_PACKAGE`,
/// `PUDDLEJUMP_TARGET`, and `PUDDLEJUMP_REPO` added to the inherited
/// environment, and with stdin closed.
///
/// Waits for the script's output streams to close as well as for it to exit,
/// so a hook that leaves a background process holding its output open makes
/// this wait for that process.
pub fn run_hooks(hooks: &[HookPlan], observer: &mut dyn HookObserver) -> Vec<HookOutcome> {
    hooks
        .iter()
        .map(|hook| {
            observer.started(hook);
            run_hook(hook, observer)
        })
        .collect()
}

fn run_hook(hook: &HookPlan, observer: &mut dyn HookObserver) -> HookOutcome {
    let outcome = |result, output| HookOutcome {
        package: hook.package.clone(),
        result,
        output,
    };
    if let Err(reason) = check_runnable(&hook.script) {
        return outcome(HookResult::NotRunnable { reason }, Vec::new());
    }
    let mut child = match spawn_hook(hook) {
        Ok(child) => child,
        Err(error) => {
            let reason = format!("cannot run {}: {error}", hook.script.display());
            return outcome(HookResult::NotRunnable { reason }, Vec::new());
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        // Both were requested as pipes above, so this cannot happen; report it
        // rather than panic.
        let _ = child.kill();
        let _ = child.wait();
        let reason = "could not capture the hook's output".to_string();
        return outcome(HookResult::NotRunnable { reason }, Vec::new());
    };
    let output = stream_output(hook, stdout, stderr, observer);
    let result = match child.wait() {
        Ok(status) if status.success() => HookResult::Succeeded,
        Ok(status) => HookResult::Failed {
            exit_code: status.code(),
        },
        Err(error) => HookResult::NotRunnable {
            reason: format!("cannot wait for {}: {error}", hook.script.display()),
        },
    };
    outcome(result, output)
}

/// `ETXTBSY`: the script is open for writing somewhere, such as an editor
/// saving it, so the kernel refuses to execute it for the moment.
const TEXT_FILE_BUSY: i32 = 26;

/// How many times to try starting a hook that is briefly busy.
const START_ATTEMPTS: usize = 100;

/// How long to wait between attempts.
const START_RETRY_DELAY: Duration = Duration::from_millis(5);

/// Starts the hook's script with its output piped.
///
/// Retries a bounded number of times if the script is briefly busy. A spawn
/// that returns an error never started the script, so trying again cannot run
/// a hook twice.
fn spawn_hook(hook: &HookPlan) -> io::Result<Child> {
    let mut attempt = 1;
    loop {
        let spawned = Command::new(&hook.script)
            .current_dir(&hook.package_dir)
            .env("PUDDLEJUMP_PACKAGE", hook.package.as_str())
            .env("PUDDLEJUMP_TARGET", &hook.target_dir)
            .env("PUDDLEJUMP_REPO", &hook.repo_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match spawned {
            Err(error)
                if error.raw_os_error() == Some(TEXT_FILE_BUSY) && attempt < START_ATTEMPTS =>
            {
                attempt += 1;
                thread::sleep(START_RETRY_DELAY);
            }
            other => return other,
        }
    }
}

/// Why `script` cannot be run, checked up front so the message is clear.
fn check_runnable(script: &Path) -> Result<(), String> {
    let metadata = fs::metadata(script).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            format!("{} does not exist", script.display())
        } else {
            format!("cannot read {}: {error}", script.display())
        }
    })?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", script.display()));
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not executable", script.display()));
    }
    Ok(())
}

/// Reads both streams on their own threads and forwards each line to the
/// observer on the calling thread, so the observer needs no `Send`. Returns
/// every line in the order it was received.
fn stream_output(
    hook: &HookPlan,
    stdout: impl Read + Send,
    stderr: impl Read + Send,
    observer: &mut dyn HookObserver,
) -> Vec<OutputLine> {
    let (sender, receiver) = mpsc::channel();
    let mut collected = Vec::new();
    thread::scope(|scope| {
        scope.spawn({
            let sender = sender.clone();
            move || forward_lines(stdout, HookOutputStream::Stdout, &sender)
        });
        scope.spawn(move || forward_lines(stderr, HookOutputStream::Stderr, &sender));
        // The loop ends when both reader threads finish and drop their senders.
        for line in receiver {
            observer.output(hook, line.stream, &line.text);
            collected.push(line);
        }
    });
    collected
}

fn forward_lines(reader: impl Read, stream: HookOutputStream, sender: &mpsc::Sender<OutputLine>) {
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
        // Hooks can write anything, so invalid UTF-8 is replaced, not fatal.
        let text = String::from_utf8_lossy(&buffer).into_owned();
        if sender.send(OutputLine { stream, text }).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_scripts::write_script;
    use std::path::PathBuf;

    /// Records everything the observer is told.
    #[derive(Default)]
    struct Recorder {
        started: Vec<String>,
        lines: Vec<(String, HookOutputStream, String)>,
    }

    impl HookObserver for Recorder {
        fn started(&mut self, hook: &HookPlan) {
            self.started.push(hook.package.to_string());
        }

        fn output(&mut self, hook: &HookPlan, stream: HookOutputStream, line: &str) {
            self.lines
                .push((hook.package.to_string(), stream, line.to_string()));
        }
    }

    /// A package directory with a hook script written into it.
    struct Fixture {
        _root: tempfile::TempDir,
        hook: HookPlan,
    }

    fn fixture(package: &str, body: &str) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let package_dir = root.path().join("packages").join(package);
        fs::create_dir_all(&package_dir).unwrap();
        let script = write_script(&package_dir, "hook.sh", body, 0o755);
        let hook = HookPlan {
            package: PackageName::new(package).unwrap(),
            reason: crate::hook_state::HookReason::NeverRun,
            script,
            package_dir,
            target_dir: root.path().join("home"),
            repo_root: root.path().to_path_buf(),
        };
        Fixture { _root: root, hook }
    }

    fn run(fixture: &Fixture) -> (HookOutcome, Recorder) {
        let mut recorder = Recorder::default();
        let mut outcomes = run_hooks(std::slice::from_ref(&fixture.hook), &mut recorder);
        (outcomes.remove(0), recorder)
    }

    fn texts(outcome: &HookOutcome, stream: HookOutputStream) -> Vec<&str> {
        outcome
            .output
            .iter()
            .filter(|line| line.stream == stream)
            .map(|line| line.text.as_str())
            .collect()
    }

    #[test]
    fn a_script_that_exits_zero_succeeded() {
        let (outcome, _) = run(&fixture("dwm", "exit 0"));
        assert_eq!(outcome.result, HookResult::Succeeded);
        assert_eq!(outcome.package.as_str(), "dwm");
    }

    #[test]
    fn a_script_that_exits_non_zero_failed_with_its_exit_code() {
        let (outcome, _) = run(&fixture("dwm", "exit 7"));
        assert_eq!(outcome.result, HookResult::Failed { exit_code: Some(7) });
    }

    #[test]
    fn a_script_killed_by_a_signal_failed_without_an_exit_code() {
        let (outcome, _) = run(&fixture("dwm", "kill -9 $$"));
        assert_eq!(outcome.result, HookResult::Failed { exit_code: None });
    }

    #[test]
    fn stdout_and_stderr_are_both_collected_in_order_per_stream() {
        let (outcome, _) = run(&fixture(
            "dwm",
            "echo one; echo two; echo oops >&2; echo three",
        ));

        assert_eq!(
            texts(&outcome, HookOutputStream::Stdout),
            ["one", "two", "three"]
        );
        assert_eq!(texts(&outcome, HookOutputStream::Stderr), ["oops"]);
    }

    #[test]
    fn the_observer_receives_every_collected_line() {
        let (outcome, recorder) = run(&fixture("dwm", "echo one; echo oops >&2"));

        let streamed: Vec<(HookOutputStream, String)> = recorder
            .lines
            .iter()
            .map(|(_, stream, text)| (*stream, text.clone()))
            .collect();
        let collected: Vec<(HookOutputStream, String)> = outcome
            .output
            .iter()
            .map(|line| (line.stream, line.text.clone()))
            .collect();
        assert_eq!(streamed, collected);
        assert_eq!(streamed.len(), 2);
    }

    #[test]
    fn the_observer_is_told_which_hook_each_line_belongs_to() {
        let (_, recorder) = run(&fixture("dwm", "echo hi"));

        assert_eq!(recorder.started, ["dwm"]);
        assert_eq!(
            recorder.lines,
            [(
                "dwm".to_string(),
                HookOutputStream::Stdout,
                "hi".to_string()
            )]
        );
    }

    /// The observer must see a line while the script is still running, not
    /// after it exits. The script waits for a file that the observer creates
    /// when it receives the first line, so the script can only finish if
    /// output was delivered live.
    #[test]
    fn output_reaches_the_observer_before_the_script_exits() {
        struct Acknowledging(PathBuf);
        impl HookObserver for Acknowledging {
            fn started(&mut self, _hook: &HookPlan) {}
            fn output(&mut self, _hook: &HookPlan, _stream: HookOutputStream, line: &str) {
                if line == "ready" {
                    fs::write(&self.0, "").unwrap();
                }
            }
        }
        let fixture = fixture(
            "dwm",
            "echo ready\ni=0\nwhile [ ! -e \"$PUDDLEJUMP_REPO/ack\" ] && [ $i -lt 500 ]; do\n  i=$((i+1)); sleep 0.01\ndone\n[ -e \"$PUDDLEJUMP_REPO/ack\" ]",
        );
        let ack = fixture.hook.repo_root.join("ack");

        let outcomes = run_hooks(std::slice::from_ref(&fixture.hook), &mut Acknowledging(ack));

        assert_eq!(outcomes[0].result, HookResult::Succeeded);
    }

    #[test]
    fn a_line_without_a_trailing_newline_is_still_delivered() {
        let (outcome, _) = run(&fixture("dwm", "printf 'no newline'"));
        assert_eq!(texts(&outcome, HookOutputStream::Stdout), ["no newline"]);
    }

    #[test]
    fn blank_lines_are_preserved() {
        let (outcome, _) = run(&fixture("dwm", "echo a; echo; echo b"));
        assert_eq!(texts(&outcome, HookOutputStream::Stdout), ["a", "", "b"]);
    }

    #[test]
    fn invalid_utf8_output_is_replaced_not_fatal() {
        let (outcome, _) = run(&fixture("dwm", "printf 'bad \\377 byte\\n'"));

        assert_eq!(outcome.result, HookResult::Succeeded);
        assert_eq!(
            texts(&outcome, HookOutputStream::Stdout),
            ["bad \u{fffd} byte"]
        );
    }

    #[test]
    fn a_script_that_writes_a_lot_does_not_deadlock() {
        let (outcome, _) = run(&fixture(
            "dwm",
            "i=0; while [ $i -lt 20000 ]; do echo \"line $i\"; echo \"err $i\" >&2; i=$((i+1)); done",
        ));

        assert_eq!(outcome.result, HookResult::Succeeded);
        assert_eq!(texts(&outcome, HookOutputStream::Stdout).len(), 20000);
        assert_eq!(texts(&outcome, HookOutputStream::Stderr).len(), 20000);
    }

    #[test]
    fn the_script_runs_in_the_package_directory() {
        let fixture = fixture("dwm", "pwd");

        let (outcome, _) = run(&fixture);

        let printed = texts(&outcome, HookOutputStream::Stdout)[0].to_string();
        assert_eq!(
            fs::canonicalize(printed).unwrap(),
            fs::canonicalize(&fixture.hook.package_dir).unwrap()
        );
    }

    #[test]
    fn the_puddlejump_environment_variables_are_set() {
        let fixture = fixture(
            "dwm",
            "echo \"$PUDDLEJUMP_PACKAGE\"; echo \"$PUDDLEJUMP_TARGET\"; echo \"$PUDDLEJUMP_REPO\"",
        );

        let (outcome, _) = run(&fixture);

        assert_eq!(
            texts(&outcome, HookOutputStream::Stdout),
            [
                "dwm",
                fixture.hook.target_dir.to_str().unwrap(),
                fixture.hook.repo_root.to_str().unwrap(),
            ]
        );
    }

    #[test]
    fn the_inherited_environment_is_kept() {
        let (outcome, _) = run(&fixture("dwm", "[ -n \"$PATH\" ]"));
        assert_eq!(outcome.result, HookResult::Succeeded);
    }

    #[test]
    fn stdin_is_closed_so_a_script_that_reads_it_sees_end_of_input() {
        let (outcome, _) = run(&fixture("dwm", "cat; echo done"));
        assert_eq!(texts(&outcome, HookOutputStream::Stdout), ["done"]);
    }

    #[test]
    fn a_missing_script_is_not_runnable() {
        let fixture = fixture("dwm", "exit 0");
        fs::remove_file(&fixture.hook.script).unwrap();

        let (outcome, recorder) = run(&fixture);

        assert!(matches!(
            outcome.result,
            HookResult::NotRunnable { ref reason } if reason.ends_with("does not exist")
        ));
        assert!(outcome.output.is_empty());
        assert_eq!(recorder.started, ["dwm"]);
    }

    #[test]
    fn a_script_that_is_not_executable_is_not_runnable() {
        let fixture = fixture("dwm", "exit 0");
        fs::set_permissions(&fixture.hook.script, fs::Permissions::from_mode(0o644)).unwrap();

        let (outcome, _) = run(&fixture);

        assert!(matches!(
            outcome.result,
            HookResult::NotRunnable { ref reason } if reason.ends_with("is not executable")
        ));
    }

    #[test]
    fn a_hook_that_is_a_directory_is_not_runnable() {
        let fixture = fixture("dwm", "exit 0");
        fs::remove_file(&fixture.hook.script).unwrap();
        fs::create_dir(&fixture.hook.script).unwrap();

        let (outcome, _) = run(&fixture);

        assert!(matches!(
            outcome.result,
            HookResult::NotRunnable { ref reason } if reason.ends_with("is not a file")
        ));
    }

    #[test]
    fn a_script_with_a_missing_interpreter_is_not_runnable() {
        let fixture = fixture("dwm", "exit 0");
        fs::write(&fixture.hook.script, "#!/no/such/interpreter\n").unwrap();

        let (outcome, _) = run(&fixture);

        assert!(matches!(outcome.result, HookResult::NotRunnable { .. }));
    }

    /// Holds `script` open for writing, which makes the kernel refuse to
    /// execute it with `ETXTBSY` until the handle is dropped.
    fn hold_open_for_writing(script: &Path) -> fs::File {
        fs::OpenOptions::new().append(true).open(script).unwrap()
    }

    #[test]
    fn a_script_that_is_briefly_busy_is_started_once_it_is_free() {
        let fixture = fixture("dwm", "echo started");
        let writer = hold_open_for_writing(&fixture.hook.script);

        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(writer);
        });
        let (outcome, _) = run(&fixture);
        release.join().unwrap();

        assert_eq!(outcome.result, HookResult::Succeeded);
        assert_eq!(texts(&outcome, HookOutputStream::Stdout), ["started"]);
    }

    #[test]
    fn a_script_that_stays_busy_is_reported_as_not_runnable_rather_than_hanging() {
        let fixture = fixture("dwm", "echo started");
        let _writer = hold_open_for_writing(&fixture.hook.script);

        let (outcome, _) = run(&fixture);

        assert!(matches!(
            outcome.result,
            HookResult::NotRunnable { ref reason } if reason.contains("Text file busy")
        ));
        assert!(outcome.output.is_empty());
    }

    #[test]
    fn a_hook_that_was_busy_is_not_run_twice() {
        let log = tempfile::tempdir().unwrap();
        let marker = log.path().join("runs");
        let fixture = fixture("dwm", &format!("echo run >> {}", marker.display()));
        let writer = hold_open_for_writing(&fixture.hook.script);

        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(writer);
        });
        let (outcome, _) = run(&fixture);
        release.join().unwrap();

        assert_eq!(outcome.result, HookResult::Succeeded);
        assert_eq!(fs::read_to_string(&marker).unwrap(), "run\n");
    }

    #[test]
    fn a_failing_hook_does_not_stop_the_hooks_after_it() {
        let first = fixture("first", "echo first; exit 1");
        let second = fixture("second", "echo second");
        let hooks = [first.hook.clone(), second.hook.clone()];

        let mut recorder = Recorder::default();
        let outcomes = run_hooks(&hooks, &mut recorder);

        assert_eq!(
            outcomes[0].result,
            HookResult::Failed { exit_code: Some(1) }
        );
        assert_eq!(outcomes[1].result, HookResult::Succeeded);
        assert_eq!(recorder.started, ["first", "second"]);
    }

    #[test]
    fn hooks_run_in_the_order_given_and_outcomes_keep_that_order() {
        let a = fixture("a", "echo a");
        let b = fixture("b", "echo b");
        let c = fixture("c", "echo c");
        let hooks = [c.hook.clone(), a.hook.clone(), b.hook.clone()];

        let mut recorder = Recorder::default();
        let outcomes = run_hooks(&hooks, &mut recorder);

        let order: Vec<&str> = outcomes.iter().map(|o| o.package.as_str()).collect();
        assert_eq!(order, ["c", "a", "b"]);
        assert_eq!(recorder.started, ["c", "a", "b"]);
    }

    #[test]
    fn running_no_hooks_produces_no_outcomes() {
        let mut recorder = Recorder::default();
        assert!(run_hooks(&[], &mut recorder).is_empty());
        assert!(recorder.started.is_empty());
    }

    #[test]
    fn the_silent_observer_still_gets_a_full_report() {
        let fixture = fixture("dwm", "echo hi");

        let outcomes = run_hooks(std::slice::from_ref(&fixture.hook), &mut SilentObserver);

        assert_eq!(texts(&outcomes[0], HookOutputStream::Stdout), ["hi"]);
    }
}
