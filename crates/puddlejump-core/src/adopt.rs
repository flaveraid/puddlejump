//! Adopting: moving existing files from the target into a package and linking
//! them. The one sanctioned way to take over a file that is already there.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::link::{FileSystemState, PathState};
use crate::package::PackageName;
use crate::plan::{package_in, target_dir, Action, PlanContext, PlanError};

/// Why a path was not adopted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AdoptRefusal {
    /// The path is not inside the package's target directory, so there is no
    /// place for it in the package.
    NotInTarget,
    /// Nothing exists at the path.
    Missing,
    /// The path is a symlink, which is never adopted.
    IsSymlink,
    /// The path is not a regular file or a directory.
    NotARegularFile,
    /// The package already has a file where this one would go.
    DestinationExists {
        /// The file already in the package.
        into: PathBuf,
    },
    /// The same file was named more than once.
    Duplicate,
}

/// One step of adopting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AdoptAction {
    /// Move a file from the target into the package.
    Move {
        /// The file in the target.
        from: PathBuf,
        /// Where it goes in the package.
        into: PathBuf,
    },
    /// Leave a path alone.
    Refuse {
        /// The path that was refused.
        path: PathBuf,
        /// Why.
        reason: AdoptRefusal,
    },
}

/// Everything adopting would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdoptPlan {
    /// The package the files go into.
    pub package: PackageName,
    /// Moves and refusals, in the order the paths were given (directories
    /// expanded to their files, sorted).
    pub moves: Vec<AdoptAction>,
    /// Links to create once the files have moved, one per move.
    pub links: Vec<Action>,
}

