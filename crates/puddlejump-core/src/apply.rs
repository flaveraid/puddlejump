//! Applying a plan: making the filesystem match what was planned.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::link::{FileSystemState, PathState};
use crate::package::PackageName;
use crate::plan::{Action, ConflictKind, PackageActions};

/// What happened when removing a directory that should be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveDirectoryOutcome {
    /// The directory was empty and has been removed.
    Removed,
    /// The directory still has contents, so it was left alone.
    NotEmpty,
}

/// The filesystem changes apply can make. Separate from [`FileSystemState`]
/// so reading and writing can be substituted independently.
pub trait FileWriter {
    /// Creates a symlink at `link` pointing at `destination`.
    ///
    /// Errors if the link cannot be created, including when something already
    /// exists at `link`.
    fn create_symlink(&mut self, destination: &Path, link: &Path) -> io::Result<()>;

    /// Removes the symlink at `link`.
    ///
    /// Errors if it cannot be removed.
    fn remove_symlink(&mut self, link: &Path) -> io::Result<()>;

    /// Creates `path` and any missing parents.
    ///
    /// Errors if they cannot be created.
    fn create_directories(&mut self, path: &Path) -> io::Result<()>;

    /// Removes the directory at `path` if it is empty.
    ///
    /// Errors if the directory cannot be removed for a reason other than
    /// still having contents.
    fn remove_directory_if_empty(&mut self, path: &Path) -> io::Result<RemoveDirectoryOutcome>;
}

/// Writes to the real filesystem as the current user.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFileWriter;

impl FileWriter for RealFileWriter {
    fn create_symlink(&mut self, destination: &Path, link: &Path) -> io::Result<()> {
        symlink(destination, link)
    }

    fn remove_symlink(&mut self, link: &Path) -> io::Result<()> {
        fs::remove_file(link)
    }

    fn create_directories(&mut self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn remove_directory_if_empty(&mut self, path: &Path) -> io::Result<RemoveDirectoryOutcome> {
        match fs::remove_dir(path) {
            Ok(()) => Ok(RemoveDirectoryOutcome::Removed),
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {
                Ok(RemoveDirectoryOutcome::NotEmpty)
            }
            Err(error) => Err(error),
        }
    }
}

/// How one planned action turned out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ActionResult {
    /// The change was made.
    Done,
    /// Nothing needed doing, or nothing was safe to do.
    Skipped {
        /// Why.
        reason: String,
    },
    /// The change could not be made. Other actions still ran.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

/// A planned action and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionOutcome {
    /// The action that was attempted.
    pub action: Action,
    /// What happened.
    pub result: ActionResult,
}

/// The outcomes for one package's actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageOutcome {
    /// The package.
    pub package: PackageName,
    /// One outcome per planned action, in order.
    pub actions: Vec<ActionOutcome>,
}

/// A plan was refused before anything was changed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApplyError {
    /// The plan has conflicts, which must be resolved first.
    #[error("{} conflict(s) must be resolved first", .0.len())]
    Conflicts(Vec<(PathBuf, ConflictKind)>),
}

/// Applies the file actions of a plan.
///
/// Refuses the whole plan, changing nothing, if any action is a conflict.
/// Each action re-checks its path immediately before acting, so a file that
/// appeared after planning is never overwritten. An action that fails is
/// recorded and the remaining actions still run.
///
/// Errors only with [`ApplyError::Conflicts`].
pub fn apply_files(
    packages: &[PackageActions],
    state: &dyn FileSystemState,
    writer: &mut dyn FileWriter,
) -> Result<Vec<PackageOutcome>, ApplyError> {
    refuse_conflicts(packages)?;
    Ok(packages
        .iter()
        .map(|package| PackageOutcome {
            package: package.package.clone(),
            actions: package
                .actions
                .iter()
                .map(|action| ActionOutcome {
                    action: action.clone(),
                    result: apply_action(action, state, writer),
                })
                .collect(),
        })
        .collect())
}

/// Checks that no action in `packages` is a conflict.
///
/// Errors with [`ApplyError::Conflicts`] listing every conflict.
pub fn refuse_conflicts(packages: &[PackageActions]) -> Result<(), ApplyError> {
    let conflicts: Vec<(PathBuf, ConflictKind)> = packages
        .iter()
        .flat_map(|package| &package.actions)
        .filter_map(|action| match action {
            Action::Conflict { path, kind } => Some((path.clone(), kind.clone())),
            _ => None,
        })
        .collect();
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(ApplyError::Conflicts(conflicts))
    }
}

