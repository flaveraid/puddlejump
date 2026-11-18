//! Status: what is linked, what is not, and what is in the way.
//!
//! Built on the planner, so "what is in the way" has one definition.

use std::path::PathBuf;

use serde::Serialize;

use crate::graph::DependencyGraph;
use crate::hook_state::{fingerprint, reason_to_run, HookReason};
use crate::link::FileSystemState;
use crate::link_state::{stale_links, StaleLink};
use crate::package::{Package, PackageName};
use crate::plan::{plan_link, Action, ConflictKind, PlanContext, PlanError, SkipReason};
use crate::state::{HookRecord, HookStatus, RepoState};
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

/// What is known about a package's hook from its last run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HookSummary {
    /// It last succeeded with the script as it is now.
    UpToDate,
    /// It has never run.
    NeverRun,
    /// It failed the last time it ran.
    Failed {
        /// The exit code, or `None` if it was killed or could not start.
        exit_code: Option<i32>,
    },
    /// The script was edited since it last succeeded.
    ScriptChanged,
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
    /// Its hook, or `None` if it declares none.
    pub hook: Option<HookSummary>,
    /// When its hook last ran, in seconds since the Unix epoch, or `None` if it
    /// has never run. Kept apart from `hook` so that field's shape is unchanged.
    pub hook_ran_at: Option<u64>,
    /// Links puddlejump made for it that it no longer provides, still in place.
    pub stale: Vec<StaleLink>,
}

/// What `record` and the script's current fingerprint say about a hook.
fn summarize_hook(record: Option<&HookRecord>, current: Option<&str>) -> HookSummary {
    match reason_to_run(record, current) {
        None => HookSummary::UpToDate,
        Some(HookReason::NeverRun) => HookSummary::NeverRun,
        Some(HookReason::ScriptChanged) => HookSummary::ScriptChanged,
        Some(HookReason::PreviouslyFailed) => HookSummary::Failed {
            exit_code: record
                .filter(|record| record.status == HookStatus::Failed)
                .and_then(|record| record.exit_code),
        },
        // `reason_to_run` never answers `Forced`; that is the user's choice.
        Some(HookReason::Forced) => HookSummary::NeverRun,
    }
}

/// The remembered last run of `package`'s hook, if it has run.
fn hook_record<'a>(package: &Package, remembered: &'a RepoState) -> Option<&'a HookRecord> {
    remembered
        .hooks
        .iter()
        .find(|record| record.package == package.name)
}

fn hook_status(package: &Package, remembered: &RepoState) -> Option<HookSummary> {
    let script = package.dir.join(package.manifest.hook.as_ref()?);
    let current = fingerprint(&script).ok();
    Some(summarize_hook(
        hook_record(package, remembered),
        current.as_deref(),
    ))
}

/// Reports the status of each named package, in the order given.
///
/// Each package is looked at on its own, so one package's conflicts never
/// change another's result. Relative package symlinks are reported as
/// [`FileState::RelativeSymlink`]. `remembered` supplies each hook's last
/// result, and which links were made for each package.
///
/// Errors if a package is unknown, its files cannot be listed, a path cannot
/// be inspected, or the package manager cannot be queried.
pub fn package_statuses(
    context: &PlanContext,
    packages: &[PackageName],
    remembered: &RepoState,
    state: &dyn FileSystemState,
    manager: Option<&dyn PackageManager>,
) -> Result<Vec<PackageStatus>, PlanError> {
    let stale = stale_links(context, remembered, state)?;
    packages
        .iter()
        .map(|package| package_status(context, package, remembered, &stale, state, manager))
        .collect()
}