/// A path could not be examined while planning.
#[derive(Debug, Error)]
pub enum AdoptError {
    /// Planning failed.
    #[error(transparent)]
    Plan(#[from] PlanError),
    /// A directory to adopt could not be read.
    #[error("cannot read {path}: {source}")]
    ReadDirectory {
        /// The directory.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

/// Plans adopting `paths` into `package`.
///
/// Each path must be inside the package's target directory. A regular file is
/// moved to the same relative location in the package. A directory is
/// expanded to the files inside it, recursively, and symlinks inside it are
/// refused rather than followed. Anything that cannot be adopted becomes a
/// [`AdoptAction::Refuse`] with a reason; the rest still go ahead.
///
/// Errors if the package is unknown, a path cannot be inspected, or a
/// directory cannot be read.
pub fn plan_adopt(
    context: &PlanContext,
    package: &PackageName,
    paths: &[PathBuf],
    state: &dyn FileSystemState,
) -> Result<AdoptPlan, AdoptError> {
    let package_data = package_in(context.repo, package)?;
    let target = target_dir(context, package_data);
    let mut moves = Vec::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        for file in expand(path, state)? {
            moves.push(plan_one(
                &file,
                &target,
                &package_data.dir,
                state,
                &mut seen,
            )?);
        }
    }
    let links = moves
        .iter()
        .filter_map(|action| match action {
            AdoptAction::Move { from, into } => Some(Action::Link {
                source: into.clone(),
                link: from.clone(),
            }),
            AdoptAction::Refuse { .. } => None,
        })
        .collect();
    Ok(AdoptPlan {
        package: package.clone(),
        moves,
        links,
    })
}

/// The paths to consider for `path`: itself, or for a real directory the
/// files and symlinks inside it, recursively and sorted. Symlinks are listed
/// as-is and refused later; a symlink to a directory is not entered.
fn expand(path: &Path, state: &dyn FileSystemState) -> Result<Vec<PathBuf>, AdoptError> {
    if state.state(path).map_err(PlanError::from)? != PathState::Directory {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut found = Vec::new();
    collect(path, state, &mut found)?;
    found.sort();
    Ok(found)
}

fn collect(
    directory: &Path,
    state: &dyn FileSystemState,
    found: &mut Vec<PathBuf>,
) -> Result<(), AdoptError> {
    let read_error = |source| AdoptError::ReadDirectory {
        path: directory.to_path_buf(),
        source,
    };
    for entry in fs::read_dir(directory).map_err(read_error)? {
        let path = entry.map_err(read_error)?.path();
        if state.state(&path).map_err(PlanError::from)? == PathState::Directory {
            collect(&path, state, found)?;
        } else {
            found.push(path);
        }
    }
    Ok(())
}

fn plan_one(
    path: &Path,
    target: &Path,
    package_dir: &Path,
    state: &dyn FileSystemState,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<AdoptAction, AdoptError> {
    let refuse = |reason| AdoptAction::Refuse {
        path: path.to_path_buf(),
        reason,
    };
    let Ok(relative) = path.strip_prefix(target) else {
        return Ok(refuse(AdoptRefusal::NotInTarget));
    };
    if relative.as_os_str().is_empty() || relative.components().any(is_parent_component) {
        return Ok(refuse(AdoptRefusal::NotInTarget));
    }
    match state.state(path).map_err(PlanError::from)? {
        PathState::Missing => return Ok(refuse(AdoptRefusal::Missing)),
        PathState::Symlink { .. } | PathState::BrokenSymlink { .. } => {
            return Ok(refuse(AdoptRefusal::IsSymlink))
        }
        PathState::Directory => return Ok(refuse(AdoptRefusal::NotARegularFile)),
        PathState::File => {}
    }
    if !seen.insert(path.to_path_buf()) {
        return Ok(refuse(AdoptRefusal::Duplicate));
    }
    let into = package_dir.join(relative);
    if state.state(&into).map_err(PlanError::from)? != PathState::Missing {
        return Ok(refuse(AdoptRefusal::DestinationExists { into }));
    }
    Ok(AdoptAction::Move {
        from: path.to_path_buf(),
        into,
    })
}

/// `..` would let a path that merely starts with the target escape it.
fn is_parent_component(component: std::path::Component) -> bool {
    matches!(component, std::path::Component::ParentDir)
}

/// How one adopt step turned out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AdoptResult {
    /// The file was moved into the package.
    Moved,
    /// The path was refused when planning.
    Refused,
    /// The step failed. The file was not lost: see the reason.
    Failed {
        /// What went wrong and where the file is.
        reason: String,
    },
}

/// An adopt action and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdoptOutcome {
    /// The planned action.
    pub action: AdoptAction,
    /// What happened.
    pub result: AdoptResult,
}

/// Moves the files of an adopt plan into the package.
///
/// Each move re-checks that the source is still a regular file and that the
/// destination is still free, so nothing in the repo is ever overwritten. A
/// move that fails is reported and the rest still run. The links are created
/// afterwards for the moves that succeeded; see [`moved_links`].
///
/// A file is never removed from the target until a complete, verified copy
/// exists in the package.
pub fn apply_adopt(plan: &AdoptPlan, state: &dyn FileSystemState) -> Vec<AdoptOutcome> {
    plan.moves
        .iter()
        .map(|action| AdoptOutcome {
            action: action.clone(),
            result: match action {
                AdoptAction::Refuse { .. } => AdoptResult::Refused,
                AdoptAction::Move { from, into } => move_one(from, into, state),
            },
        })
        .collect()
}

/// The links to create for the files that were actually moved: each moved
/// file is linked back from where it came.
pub fn moved_links(outcomes: &[AdoptOutcome]) -> Vec<Action> {
    outcomes
        .iter()
        .filter(|outcome| outcome.result == AdoptResult::Moved)
        .filter_map(|outcome| match &outcome.action {
            AdoptAction::Move { from, into } => Some(Action::Link {
                source: into.clone(),
                link: from.clone(),
            }),
            AdoptAction::Refuse { .. } => None,
        })
        .collect()
}

fn move_one(from: &Path, into: &Path, state: &dyn FileSystemState) -> AdoptResult {
    let failed = |reason: String| AdoptResult::Failed { reason };
    match state.state(from) {
        Ok(PathState::File) => {}
        Ok(_) => return failed(format!("{} is no longer a regular file", from.display())),
        Err(error) => return failed(error.to_string()),
    }
    match state.state(into) {
        Ok(PathState::Missing) => {}
        Ok(_) => return failed(format!("{} already exists", into.display())),
        Err(error) => return failed(error.to_string()),
    }
    if let Some(parent) = into.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return failed(format!("cannot create {}: {error}", parent.display()));
        }
    }
    match fs::rename(from, into) {
        Ok(()) => AdoptResult::Moved,
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            match copy_verify_remove(from, into) {
                Ok(()) => AdoptResult::Moved,
                Err(error) => failed(error.to_string()),
            }
        }
        Err(error) => failed(format!(
            "cannot move {} to {}: {error}",
            from.display(),
            into.display()
        )),
    }
}

