//! Starting a new dotfiles repo: the directories and starter files a repo
//! needs, created without ever overwriting anything.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::config::CONFIG_FILE_NAME;
use crate::execute::Verdict;
use crate::host::{HostName, HOSTS_DIR_NAME};
use crate::link::{FileSystemState, FileSystemStateError, PathState};
use crate::package::PACKAGES_DIR_NAME;

/// The starter `puddlejump.toml`, with every key it accepts. The key is
/// commented out, so the repo behaves as if there were no config until the user
/// opts in; setting it would make a machine without that manager an error.
pub const STARTER_CONFIG: &str = "\
# puddlejump repo config. Every key is optional.

# The package manager to use instead of detecting one from PATH.
# Only \"xbps\" (Void Linux) is supported. If set, it must be installed.
# package_manager = \"xbps\"
";

/// The starter host profile, with every key it accepts. A machine that wants
/// nothing yet.
pub const STARTER_HOST_PROFILE: &str = "\
# Packages this machine wants, by name. Every key is optional.
#
# List only the packages you want linked; the packages they depend on (see
# `depends` in a package's manifest.toml) are added automatically.
#
# With no packages listed here, `puddlejump link` links nothing for this host.
# packages = [\"zsh\", \"nvim\"]
packages = []
";

/// Why a path was left alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InitSkip {
    /// A directory or file is already there.
    AlreadyExists,
}

/// What is in the way of creating a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InitConflict {
    /// A directory is needed but a file (or anything else) is there.
    NotADirectory,
    /// A file is needed but a directory or symlink is there.
    NotAFile,
}

/// One step of creating a repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InitStep {
    /// Create a directory, and any missing parents.
    CreateDirectory {
        /// The directory.
        path: PathBuf,
    },
    /// Create a file with these contents. It must not exist yet.
    CreateFile {
        /// The file.
        path: PathBuf,
        /// What to write.
        contents: String,
    },
    /// Nothing to do: it is already there.
    Skip {
        /// The path that was left alone.
        path: PathBuf,
        /// Why.
        reason: InitSkip,
    },
    /// Cannot proceed without user intervention.
    Conflict {
        /// The path in the way.
        path: PathBuf,
        /// What is in the way.
        kind: InitConflict,
    },
}

/// Something worth telling the user that does not stop the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InitNote {
    /// The machine's hostname is unknown or not usable as a profile name, so
    /// no host profile is created.
    NoHostProfile {
        /// Why there is none.
        reason: String,
    },
}

/// Everything creating a repo would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitPlan {
    /// The repo root.
    pub root: PathBuf,
    /// What to do, in order.
    pub steps: Vec<InitStep>,
    /// Things worth telling the user.
    pub notes: Vec<InitNote>,
}

