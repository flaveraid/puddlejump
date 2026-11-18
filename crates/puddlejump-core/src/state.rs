//! Persistent state: what puddlejump remembers between runs.
//!
//! One file per user, `$XDG_STATE_HOME/puddlejump/state.toml`, holds a section
//! per repo with the symlinks puddlejump created and the last result of each
//! hook. It is a record of what happened, never a substitute for looking at the
//! filesystem: a record that disagrees with the disk is simply out of date, and
//! nothing is ever changed on its say-so alone. Deleting the file loses only
//! that history.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hook::HookResult;
use crate::package::PackageName;

/// The directory under the user's state directory that holds the state.
const STATE_DIR_NAME: &str = "puddlejump";

/// The filename of the state file.
const STATE_FILE_NAME: &str = "state.toml";

/// The only file format version this build reads and writes.
const FORMAT_VERSION: u32 = 1;

/// Where the state file lives: `$XDG_STATE_HOME/puddlejump/state.toml`, or
/// `<home>/.local/state/puddlejump/state.toml` when `xdg_state_home` is unset,
/// empty, or not absolute (the XDG rule is to ignore a relative value).
pub fn state_path(xdg_state_home: Option<&Path>, home: &Path) -> PathBuf {
    let base = xdg_state_home
        .filter(|directory| directory.is_absolute())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".local").join("state"));
    base.join(STATE_DIR_NAME).join(STATE_FILE_NAME)
}

/// A symlink puddlejump created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRecord {
    /// The package the link belongs to.
    pub package: PackageName,
    /// Where the symlink is.
    pub link: PathBuf,
    /// What it pointed at when it was created.
    pub destination: PathBuf,
    /// When it was created, in seconds since the Unix epoch. Zero when unknown,
    /// as for a record written before this was kept.
    #[serde(default)]
    pub linked_at: u64,
}

/// `time` as seconds since the Unix epoch, as the state file stores it.
///
/// A clock before the epoch gives zero rather than an error: a wrong clock must
/// not fail a run that already did its work.
pub fn epoch_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// How a hook last turned out, reduced to what a later run needs to decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    /// The script exited with status 0.
    Succeeded,
    /// The script ran and failed, or could not be started.
    Failed,
}

impl From<&HookResult> for HookStatus {
    fn from(result: &HookResult) -> Self {
        match result {
            HookResult::Succeeded => HookStatus::Succeeded,
            HookResult::Failed { .. } | HookResult::NotRunnable { .. } => HookStatus::Failed,
        }
    }
}

/// The last time a package's hook ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRecord {
    /// The package that declares the hook.
    pub package: PackageName,
    /// How it turned out.
    pub status: HookStatus,
    /// The exit code, if it ran and exited rather than being killed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// A fingerprint of the script's contents when it ran, to notice edits.
    pub fingerprint: String,
    /// When it ran, in seconds since the Unix epoch.
    pub ran_at: u64,
}

/// Everything remembered about one repo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoState {
    /// The repo root this section belongs to.
    pub path: PathBuf,
    /// Symlinks puddlejump created, sorted by link path.
    #[serde(default, rename = "link", skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkRecord>,
    /// The last run of each hook, sorted by package.
    #[serde(default, rename = "hook", skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookRecord>,
}

/// Why the state could not be read or written.
#[derive(Debug, Error)]
pub enum StateError {
    /// The file exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The state path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The file is not valid state.
    #[error("invalid state file {path}: {source}")]
    Syntax {
        /// The state path.
        path: PathBuf,
        /// What is wrong with it.
        source: toml::de::Error,
    },
    /// The file was written by a different, incompatible version.
    #[error("state file {path} has format version {found}, but this puddlejump reads version {expected}")]
    UnsupportedVersion {
        /// The state path.
        path: PathBuf,
        /// The version in the file.
        found: u32,
        /// The version this build understands.
        expected: u32,
    },
    /// The file could not be written.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The state path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

/// The on-disk shape of the state file.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    version: u32,
    #[serde(default, rename = "repo")]
    repos: Vec<RepoState>,
}

