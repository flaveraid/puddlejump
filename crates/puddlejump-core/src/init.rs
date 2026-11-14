//! Starting a new dotfiles repo: the directories and starter files a repo
//! needs, created without ever overwriting anything.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::config::CONFIG_FILE_NAME;
use crate::host::{HostName, HOSTS_DIR_NAME};
use crate::link::{FileSystemState, FileSystemStateError, PathState};
use crate::package::PACKAGES_DIR_NAME;

/// The starter `puddlejump.toml`. Everything is commented out, so the repo
/// behaves as if there were no config until the user opts in.
pub const STARTER_CONFIG: &str = "\
# Uncomment to skip package manager auto-detection. Only \"xbps\" is supported.
# package_manager = \"xbps\"
";

/// The starter host profile: a machine that wants nothing yet.
pub const STARTER_HOST_PROFILE: &str = "\
# Packages this machine wants. Their dependencies are added automatically.
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
/// example to add the profile for a second machine. If `hostname` is `None` or
/// not a valid profile name, the profile is left out and a note says why.
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
fn host_profile_path(root: &Path, hostname: Option<&str>) -> Result<PathBuf, String> {
    let hostname = hostname.ok_or_else(|| "the machine's hostname is unknown".to_string())?;
    let name = HostName::new(hostname)
        .map_err(|error| format!("the hostname cannot be a profile name: {error}"))?;
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
    use crate::config::RepoConfig;
    use crate::host::HostProfile;
    use crate::link::RealFileSystem;
    use crate::package::Repo;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn init_at(root: &Path, hostname: Option<&str>) -> (InitPlan, Vec<InitOutcome>) {
        let plan = plan_init(root, hostname, &RealFileSystem).unwrap();
        let outcomes = execute_init(&plan, &RealFileSystem).unwrap();
        (plan, outcomes)
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
    fn a_hostname_that_cannot_be_a_profile_name_skips_only_the_host_profile() {
        for hostname in ["", ".hidden", "a/b", "../etc", "has space"] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("dotfiles");

            let (plan, _) = init_at(&root, Some(hostname));

            assert_eq!(
                fs::read_dir(root.join("hosts")).unwrap().count(),
                0,
                "{hostname:?}"
            );
            assert!(
                matches!(&plan.notes[..], [InitNote::NoHostProfile { reason }]
                    if reason.contains("cannot be a profile name")),
                "{hostname:?}"
            );
        }
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
}