/// Why a repo could not be initialized.
#[derive(Debug, Error)]
pub enum InitError {
    /// A path could not be inspected.
    #[error(transparent)]
    FileSystem(#[from] FileSystemStateError),
    /// The plan has conflicts, which must be resolved first.
    #[error("{} path(s) are in the way", .0.len())]
    Conflicts(Vec<PathBuf>),
}

/// Plans creating a repo at `root`: the repo directory, `puddlejump.toml`,
/// `hosts/` and `packages/`, and a profile named for `hostname`.
///
/// A path that already exists is left alone, so this is safe to run again, for
/// example to add the profile for a second machine. The hostname is converted
/// to a safe filename (see [`HostName::from_hostname`]); if it is `None` or
/// nothing usable is left, the profile is left out and a note says why.
///
/// Errors if a path cannot be inspected.
pub fn plan_init(
    root: &Path,
    hostname: Option<&str>,
    state: &dyn FileSystemState,
) -> Result<InitPlan, InitError> {
    let root_step = directory_step(root, state)?;
    if matches!(root_step, InitStep::Conflict { .. }) {
        // Nothing below a path that is not a directory can be inspected, and
        // the root is the one thing the user named, so report only that.
        return Ok(InitPlan {
            root: root.to_path_buf(),
            steps: vec![root_step],
            notes: Vec::new(),
        });
    }
    let mut steps = vec![root_step];
    steps.push(file_step(
        root.join(CONFIG_FILE_NAME),
        STARTER_CONFIG,
        state,
    )?);
    steps.push(directory_step(&root.join(HOSTS_DIR_NAME), state)?);
    steps.push(directory_step(&root.join(PACKAGES_DIR_NAME), state)?);
    let mut notes = Vec::new();
    match host_profile_path(root, hostname) {
        Ok(path) => steps.push(file_step(path, STARTER_HOST_PROFILE, state)?),
        Err(reason) => notes.push(InitNote::NoHostProfile { reason }),
    }
    Ok(InitPlan {
        root: root.to_path_buf(),
        steps,
        notes,
    })
}

/// Where the profile for `hostname` goes, or why there is no profile.
///
/// The hostname is converted to a safe filename the same way host selection
/// converts it, so the profile is found again by the machine it was made for.
fn host_profile_path(root: &Path, hostname: Option<&str>) -> Result<PathBuf, String> {
    let hostname = hostname.ok_or_else(|| "the machine's hostname is unknown".to_string())?;
    let name = HostName::from_hostname(hostname).ok_or_else(|| {
        format!("the hostname {hostname:?} has nothing left to name a profile after")
    })?;
    Ok(root.join(HOSTS_DIR_NAME).join(format!("{name}.toml")))
}

/// What to do about a directory that should exist.
fn directory_step(path: &Path, state: &dyn FileSystemState) -> Result<InitStep, InitError> {
    let path = path.to_path_buf();
    Ok(match state.state(&path)? {
        PathState::Missing => InitStep::CreateDirectory { path },
        PathState::Directory => InitStep::Skip {
            path,
            reason: InitSkip::AlreadyExists,
        },
        PathState::File | PathState::Symlink { .. } | PathState::BrokenSymlink { .. } => {
            InitStep::Conflict {
                path,
                kind: InitConflict::NotADirectory,
            }
        }
    })
}

/// What to do about a file that should exist.
fn file_step(
    path: PathBuf,
    contents: &str,
    state: &dyn FileSystemState,
) -> Result<InitStep, InitError> {
    Ok(match state.state(&path)? {
        PathState::Missing => InitStep::CreateFile {
            path,
            contents: contents.to_string(),
        },
        PathState::File => InitStep::Skip {
            path,
            reason: InitSkip::AlreadyExists,
        },
        PathState::Directory | PathState::Symlink { .. } | PathState::BrokenSymlink { .. } => {
            InitStep::Conflict {
                path,
                kind: InitConflict::NotAFile,
            }
        }
    })
}

/// How one step turned out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InitResult {
    /// The directory or file was created.
    Created,
    /// It was already there, so it was left alone.
    Skipped,
    /// It could not be created. Other steps still ran.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

/// A step and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitOutcome {
    /// The planned step.
    pub step: InitStep,
    /// What happened.
    pub result: InitResult,
}

/// Whether initializing succeeded or something in it failed.
///
/// Planning problems that stop a run, such as conflicts, are
/// [`InitError`]s, not outcomes, so a list of outcomes is judged only on
/// whether a step failed.
pub fn init_verdict(outcomes: &[InitOutcome]) -> Verdict {
    let failed = outcomes
        .iter()
        .any(|outcome| matches!(outcome.result, InitResult::Failed { .. }));
    if failed {
        Verdict::Failed
    } else {
        Verdict::Success
    }
}