/// Why a copy-then-remove move did not complete.
#[derive(Debug, Error)]
pub enum CopyMoveError {
    /// Copying failed. The original is untouched and no partial copy remains.
    #[error("cannot copy {from} to {into}: {source}; the original was left in place")]
    Copy {
        /// The original.
        from: PathBuf,
        /// The intended copy.
        into: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The copy did not match the original. It was discarded and the original
    /// is untouched.
    #[error("the copy of {from} did not match the original; the original was left in place")]
    Mismatch {
        /// The original.
        from: PathBuf,
    },
    /// The copy is in place but the original could not be removed, so the file
    /// now exists in both places.
    #[error(
        "copied {from} to {into} but could not remove the original: {source}; \
         the file now exists in both places"
    )]
    RemoveOriginal {
        /// The original, still present.
        from: PathBuf,
        /// The verified copy.
        into: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

/// Moves `from` to `into` by copying, for when `rename` cannot cross
/// filesystems.
///
/// Copies to a temporary name beside `into`, checks the copy matches byte for
/// byte, renames it into place, and only then removes the original. At every
/// failure the original is still in place and no partial file is left under
/// the final name.
///
/// Errors with a [`CopyMoveError`] saying which step failed and where the file
/// is.
pub fn copy_verify_remove(from: &Path, into: &Path) -> Result<(), CopyMoveError> {
    let temporary = temporary_name(into);
    let copy_error = |source| CopyMoveError::Copy {
        from: from.to_path_buf(),
        into: into.to_path_buf(),
        source,
    };
    if let Err(source) = fs::copy(from, &temporary) {
        let _ = fs::remove_file(&temporary);
        return Err(copy_error(source));
    }
    match same_contents(from, &temporary) {
        Ok(true) => {}
        Ok(false) => {
            let _ = fs::remove_file(&temporary);
            return Err(CopyMoveError::Mismatch {
                from: from.to_path_buf(),
            });
        }
        Err(source) => {
            let _ = fs::remove_file(&temporary);
            return Err(copy_error(source));
        }
    }
    if let Err(source) = fs::rename(&temporary, into) {
        let _ = fs::remove_file(&temporary);
        return Err(copy_error(source));
    }
    fs::remove_file(from).map_err(|source| CopyMoveError::RemoveOriginal {
        from: from.to_path_buf(),
        into: into.to_path_buf(),
        source,
    })
}

/// A name beside `path` that a real file would not have, so an interrupted
/// copy never looks like an existing destination.
fn temporary_name(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".puddlejump-adopting");
    path.with_file_name(name)
}

