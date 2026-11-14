//! Status: what is linked, what is not, and what is in the way.
//!
//! Built on the planner, so "what is in the way" has one definition.

use std::path::PathBuf;

use serde::Serialize;

use crate::graph::DependencyGraph;
use crate::link::FileSystemState;
use crate::package::PackageName;
use crate::plan::{plan_link, Action, ConflictKind, PlanContext, PlanError, SkipReason};
use crate::system::{plan_system, PackageManager, SystemNotice};

/// The state of one file of a package in the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FileState {
    /// The link exists and points at the package's file.
    Linked,
    /// Nothing is there yet; linking would create it.
    NotLinked,
    /// Something is in the way; linking would refuse.
    Conflict(ConflictKind),
    /// The package's own symlink has a relative destination, so it cannot be
    /// copied.
    RelativeSymlink,
}

/// One file of a package and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileStatus {
    /// Where the link is, or would be.
    pub link: PathBuf,
    /// Its state.
    pub state: FileState,
}

/// A summary of a package's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackageState {
    /// The package has no files.
    Empty,
    /// Every file is linked.
    Linked,
    /// No file is linked and nothing is in the way.
    NotLinked,
    /// Some files are linked and the rest could be, with nothing in the way.
    Partial,
    /// At least one file has something in the way.
    Problem,
}

/// What is known about a package's declared system packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SystemStatus {
    /// The package declares no system packages for the manager in use.
    NothingDeclared,
    /// These declared packages are installed.
    AllInstalled,
    /// These declared packages are missing.
    Missing {
        /// The missing package names, sorted.
        packages: Vec<String>,
    },
    /// The package declares system packages that cannot be checked, because
    /// there is no package manager or the manifest has no list for it.
    Unknown,
}

/// The status of one package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageStatus {
    /// The package.
    pub package: PackageName,
    /// A summary of its files.
    pub state: PackageState,
    /// Each of its files, sorted by link path.
    pub files: Vec<FileStatus>,
    /// Its declared system packages.
    pub system: SystemStatus,
}

/// Reports the status of each named package, in the order given.
///
/// Each package is looked at on its own, so one package's conflicts never
/// change another's result. Relative package symlinks are reported as
/// [`FileState::RelativeSymlink`].
///
/// Errors if a package is unknown, its files cannot be listed, a path cannot
/// be inspected, or the package manager cannot be queried.
pub fn package_statuses(
    context: &PlanContext,
    packages: &[PackageName],
    state: &dyn FileSystemState,
    manager: Option<&dyn PackageManager>,
) -> Result<Vec<PackageStatus>, PlanError> {
    packages
        .iter()
        .map(|package| package_status(context, package, state, manager))
        .collect()
}

fn package_status(
    context: &PlanContext,
    package: &PackageName,
    state: &dyn FileSystemState,
    manager: Option<&dyn PackageManager>,
) -> Result<PackageStatus, PlanError> {
    let graph = DependencyGraph::without_dependencies(context.repo, std::slice::from_ref(package))
        .map_err(|error| PlanError::UnknownPackage(error.0))?;
    let plan = plan_link(context, &graph, state)?;
    let mut files: Vec<FileStatus> = plan
        .packages
        .iter()
        .flat_map(|package| &package.actions)
        .filter_map(file_status)
        .collect();
    files.sort_by(|a, b| a.link.cmp(&b.link));
    let system = system_status(context, &graph, manager)?;
    Ok(PackageStatus {
        package: package.clone(),
        state: summarize(&files),
        files,
        system,
    })
}

fn file_status(action: &Action) -> Option<FileStatus> {
    let (link, state) = match action {
        Action::Skip {
            link,
            reason: SkipReason::AlreadyLinked,
        } => (link, FileState::Linked),
        Action::Skip {
            link,
            reason: SkipReason::RelativeSymlink,
        } => (link, FileState::RelativeSymlink),
        Action::Link { link, .. } | Action::LinkSymlinkCopy { link, .. } => {
            (link, FileState::NotLinked)
        }
        Action::Conflict { path, kind } => (path, FileState::Conflict(kind.clone())),
        // Linking never plans these other skips or any removals.
        Action::Skip { .. } | Action::Unlink { .. } | Action::RemoveDirectoryIfEmpty { .. } => {
            return None
        }
    };
    Some(FileStatus {
        link: link.clone(),
        state,
    })
}