fn apply_action(
    action: &Action,
    state: &dyn FileSystemState,
    writer: &mut dyn FileWriter,
) -> ActionResult {
    match action {
        Action::Link { source, link } => create_link(source, link, state, writer),
        Action::LinkSymlinkCopy { destination, link } => {
            create_link(destination, link, state, writer)
        }
        Action::Unlink {
            link,
            expected_destination,
        } => remove_link(link, expected_destination, state, writer),
        Action::Skip { reason, .. } => ActionResult::Skipped {
            reason: reason.to_string(),
        },
        // Conflicts are refused before any action runs.
        Action::Conflict { .. } => ActionResult::Skipped {
            reason: "conflict".to_string(),
        },
        Action::RemoveDirectoryIfEmpty { path } => remove_directory(path, writer),
    }
}

fn create_link(
    destination: &Path,
    link: &Path,
    state: &dyn FileSystemState,
    writer: &mut dyn FileWriter,
) -> ActionResult {
    match state.state(link) {
        Ok(PathState::Missing) => {}
        Ok(PathState::Symlink {
            destination: existing,
        }) if existing == destination => {
            return ActionResult::Skipped {
                reason: "already linked".to_string(),
            }
        }
        Ok(_) => return failed("path changed since planning; leaving it alone"),
        Err(error) => return failed(error),
    }
    if let Some(parent) = link.parent() {
        if let Err(error) = writer.create_directories(parent) {
            return failed(format!("cannot create {}: {error}", parent.display()));
        }
    }
    match writer.create_symlink(destination, link) {
        Ok(()) => ActionResult::Done,
        Err(error) => failed(error),
    }
}

fn remove_link(
    link: &Path,
    expected_destination: &Path,
    state: &dyn FileSystemState,
    writer: &mut dyn FileWriter,
) -> ActionResult {
    match state.state(link) {
        Ok(PathState::Symlink { destination } | PathState::BrokenSymlink { destination })
            if destination == expected_destination => {}
        Ok(PathState::Missing) => {
            return ActionResult::Skipped {
                reason: "already removed".to_string(),
            }
        }
        Ok(_) => return failed("path changed since planning; leaving it alone"),
        Err(error) => return failed(error),
    }
    match writer.remove_symlink(link) {
        Ok(()) => ActionResult::Done,
        Err(error) => failed(error),
    }
}

