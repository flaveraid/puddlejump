//! A [`FileWriter`] that makes its changes through external commands, so they
//! can run as root without puddlejump itself being root.

use std::fs;
use std::io;
use std::path::Path;

use super::command::{CommandOutput, CommandRunner};
use super::privilege::Privilege;
use crate::apply::{FileWriter, RemoveDirectoryOutcome};

/// Creates and removes links with `ln`, `rm`, `mkdir`, and `rmdir`, run
/// through a [`Privilege`].
#[derive(Debug)]
pub struct PrivilegedFileWriter<R> {
    runner: R,
    privilege: Privilege,
}

impl<R: CommandRunner> PrivilegedFileWriter<R> {
    /// Creates a writer that runs commands with `runner`, getting root the way
    /// `privilege` says.
    pub fn new(runner: R, privilege: Privilege) -> Self {
        Self { runner, privilege }
    }

    fn run(&self, program: &str, args: &[&Path], flags: &[&str]) -> io::Result<()> {
        let arguments: Vec<String> = flags
            .iter()
            .map(|flag| flag.to_string())
            .chain(args.iter().map(|path| path.to_string_lossy().into_owned()))
            .collect();
        let (program, arguments) = self.privilege.wrap(program, &arguments);
        let output = self.runner.run(&program, &arguments)?;
        if output.succeeded() {
            Ok(())
        } else {
            Err(io::Error::other(failure_message(&program, &output)))
        }
    }
}

impl<R: CommandRunner> FileWriter for PrivilegedFileWriter<R> {
    fn create_symlink(&mut self, destination: &Path, link: &Path) -> io::Result<()> {
        // `--` stops a path that starts with a dash from being read as a flag.
        self.run("ln", &[destination, link], &["-s", "--"])
    }

    fn remove_symlink(&mut self, link: &Path) -> io::Result<()> {
        self.run("rm", &[link], &["--"])
    }

    fn create_directories(&mut self, path: &Path) -> io::Result<()> {
        self.run("mkdir", &[path], &["-p", "--"])
    }

    /// Looks inside the directory itself instead of reading `rmdir`'s error
    /// message, which is translated and so cannot be matched reliably.
    fn remove_directory_if_empty(&mut self, path: &Path) -> io::Result<RemoveDirectoryOutcome> {
        if fs::read_dir(path)?.next().is_some() {
            return Ok(RemoveDirectoryOutcome::NotEmpty);
        }
        self.run("rmdir", &[path], &["--"])?;
        Ok(RemoveDirectoryOutcome::Removed)
    }
}