fn summarize(files: &[FileStatus]) -> PackageState {
    if files.is_empty() {
        return PackageState::Empty;
    }
    let any = |wanted: fn(&FileState) -> bool| files.iter().any(|file| wanted(&file.state));
    if any(|state| matches!(state, FileState::Conflict(_))) {
        return PackageState::Problem;
    }
    let linked = files
        .iter()
        .filter(|f| f.state == FileState::Linked)
        .count();
    if linked == files.len() {
        PackageState::Linked
    } else if linked == 0 {
        PackageState::NotLinked
    } else {
        PackageState::Partial
    }
}

fn system_status(
    context: &PlanContext,
    graph: &DependencyGraph,
    manager: Option<&dyn PackageManager>,
) -> Result<SystemStatus, PlanError> {
    let plan = plan_system(context.repo, graph, manager)?;
    // With no manager nothing can be queried, so what is missing is unknown.
    // A manifest with no list for the manager in use wants nothing from it,
    // so that is not unknown, just nothing declared.
    let cannot_check = plan
        .notices
        .iter()
        .any(|notice| matches!(notice, SystemNotice::NoManagerAvailable { .. }));
    if cannot_check {
        return Ok(SystemStatus::Unknown);
    }
    Ok(if !plan.to_install.is_empty() {
        SystemStatus::Missing {
            packages: plan.to_install,
        }
    } else if !plan.already_installed.is_empty() {
        SystemStatus::AllInstalled
    } else {
        SystemStatus::NothingDeclared
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::RealFileSystem;
    use crate::manifest::Manifest;
    use crate::package::Repo;
    use crate::system::{ManagerId, SystemError};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    /// A repo and an empty home, both in one temp directory.
    struct World {
        root: tempfile::TempDir,
        repo: Repo,
    }

    impl World {
        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }

        fn source(&self, package: &str, file: &str) -> PathBuf {
            self.root
                .path()
                .join("repo/packages")
                .join(package)
                .join(file)
        }

        fn link_in_home(&self, file: &str, package: &str) {
            let link = self.home().join(file);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(self.source(package, file), link).unwrap();
        }
    }

    /// Builds a repo where each package has the given files.
    fn world(packages: &[(&str, &[&str])]) -> World {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("home")).unwrap();
        let packages_dir = root.path().join("repo/packages");
        fs::create_dir_all(&packages_dir).unwrap();
        for (package, files) in packages {
            let dir = packages_dir.join(package);
            fs::create_dir_all(&dir).unwrap();
            for file in *files {
                let path = dir.join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "").unwrap();
            }
        }
        let repo = Repo::discover(&root.path().join("repo")).unwrap();
        World { root, repo }
    }

    fn with_manifest(world: &mut World, package: &str, manifest: Manifest) {
        world
            .repo
            .packages
            .get_mut(&name(package))
            .unwrap()
            .manifest = manifest;
    }

    fn statuses(world: &World, packages: &[&str]) -> Vec<PackageStatus> {
        statuses_with(world, packages, None).unwrap()
    }

    fn statuses_with(
        world: &World,
        packages: &[&str],
        manager: Option<&dyn PackageManager>,
    ) -> Result<Vec<PackageStatus>, PlanError> {
        let home = world.home();
        let context = PlanContext {
            repo: &world.repo,
            home: &home,
            target_override: None,
        };
        let names: Vec<PackageName> = packages.iter().map(|p| name(p)).collect();
        package_statuses(&context, &names, &RealFileSystem, manager)
    }

    fn only(world: &World, package: &str) -> PackageStatus {
        statuses(world, &[package]).remove(0)
    }

    fn states(status: &PackageStatus) -> Vec<&FileState> {
        status.files.iter().map(|file| &file.state).collect()
    }

    #[test]
    fn a_package_with_every_file_linked_is_linked() {
        let world = world(&[("zsh", &[".zshrc", ".zshenv"])]);
        world.link_in_home(".zshrc", "zsh");
        world.link_in_home(".zshenv", "zsh");

        let status = only(&world, "zsh");

        assert_eq!(status.state, PackageState::Linked);
        assert_eq!(states(&status), [&FileState::Linked, &FileState::Linked]);
    }

    #[test]
    fn a_package_with_nothing_linked_is_not_linked() {
        let world = world(&[("zsh", &[".zshrc"])]);

        let status = only(&world, "zsh");

        assert_eq!(status.state, PackageState::NotLinked);
        assert_eq!(states(&status), [&FileState::NotLinked]);
    }

    #[test]
    fn a_package_with_some_files_linked_is_partial() {
        let world = world(&[("zsh", &[".zshrc", ".zshenv"])]);
        world.link_in_home(".zshenv", "zsh");

        let status = only(&world, "zsh");

        assert_eq!(status.state, PackageState::Partial);
        // Sorted by link path, so `.zshenv` (linked) comes before `.zshrc`.
        assert_eq!(states(&status), [&FileState::Linked, &FileState::NotLinked]);
    }

    #[test]
    fn a_package_with_no_files_is_empty() {
        let world = world(&[("empty", &[])]);

        let status = only(&world, "empty");

        assert_eq!(status.state, PackageState::Empty);
        assert!(status.files.is_empty());
    }

    #[test]
    fn a_real_file_in_the_way_makes_the_package_a_problem() {
        let world = world(&[("zsh", &[".zshrc"])]);
        fs::write(world.home().join(".zshrc"), "mine").unwrap();

        let status = only(&world, "zsh");

        assert_eq!(status.state, PackageState::Problem);
        assert_eq!(
            states(&status),
            [&FileState::Conflict(ConflictKind::RealFile)]
        );
    }

    #[test]
    fn a_problem_outranks_linked_files_in_the_same_package() {
        let world = world(&[("zsh", &[".zshrc", ".zshenv"])]);
        world.link_in_home(".zshrc", "zsh");
        fs::write(world.home().join(".zshenv"), "mine").unwrap();

        assert_eq!(only(&world, "zsh").state, PackageState::Problem);
    }

    #[test]
    fn a_link_pointing_somewhere_else_is_a_foreign_symlink_conflict() {
        let world = world(&[("zsh", &[".zshrc"])]);
        // The destination must exist, or the link would count as broken.
        let elsewhere = world.root.path().join("elsewhere");
        fs::write(&elsewhere, "").unwrap();
        symlink(&elsewhere, world.home().join(".zshrc")).unwrap();

        let status = only(&world, "zsh");

        assert_eq!(
            states(&status),
            [&FileState::Conflict(ConflictKind::ForeignSymlink)]
        );
    }

    #[test]
    fn a_broken_link_is_reported_as_a_broken_symlink_conflict() {
        let world = world(&[("zsh", &[".zshrc"])]);
        symlink("/gone/for/good", world.home().join(".zshrc")).unwrap();

        let status = only(&world, "zsh");

        assert_eq!(
            states(&status),
            [&FileState::Conflict(ConflictKind::BrokenSymlink)]
        );
    }

    #[test]
    fn a_link_into_another_package_names_that_package() {
        let world = world(&[("zsh", &[".zshrc"]), ("bash", &[".zshrc"])]);
        world.link_in_home(".zshrc", "bash");

        let status = only(&world, "zsh");

        assert_eq!(
            states(&status),
            [&FileState::Conflict(ConflictKind::OtherPackage {
                owner: name("bash")
            })]
        );
    }

    #[test]
    fn a_package_symlink_with_a_relative_destination_is_reported_as_such() {
        let world = world(&[("zsh", &[])]);
        symlink("../shared", world.source("zsh", "current")).unwrap();

        let status = only(&world, "zsh");

        assert_eq!(states(&status), [&FileState::RelativeSymlink]);
    }

    #[test]
    fn files_are_sorted_by_link_path_and_carry_their_full_paths() {
        let world = world(&[("zsh", &["b", "a"])]);

        let status = only(&world, "zsh");

        let links: Vec<&Path> = status.files.iter().map(|f| f.link.as_path()).collect();
        assert_eq!(links, [world.home().join("a"), world.home().join("b")]);
    }

    #[test]
    fn each_package_is_judged_on_its_own() {
        let world = world(&[("a", &[".rc"]), ("b", &[".rc"])]);

        let all = statuses(&world, &["a", "b"]);

        assert_eq!(all[0].state, PackageState::NotLinked);
        assert_eq!(all[1].state, PackageState::NotLinked);
    }

    #[test]
    fn results_follow_the_order_requested() {
        let world = world(&[("a", &[".a"]), ("b", &[".b"])]);

        let all = statuses(&world, &["b", "a"]);

        let order: Vec<&str> = all.iter().map(|s| s.package.as_str()).collect();
        assert_eq!(order, ["b", "a"]);
    }

    #[test]
    fn an_unknown_package_is_an_error() {
        let world = world(&[("zsh", &[".zshrc"])]);

        let error = statuses_with(&world, &["nope"], None).unwrap_err();

        assert!(matches!(error, PlanError::UnknownPackage(package) if package == name("nope")));
    }

    struct FakeManager {
        installed: Vec<&'static str>,
        fail: bool,
    }

    impl PackageManager for FakeManager {
        fn id(&self) -> ManagerId {
            ManagerId::Xbps
        }

        fn is_installed(&self, package: &str) -> Result<bool, SystemError> {
            if self.fail {
                return Err(SystemError::Query {
                    package: package.to_string(),
                    reason: "boom".to_string(),
                });
            }
            Ok(self.installed.contains(&package))
        }

        fn install(&self, _packages: &[String]) -> Result<(), SystemError> {
            Ok(())
        }
    }

    fn declares(world: &mut World, package: &str, xbps: Option<&[&str]>) {
        let system_packages = Some(match xbps {
            Some(list) => BTreeMap::from([(
                ManagerId::Xbps,
                list.iter().map(|name| name.to_string()).collect(),
            )]),
            None => BTreeMap::new(),
        });
        with_manifest(
            world,
            package,
            Manifest {
                system_packages,
                ..Manifest::default()
            },
        );
    }

    fn system_of(world: &World, manager: Option<&dyn PackageManager>) -> SystemStatus {
        statuses_with(world, &["dwm"], manager)
            .unwrap()
            .remove(0)
            .system
    }

    #[test]
    fn missing_system_packages_are_listed_sorted() {
        let mut world = world(&[("dwm", &[".dwmrc"])]);
        declares(&mut world, "dwm", Some(&["gcc", "base-devel", "make"]));
        let manager = FakeManager {
            installed: vec!["make"],
            fail: false,
        };

        assert_eq!(
            system_of(&world, Some(&manager)),
            SystemStatus::Missing {
                packages: vec!["base-devel".to_string(), "gcc".to_string()]
            }
        );
    }

    #[test]
    fn system_packages_that_are_all_present_are_all_installed() {
        let mut world = world(&[("dwm", &[".dwmrc"])]);
        declares(&mut world, "dwm", Some(&["gcc"]));
        let manager = FakeManager {
            installed: vec!["gcc"],
            fail: false,
        };

        assert_eq!(
            system_of(&world, Some(&manager)),
            SystemStatus::AllInstalled
        );
    }

    #[test]
    fn a_package_declaring_nothing_has_nothing_declared() {
        let world = world(&[("dwm", &[".dwmrc"])]);
        let manager = FakeManager {
            installed: vec![],
            fail: false,
        };

        assert_eq!(
            system_of(&world, Some(&manager)),
            SystemStatus::NothingDeclared
        );
    }

    #[test]
    fn an_empty_list_for_the_manager_has_nothing_declared() {
        let mut world = world(&[("dwm", &[".dwmrc"])]);
        declares(&mut world, "dwm", Some(&[]));
        let manager = FakeManager {
            installed: vec![],
            fail: false,
        };

        assert_eq!(
            system_of(&world, Some(&manager)),
            SystemStatus::NothingDeclared
        );
    }

    #[test]
    fn a_manifest_with_no_list_for_the_manager_in_use_has_nothing_declared() {
        let mut world = world(&[("dwm", &[".dwmrc"])]);
        declares(&mut world, "dwm", None);
        let manager = FakeManager {
            installed: vec![],
            fail: false,
        };

        assert_eq!(
            system_of(&world, Some(&manager)),
            SystemStatus::NothingDeclared
        );
    }

    #[test]
    fn with_no_package_manager_declared_system_packages_are_unknown_not_missing() {
        let mut world = world(&[("dwm", &[".dwmrc"])]);
        declares(&mut world, "dwm", Some(&["gcc"]));

        assert_eq!(system_of(&world, None), SystemStatus::Unknown);
    }

    #[test]
    fn with_no_package_manager_a_package_declaring_nothing_is_still_nothing_declared() {
        let world = world(&[("dwm", &[".dwmrc"])]);

        assert_eq!(system_of(&world, None), SystemStatus::NothingDeclared);
    }

    #[test]
    fn a_failing_package_manager_query_is_an_error() {
        let mut world = world(&[("dwm", &[".dwmrc"])]);
        declares(&mut world, "dwm", Some(&["gcc"]));
        let manager = FakeManager {
            installed: vec![],
            fail: true,
        };

        let error = statuses_with(&world, &["dwm"], Some(&manager)).unwrap_err();

        assert!(matches!(error, PlanError::System(_)));
    }

    #[test]
    fn system_status_looks_only_at_the_package_itself_not_its_dependencies() {
        let mut world = world(&[("app", &[".apprc"]), ("base", &[".baserc"])]);
        declares(&mut world, "base", Some(&["gcc"]));
        with_manifest(
            &mut world,
            "app",
            Manifest {
                depends: vec![name("base")],
                ..Manifest::default()
            },
        );
        let manager = FakeManager {
            installed: vec![],
            fail: false,
        };

        let status = statuses_with(&world, &["app"], Some(&manager))
            .unwrap()
            .remove(0);

        assert_eq!(status.system, SystemStatus::NothingDeclared);
    }
}