/// Everything puddlejump remembers, for every repo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    repos: Vec<RepoState>,
}

impl State {
    /// Reads the state at `path`. A missing file is not an error and gives an
    /// empty state, since that is what a first run looks like.
    ///
    /// Errors if the file exists but cannot be read, is not valid state, or has
    /// an unsupported format version. It is never silently replaced, so a
    /// damaged file cannot quietly lose history.
    pub fn load(path: &Path) -> Result<Self, StateError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(StateError::Read {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        let file: StateFile = toml::from_str(&text).map_err(|source| StateError::Syntax {
            path: path.to_path_buf(),
            source,
        })?;
        if file.version != FORMAT_VERSION {
            return Err(StateError::UnsupportedVersion {
                path: path.to_path_buf(),
                found: file.version,
                expected: FORMAT_VERSION,
            });
        }
        Ok(Self { repos: file.repos })
    }

    /// Writes the state to `path`, creating its directory if needed.
    ///
    /// The file is written to a temporary name in the same directory and then
    /// renamed over the old one, so a crash leaves either the old state or the
    /// new one, never half of each.
    ///
    /// Errors if the directory cannot be created or the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let write_error = |source| StateError::Write {
            path: path.to_path_buf(),
            source,
        };
        let text = self
            .to_toml()
            .map_err(|source| write_error(io::Error::new(io::ErrorKind::InvalidData, source)))?;
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory).map_err(write_error)?;
        let temporary = path.with_extension("toml.tmp");
        write_synced(&temporary, &text).map_err(write_error)?;
        fs::rename(&temporary, path).map_err(|source| {
            let _ = fs::remove_file(&temporary);
            write_error(source)
        })
    }

    fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(&StateFile {
            version: FORMAT_VERSION,
            repos: self.repos.clone(),
        })
    }

    /// What is remembered about the repo rooted at `root`; empty if nothing is.
    pub fn repo(&self, root: &Path) -> RepoState {
        self.repos
            .iter()
            .find(|repo| repo.path == root)
            .cloned()
            .unwrap_or_else(|| RepoState {
                path: root.to_path_buf(),
                ..RepoState::default()
            })
    }

    /// Replaces what is remembered about `repo`'s root, keeping other repos.
    ///
    /// A repo with nothing left to remember is dropped, so the file does not
    /// accumulate empty sections.
    pub fn set_repo(&mut self, mut repo: RepoState) {
        repo.links.sort_by(|a, b| a.link.cmp(&b.link));
        repo.hooks.sort_by(|a, b| a.package.cmp(&b.package));
        self.repos.retain(|existing| existing.path != repo.path);
        if !repo.links.is_empty() || !repo.hooks.is_empty() {
            self.repos.push(repo);
            self.repos.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }
}

fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn link_record(package: &str, link: &str) -> LinkRecord {
        LinkRecord {
            package: name(package),
            link: PathBuf::from(link),
            destination: PathBuf::from("/repo/packages").join(package),
            linked_at: 100,
        }
    }

    #[test]
    fn epoch_seconds_counts_whole_seconds_since_the_epoch() {
        let time = UNIX_EPOCH + std::time::Duration::from_millis(1500);

        assert_eq!(epoch_seconds(time), 1);
    }

    #[test]
    fn a_clock_before_the_epoch_is_zero_rather_than_an_error() {
        let before = UNIX_EPOCH - std::time::Duration::from_secs(10);

        assert_eq!(epoch_seconds(before), 0);
    }

    #[test]
    fn a_link_record_written_without_a_time_loads_as_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        fs::write(
            &path,
            "version = 1\n\n[[repo]]\npath = \"/repo\"\n\n[[repo.link]]\n\
             package = \"zsh\"\nlink = \"/h/.zshrc\"\ndestination = \"/r/.zshrc\"\n",
        )
        .unwrap();

        let state = State::load(&path).unwrap();

        assert_eq!(state.repo(Path::new("/repo")).links[0].linked_at, 0);
    }

    #[test]
    fn the_time_a_link_was_made_is_saved_and_loaded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        let mut state = State::default();
        state.set_repo(repo_with(vec![link_record("zsh", "/h/.zshrc")], vec![]));

        state.save(&path).unwrap();

        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.repo(Path::new("/repo")).links[0].linked_at, 100);
    }

    fn hook_record(package: &str, status: HookStatus) -> HookRecord {
        HookRecord {
            package: name(package),
            status,
            exit_code: None,
            fingerprint: "abc".to_string(),
            ran_at: 100,
        }
    }

    fn repo_with(links: Vec<LinkRecord>, hooks: Vec<HookRecord>) -> RepoState {
        RepoState {
            path: PathBuf::from("/repo"),
            links,
            hooks,
        }
    }

    #[test]
    fn the_state_path_uses_xdg_state_home_when_it_is_absolute() {
        let path = state_path(Some(Path::new("/xdg")), Path::new("/home/me"));

        assert_eq!(path, Path::new("/xdg/puddlejump/state.toml"));
    }

    #[test]
    fn the_state_path_falls_back_to_the_home_directory() {
        let path = state_path(None, Path::new("/home/me"));

        assert_eq!(
            path,
            Path::new("/home/me/.local/state/puddlejump/state.toml")
        );
    }

    #[test]
    fn a_relative_xdg_state_home_is_ignored() {
        let path = state_path(Some(Path::new("relative/dir")), Path::new("/home/me"));

        assert_eq!(
            path,
            Path::new("/home/me/.local/state/puddlejump/state.toml")
        );
    }

    #[test]
    fn a_missing_state_file_loads_as_empty_state() {
        let directory = tempfile::tempdir().unwrap();

        let state = State::load(&directory.path().join("state.toml")).unwrap();

        assert_eq!(state, State::default());
    }

    #[test]
    fn saved_state_loads_back_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        let mut state = State::default();
        let mut failed = hook_record("dwm", HookStatus::Failed);
        failed.exit_code = Some(3);
        state.set_repo(repo_with(
            vec![link_record("zsh", "/home/me/.zshrc")],
            vec![failed, hook_record("zsh", HookStatus::Succeeded)],
        ));

        state.save(&path).unwrap();

        assert_eq!(State::load(&path).unwrap(), state);
    }

    #[test]
    fn saving_creates_the_missing_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/state/state.toml");

        State::default().save(&path).unwrap();

        assert!(path.is_file());
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");

        State::default().save(&path).unwrap();

        let names: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["state.toml"]);
    }

    #[test]
    fn saving_replaces_the_previous_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        let mut first = State::default();
        first.set_repo(repo_with(vec![link_record("zsh", "/a")], vec![]));
        first.save(&path).unwrap();

        State::default().save(&path).unwrap();

        assert_eq!(State::load(&path).unwrap(), State::default());
    }

    #[test]
    fn saving_into_an_unwritable_place_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("file");
        fs::write(&blocker, "").unwrap();

        let error = State::default()
            .save(&blocker.join("state.toml"))
            .unwrap_err();

        assert!(matches!(error, StateError::Write { .. }), "{error}");
    }

    #[test]
    fn a_corrupt_state_file_is_an_error_that_names_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        fs::write(&path, "this is [not toml").unwrap();

        let error = State::load(&path).unwrap_err();

        assert!(matches!(error, StateError::Syntax { .. }), "{error}");
        assert!(error.to_string().contains("state.toml"));
    }

    #[test]
    fn a_state_file_with_an_unknown_key_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        fs::write(&path, "version = 1\nsurprise = true\n").unwrap();

        assert!(matches!(State::load(&path), Err(StateError::Syntax { .. })));
    }

    #[test]
    fn a_state_file_without_a_version_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        fs::write(&path, "").unwrap();

        assert!(matches!(State::load(&path), Err(StateError::Syntax { .. })));
    }

    #[test]
    fn a_state_file_from_another_version_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        fs::write(&path, "version = 2\n").unwrap();

        let error = State::load(&path).unwrap_err();

        assert!(
            matches!(
                error,
                StateError::UnsupportedVersion {
                    found: 2,
                    expected: 1,
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_state_file_that_cannot_be_read_is_an_error() {
        let directory = tempfile::tempdir().unwrap();

        // A directory where the file should be reads as an I/O error.
        let error = State::load(directory.path()).unwrap_err();

        assert!(matches!(error, StateError::Read { .. }), "{error}");
    }

    #[test]
    fn an_unknown_repo_has_empty_state_rooted_at_it() {
        let state = State::default();

        let repo = state.repo(Path::new("/elsewhere"));

        assert_eq!(repo.path, Path::new("/elsewhere"));
        assert!(repo.links.is_empty() && repo.hooks.is_empty());
    }

    #[test]
    fn repos_are_kept_apart() {
        let mut state = State::default();
        state.set_repo(repo_with(vec![link_record("zsh", "/a")], vec![]));
        state.set_repo(RepoState {
            path: PathBuf::from("/other"),
            links: vec![link_record("vim", "/b")],
            hooks: vec![],
        });

        assert_eq!(state.repo(Path::new("/repo")).links.len(), 1);
        assert_eq!(
            state.repo(Path::new("/other")).links[0].package,
            name("vim")
        );
    }

    #[test]
    fn setting_a_repo_replaces_only_that_repo() {
        let mut state = State::default();
        state.set_repo(repo_with(vec![link_record("zsh", "/a")], vec![]));
        state.set_repo(RepoState {
            path: PathBuf::from("/other"),
            links: vec![link_record("vim", "/b")],
            hooks: vec![],
        });

        state.set_repo(repo_with(vec![link_record("bash", "/c")], vec![]));

        assert_eq!(
            state.repo(Path::new("/repo")).links[0].package,
            name("bash")
        );
        assert_eq!(
            state.repo(Path::new("/other")).links[0].package,
            name("vim")
        );
    }

    #[test]
    fn a_repo_with_nothing_left_is_dropped_from_the_file() {
        let mut state = State::default();
        state.set_repo(repo_with(vec![link_record("zsh", "/a")], vec![]));

        state.set_repo(repo_with(vec![], vec![]));

        assert_eq!(state, State::default());
    }

    #[test]
    fn records_are_stored_sorted_so_the_file_is_stable() {
        let mut state = State::default();
        state.set_repo(repo_with(
            vec![link_record("b", "/z"), link_record("a", "/y")],
            vec![
                hook_record("zed", HookStatus::Succeeded),
                hook_record("amp", HookStatus::Succeeded),
            ],
        ));

        let repo = state.repo(Path::new("/repo"));

        assert_eq!(repo.links[0].link, Path::new("/y"));
        assert_eq!(repo.hooks[0].package, name("amp"));
    }

    #[test]
    fn a_hook_result_reduces_to_succeeded_or_failed() {
        assert_eq!(
            HookStatus::from(&HookResult::Succeeded),
            HookStatus::Succeeded
        );
        assert_eq!(
            HookStatus::from(&HookResult::Failed { exit_code: Some(1) }),
            HookStatus::Failed
        );
        assert_eq!(
            HookStatus::from(&HookResult::Failed { exit_code: None }),
            HookStatus::Failed
        );
        assert_eq!(
            HookStatus::from(&HookResult::NotRunnable {
                reason: "gone".to_string()
            }),
            HookStatus::Failed
        );
    }
}