fn remove_directory(path: &Path, writer: &mut dyn FileWriter) -> ActionResult {
    match writer.remove_directory_if_empty(path) {
        Ok(RemoveDirectoryOutcome::Removed) => ActionResult::Done,
        Ok(RemoveDirectoryOutcome::NotEmpty) => ActionResult::Skipped {
            reason: "not empty".to_string(),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => ActionResult::Skipped {
            reason: "already removed".to_string(),
        },
        Err(error) => failed(error),
    }
}

fn failed(reason: impl ToString) -> ActionResult {
    ActionResult::Failed {
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::RealFileSystem;
    use crate::plan::SkipReason;
    use std::os::unix::fs::PermissionsExt;

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn package_actions(actions: Vec<Action>) -> Vec<PackageActions> {
        vec![PackageActions {
            package: name("zsh"),
            actions,
        }]
    }

    fn apply(actions: Vec<Action>) -> Vec<ActionOutcome> {
        let mut outcomes = apply_files(
            &package_actions(actions),
            &RealFileSystem,
            &mut RealFileWriter,
        )
        .unwrap();
        outcomes.remove(0).actions
    }

    fn link(source: &Path, link: &Path) -> Action {
        Action::Link {
            source: source.to_path_buf(),
            link: link.to_path_buf(),
        }
    }

    fn unlink(link: &Path, expected_destination: &Path) -> Action {
        Action::Unlink {
            link: link.to_path_buf(),
            expected_destination: expected_destination.to_path_buf(),
        }
    }

    fn results(outcomes: &[ActionOutcome]) -> Vec<&ActionResult> {
        outcomes.iter().map(|outcome| &outcome.result).collect()
    }

    fn is_failed(result: &ActionResult) -> bool {
        matches!(result, ActionResult::Failed { .. })
    }

    #[test]
    fn linking_creates_a_symlink_to_the_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("repo/.zshrc");
        let target = root.path().join("home/.zshrc");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "set -e").unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();

        let outcomes = apply(vec![link(&source, &target)]);

        assert_eq!(results(&outcomes), [&ActionResult::Done]);
        assert_eq!(fs::read_link(&target).unwrap(), source);
        assert_eq!(fs::read_to_string(&target).unwrap(), "set -e");
    }

    #[test]
    fn linking_creates_missing_parent_directories() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("home/.config/nvim/init.lua");
        fs::write(&source, "").unwrap();

        let outcomes = apply(vec![link(&source, &target)]);

        assert_eq!(results(&outcomes), [&ActionResult::Done]);
        assert!(fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn a_symlink_copy_points_at_the_packages_destination() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("current");

        let outcomes = apply(vec![Action::LinkSymlinkCopy {
            destination: PathBuf::from("/opt/zsh/5.9"),
            link: target.clone(),
        }]);

        assert_eq!(results(&outcomes), [&ActionResult::Done]);
        assert_eq!(fs::read_link(&target).unwrap(), Path::new("/opt/zsh/5.9"));
    }

    #[test]
    fn applying_twice_skips_links_that_already_exist() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("link");
        fs::write(&source, "").unwrap();
        let actions = vec![link(&source, &target)];

        apply(actions.clone());
        let second = apply(actions);

        assert_eq!(
            results(&second),
            [&ActionResult::Skipped {
                reason: "already linked".to_string()
            }]
        );
    }

    #[test]
    fn a_real_file_that_appeared_after_planning_is_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("link");
        fs::write(&source, "from the repo").unwrap();
        fs::write(&target, "precious").unwrap();

        let outcomes = apply(vec![link(&source, &target)]);

        assert!(is_failed(&outcomes[0].result));
        assert_eq!(fs::read_to_string(&target).unwrap(), "precious");
        assert!(!fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn a_directory_that_appeared_after_planning_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("link");
        fs::write(&source, "").unwrap();
        fs::create_dir(&target).unwrap();

        let outcomes = apply(vec![link(&source, &target)]);

        assert!(is_failed(&outcomes[0].result));
        assert!(target.is_dir());
    }

    #[test]
    fn a_different_symlink_that_appeared_after_planning_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("link");
        fs::write(&source, "").unwrap();
        symlink("/somewhere/else", &target).unwrap();

        let outcomes = apply(vec![link(&source, &target)]);

        assert!(is_failed(&outcomes[0].result));
        assert_eq!(
            fs::read_link(&target).unwrap(),
            Path::new("/somewhere/else")
        );
    }

    #[test]
    fn a_failed_action_does_not_stop_the_ones_after_it() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let blocked = root.path().join("blocked");
        let free = root.path().join("free");
        fs::write(&source, "").unwrap();
        fs::write(&blocked, "").unwrap();

        let outcomes = apply(vec![link(&source, &blocked), link(&source, &free)]);

        assert!(is_failed(&outcomes[0].result));
        assert_eq!(outcomes[1].result, ActionResult::Done);
        assert!(free.is_symlink());
    }

    #[test]
    fn linking_into_a_read_only_directory_fails_with_the_os_error() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let locked = root.path().join("locked");
        fs::write(&source, "").unwrap();
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

        let outcomes = apply(vec![link(&source, &locked.join("link"))]);

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_failed(&outcomes[0].result));
        assert!(!locked.join("link").exists());
    }

    #[test]
    fn unlinking_removes_a_symlink_with_the_expected_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("link");
        fs::write(&source, "").unwrap();
        symlink(&source, &target).unwrap();

        let outcomes = apply(vec![unlink(&target, &source)]);

        assert_eq!(results(&outcomes), [&ActionResult::Done]);
        assert!(fs::symlink_metadata(&target).is_err());
        assert!(source.exists());
    }

    #[test]
    fn unlinking_removes_a_broken_symlink_with_the_expected_destination() {
        let root = tempfile::tempdir().unwrap();
        let gone = root.path().join("gone");
        let target = root.path().join("link");
        symlink(&gone, &target).unwrap();

        let outcomes = apply(vec![unlink(&target, &gone)]);

        assert_eq!(results(&outcomes), [&ActionResult::Done]);
    }

    #[test]
    fn unlinking_leaves_a_symlink_that_now_points_elsewhere() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("link");
        symlink("/somewhere/else", &target).unwrap();

        let outcomes = apply(vec![unlink(&target, &source)]);

        assert!(is_failed(&outcomes[0].result));
        assert_eq!(
            fs::read_link(&target).unwrap(),
            Path::new("/somewhere/else")
        );
    }

    #[test]
    fn unlinking_never_removes_a_real_file() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("file");
        fs::write(&target, "precious").unwrap();

        let outcomes = apply(vec![unlink(&target, &root.path().join("source"))]);

        assert!(is_failed(&outcomes[0].result));
        assert_eq!(fs::read_to_string(&target).unwrap(), "precious");
    }

    #[test]
    fn unlinking_something_already_gone_is_a_skip() {
        let root = tempfile::tempdir().unwrap();

        let outcomes = apply(vec![unlink(
            &root.path().join("missing"),
            &root.path().join("source"),
        )]);

        assert_eq!(
            results(&outcomes),
            [&ActionResult::Skipped {
                reason: "already removed".to_string()
            }]
        );
    }

    #[test]
    fn removing_an_empty_directory_removes_it() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("empty");
        fs::create_dir(&directory).unwrap();

        let outcomes = apply(vec![Action::RemoveDirectoryIfEmpty {
            path: directory.clone(),
        }]);

        assert_eq!(results(&outcomes), [&ActionResult::Done]);
        assert!(!directory.exists());
    }

    #[test]
    fn removing_a_directory_with_contents_is_a_skip_and_keeps_it() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("full");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("keep"), "").unwrap();

        let outcomes = apply(vec![Action::RemoveDirectoryIfEmpty {
            path: directory.clone(),
        }]);

        assert_eq!(
            results(&outcomes),
            [&ActionResult::Skipped {
                reason: "not empty".to_string()
            }]
        );
        assert!(directory.join("keep").exists());
    }

    #[test]
    fn removing_a_directory_that_is_already_gone_is_a_skip() {
        let root = tempfile::tempdir().unwrap();

        let outcomes = apply(vec![Action::RemoveDirectoryIfEmpty {
            path: root.path().join("gone"),
        }]);

        assert_eq!(
            results(&outcomes),
            [&ActionResult::Skipped {
                reason: "already removed".to_string()
            }]
        );
    }

    #[test]
    fn pruning_deepest_first_removes_a_chain_of_empty_directories() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("a/b");
        fs::create_dir_all(&deep).unwrap();

        let outcomes = apply(vec![
            Action::RemoveDirectoryIfEmpty { path: deep },
            Action::RemoveDirectoryIfEmpty {
                path: root.path().join("a"),
            },
        ]);

        assert_eq!(
            results(&outcomes),
            [&ActionResult::Done, &ActionResult::Done]
        );
        assert!(!root.path().join("a").exists());
    }

    #[test]
    fn skip_actions_are_reported_with_a_readable_reason() {
        let root = tempfile::tempdir().unwrap();

        let outcomes = apply(vec![Action::Skip {
            link: root.path().join("x"),
            reason: SkipReason::NotOurSymlink,
        }]);

        assert_eq!(
            results(&outcomes),
            [&ActionResult::Skipped {
                reason: "symlink points somewhere else".to_string()
            }]
        );
    }

    #[test]
    fn a_plan_with_a_conflict_is_refused_and_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let fine = root.path().join("fine");
        let clash = root.path().join("clash");
        fs::write(&source, "").unwrap();

        let error = apply_files(
            &package_actions(vec![
                link(&source, &fine),
                Action::Conflict {
                    path: clash.clone(),
                    kind: ConflictKind::RealFile,
                },
            ]),
            &RealFileSystem,
            &mut RealFileWriter,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ApplyError::Conflicts(vec![(clash, ConflictKind::RealFile)])
        );
        assert!(fs::symlink_metadata(&fine).is_err());
    }

    #[test]
    fn every_conflict_across_packages_is_listed_in_the_refusal() {
        let conflict = |path: &str| Action::Conflict {
            path: PathBuf::from(path),
            kind: ConflictKind::RealFile,
        };
        let packages = vec![
            PackageActions {
                package: name("a"),
                actions: vec![conflict("/x")],
            },
            PackageActions {
                package: name("b"),
                actions: vec![conflict("/y")],
            },
        ];

        let error = apply_files(&packages, &RealFileSystem, &mut RealFileWriter).unwrap_err();

        let ApplyError::Conflicts(conflicts) = error;
        assert_eq!(conflicts.len(), 2);
        assert_eq!(
            ApplyError::Conflicts(conflicts).to_string(),
            "2 conflict(s) must be resolved first"
        );
    }

    #[test]
    fn outcomes_keep_package_and_action_order() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, "").unwrap();
        let packages = vec![
            PackageActions {
                package: name("a"),
                actions: vec![link(&source, &root.path().join("one"))],
            },
            PackageActions {
                package: name("b"),
                actions: vec![link(&source, &root.path().join("two"))],
            },
        ];

        let outcomes = apply_files(&packages, &RealFileSystem, &mut RealFileWriter).unwrap();

        let order: Vec<&str> = outcomes.iter().map(|o| o.package.as_str()).collect();
        assert_eq!(order, ["a", "b"]);
    }
}