/// Creates what `plan` says.
///
/// Refuses the whole plan, creating nothing, if any step is a conflict. Each
/// step re-checks its path right before acting, so a file that appeared after
/// planning is never overwritten. A step that fails is recorded and the rest
/// still run.
///
/// Errors with [`InitError::Conflicts`] if any step is a conflict.
pub fn execute_init(
    plan: &InitPlan,
    state: &dyn FileSystemState,
) -> Result<Vec<InitOutcome>, InitError> {
    let conflicts: Vec<PathBuf> = plan
        .steps
        .iter()
        .filter_map(|step| match step {
            InitStep::Conflict { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    if !conflicts.is_empty() {
        return Err(InitError::Conflicts(conflicts));
    }
    Ok(plan
        .steps
        .iter()
        .map(|step| InitOutcome {
            step: step.clone(),
            result: run_step(step, state),
        })
        .collect())
}

fn run_step(step: &InitStep, state: &dyn FileSystemState) -> InitResult {
    match step {
        InitStep::Skip { .. } | InitStep::Conflict { .. } => InitResult::Skipped,
        InitStep::CreateDirectory { path } => match state.state(path) {
            Ok(PathState::Missing) => finish(fs::create_dir_all(path)),
            Ok(PathState::Directory) => InitResult::Skipped,
            Ok(_) => failed("something that is not a directory appeared here"),
            Err(error) => failed(error),
        },
        InitStep::CreateFile { path, contents } => match state.state(path) {
            Ok(PathState::Missing) => finish(create_new_file(path, contents)),
            Ok(PathState::File) => InitResult::Skipped,
            Ok(_) => failed("something that is not a file appeared here"),
            Err(error) => failed(error),
        },
    }
}

/// Writes a new file, failing if it exists, so a race cannot overwrite one.
fn create_new_file(path: &Path, contents: &str) -> io::Result<()> {
    use io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

fn finish(result: io::Result<()>) -> InitResult {
    match result {
        Ok(()) => InitResult::Created,
        Err(error) => failed(error),
    }
}

fn failed(reason: impl ToString) -> InitResult {
    InitResult::Failed {
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RepoConfig, CONFIG_KEYS};
    use crate::host::{HostProfile, PROFILE_KEYS};
    use crate::link::RealFileSystem;
    use crate::package::Repo;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn init_at(root: &Path, hostname: Option<&str>) -> (InitPlan, Vec<InitOutcome>) {
        let plan = plan_init(root, hostname, &RealFileSystem).unwrap();
        let outcomes = execute_init(&plan, &RealFileSystem).unwrap();
        (plan, outcomes)
    }

    /// The names of the files under `root/hosts`, sorted.
    fn host_profiles(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root.join("hosts"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn results(outcomes: &[InitOutcome]) -> Vec<&InitResult> {
        outcomes.iter().map(|outcome| &outcome.result).collect()
    }

    fn all(result: InitResult, count: usize) -> Vec<InitResult> {
        vec![result; count]
    }

    #[test]
    fn a_fresh_path_gets_every_directory_and_file() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        let (_, outcomes) = init_at(&root, Some("laptop"));

        assert!(outcomes
            .iter()
            .all(|outcome| outcome.result == InitResult::Created));
        assert!(root.join("puddlejump.toml").is_file());
        assert!(root.join("hosts").is_dir());
        assert!(root.join("packages").is_dir());
        assert!(root.join("hosts/laptop.toml").is_file());
    }

    #[test]
    fn missing_parent_directories_are_created() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("a/b/c/dotfiles");

        init_at(&root, Some("laptop"));

        assert!(root.join("packages").is_dir());
    }

    #[test]
    fn the_plan_lists_the_steps_in_a_creatable_order() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();

        let paths: Vec<&Path> = plan
            .steps
            .iter()
            .map(|step| match step {
                InitStep::CreateDirectory { path } | InitStep::CreateFile { path, .. } => {
                    path.as_path()
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            paths,
            [
                root.as_path(),
                root.join("puddlejump.toml").as_path(),
                root.join("hosts").as_path(),
                root.join("packages").as_path(),
                root.join("hosts/laptop.toml").as_path(),
            ]
        );
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn a_planned_file_is_created_after_the_directory_that_holds_it() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();

        let position = |wanted: &Path| {
            plan.steps
                .iter()
                .position(|step| match step {
                    InitStep::CreateDirectory { path } | InitStep::CreateFile { path, .. } => {
                        path == wanted
                    }
                    _ => false,
                })
                .unwrap()
        };
        assert!(position(&root) < position(&root.join("puddlejump.toml")));
        assert!(position(&root.join("hosts")) < position(&root.join("hosts/laptop.toml")));
    }

    #[test]
    fn the_starter_config_is_accepted_and_changes_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        init_at(&root, Some("laptop"));

        assert_eq!(RepoConfig::load(&root).unwrap(), RepoConfig::default());
    }

    #[test]
    fn the_starter_host_profile_loads_with_no_packages() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        init_at(&root, Some("laptop"));

        let profile = HostProfile::load(&root, &HostName::new("laptop").unwrap()).unwrap();
        assert!(profile.packages.is_empty());
    }

    #[test]
    fn a_new_repo_is_accepted_by_repo_discovery_with_no_packages() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        init_at(&root, Some("laptop"));

        assert!(Repo::discover(&root).unwrap().packages.is_empty());
    }

    #[test]
    fn running_it_again_creates_nothing_and_skips_everything() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        init_at(&root, Some("laptop"));

        let (plan, outcomes) = init_at(&root, Some("laptop"));

        assert_eq!(
            results(&outcomes),
            all(InitResult::Skipped, 5).iter().collect::<Vec<_>>()
        );
        assert!(plan
            .steps
            .iter()
            .all(|step| matches!(step, InitStep::Skip { .. })));
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        fs::create_dir_all(root.join("hosts")).unwrap();
        fs::write(root.join("puddlejump.toml"), "package_manager = \"xbps\"\n").unwrap();
        fs::write(root.join("hosts/laptop.toml"), "packages = [\"zsh\"]\n").unwrap();

        init_at(&root, Some("laptop"));

        assert_eq!(
            fs::read_to_string(root.join("puddlejump.toml")).unwrap(),
            "package_manager = \"xbps\"\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("hosts/laptop.toml")).unwrap(),
            "packages = [\"zsh\"]\n"
        );
    }

    #[test]
    fn a_second_machine_gets_only_its_own_profile() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        init_at(&root, Some("laptop"));

        let (_, outcomes) = init_at(&root, Some("desktop"));

        let created: Vec<&InitOutcome> = outcomes
            .iter()
            .filter(|outcome| outcome.result == InitResult::Created)
            .collect();
        assert_eq!(created.len(), 1);
        assert!(root.join("hosts/desktop.toml").is_file());
        assert!(root.join("hosts/laptop.toml").is_file());
    }

    #[test]
    fn an_existing_empty_directory_is_filled_in() {
        let parent = tempfile::tempdir().unwrap();

        let (_, outcomes) = init_at(parent.path(), Some("laptop"));

        assert_eq!(outcomes[0].result, InitResult::Skipped);
        assert!(parent.path().join("packages").is_dir());
    }

    #[test]
    fn existing_unrelated_files_in_the_directory_are_left_alone() {
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join("README.md"), "mine").unwrap();
        fs::create_dir(parent.path().join(".git")).unwrap();

        init_at(parent.path(), Some("laptop"));

        assert_eq!(
            fs::read_to_string(parent.path().join("README.md")).unwrap(),
            "mine"
        );
        assert!(parent.path().join(".git").is_dir());
        assert!(parent.path().join("packages").is_dir());
    }

    #[test]
    fn an_unknown_hostname_skips_only_the_host_profile_and_says_why() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        let (plan, _) = init_at(&root, None);

        assert!(root.join("puddlejump.toml").is_file());
        assert!(root.join("packages").is_dir());
        assert_eq!(fs::read_dir(root.join("hosts")).unwrap().count(), 0);
        assert_eq!(
            plan.notes,
            [InitNote::NoHostProfile {
                reason: "the machine's hostname is unknown".to_string()
            }]
        );
    }

    #[test]
    fn a_hostname_that_is_not_a_safe_filename_is_converted_to_one() {
        let cases = [
            ("my laptop", "my-laptop.toml"),
            ("a/b", "a-b.toml"),
            ("a\tb", "a-b.toml"),
            (".hidden", "hidden.toml"),
            ("-flag", "flag.toml"),
            ("../../escaped", "escaped.toml"),
        ];

        for (hostname, expected) in cases {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("dotfiles");

            let (plan, _) = init_at(&root, Some(hostname));

            assert_eq!(host_profiles(&root), [expected], "{hostname:?}");
            assert!(plan.notes.is_empty(), "{hostname:?}");
        }
    }

    #[test]
    fn a_hostname_with_nothing_usable_in_it_skips_only_the_host_profile_and_says_why() {
        for hostname in ["", "   ", "...", "---", "\u{0}"] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("dotfiles");

            let (plan, _) = init_at(&root, Some(hostname));

            assert!(host_profiles(&root).is_empty(), "{hostname:?}");
            assert!(root.join("puddlejump.toml").is_file(), "{hostname:?}");
            assert!(
                matches!(&plan.notes[..], [InitNote::NoHostProfile { reason }]
                    if reason.contains("nothing left to name a profile after")),
                "{hostname:?}"
            );
        }
    }

    #[test]
    fn a_converted_profile_is_found_again_by_the_hostname_it_was_made_for() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        init_at(&root, Some("my laptop"));

        let available = crate::host::available_hosts(&root).unwrap();
        let chosen =
            crate::host::select_host(None, None, None, Some("my laptop"), &available, &root)
                .unwrap();

        assert_eq!(chosen.unwrap().name.as_str(), "my-laptop");
    }

    #[test]
    fn a_profile_for_a_hostname_with_dots_keeps_its_name_and_is_found_again() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        init_at(&root, Some("lenovo.local"));

        let available = crate::host::available_hosts(&root).unwrap();
        let chosen =
            crate::host::select_host(None, None, None, Some("lenovo.local"), &available, &root)
                .unwrap();

        assert_eq!(host_profiles(&root), ["lenovo.local.toml"]);
        assert_eq!(chosen.unwrap().name.as_str(), "lenovo.local");
    }

    #[test]
    fn a_hostname_with_dots_is_a_valid_profile_name() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        init_at(&root, Some("lenovo.local"));

        assert!(root.join("hosts/lenovo.local.toml").is_file());
    }

    #[test]
    fn a_hostname_cannot_escape_the_hosts_directory() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        init_at(&root, Some("../../escaped"));

        assert!(!parent.path().join("escaped.toml").exists());
        assert!(!parent.path().join("../escaped.toml").exists());
        assert_eq!(host_profiles(&root), ["escaped.toml"]);
    }

    #[test]
    fn a_file_where_the_repo_directory_should_be_is_a_conflict_and_creates_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        fs::write(&root, "i am a file").unwrap();

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();

        assert_eq!(
            plan.steps,
            [InitStep::Conflict {
                path: root.clone(),
                kind: InitConflict::NotADirectory,
            }],
            "only the path the user named is reported, not paths below it"
        );
        let error = execute_init(&plan, &RealFileSystem).unwrap_err();
        assert!(
            matches!(&error, InitError::Conflicts(paths) if paths == std::slice::from_ref(&root))
        );
        assert_eq!(fs::read_to_string(&root).unwrap(), "i am a file");
    }

    #[test]
    fn a_symlink_to_a_directory_where_the_repo_should_be_is_a_conflict() {
        let parent = tempfile::tempdir().unwrap();
        let real = parent.path().join("real");
        let root = parent.path().join("dotfiles");
        fs::create_dir(&real).unwrap();
        symlink(&real, &root).unwrap();

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();

        assert_eq!(
            plan.steps,
            [InitStep::Conflict {
                path: root,
                kind: InitConflict::NotADirectory,
            }]
        );
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
    }

    #[test]
    fn a_file_where_a_directory_is_needed_is_refused_and_creates_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("packages"), "i am a file").unwrap();

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();
        let error = execute_init(&plan, &RealFileSystem).unwrap_err();

        assert!(matches!(&error, InitError::Conflicts(paths) if paths == &[root.join("packages")]));
        assert_eq!(error.to_string(), "1 path(s) are in the way");
        assert!(
            !root.join("puddlejump.toml").exists(),
            "nothing was created"
        );
        assert!(!root.join("hosts").exists());
    }

    #[test]
    fn a_directory_where_a_file_is_needed_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        fs::create_dir_all(root.join("puddlejump.toml")).unwrap();

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();

        assert!(plan.steps.contains(&InitStep::Conflict {
            path: root.join("puddlejump.toml"),
            kind: InitConflict::NotAFile,
        }));
        assert!(execute_init(&plan, &RealFileSystem).is_err());
        assert!(!root.join("hosts").exists());
    }

    #[test]
    fn a_symlink_is_never_followed_or_replaced() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        let elsewhere = parent.path().join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::write(&elsewhere, "precious").unwrap();
        symlink(&elsewhere, root.join("puddlejump.toml")).unwrap();

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();

        assert!(plan.steps.contains(&InitStep::Conflict {
            path: root.join("puddlejump.toml"),
            kind: InitConflict::NotAFile,
        }));
        assert!(execute_init(&plan, &RealFileSystem).is_err());
        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "precious");
    }

    #[test]
    fn a_file_that_appears_after_planning_is_not_overwritten() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("puddlejump.toml"), "arrived later").unwrap();

        let outcomes = execute_init(&plan, &RealFileSystem).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("puddlejump.toml")).unwrap(),
            "arrived later"
        );
        let config_outcome = outcomes
            .iter()
            .find(|outcome| matches!(&outcome.step, InitStep::CreateFile { path, .. } if path.ends_with("puddlejump.toml")))
            .unwrap();
        assert_eq!(config_outcome.result, InitResult::Skipped);
    }

    #[test]
    fn a_directory_that_becomes_a_file_after_planning_fails_that_step_only() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("packages"), "oops").unwrap();

        let outcomes = execute_init(&plan, &RealFileSystem).unwrap();

        let packages = outcomes
            .iter()
            .find(|outcome| matches!(&outcome.step, InitStep::CreateDirectory { path } if path.ends_with("packages")))
            .unwrap();
        assert!(matches!(packages.result, InitResult::Failed { .. }));
        assert!(
            root.join("hosts/laptop.toml").is_file(),
            "other steps still ran"
        );
        assert_eq!(fs::read_to_string(root.join("packages")).unwrap(), "oops");
    }

    #[test]
    fn a_failure_to_create_is_reported_and_the_rest_still_run() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("hosts")).unwrap();
        fs::set_permissions(root.join("hosts"), fs::Permissions::from_mode(0o555)).unwrap();

        let plan = plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();
        let outcomes = execute_init(&plan, &RealFileSystem).unwrap();

        fs::set_permissions(root.join("hosts"), fs::Permissions::from_mode(0o755)).unwrap();
        let profile = outcomes
            .iter()
            .find(|outcome| matches!(&outcome.step, InitStep::CreateFile { path, .. } if path.ends_with("laptop.toml")))
            .unwrap();
        assert!(matches!(profile.result, InitResult::Failed { .. }));
        assert!(root.join("puddlejump.toml").is_file());
        assert!(root.join("packages").is_dir());
    }

    fn outcome_with(result: InitResult) -> InitOutcome {
        InitOutcome {
            step: InitStep::CreateDirectory {
                path: PathBuf::from("/x"),
            },
            result,
        }
    }

    #[test]
    fn the_verdict_is_failed_only_if_a_step_failed() {
        let failed = InitResult::Failed {
            reason: "no".to_string(),
        };

        assert_eq!(init_verdict(&[]), Verdict::Success);
        assert_eq!(
            init_verdict(&[
                outcome_with(InitResult::Created),
                outcome_with(InitResult::Skipped)
            ]),
            Verdict::Success
        );
        assert_eq!(
            init_verdict(&[outcome_with(InitResult::Created), outcome_with(failed)]),
            Verdict::Failed
        );
    }

    #[test]
    fn planning_changes_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dotfiles");

        plan_init(&root, Some("laptop"), &RealFileSystem).unwrap();

        assert!(!root.exists());
    }

    #[test]
    fn the_generated_files_end_with_a_newline_and_are_not_empty() {
        for contents in [STARTER_CONFIG, STARTER_HOST_PROFILE] {
            assert!(contents.ends_with('\n'));
            assert!(!contents.trim().is_empty());
        }
    }

    #[test]
    fn the_created_directory_names_match_the_ones_the_rest_of_the_program_reads() {
        assert_eq!(HOSTS_DIR_NAME, "hosts");
        assert_eq!(PACKAGES_DIR_NAME, "packages");
        assert_eq!(CONFIG_FILE_NAME, "puddlejump.toml");
    }

    /// Uncomments every `# key = value` example line in `starter`, so the
    /// documented keys can be fed to the real parser.
    fn with_examples_enabled(starter: &str) -> String {
        starter
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if rest.contains(" = ") => rest.to_string(),
                _ => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_starter_config_documents_every_key_the_parser_accepts() {
        // A probe: each real key must be accepted, and anything else rejected,
        // so this list cannot drift from the parser unnoticed.
        for key in CONFIG_KEYS {
            let probe = format!("{key} = \"xbps\"");
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(CONFIG_FILE_NAME), probe).unwrap();
            assert!(
                RepoConfig::load(dir.path()).is_ok(),
                "{key} should be a key"
            );
            assert!(
                STARTER_CONFIG.contains(key),
                "the starter config never mentions {key}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(CONFIG_FILE_NAME), "no_such_key = 1").unwrap();
        assert!(RepoConfig::load(dir.path()).is_err());
    }

    #[test]
    fn the_starter_host_profile_documents_every_key_the_parser_accepts() {
        for key in PROFILE_KEYS {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("hosts")).unwrap();
            fs::write(dir.path().join("hosts/probe.toml"), format!("{key} = []")).unwrap();
            let name = HostName::new("probe").unwrap();
            assert!(
                HostProfile::load(dir.path(), &name).is_ok(),
                "{key} should be a key"
            );
            assert!(
                STARTER_HOST_PROFILE.contains(key),
                "the starter profile never mentions {key}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("hosts")).unwrap();
        fs::write(dir.path().join("hosts/probe.toml"), "no_such_key = 1").unwrap();
        assert!(HostProfile::load(dir.path(), &HostName::new("probe").unwrap()).is_err());
    }

    #[test]
    fn every_documented_example_in_the_starter_config_is_valid_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            with_examples_enabled(STARTER_CONFIG),
        )
        .unwrap();

        let config = RepoConfig::load(dir.path()).unwrap();

        assert!(
            config.package_manager.is_some(),
            "the example enables a manager"
        );
    }

    #[test]
    fn every_documented_example_in_the_starter_host_profile_is_valid_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("hosts")).unwrap();
        // The live `packages = []` line comes after the commented example, so
        // keep only the enabled example to check the example itself parses.
        let enabled: String = with_examples_enabled(STARTER_HOST_PROFILE)
            .lines()
            .filter(|line| *line != "packages = []")
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.path().join("hosts/probe.toml"), enabled).unwrap();

        let profile = HostProfile::load(dir.path(), &HostName::new("probe").unwrap()).unwrap();

        assert_eq!(profile.packages.len(), 2, "the example lists two packages");
    }

    #[test]
    fn the_starters_explain_each_key_in_comments() {
        for starter in [STARTER_CONFIG, STARTER_HOST_PROFILE] {
            let comments = starter.lines().filter(|line| line.starts_with('#')).count();
            assert!(comments >= 2, "a starter with no explanation: {starter:?}");
        }
    }
}