/// A one-line description of why a command failed.
fn failure_message(program: &str, output: &CommandOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    match output.exit_code {
        Some(code) => format!("{program} exited with status {code}"),
        None => format!("{program} was killed by a signal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::command::fake::FakeCommandRunner;
    use std::path::PathBuf;

    fn writer(
        runner: FakeCommandRunner,
        privilege: Privilege,
    ) -> PrivilegedFileWriter<FakeCommandRunner> {
        PrivilegedFileWriter::new(runner, privilege)
    }

    #[test]
    fn creating_a_symlink_runs_ln_through_sudo() {
        let mut writer = writer(FakeCommandRunner::new(), Privilege::UseSudo);

        writer
            .create_symlink(Path::new("/repo/a.conf"), Path::new("/etc/a.conf"))
            .unwrap();

        assert_eq!(
            writer.runner.lines(),
            ["sudo ln -s -- /repo/a.conf /etc/a.conf"]
        );
    }

    #[test]
    fn creating_a_symlink_as_root_runs_ln_directly() {
        let mut writer = writer(FakeCommandRunner::new(), Privilege::AlreadyRoot);

        writer
            .create_symlink(Path::new("/repo/a.conf"), Path::new("/etc/a.conf"))
            .unwrap();

        assert_eq!(writer.runner.lines(), ["ln -s -- /repo/a.conf /etc/a.conf"]);
    }

    #[test]
    fn a_path_starting_with_a_dash_is_not_taken_for_a_flag() {
        let mut writer = writer(FakeCommandRunner::new(), Privilege::AlreadyRoot);

        writer.remove_symlink(Path::new("-rf")).unwrap();

        assert_eq!(writer.runner.lines(), ["rm -- -rf"]);
    }

    #[test]
    fn removing_a_symlink_runs_rm_through_sudo() {
        let mut writer = writer(FakeCommandRunner::new(), Privilege::UseSudo);

        writer.remove_symlink(Path::new("/etc/a.conf")).unwrap();

        assert_eq!(writer.runner.lines(), ["sudo rm -- /etc/a.conf"]);
    }

    #[test]
    fn creating_directories_runs_mkdir_with_parents() {
        let mut writer = writer(FakeCommandRunner::new(), Privilege::UseSudo);

        writer
            .create_directories(Path::new("/etc/sysctl.d"))
            .unwrap();

        assert_eq!(writer.runner.lines(), ["sudo mkdir -p -- /etc/sysctl.d"]);
    }

    #[test]
    fn a_failing_command_becomes_an_io_error_carrying_stderr() {
        let mut writer = writer(
            FakeCommandRunner::new().exit(1, "ln: File exists\n"),
            Privilege::AlreadyRoot,
        );

        let error = writer
            .create_symlink(Path::new("/a"), Path::new("/b"))
            .unwrap_err();

        assert_eq!(error.to_string(), "ln: File exists");
    }

    #[test]
    fn a_failing_command_with_no_stderr_names_the_program_and_status() {
        let mut writer = writer(FakeCommandRunner::new().exit(4, ""), Privilege::UseSudo);

        let error = writer.remove_symlink(Path::new("/a")).unwrap_err();

        assert_eq!(error.to_string(), "sudo exited with status 4");
    }

    #[test]
    fn a_command_that_cannot_start_is_an_error() {
        let mut writer = writer(
            FakeCommandRunner::new().reply(Err(io::Error::from(io::ErrorKind::NotFound))),
            Privilege::UseSudo,
        );

        let error = writer.remove_symlink(Path::new("/a")).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn removing_an_empty_directory_runs_rmdir() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("empty");
        fs::create_dir(&directory).unwrap();
        let mut writer = writer(FakeCommandRunner::new(), Privilege::UseSudo);

        let outcome = writer.remove_directory_if_empty(&directory).unwrap();

        assert_eq!(outcome, RemoveDirectoryOutcome::Removed);
        assert_eq!(
            writer.runner.lines(),
            [format!("sudo rmdir -- {}", directory.display())]
        );
    }

    #[test]
    fn a_directory_with_contents_is_left_alone_without_running_anything() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("keep"), "").unwrap();
        let mut writer = writer(FakeCommandRunner::new(), Privilege::UseSudo);

        let outcome = writer.remove_directory_if_empty(root.path()).unwrap();

        assert_eq!(outcome, RemoveDirectoryOutcome::NotEmpty);
        assert!(writer.runner.lines().is_empty());
    }

    #[test]
    fn a_directory_that_does_not_exist_reports_not_found_without_running_anything() {
        let root = tempfile::tempdir().unwrap();
        let missing: PathBuf = root.path().join("gone");
        let mut writer = writer(FakeCommandRunner::new(), Privilege::UseSudo);

        let error = writer.remove_directory_if_empty(&missing).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(writer.runner.lines().is_empty());
    }

    #[test]
    fn a_failing_rmdir_on_an_empty_directory_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = writer(
            FakeCommandRunner::new().exit(1, "rmdir: Permission denied\n"),
            Privilege::UseSudo,
        );

        let error = writer.remove_directory_if_empty(root.path()).unwrap_err();

        assert_eq!(error.to_string(), "rmdir: Permission denied");
    }
}