/// Whether two files have identical contents, compared in chunks so a large
/// file is never held in memory twice.
fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    use std::io::Read;
    let (mut a, mut b) = (fs::File::open(a)?, fs::File::open(b)?);
    let (mut buffer_a, mut buffer_b) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let read_a = read_full(&mut a, &mut buffer_a)?;
        let read_b = read_full(&mut b, &mut buffer_b)?;
        if read_a != read_b || buffer_a[..read_a] != buffer_b[..read_b] {
            return Ok(false);
        }
        if read_a == 0 {
            return Ok(true);
        }
    }

    /// Fills `buffer` unless the file ends first, since a single `read` may
    /// return fewer bytes than are left.
    fn read_full(file: &mut fs::File, buffer: &mut [u8]) -> io::Result<usize> {
        let mut filled = 0;
        while filled < buffer.len() {
            match file.read(&mut buffer[filled..])? {
                0 => break,
                read => filled += read,
            }
        }
        Ok(filled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::RealFileSystem;
    use crate::manifest::{Manifest, Target};
    use crate::package::Repo;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    /// A repo with one package and an empty home, in one temp directory.
    struct World {
        root: tempfile::TempDir,
        repo: Repo,
    }

    impl World {
        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }

        fn package_dir(&self, package: &str) -> PathBuf {
            self.root.path().join("repo/packages").join(package)
        }

        fn write_home(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.home().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        fn write_package(&self, package: &str, relative: &str, contents: &str) {
            let path = self.package_dir(package).join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    fn world(packages: &[&str]) -> World {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("home")).unwrap();
        for package in packages {
            fs::create_dir_all(root.path().join("repo/packages").join(package)).unwrap();
        }
        let repo = Repo::discover(&root.path().join("repo")).unwrap();
        World { root, repo }
    }

    fn plan(world: &World, package: &str, paths: &[PathBuf]) -> AdoptPlan {
        let home = world.home();
        let context = PlanContext {
            repo: &world.repo,
            home: &home,
            target_override: None,
        };
        plan_adopt(&context, &name(package), paths, &RealFileSystem).unwrap()
    }

    fn refusals(plan: &AdoptPlan) -> Vec<(&Path, &AdoptRefusal)> {
        plan.moves
            .iter()
            .filter_map(|action| match action {
                AdoptAction::Refuse { path, reason } => Some((path.as_path(), reason)),
                AdoptAction::Move { .. } => None,
            })
            .collect()
    }

    fn move_action(from: &Path, into: &Path) -> AdoptAction {
        AdoptAction::Move {
            from: from.to_path_buf(),
            into: into.to_path_buf(),
        }
    }

    #[test]
    fn a_file_in_the_target_is_planned_to_move_to_the_same_place_in_the_package() {
        let world = world(&["zsh"]);
        let file = world.write_home(".zshrc", "export A=1");

        let plan = plan(&world, "zsh", std::slice::from_ref(&file));

        let into = world.package_dir("zsh").join(".zshrc");
        assert_eq!(plan.package, name("zsh"));
        assert_eq!(plan.moves, [move_action(&file, &into)]);
        assert_eq!(
            plan.links,
            [Action::Link {
                source: into,
                link: file
            }]
        );
    }

    #[test]
    fn a_nested_file_keeps_its_relative_path_in_the_package() {
        let world = world(&["nvim"]);
        let file = world.write_home(".config/nvim/init.lua", "");

        let plan = plan(&world, "nvim", std::slice::from_ref(&file));

        assert_eq!(
            plan.moves,
            [move_action(
                &file,
                &world.package_dir("nvim").join(".config/nvim/init.lua")
            )]
        );
    }

    #[test]
    fn a_directory_is_expanded_to_its_files_sorted() {
        let world = world(&["nvim"]);
        world.write_home(".config/nvim/lua/b.lua", "");
        world.write_home(".config/nvim/init.lua", "");
        world.write_home(".config/nvim/lua/a.lua", "");

        let plan = plan(&world, "nvim", &[world.home().join(".config/nvim")]);

        let from: Vec<&Path> = plan
            .moves
            .iter()
            .map(|action| match action {
                AdoptAction::Move { from, .. } => from.as_path(),
                AdoptAction::Refuse { path, .. } => path.as_path(),
            })
            .collect();
        assert_eq!(
            from,
            [
                world.home().join(".config/nvim/init.lua"),
                world.home().join(".config/nvim/lua/a.lua"),
                world.home().join(".config/nvim/lua/b.lua"),
            ]
        );
    }

    #[test]
    fn a_symlink_inside_an_adopted_directory_is_refused_not_followed() {
        let world = world(&["nvim"]);
        world.write_home(".config/nvim/init.lua", "");
        let outside = world.root.path().join("outside");
        fs::write(&outside, "").unwrap();
        symlink(&outside, world.home().join(".config/nvim/link")).unwrap();

        let plan = plan(&world, "nvim", &[world.home().join(".config/nvim")]);

        assert_eq!(
            refusals(&plan),
            [(
                world.home().join(".config/nvim/link").as_path(),
                &AdoptRefusal::IsSymlink
            )]
        );
        assert_eq!(plan.links.len(), 1);
    }

    #[test]
    fn a_symlink_to_a_directory_inside_an_adopted_directory_is_not_entered() {
        let world = world(&["nvim"]);
        world.write_home(".config/nvim/init.lua", "");
        let elsewhere = world.root.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("secret"), "").unwrap();
        symlink(&elsewhere, world.home().join(".config/nvim/dir-link")).unwrap();

        let plan = plan(&world, "nvim", &[world.home().join(".config/nvim")]);

        let refused: Vec<_> = refusals(&plan)
            .into_iter()
            .map(|(_, reason)| reason)
            .collect();
        assert_eq!(refused, [&AdoptRefusal::IsSymlink]);
        assert_eq!(plan.links.len(), 1);
    }

    #[test]
    fn a_symlink_given_directly_is_refused() {
        let world = world(&["zsh"]);
        let target = world.root.path().join("real");
        fs::write(&target, "").unwrap();
        let link = world.home().join(".zshrc");
        symlink(&target, &link).unwrap();

        let plan = plan(&world, "zsh", std::slice::from_ref(&link));

        assert_eq!(
            refusals(&plan),
            [(link.as_path(), &AdoptRefusal::IsSymlink)]
        );
        assert!(plan.links.is_empty());
    }

    #[test]
    fn a_broken_symlink_is_refused() {
        let world = world(&["zsh"]);
        let link = world.home().join(".zshrc");
        symlink("/gone", &link).unwrap();

        let plan = plan(&world, "zsh", std::slice::from_ref(&link));

        assert_eq!(
            refusals(&plan),
            [(link.as_path(), &AdoptRefusal::IsSymlink)]
        );
    }

    #[test]
    fn a_missing_path_is_refused() {
        let world = world(&["zsh"]);
        let missing = world.home().join(".nothing");

        let plan = plan(&world, "zsh", std::slice::from_ref(&missing));

        assert_eq!(
            refusals(&plan),
            [(missing.as_path(), &AdoptRefusal::Missing)]
        );
    }

    #[test]
    fn a_path_outside_the_target_is_refused() {
        let world = world(&["zsh"]);
        let outside = world.root.path().join("elsewhere");
        fs::write(&outside, "").unwrap();

        let plan = plan(&world, "zsh", std::slice::from_ref(&outside));

        assert_eq!(
            refusals(&plan),
            [(outside.as_path(), &AdoptRefusal::NotInTarget)]
        );
    }

    #[test]
    fn a_path_that_climbs_out_of_the_target_with_dot_dot_is_refused() {
        let world = world(&["zsh"]);
        let outside = world.root.path().join("elsewhere");
        fs::write(&outside, "").unwrap();
        let sneaky = world.home().join("../elsewhere");

        let plan = plan(&world, "zsh", std::slice::from_ref(&sneaky));

        assert_eq!(
            refusals(&plan),
            [(sneaky.as_path(), &AdoptRefusal::NotInTarget)]
        );
    }

    #[test]
    fn the_target_directory_itself_is_not_adoptable_as_a_file() {
        let world = world(&["zsh"]);
        world.write_home(".zshrc", "");

        let plan = plan(&world, "zsh", &[world.home()]);

        // Given as a directory it expands to its files, none of which is the
        // target itself.
        assert!(refusals(&plan).is_empty());
        assert_eq!(plan.moves.len(), 1);
    }

    #[test]
    fn a_file_whose_destination_exists_in_the_package_is_refused() {
        let world = world(&["zsh"]);
        let file = world.write_home(".zshrc", "mine");
        world.write_package("zsh", ".zshrc", "already in the repo");

        let plan = plan(&world, "zsh", std::slice::from_ref(&file));

        let into = world.package_dir("zsh").join(".zshrc");
        assert_eq!(
            refusals(&plan),
            [(file.as_path(), &AdoptRefusal::DestinationExists { into })]
        );
        assert!(plan.links.is_empty());
    }

    #[test]
    fn the_same_file_named_twice_is_planned_once() {
        let world = world(&["zsh"]);
        let file = world.write_home(".zshrc", "");

        let plan = plan(&world, "zsh", &[file.clone(), file.clone()]);

        assert_eq!(plan.moves.len(), 2);
        assert_eq!(
            refusals(&plan),
            [(file.as_path(), &AdoptRefusal::Duplicate)]
        );
        assert_eq!(plan.links.len(), 1);
    }

    #[test]
    fn a_file_inside_a_directory_that_is_also_named_is_planned_once() {
        let world = world(&["nvim"]);
        let file = world.write_home(".config/nvim/init.lua", "");

        let plan = plan(
            &world,
            "nvim",
            &[world.home().join(".config/nvim"), file.clone()],
        );

        assert_eq!(
            refusals(&plan),
            [(file.as_path(), &AdoptRefusal::Duplicate)]
        );
        assert_eq!(plan.links.len(), 1);
    }

    #[test]
    fn a_refusal_does_not_stop_other_paths_from_being_planned() {
        let world = world(&["zsh"]);
        let good = world.write_home(".zshrc", "");
        let missing = world.home().join(".nothing");

        let plan = plan(&world, "zsh", &[missing, good.clone()]);

        assert_eq!(plan.moves.len(), 2);
        assert_eq!(plan.links.len(), 1);
    }

    #[test]
    fn an_unknown_package_is_an_error() {
        let world = world(&["zsh"]);
        let home = world.home();
        let context = PlanContext {
            repo: &world.repo,
            home: &home,
            target_override: None,
        };

        let error = plan_adopt(&context, &name("nope"), &[], &RealFileSystem).unwrap_err();

        assert!(matches!(
            error,
            AdoptError::Plan(PlanError::UnknownPackage(package)) if package == name("nope")
        ));
    }

    #[test]
    fn a_package_with_an_absolute_target_adopts_relative_to_that_target() {
        let mut world = world(&["sysctl"]);
        let etc = world.root.path().join("etc");
        fs::create_dir_all(etc.join("sysctl.d")).unwrap();
        fs::write(etc.join("sysctl.d/99.conf"), "").unwrap();
        world
            .repo
            .packages
            .get_mut(&name("sysctl"))
            .unwrap()
            .manifest = Manifest {
            target: Target::Absolute(etc.clone()),
            ..Manifest::default()
        };

        let plan = plan(&world, "sysctl", &[etc.join("sysctl.d/99.conf")]);

        assert_eq!(
            plan.moves,
            [move_action(
                &etc.join("sysctl.d/99.conf"),
                &world.package_dir("sysctl").join("sysctl.d/99.conf")
            )]
        );
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_an_error() {
        let world = world(&["zsh"]);
        let locked = world.home().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let home = world.home();
        let context = PlanContext {
            repo: &world.repo,
            home: &home,
            target_override: None,
        };

        let result = plan_adopt(
            &context,
            &name("zsh"),
            std::slice::from_ref(&locked),
            &RealFileSystem,
        );

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(AdoptError::ReadDirectory { .. })));
    }

    fn adopt(world: &World, package: &str, paths: &[PathBuf]) -> (AdoptPlan, Vec<AdoptOutcome>) {
        let plan = plan(world, package, paths);
        let outcomes = apply_adopt(&plan, &RealFileSystem);
        (plan, outcomes)
    }

    #[test]
    fn applying_moves_the_file_into_the_package_with_its_contents() {
        let world = world(&["zsh"]);
        let file = world.write_home(".zshrc", "export A=1");

        let (_, outcomes) = adopt(&world, "zsh", std::slice::from_ref(&file));

        assert_eq!(outcomes[0].result, AdoptResult::Moved);
        assert!(!file.exists());
        assert_eq!(
            fs::read_to_string(world.package_dir("zsh").join(".zshrc")).unwrap(),
            "export A=1"
        );
    }

    #[test]
    fn applying_creates_the_missing_directories_inside_the_package() {
        let world = world(&["nvim"]);
        let file = world.write_home(".config/nvim/lua/a.lua", "x");

        adopt(&world, "nvim", &[file]);

        assert!(world
            .package_dir("nvim")
            .join(".config/nvim/lua/a.lua")
            .is_file());
    }

    #[test]
    fn applying_preserves_the_file_mode() {
        let world = world(&["ssh"]);
        let file = world.write_home(".ssh/config", "Host *");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

        adopt(&world, "ssh", &[file]);

        let moved = world.package_dir("ssh").join(".ssh/config");
        assert_eq!(
            fs::metadata(moved).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn applying_never_overwrites_a_file_that_appeared_in_the_package_after_planning() {
        let world = world(&["zsh"]);
        let file = world.write_home(".zshrc", "mine");
        let plan = plan(&world, "zsh", std::slice::from_ref(&file));
        world.write_package("zsh", ".zshrc", "appeared later");

        let outcomes = apply_adopt(&plan, &RealFileSystem);

        assert!(matches!(outcomes[0].result, AdoptResult::Failed { .. }));
        assert_eq!(fs::read_to_string(&file).unwrap(), "mine");
        assert_eq!(
            fs::read_to_string(world.package_dir("zsh").join(".zshrc")).unwrap(),
            "appeared later"
        );
    }

    #[test]
    fn applying_leaves_a_file_that_was_replaced_by_a_symlink_after_planning() {
        let world = world(&["zsh"]);
        let file = world.write_home(".zshrc", "mine");
        let plan = plan(&world, "zsh", std::slice::from_ref(&file));
        fs::remove_file(&file).unwrap();
        let elsewhere = world.root.path().join("elsewhere");
        fs::write(&elsewhere, "other").unwrap();
        symlink(&elsewhere, &file).unwrap();

        let outcomes = apply_adopt(&plan, &RealFileSystem);

        assert!(matches!(outcomes[0].result, AdoptResult::Failed { .. }));
        assert!(file.is_symlink());
        assert!(!world.package_dir("zsh").join(".zshrc").exists());
    }

    #[test]
    fn applying_reports_refusals_and_still_moves_the_rest() {
        let world = world(&["zsh"]);
        let good = world.write_home(".zshrc", "");
        let missing = world.home().join(".nothing");

        let (_, outcomes) = adopt(&world, "zsh", &[missing, good]);

        assert_eq!(outcomes[0].result, AdoptResult::Refused);
        assert_eq!(outcomes[1].result, AdoptResult::Moved);
    }

    #[test]
    fn a_plan_with_nothing_to_adopt_changes_nothing() {
        let world = world(&["zsh"]);

        let (_, outcomes) = adopt(&world, "zsh", &[]);

        assert!(outcomes.is_empty());
    }

    #[test]
    fn only_the_links_for_files_that_moved_are_returned() {
        let world = world(&["zsh"]);
        let moved = world.write_home(".zshrc", "");
        let blocked = world.write_home(".zshenv", "");
        let plan = plan(&world, "zsh", &[moved.clone(), blocked.clone()]);
        world.write_package("zsh", ".zshenv", "appeared later");

        let outcomes = apply_adopt(&plan, &RealFileSystem);
        let links = moved_links(&outcomes);

        assert_eq!(
            links,
            [Action::Link {
                source: world.package_dir("zsh").join(".zshrc"),
                link: moved
            }]
        );
    }

    #[test]
    fn no_links_are_returned_when_nothing_moved() {
        let world = world(&["zsh"]);
        let file = world.write_home(".zshrc", "");
        let plan = plan(&world, "zsh", &[file]);
        world.write_package("zsh", ".zshrc", "appeared later");

        let outcomes = apply_adopt(&plan, &RealFileSystem);

        assert!(moved_links(&outcomes).is_empty());
    }

    #[test]
    fn copy_verify_remove_leaves_the_destination_with_the_same_contents_and_removes_the_original() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let into = root.path().join("into");
        fs::write(&from, "contents").unwrap();

        copy_verify_remove(&from, &into).unwrap();

        assert!(!from.exists());
        assert_eq!(fs::read_to_string(&into).unwrap(), "contents");
    }

    #[test]
    fn copy_verify_remove_leaves_no_temporary_file_behind() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let into = root.path().join("into");
        fs::write(&from, "contents").unwrap();

        copy_verify_remove(&from, &into).unwrap();

        let mut names: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["into"]);
    }

    #[test]
    fn copy_verify_remove_preserves_the_mode() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let into = root.path().join("into");
        fs::write(&from, "secret").unwrap();
        fs::set_permissions(&from, fs::Permissions::from_mode(0o600)).unwrap();

        copy_verify_remove(&from, &into).unwrap();

        assert_eq!(
            fs::metadata(&into).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn copy_verify_remove_handles_a_file_larger_than_its_comparison_buffer() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let into = root.path().join("into");
        let contents: Vec<u8> = (0..300_000u32).map(|n| (n % 251) as u8).collect();
        fs::write(&from, &contents).unwrap();

        copy_verify_remove(&from, &into).unwrap();

        assert_eq!(fs::read(&into).unwrap(), contents);
    }

    #[test]
    fn copy_verify_remove_handles_an_empty_file() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let into = root.path().join("into");
        fs::write(&from, "").unwrap();

        copy_verify_remove(&from, &into).unwrap();

        assert!(into.exists());
        assert!(!from.exists());
    }

    #[test]
    fn copy_verify_remove_keeps_the_original_when_the_copy_cannot_be_made() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        fs::write(&from, "precious").unwrap();
        let into = root.path().join("no-such-directory/into");

        let error = copy_verify_remove(&from, &into).unwrap_err();

        assert!(matches!(error, CopyMoveError::Copy { .. }));
        assert_eq!(fs::read_to_string(&from).unwrap(), "precious");
        assert!(error.to_string().contains("original was left in place"));
    }

    #[test]
    fn copy_verify_remove_keeps_the_original_when_the_source_cannot_be_read() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        fs::write(&from, "precious").unwrap();
        fs::set_permissions(&from, fs::Permissions::from_mode(0o000)).unwrap();
        let into = root.path().join("into");

        let result = copy_verify_remove(&from, &into);

        fs::set_permissions(&from, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(result, Err(CopyMoveError::Copy { .. })));
        assert!(from.exists());
        assert!(!into.exists());
    }

    #[test]
    fn copy_verify_remove_reports_that_the_file_exists_in_both_places_if_the_original_stays() {
        let root = tempfile::tempdir().unwrap();
        let locked = root.path().join("locked");
        fs::create_dir(&locked).unwrap();
        let from = locked.join("from");
        fs::write(&from, "contents").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let into = root.path().join("into");

        let result = copy_verify_remove(&from, &into);

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let error = result.unwrap_err();
        assert!(matches!(error, CopyMoveError::RemoveOriginal { .. }));
        assert!(error.to_string().contains("exists in both places"));
        assert!(from.exists());
        assert_eq!(fs::read_to_string(&into).unwrap(), "contents");
    }

    #[test]
    fn the_temporary_name_cannot_be_mistaken_for_a_real_destination() {
        let temporary = temporary_name(Path::new("/repo/packages/zsh/.zshrc"));

        assert_eq!(
            temporary,
            Path::new("/repo/packages/zsh/.zshrc.puddlejump-adopting")
        );
    }

    #[test]
    fn identical_files_compare_equal_and_different_ones_do_not() {
        let root = tempfile::tempdir().unwrap();
        let (a, b, c, d) = (
            root.path().join("a"),
            root.path().join("b"),
            root.path().join("c"),
            root.path().join("d"),
        );
        fs::write(&a, "same").unwrap();
        fs::write(&b, "same").unwrap();
        fs::write(&c, "diff").unwrap();
        fs::write(&d, "same but longer").unwrap();

        assert!(same_contents(&a, &b).unwrap());
        assert!(!same_contents(&a, &c).unwrap());
        assert!(!same_contents(&a, &d).unwrap());
    }

    #[test]
    fn a_difference_late_in_a_large_file_is_detected() {
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (root.path().join("a"), root.path().join("b"));
        let mut contents = vec![7u8; 200_000];
        fs::write(&a, &contents).unwrap();
        *contents.last_mut().unwrap() = 8;
        fs::write(&b, &contents).unwrap();

        assert!(!same_contents(&a, &b).unwrap());
    }
}
