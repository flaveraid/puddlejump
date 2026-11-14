//! Shared setup for the CLI integration tests: a temp repo, a temp home, and
//! fake system tools on a controlled `PATH`.

#![allow(dead_code)] // Each test file uses a different subset of these helpers.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Ordinary tools that scripts in the tests use, linked into the sandbox so
/// that `PATH` contains nothing else. In particular the host's real `sudo`
/// and `xbps-*` must never be found.
const BASIC_TOOLS: [&str; 2] = ["touch", "cat"];

/// A throwaway repo, home directory, and directory of fake tools.
pub struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    /// An empty repo (with a `packages/` directory), an empty home, and an
    /// empty directory of fake tools.
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for directory in ["repo/packages", "home", "bin"] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        for tool in BASIC_TOOLS {
            let source = which(tool);
            std::os::unix::fs::symlink(source, root.path().join("bin").join(tool)).unwrap();
        }
        Self { root }
    }

    pub fn repo(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    pub fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    /// The directory of fake tools, for tests that remove one.
    pub fn bin_path(&self) -> PathBuf {
        self.bin()
    }

    /// Where the fake tools record how they were called.
    pub fn calls_log(&self) -> PathBuf {
        self.root.path().join("calls.log")
    }

    /// Every call the fake tools recorded, one per line.
    pub fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.calls_log())
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Writes a file in the repo at `relative`, creating directories.
    pub fn repo_file(&self, relative: &str, contents: &str) -> PathBuf {
        write(&self.repo().join(relative), contents)
    }

    /// Writes a package file at `packages/<package>/<relative>`.
    pub fn package_file(&self, package: &str, relative: &str, contents: &str) -> PathBuf {
        self.repo_file(&format!("packages/{package}/{relative}"), contents)
    }

    /// Writes `packages/<package>/manifest.toml`.
    pub fn manifest(&self, package: &str, contents: &str) {
        self.package_file(package, "manifest.toml", contents);
    }

    /// Writes an executable `#!/bin/sh` script in a package.
    pub fn package_script(&self, package: &str, relative: &str, body: &str) -> PathBuf {
        let path = self.package_file(package, relative, &format!("#!/bin/sh\n{body}\n"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Writes a file in the home directory.
    pub fn home_file(&self, relative: &str, contents: &str) -> PathBuf {
        write(&self.home().join(relative), contents)
    }

    /// Installs fake `xbps-query`, `xbps-install`, and `sudo`.
    ///
    /// `installed` lists the packages `xbps-query` reports as installed; any
    /// other package makes it exit 2, as the real tool does. Every call is
    /// logged to [`Sandbox::calls_log`].
    pub fn fake_xbps(&self, installed: &[&str]) {
        let log = self.calls_log();
        let known = self.root.path().join("installed");
        fs::write(&known, installed.join("\n")).unwrap();
        self.fake_tool(
            "xbps-query",
            &format!(
                "echo \"xbps-query $*\" >> {log}\n\
                 for package in $(cat {known}); do\n\
                 [ \"$3\" = \"$package\" ] && exit 0\n\
                 done\n\
                 exit 2",
                log = log.display(),
                known = known.display()
            ),
        );
        self.fake_tool(
            "xbps-install",
            &format!("echo \"xbps-install $*\" >> {}", log.display()),
        );
        self.fake_sudo();
    }

    /// Installs a fake `sudo` that logs itself and runs its arguments.
    pub fn fake_sudo(&self) {
        self.fake_tool(
            "sudo",
            &format!(
                "echo \"sudo $*\" >> {}\nexec \"$@\"",
                self.calls_log().display()
            ),
        );
    }

    /// Installs a tool called `name` that runs `body` as a shell script.
    pub fn fake_tool(&self, name: &str, body: &str) {
        let path = self.bin().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The binary under test, run in this sandbox's repo with a controlled
    /// environment: its own `HOME` and `PATH`, and no `PUDDLEJUMP_*`
    /// variables.
    pub fn command(&self) -> Command {
        let mut command = self.command_without_repo();
        command.arg("--repo").arg(self.repo());
        command
    }

    /// Like [`Sandbox::command`] but without `--repo`, for tests that choose
    /// the repo themselves (or rely on the current directory).
    pub fn command_without_repo(&self) -> Command {
        let mut command = Command::cargo_bin("puddlejump").unwrap();
        command
            .current_dir(self.root.path())
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", self.bin());
        command
    }
}

/// Where the host keeps `tool`, found by searching the host's own `PATH`.
fn which(tool: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("the test run has a PATH");
    std::env::split_paths(&path)
        .map(|directory| directory.join(tool))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{tool} must be installed to run these tests"))
}

fn write(path: &Path, contents: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    path.to_path_buf()
}

/// What a symlink points to, or `None` if it is not a symlink.
pub fn link_target(path: &Path) -> Option<PathBuf> {
    fs::read_link(path).ok()
}
