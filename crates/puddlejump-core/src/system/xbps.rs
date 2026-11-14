//! The xbps backend (Void Linux). The only place that knows xbps commands.

use super::command::{CommandOutput, CommandRunner};
use super::privilege::Privilege;
use super::{ManagerId, PackageManager, SystemError};

/// `xbps-query` exits with this code when the package is not installed.
const NOT_INSTALLED_EXIT_CODE: i32 = 2;

/// Manages system packages with xbps.
#[derive(Debug)]
pub struct Xbps<R> {
    runner: R,
    privilege: Privilege,
}

impl<R: CommandRunner> Xbps<R> {
    /// Creates a backend that runs commands with `runner` and gets root for
    /// installs the way `privilege` says.
    pub fn new(runner: R, privilege: Privilege) -> Self {
        Self { runner, privilege }
    }
}

impl<R: CommandRunner> PackageManager for Xbps<R> {
    fn id(&self) -> ManagerId {
        ManagerId::Xbps
    }

    /// Runs `xbps-query -p pkgver <package>`: exit 0 means installed, exit 2
    /// means not installed, and anything else is an error carrying stderr.
    fn is_installed(&self, package: &str) -> Result<bool, SystemError> {
        let query_error = |reason: String| SystemError::Query {
            package: package.to_string(),
            reason,
        };
        let args = ["-p", "pkgver", package].map(String::from);
        let output = self
            .runner
            .run("xbps-query", &args)
            .map_err(|error| query_error(format!("cannot run xbps-query: {error}")))?;
        match output.exit_code {
            Some(0) => Ok(true),
            Some(NOT_INSTALLED_EXIT_CODE) => Ok(false),
            _ => Err(query_error(failure_reason(&output))),
        }
    }

    /// Runs a single `xbps-install -y <packages...>`, as root. Does nothing
    /// for an empty list.
    fn install(&self, packages: &[String]) -> Result<(), SystemError> {
        if packages.is_empty() {
            return Ok(());
        }
        let install_error = |reason: String| SystemError::Install {
            packages: packages.to_vec(),
            reason,
        };
        let args: Vec<String> = ["-y"]
            .into_iter()
            .map(String::from)
            .chain(packages.iter().cloned())
            .collect();
        let (program, args) = self.privilege.wrap("xbps-install", &args);
        let output = self
            .runner
            .run(&program, &args)
            .map_err(|error| install_error(format!("cannot run {program}: {error}")))?;
        if output.succeeded() {
            Ok(())
        } else {
            Err(install_error(failure_reason(&output)))
        }
    }
}

/// A one-line description of why a command failed: its stderr if it wrote
/// any, otherwise its exit status.
fn failure_reason(output: &CommandOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    match output.exit_code {
        Some(code) => format!("exited with status {code}"),
        None => "killed by a signal".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::command::fake::FakeCommandRunner;
    use std::io;

    fn xbps(runner: FakeCommandRunner, privilege: Privilege) -> Xbps<FakeCommandRunner> {
        Xbps::new(runner, privilege)
    }

    fn strings(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn xbps_identifies_itself() {
        let manager = xbps(FakeCommandRunner::new(), Privilege::AlreadyRoot);
        assert_eq!(manager.id(), ManagerId::Xbps);
    }

    #[test]
    fn the_query_runs_xbps_query_for_the_pkgver_without_privilege() {
        let manager = xbps(FakeCommandRunner::new(), Privilege::UseSudo);

        manager.is_installed("fd").unwrap();

        assert_eq!(manager.runner.lines(), ["xbps-query -p pkgver fd"]);
    }

    #[test]
    fn exit_code_zero_means_installed() {
        let manager = xbps(FakeCommandRunner::new().exit(0, ""), Privilege::AlreadyRoot);
        assert!(manager.is_installed("fd").unwrap());
    }

    #[test]
    fn exit_code_two_means_not_installed() {
        let manager = xbps(
            FakeCommandRunner::new().exit(2, "Package 'fd' not found"),
            Privilege::AlreadyRoot,
        );
        assert!(!manager.is_installed("fd").unwrap());
    }

    #[test]
    fn any_other_exit_code_is_a_query_error_carrying_stderr() {
        let manager = xbps(
            FakeCommandRunner::new().exit(1, "database is corrupt\n"),
            Privilege::AlreadyRoot,
        );

        let error = manager.is_installed("fd").unwrap_err();

        assert!(matches!(
            error,
            SystemError::Query { package, reason }
                if package == "fd" && reason == "database is corrupt"
        ));
    }

    #[test]
    fn a_failed_query_with_no_stderr_reports_the_exit_status() {
        let manager = xbps(FakeCommandRunner::new().exit(9, ""), Privilege::AlreadyRoot);

        let error = manager.is_installed("fd").unwrap_err();

        assert!(matches!(
            error,
            SystemError::Query { reason, .. } if reason == "exited with status 9"
        ));
    }

    #[test]
    fn a_query_killed_by_a_signal_is_an_error() {
        let killed = Ok(CommandOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
        });
        let manager = xbps(
            FakeCommandRunner::new().reply(killed),
            Privilege::AlreadyRoot,
        );

        let error = manager.is_installed("fd").unwrap_err();

        assert!(matches!(
            error,
            SystemError::Query { reason, .. } if reason == "killed by a signal"
        ));
    }

    #[test]
    fn a_query_when_xbps_query_cannot_start_is_an_error() {
        let manager = xbps(
            FakeCommandRunner::new().reply(Err(io::Error::from(io::ErrorKind::NotFound))),
            Privilege::AlreadyRoot,
        );

        let error = manager.is_installed("fd").unwrap_err();

        assert!(matches!(
            error,
            SystemError::Query { reason, .. } if reason.starts_with("cannot run xbps-query")
        ));
    }

    #[test]
    fn install_runs_one_xbps_install_for_all_packages() {
        let manager = xbps(FakeCommandRunner::new(), Privilege::AlreadyRoot);

        manager
            .install(&strings(&["fd", "base-devel", "git"]))
            .unwrap();

        assert_eq!(
            manager.runner.lines(),
            ["xbps-install -y fd base-devel git"]
        );
    }

    #[test]
    fn install_goes_through_sudo_when_not_root() {
        let manager = xbps(FakeCommandRunner::new(), Privilege::UseSudo);

        manager.install(&strings(&["fd"])).unwrap();

        assert_eq!(manager.runner.lines(), ["sudo xbps-install -y fd"]);
    }

    #[test]
    fn install_of_nothing_runs_no_command() {
        let manager = xbps(FakeCommandRunner::new(), Privilege::UseSudo);

        manager.install(&[]).unwrap();

        assert!(manager.runner.lines().is_empty());
    }

    #[test]
    fn a_failed_install_names_the_packages_and_carries_stderr() {
        let manager = xbps(
            FakeCommandRunner::new().exit(1, "no space left on device\n"),
            Privilege::AlreadyRoot,
        );

        let error = manager.install(&strings(&["fd", "git"])).unwrap_err();

        assert!(matches!(
            error,
            SystemError::Install { packages, reason }
                if packages == strings(&["fd", "git"]) && reason == "no space left on device"
        ));
    }

    #[test]
    fn an_install_when_sudo_cannot_start_is_an_error_naming_the_program() {
        let manager = xbps(
            FakeCommandRunner::new().reply(Err(io::Error::from(io::ErrorKind::NotFound))),
            Privilege::UseSudo,
        );

        let error = manager.install(&strings(&["fd"])).unwrap_err();

        assert!(matches!(
            error,
            SystemError::Install { reason, .. } if reason.starts_with("cannot run sudo")
        ));
    }
}