fn package_status(
    context: &PlanContext,
    package: &PackageName,
    remembered: &RepoState,
    stale: &[StaleLink],
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
    let declared = context.repo.packages.get(package);
    let hook = declared.and_then(|declared| hook_status(declared, remembered));
    // Only a package that declares a hook has a hook time, even if the file
    // still remembers an earlier hook it has since dropped.
    let hook_ran_at = declared
        .filter(|declared| declared.manifest.hook.is_some())
        .and_then(|declared| hook_record(declared, remembered))
        .map(|record| record.ran_at);
    let stale = stale
        .iter()
        .filter(|link| link.package == *package)
        .cloned()
        .collect();
    Ok(PackageStatus {
        package: package.clone(),
        state: summarize(&files),
        files,
        system,
        hook,
        hook_ran_at,
        stale,
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
    use crate::state::LinkRecord;
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
        package_statuses(
            &context,
            &names,
            &RepoState::default(),
            &RealFileSystem,
            manager,
        )
    }

    fn only(world: &World, package: &str) -> PackageStatus {
        statuses(world, &[package]).remove(0)
    }

    fn states(status: &PackageStatus) -> Vec<&FileState> {
        status.files.iter().map(|file| &file.state).collect()
    }

    /// A world whose `dwm` package declares `setup.sh`, which exists on disk.
    fn world_with_hook() -> World {
        let mut world = world(&[("dwm", &[".dwmrc", "setup.sh"])]);
        let manifest = Manifest {
            hook: Some(PathBuf::from("setup.sh")),
            ..Manifest::default()
        };
        with_manifest(&mut world, "dwm", manifest);
        world
    }

    fn hook_summary_with(world: &World, records: Vec<HookRecord>) -> Option<HookSummary> {
        let home = world.home();
        let context = PlanContext {
            repo: &world.repo,
            home: &home,
            target_override: None,
        };
        let remembered = RepoState {
            hooks: records,
            ..RepoState::default()
        };
        package_statuses(&context, &[name("dwm")], &remembered, &RealFileSystem, None)
            .unwrap()
            .remove(0)
            .hook
    }

    fn hook_record(world: &World, status: HookStatus, exit_code: Option<i32>) -> HookRecord {
        HookRecord {
            package: name("dwm"),
            status,
            exit_code,
            fingerprint: fingerprint(&world.source("dwm", "setup.sh")).unwrap(),
            ran_at: 0,
        }
    }

    fn stale_with(world: &World, package: &str, records: Vec<LinkRecord>) -> Vec<StaleLink> {
        let home = world.home();
        let context = PlanContext {
            repo: &world.repo,
            home: &home,
            target_override: None,
        };
        let remembered = RepoState {
            links: records,
            ..RepoState::default()
        };
        package_statuses(
            &context,
            &[name(package)],
            &remembered,
            &RealFileSystem,
            None,
        )
        .unwrap()
        .remove(0)
        .stale
    }

    fn link_record(world: &World, package: &str, file: &str) -> LinkRecord {
        LinkRecord {
            package: name(package),
            link: world.home().join(file),
            destination: world.source(package, file),
            linked_at: 700,
        }
    }

    #[test]
    fn a_stale_link_carries_the_time_it_was_made() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let removed = link_record(&world, "zsh", ".zshenv");
        symlink(&removed.destination, &removed.link).unwrap();

        let stale = stale_with(&world, "zsh", vec![removed]);

        assert_eq!(stale[0].linked_at, 700);
    }

    fn hook_ran_at_with(world: &World, records: Vec<HookRecord>) -> Option<u64> {
        let home = world.home();
        let context = PlanContext {
            repo: &world.repo,
            home: &home,
            target_override: None,
        };
        let remembered = RepoState {
            hooks: records,
            ..RepoState::default()
        };
        package_statuses(&context, &[name("dwm")], &remembered, &RealFileSystem, None)
            .unwrap()
            .remove(0)
            .hook_ran_at
    }

    #[test]
    fn a_hook_that_ran_reports_when() {
        let world = world_with_hook();
        let mut record = hook_record(&world, HookStatus::Failed, Some(1));
        record.ran_at = 9000;

        assert_eq!(hook_ran_at_with(&world, vec![record]), Some(9000));
    }

    #[test]
    fn a_hook_that_succeeded_still_reports_when_it_ran() {
        let world = world_with_hook();
        let mut record = hook_record(&world, HookStatus::Succeeded, Some(0));
        record.ran_at = 9000;

        assert_eq!(hook_ran_at_with(&world, vec![record]), Some(9000));
    }

    #[test]
    fn a_hook_that_never_ran_has_no_time() {
        let world = world_with_hook();

        assert_eq!(hook_ran_at_with(&world, vec![]), None);
    }

    #[test]
    fn another_packages_hook_time_is_not_reported() {
        let world = world_with_hook();
        let mut other = hook_record(&world, HookStatus::Succeeded, Some(0));
        other.package = name("other");

        assert_eq!(hook_ran_at_with(&world, vec![other]), None);
    }

    #[test]
    fn a_package_without_a_hook_has_no_hook_time() {
        let world = world(&[("zsh", &[".zshrc"])]);

        assert_eq!(only(&world, "zsh").hook_ran_at, None);
    }

    #[test]
    fn a_package_that_dropped_its_hook_does_not_report_the_old_hook_time() {
        let world = world(&[("dwm", &[".dwmrc"])]);
        let mut record = hook_record(&world_with_hook(), HookStatus::Succeeded, Some(0));
        record.ran_at = 9000;

        assert_eq!(hook_ran_at_with(&world, vec![record]), None);
    }

    #[test]
    fn a_link_for_a_file_removed_from_the_package_is_reported_as_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let removed = link_record(&world, "zsh", ".zshenv");
        symlink(&removed.destination, &removed.link).unwrap();

        let stale = stale_with(&world, "zsh", vec![removed.clone()]);

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].link, removed.link);
    }

    #[test]
    fn a_package_whose_links_are_all_wanted_has_nothing_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let wanted = link_record(&world, "zsh", ".zshrc");
        symlink(&wanted.destination, &wanted.link).unwrap();

        assert!(stale_with(&world, "zsh", vec![wanted]).is_empty());
    }

    #[test]
    fn another_packages_stale_link_is_not_reported_for_this_package() {
        let world = world(&[("zsh", &[".zshrc"]), ("vim", &[".vimrc"])]);
        let removed = link_record(&world, "vim", ".vimswap");
        symlink(&removed.destination, &removed.link).unwrap();

        assert!(stale_with(&world, "zsh", vec![removed]).is_empty());
    }

    #[test]
    fn a_package_without_a_hook_has_no_hook_status() {
        let world = world(&[("zsh", &[".zshrc"])]);

        assert_eq!(only(&world, "zsh").hook, None);
    }

    #[test]
    fn a_hook_that_never_ran_says_so() {
        let world = world_with_hook();

        assert_eq!(
            hook_summary_with(&world, vec![]),
            Some(HookSummary::NeverRun)
        );
    }

    #[test]
    fn a_hook_that_succeeded_with_the_current_script_is_up_to_date() {
        let world = world_with_hook();
        let record = hook_record(&world, HookStatus::Succeeded, Some(0));

        assert_eq!(
            hook_summary_with(&world, vec![record]),
            Some(HookSummary::UpToDate)
        );
    }

    #[test]
    fn a_hook_that_failed_reports_its_exit_code() {
        let world = world_with_hook();
        let record = hook_record(&world, HookStatus::Failed, Some(3));

        assert_eq!(
            hook_summary_with(&world, vec![record]),
            Some(HookSummary::Failed { exit_code: Some(3) })
        );
    }

    #[test]
    fn a_hook_killed_by_a_signal_has_no_exit_code() {
        let world = world_with_hook();
        let record = hook_record(&world, HookStatus::Failed, None);

        assert_eq!(
            hook_summary_with(&world, vec![record]),
            Some(HookSummary::Failed { exit_code: None })
        );
    }

    #[test]
    fn a_hook_edited_since_it_succeeded_is_reported_as_changed() {
        let world = world_with_hook();
        let record = hook_record(&world, HookStatus::Succeeded, Some(0));
        fs::write(world.source("dwm", "setup.sh"), "echo edited").unwrap();

        assert_eq!(
            hook_summary_with(&world, vec![record]),
            Some(HookSummary::ScriptChanged)
        );
    }

    #[test]
    fn a_record_for_another_package_is_ignored() {
        let world = world_with_hook();
        let mut record = hook_record(&world, HookStatus::Succeeded, Some(0));
        record.package = name("other");

        assert_eq!(
            hook_summary_with(&world, vec![record]),
            Some(HookSummary::NeverRun)
        );
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

        fn install(
            &self,
            _packages: &[String],
            _on_line: crate::system::command::LineCallback,
        ) -> Result<(), SystemError> {
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
