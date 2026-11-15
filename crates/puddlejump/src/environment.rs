//! Reading the process environment, so the rest of the program (and core) never
//! has to.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// The environment variable naming the repo.
pub const REPO_VARIABLE: &str = "PUDDLEJUMP_REPO";

/// The environment variable naming the host profile.
pub const HOST_VARIABLE: &str = "PUDDLEJUMP_HOST";

/// The user's home directory.
///
/// Errors if `HOME` is not set, since a manifest target of `~` would have
/// nowhere to go.
pub fn home_directory() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set, so `~` cannot be resolved"))
}

/// The repo named by `PUDDLEJUMP_REPO`, if set and not empty.
pub fn repo_from_environment() -> Option<PathBuf> {
    env::var_os(REPO_VARIABLE)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The `XDG_CONFIG_HOME` directory, if set and not empty.
///
/// Whether it is usable (absolute) is decided by core, which owns the rule.
pub fn xdg_config_home_from_environment() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The host named by `PUDDLEJUMP_HOST`, if set, not empty, and valid UTF-8.
pub fn host_from_environment() -> Option<String> {
    env::var(HOST_VARIABLE)
        .ok()
        .filter(|value| !value.is_empty())
}

/// Whether `program` is an executable file in one of the `PATH` directories.
///
/// `path` is the value of `PATH`, passed in so tests can set it explicitly.
pub fn is_on_path(program: &str, path: Option<&OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    env::split_paths(path).any(|directory| is_executable_file(&directory.join(program)))
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Whether the process runs as root.
pub fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn executable(directory: &Path, name: &str) {
        let path = directory.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn path_of(directories: &[&Path]) -> std::ffi::OsString {
        env::join_paths(directories).unwrap()
    }

    #[test]
    fn a_program_in_a_path_directory_is_found() {
        let directory = tempfile::tempdir().unwrap();
        executable(directory.path(), "sudo");

        assert!(is_on_path("sudo", Some(&path_of(&[directory.path()]))));
    }

    #[test]
    fn a_program_in_a_later_path_directory_is_found() {
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        executable(second.path(), "sudo");

        assert!(is_on_path(
            "sudo",
            Some(&path_of(&[first.path(), second.path()]))
        ));
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let directory = tempfile::tempdir().unwrap();

        assert!(!is_on_path("sudo", Some(&path_of(&[directory.path()]))));
    }

    #[test]
    fn a_file_without_the_executable_bit_is_not_found() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sudo");
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(!is_on_path("sudo", Some(&path_of(&[directory.path()]))));
    }

    #[test]
    fn a_directory_named_like_the_program_is_not_found() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("sudo")).unwrap();

        assert!(!is_on_path("sudo", Some(&path_of(&[directory.path()]))));
    }

    #[test]
    fn an_unset_path_finds_nothing() {
        assert!(!is_on_path("sudo", None));
    }

    #[test]
    fn an_empty_path_finds_nothing() {
        assert!(!is_on_path("sudo", Some(OsStr::new(""))));
    }

    #[test]
    fn running_as_root_agrees_with_the_effective_user() {
        let uid = fs::metadata("/proc/self").unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(running_as_root(), uid.uid() == 0);
    }
}
