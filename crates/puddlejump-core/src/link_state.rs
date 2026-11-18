//! Keeping the remembered symlinks in step with what a run did, and finding
//! the ones that are no longer wanted.
//!
//! A record is a note that puddlejump made a link, not proof that it is still
//! there. Updating records ([`remember_links`], [`forget_links`]) never reads
//! the filesystem: it only adds a record for a link that was created and drops
//! one for a link known to be gone. Acting on records ([`stale_links`]) always
//! checks the disk first, so puddlejump never claims a link it did not make.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

use crate::apply::{ActionResult, PackageOutcome};
use crate::link::{FileSystemState, PathState};
use crate::package::PackageName;
use crate::plan::{
    package_in, plan_unlink, prune_actions, target_dir, Action, PackageActions, PlanContext,
    PlanError, RootReason, SkipReason,
};
use crate::state::{epoch_seconds, LinkRecord, RepoState};

/// A link puddlejump made that its package no longer provides.
///
/// It is still on disk exactly as recorded, so removing it is safe: it is a
/// symlink to the destination puddlejump gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleLink {
    /// The package that made the link.
    pub package: PackageName,
    /// Where the symlink is.
    pub link: PathBuf,
    /// What it points at.
    pub destination: PathBuf,
    /// When puddlejump made it, in seconds since the Unix epoch, or zero if
    /// that was not recorded.
    pub linked_at: u64,
}

/// The links `remembered` says puddlejump made that are still in place but no
/// longer wanted: the package is gone from the repo, or no longer has a file
/// for that link.
///
/// A record is only a note, so each is checked against the disk. A link that
/// is gone, was replaced by a real file, or now points somewhere else is not
/// returned, so puddlejump never claims something it did not make. Sorted by
/// link path.
///
/// Errors if a package's files cannot be listed or a path cannot be inspected.
pub fn stale_links(
    context: &PlanContext,
    remembered: &RepoState,
    state: &dyn FileSystemState,
) -> Result<Vec<StaleLink>, PlanError> {
    let mut stale = Vec::new();
    for record in &remembered.links {
        if still_provided(context, record)? || !is_intact(record, state)? {
            continue;
        }
        stale.push(StaleLink {
            package: record.package.clone(),
            link: record.link.clone(),
            destination: record.destination.clone(),
            linked_at: record.linked_at,
        });
    }
    stale.sort_by(|a, b| a.link.cmp(&b.link));
    Ok(stale)
}

/// Plans `unlink` for the named packages: their current links, plus any
/// stale link they made, including those of a package no longer in the repo.
///
/// A named package that is not in the repo and has nothing stale is an error,
/// as it is for any other command. Stale removals of a package are added to
/// that package's own actions, so each package appears once. With `prune`,
/// directories are pruned after all removals, but only for packages still in
/// the repo, because only they say where their target is.
///
/// Errors if a package is unknown and has nothing stale, its files cannot be
/// listed, or a path cannot be inspected.
pub fn plan_unlink_with_stale(
    context: &PlanContext,
    packages: &[PackageName],
    prune: bool,
    remembered: &RepoState,
    state: &dyn FileSystemState,
) -> Result<Vec<PackageActions>, PlanError> {
    let stale = plan_unlink_stale(context, packages, remembered, state)?;
    let in_repo: Vec<PackageName> = packages
        .iter()
        .filter(|package| context.repo.packages.contains_key(*package))
        .cloned()
        .collect();
    if let Some(unknown) = packages
        .iter()
        .find(|package| !in_repo.contains(package) && !stale.iter().any(|s| s.package == **package))
    {
        return Err(PlanError::UnknownPackage(unknown.clone()));
    }
    // Pruning is planned once over every removal below, not by `plan_unlink`:
    // a directory must only be pruned after the stale links inside it are gone.
    let mut current = plan_unlink(context, &in_repo, false, state)?;
    let mut plans = Vec::new();
    for package in packages {
        let mut actions = match current.iter().position(|plan| plan.package == *package) {
            Some(index) => current.remove(index).actions,
            None => Vec::new(),
        };
        if let Some(extra) = stale.iter().find(|plan| plan.package == *package) {
            actions.extend(extra.actions.clone());
        }
        // A package gone from the repo has no manifest to say where its target
        // is, so there is no safe boundary to prune up to; its directories stay.
        if let (true, Ok(declared)) = (prune, package_in(context.repo, package)) {
            actions.extend(prune_actions(&target_dir(context, declared), &actions));
        }
        plans.push(PackageActions {
            package: package.clone(),
            actions,
        });
    }
    Ok(plans)
}

/// Why removing the links in `plan` needs root: one reason per package whose
/// link sits in a directory the current user cannot write.
///
/// A package gone from the repo has no manifest to say it needs root, so the
/// only evidence is whether the directory holding the link can be written.
pub fn root_reasons_for_stale(
    plan: &[PackageActions],
    state: &dyn FileSystemState,
) -> Vec<RootReason> {
    let mut reasons = Vec::new();
    for package in plan {
        let unwritable = package.actions.iter().find_map(|action| match action {
            Action::Unlink { link, .. } => {
                link.parent().filter(|parent| !state.is_writable(parent))
            }
            _ => None,
        });
        if let Some(path) = unwritable {
            reasons.push(RootReason::TargetNotWritable {
                package: package.package.clone(),
                path: path.to_path_buf(),
            });
        }
    }
    reasons
}

/// Whether the record's package still has a file that links at its path.
fn still_provided(context: &PlanContext, record: &LinkRecord) -> Result<bool, PlanError> {
    let Ok(package) = package_in(context.repo, &record.package) else {
        return Ok(false);
    };
    let target_dir = target_dir(context, package);
    let provided: BTreeSet<PathBuf> = package
        .files()?
        .into_iter()
        .map(|relative| target_dir.join(relative))
        .collect();
    Ok(provided.contains(&record.link))
}

/// Whether the path is still a symlink to the recorded destination.
fn is_intact(record: &LinkRecord, state: &dyn FileSystemState) -> Result<bool, PlanError> {
    Ok(matches!(
        state.state(&record.link)?,
        PathState::Symlink { destination } | PathState::BrokenSymlink { destination }
            if destination == record.destination
    ))
}

/// Plans removing the stale links of the named packages, which may no longer
/// exist in the repo.
///
/// Each action is an [`Action::Unlink`] expecting the recorded destination, so
/// it re-checks the path when applied like any other removal. Packages with
/// nothing stale are left out.
///
/// Errors if a package's files cannot be listed or a path cannot be inspected.
pub fn plan_unlink_stale(
    context: &PlanContext,
    packages: &[PackageName],
    remembered: &RepoState,
    state: &dyn FileSystemState,
) -> Result<Vec<PackageActions>, PlanError> {
    let stale = stale_links(context, remembered, state)?;
    Ok(packages
        .iter()
        .filter_map(|package| {
            let actions: Vec<Action> = stale
                .iter()
                .filter(|link| link.package == *package)
                .map(|link| Action::Unlink {
                    link: link.link.clone(),
                    expected_destination: link.destination.clone(),
                })
                .collect();
            (!actions.is_empty()).then(|| PackageActions {
                package: package.clone(),
                actions,
            })
        })
        .collect())
}

/// Remembers every link that `outcomes` created.
///
/// Only a link that was actually created is recorded: a link that was already
/// in place was not made by puddlejump, and one that failed or was skipped
/// does not exist. A link recorded earlier at the same path is replaced, so
/// its time is when it was last created. Each record is stamped with `now`.
pub fn remember_links(repo: &mut RepoState, outcomes: &[PackageOutcome], now: SystemTime) {
    for package in outcomes {
        for outcome in &package.actions {
            if outcome.result != ActionResult::Done {
                continue;
            }
            let (link, destination) = match &outcome.action {
                Action::Link { source, link } => (link, source),
                Action::LinkSymlinkCopy { destination, link } => (link, destination),
                _ => continue,
            };
            repo.links.retain(|record| record.link != *link);
            repo.links.push(LinkRecord {
                package: package.package.clone(),
                link: link.clone(),
                destination: destination.clone(),
                linked_at: epoch_seconds(now),
            });
        }
    }
}

/// Forgets every link that `outcomes` removed or found already gone.
///
/// A removal that failed keeps its record, because the link is still there. A
/// path that something else occupies keeps it too: that link is not known to
/// be gone.
pub fn forget_links(repo: &mut RepoState, outcomes: &[PackageOutcome]) {
    for package in outcomes {
        for outcome in &package.actions {
            if let Some(link) = removed_link(&outcome.action, &outcome.result) {
                repo.links.retain(|record| record.link != *link);
            }
        }
    }
}

/// The link that no longer exists after `action` ended with `result`, if any.
fn removed_link<'a>(action: &'a Action, result: &ActionResult) -> Option<&'a std::path::PathBuf> {
    match (action, result) {
        // An unlink is only skipped when the link had already gone by the
        // time it ran; a path that changed to something else is a failure.
        (Action::Unlink { link, .. }, ActionResult::Done | ActionResult::Skipped { .. }) => {
            Some(link)
        }
        (
            Action::Skip {
                link,
                reason: SkipReason::NotLinked,
            },
            _,
        ) => Some(link),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::ActionOutcome;
    use crate::link::RealFileSystem;
    use crate::package::Repo;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::time::{Duration, UNIX_EPOCH};

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn path(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    fn done() -> ActionResult {
        ActionResult::Done
    }

    fn skipped(reason: &str) -> ActionResult {
        ActionResult::Skipped {
            reason: reason.to_string(),
        }
    }

    fn failed() -> ActionResult {
        ActionResult::Failed {
            reason: "no".to_string(),
        }
    }

    fn link_action(source: &str, link: &str) -> Action {
        Action::Link {
            source: path(source),
            link: path(link),
        }
    }

    fn unlink_action(link: &str, destination: &str) -> Action {
        Action::Unlink {
            link: path(link),
            expected_destination: path(destination),
        }
    }

    fn outcomes(package: &str, actions: Vec<(Action, ActionResult)>) -> Vec<PackageOutcome> {
        vec![PackageOutcome {
            package: name(package),
            actions: actions
                .into_iter()
                .map(|(action, result)| ActionOutcome { action, result })
                .collect(),
        }]
    }

    /// The moment the tests say a run happened, in seconds since the epoch.
    const NOW: u64 = 500;

    /// An earlier moment, for records that already existed.
    const EARLIER: u64 = 100;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// A record made at `NOW`, which is what `remember_links(.., at(NOW))` writes.
    fn record(package: &str, link: &str, destination: &str) -> LinkRecord {
        record_at(package, link, destination, NOW)
    }

    fn record_at(package: &str, link: &str, destination: &str, linked_at: u64) -> LinkRecord {
        LinkRecord {
            package: name(package),
            link: path(link),
            destination: path(destination),
            linked_at,
        }
    }

    fn remember(repo: &mut RepoState, outcomes: &[PackageOutcome]) {
        remember_links(repo, outcomes, at(NOW));
    }

    fn repo_with(links: Vec<LinkRecord>) -> RepoState {
        RepoState {
            links,
            ..RepoState::default()
        }
    }

    #[test]
    fn a_created_link_is_remembered_with_its_package_and_destination() {
        let mut repo = RepoState::default();

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done())],
            ),
        );

        assert_eq!(
            repo.links,
            [record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]
        );
    }

    #[test]
    fn a_created_link_is_stamped_with_the_time_it_was_made() {
        let mut repo = RepoState::default();

        remember_links(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done())],
            ),
            at(1234),
        );

        assert_eq!(repo.links[0].linked_at, 1234);
    }

    #[test]
    fn a_clock_before_the_epoch_stamps_a_link_as_unknown_rather_than_failing() {
        let mut repo = RepoState::default();

        remember_links(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done())],
            ),
            UNIX_EPOCH - Duration::from_secs(10),
        );

        assert_eq!(repo.links[0].linked_at, 0);
    }

    #[test]
    fn a_created_copy_of_a_package_symlink_is_remembered_with_its_destination() {
        let mut repo = RepoState::default();
        let action = Action::LinkSymlinkCopy {
            destination: path("/usr/share/theme"),
            link: path("/h/.theme"),
        };

        remember(&mut repo, &outcomes("zsh", vec![(action, done())]));

        assert_eq!(repo.links, [record("zsh", "/h/.theme", "/usr/share/theme")]);
    }

    #[test]
    fn a_link_that_was_already_in_place_is_not_remembered() {
        let mut repo = RepoState::default();

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(
                    link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"),
                    skipped("already linked"),
                )],
            ),
        );

        assert!(repo.links.is_empty());
    }

    #[test]
    fn a_link_that_failed_is_not_remembered() {
        let mut repo = RepoState::default();

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), failed())],
            ),
        );

        assert!(repo.links.is_empty());
    }

    #[test]
    fn actions_that_are_not_links_are_not_remembered() {
        let mut repo = RepoState::default();

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![
                    (unlink_action("/h/.zshrc", "/r/packages/zsh/.zshrc"), done()),
                    (
                        Action::RemoveDirectoryIfEmpty {
                            path: path("/h/.d"),
                        },
                        done(),
                    ),
                ],
            ),
        );

        assert!(repo.links.is_empty());
    }

    #[test]
    fn remembering_a_link_again_replaces_the_record_at_that_path() {
        let mut repo = repo_with(vec![record("old", "/h/.zshrc", "/r/packages/old/.zshrc")]);

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done())],
            ),
        );

        assert_eq!(
            repo.links,
            [record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]
        );
    }

    #[test]
    fn remembering_a_link_again_refreshes_the_time_it_was_made() {
        let mut repo = repo_with(vec![record_at(
            "zsh",
            "/h/.zshrc",
            "/r/packages/zsh/.zshrc",
            EARLIER,
        )]);

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done())],
            ),
        );

        assert_eq!(repo.links[0].linked_at, NOW);
    }

    #[test]
    fn remembering_leaves_the_time_of_other_links_alone() {
        let mut repo = repo_with(vec![record_at(
            "vim",
            "/h/.vimrc",
            "/r/packages/vim/.vimrc",
            EARLIER,
        )]);

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done())],
            ),
        );

        let vim = repo
            .links
            .iter()
            .find(|r| r.package == name("vim"))
            .unwrap();
        assert_eq!(vim.linked_at, EARLIER);
    }

    #[test]
    fn remembering_keeps_the_records_of_other_links() {
        let mut repo = repo_with(vec![record("vim", "/h/.vimrc", "/r/packages/vim/.vimrc")]);

        remember(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done())],
            ),
        );

        assert_eq!(repo.links.len(), 2);
    }

    #[test]
    fn a_removed_link_is_forgotten() {
        let mut repo = repo_with(vec![record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]);

        forget_links(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(unlink_action("/h/.zshrc", "/r/packages/zsh/.zshrc"), done())],
            ),
        );

        assert!(repo.links.is_empty());
    }

    #[test]
    fn a_link_that_was_already_removed_is_forgotten() {
        let mut repo = repo_with(vec![record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]);

        forget_links(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(
                    unlink_action("/h/.zshrc", "/r/packages/zsh/.zshrc"),
                    skipped("already removed"),
                )],
            ),
        );

        assert!(repo.links.is_empty());
    }

    #[test]
    fn a_link_that_was_never_there_is_forgotten() {
        let mut repo = repo_with(vec![record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]);
        let action = Action::Skip {
            link: path("/h/.zshrc"),
            reason: SkipReason::NotLinked,
        };

        forget_links(
            &mut repo,
            &outcomes("zsh", vec![(action, skipped("not linked"))]),
        );

        assert!(repo.links.is_empty());
    }

    #[test]
    fn a_removal_that_failed_keeps_the_record() {
        let mut repo = repo_with(vec![record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]);

        forget_links(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(
                    unlink_action("/h/.zshrc", "/r/packages/zsh/.zshrc"),
                    failed(),
                )],
            ),
        );

        assert_eq!(repo.links.len(), 1);
    }

    #[test]
    fn a_path_that_something_else_now_occupies_keeps_the_record() {
        let mut repo = repo_with(vec![record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]);

        for reason in [SkipReason::NotOurSymlink, SkipReason::NotASymlink] {
            let action = Action::Skip {
                link: path("/h/.zshrc"),
                reason,
            };
            forget_links(
                &mut repo,
                &outcomes("zsh", vec![(action, skipped("something else is there"))]),
            );
        }

        assert_eq!(repo.links.len(), 1);
    }

    #[test]
    fn forgetting_keeps_the_records_of_other_links() {
        let mut repo = repo_with(vec![
            record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc"),
            record("zsh", "/h/.zshenv", "/r/packages/zsh/.zshenv"),
        ]);

        forget_links(
            &mut repo,
            &outcomes(
                "zsh",
                vec![(unlink_action("/h/.zshrc", "/r/packages/zsh/.zshrc"), done())],
            ),
        );

        assert_eq!(
            repo.links,
            [record("zsh", "/h/.zshenv", "/r/packages/zsh/.zshenv")]
        );
    }

    /// A repo with the given packages and an empty home, both on disk.
    struct World {
        root: tempfile::TempDir,
        home: PathBuf,
        repo: Repo,
    }

    impl World {
        fn source(&self, package: &str, file: &str) -> PathBuf {
            self.root
                .path()
                .join("repo/packages")
                .join(package)
                .join(file)
        }

        fn in_home(&self, file: &str) -> PathBuf {
            self.home.join(file)
        }

        fn context(&self) -> PlanContext<'_> {
            PlanContext {
                repo: &self.repo,
                home: &self.home,
                target_override: None,
            }
        }

        /// Makes the symlink `file` in the home point at `destination`.
        fn symlink_in_home(&self, file: &str, destination: &Path) {
            let link = self.in_home(file);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(destination, link).unwrap();
        }

        fn record(&self, package: &str, file: &str) -> LinkRecord {
            LinkRecord {
                package: name(package),
                link: self.in_home(file),
                destination: self.source(package, file),
                linked_at: EARLIER,
            }
        }
    }

    /// Builds a repo where each package has the given files.
    fn world(packages: &[(&str, &[&str])]) -> World {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
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
        World { root, home, repo }
    }

    fn remembered(links: Vec<LinkRecord>) -> RepoState {
        repo_with(links)
    }

    fn stale(world: &World, links: Vec<LinkRecord>) -> Vec<StaleLink> {
        stale_links(&world.context(), &remembered(links), &RealFileSystem).unwrap()
    }

    #[test]
    fn a_recorded_link_whose_file_was_removed_from_the_package_is_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let removed = world.record("zsh", ".zshenv");
        world.symlink_in_home(".zshenv", &removed.destination);

        let found = stale(&world, vec![removed.clone()]);

        assert_eq!(
            found,
            [StaleLink {
                package: removed.package,
                link: removed.link,
                destination: removed.destination,
                linked_at: EARLIER,
            }]
        );
    }

    #[test]
    fn a_stale_link_carries_the_time_it_was_made() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let mut removed = world.record("zsh", ".zshenv");
        removed.linked_at = 4321;
        world.symlink_in_home(".zshenv", &removed.destination);

        let found = stale(&world, vec![removed]);

        assert_eq!(found[0].linked_at, 4321);
    }

    #[test]
    fn a_recorded_link_of_a_package_gone_from_the_repo_is_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let gone = world.record("vim", ".vimrc");
        world.symlink_in_home(".vimrc", &gone.destination);

        let found = stale(&world, vec![gone]);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].package, name("vim"));
    }

    #[test]
    fn a_recorded_link_the_package_still_provides_is_not_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let current = world.record("zsh", ".zshrc");
        world.symlink_in_home(".zshrc", &current.destination);

        assert!(stale(&world, vec![current]).is_empty());
    }

    #[test]
    fn a_recorded_link_that_is_already_gone_is_not_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);

        assert!(stale(&world, vec![world.record("zsh", ".zshenv")]).is_empty());
    }

    #[test]
    fn a_recorded_path_that_is_now_a_real_file_is_not_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let replaced = world.record("zsh", ".zshenv");
        fs::write(&replaced.link, "mine").unwrap();

        assert!(stale(&world, vec![replaced]).is_empty());
    }

    #[test]
    fn a_recorded_path_that_now_points_elsewhere_is_not_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let moved = world.record("zsh", ".zshenv");
        world.symlink_in_home(".zshenv", Path::new("/somewhere/else"));

        assert!(stale(&world, vec![moved]).is_empty());
    }

    #[test]
    fn a_stale_link_whose_destination_is_missing_is_still_stale() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let removed = world.record("zsh", ".zshenv");
        assert!(!removed.destination.exists());
        world.symlink_in_home(".zshenv", &removed.destination);

        assert_eq!(stale(&world, vec![removed]).len(), 1);
    }

    #[test]
    fn stale_links_are_sorted_by_link_path() {
        let world = world(&[]);
        let b = world.record("vim", ".b");
        let a = world.record("vim", ".a");
        world.symlink_in_home(".b", &b.destination);
        world.symlink_in_home(".a", &a.destination);

        let found = stale(&world, vec![b, a]);

        let links: Vec<&PathBuf> = found.iter().map(|link| &link.link).collect();
        assert_eq!(links, [&world.in_home(".a"), &world.in_home(".b")]);
    }

    #[test]
    fn unlinking_stale_links_plans_an_unlink_expecting_the_recorded_destination() {
        let world = world(&[]);
        let gone = world.record("vim", ".vimrc");
        world.symlink_in_home(".vimrc", &gone.destination);

        let plan = plan_unlink_stale(
            &world.context(),
            &[name("vim")],
            &remembered(vec![gone.clone()]),
            &RealFileSystem,
        )
        .unwrap();

        assert_eq!(
            plan,
            [PackageActions {
                package: name("vim"),
                actions: vec![Action::Unlink {
                    link: gone.link,
                    expected_destination: gone.destination,
                }],
            }]
        );
    }

    #[test]
    fn unlinking_stale_links_only_plans_the_named_packages() {
        let world = world(&[]);
        let vim = world.record("vim", ".vimrc");
        let emacs = world.record("emacs", ".emacs");
        world.symlink_in_home(".vimrc", &vim.destination);
        world.symlink_in_home(".emacs", &emacs.destination);

        let plan = plan_unlink_stale(
            &world.context(),
            &[name("vim")],
            &remembered(vec![vim, emacs]),
            &RealFileSystem,
        )
        .unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].package, name("vim"));
    }

    fn unlink_with_stale(
        world: &World,
        packages: &[&str],
        prune: bool,
        links: Vec<LinkRecord>,
    ) -> Result<Vec<PackageActions>, PlanError> {
        let packages: Vec<PackageName> = packages.iter().map(|package| name(package)).collect();
        plan_unlink_with_stale(
            &world.context(),
            &packages,
            prune,
            &remembered(links),
            &RealFileSystem,
        )
    }

    fn unlinked_paths(plan: &[PackageActions]) -> Vec<PathBuf> {
        plan.iter()
            .flat_map(|package| &package.actions)
            .filter_map(|action| match action {
                Action::Unlink { link, .. } => Some(link.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unlink_of_a_package_gone_from_the_repo_removes_its_recorded_links() {
        let world = world(&[]);
        let gone = world.record("vim", ".vimrc");
        world.symlink_in_home(".vimrc", &gone.destination);

        let plan = unlink_with_stale(&world, &["vim"], false, vec![gone.clone()]).unwrap();

        assert_eq!(unlinked_paths(&plan), [gone.link]);
    }

    #[test]
    fn unlink_of_a_package_gone_from_the_repo_with_nothing_recorded_is_an_error() {
        let world = world(&[]);

        let error = unlink_with_stale(&world, &["vim"], false, vec![]).unwrap_err();

        assert!(matches!(error, PlanError::UnknownPackage(package) if package == name("vim")));
    }

    #[test]
    fn unlink_of_a_package_gone_from_the_repo_whose_links_are_all_gone_is_an_error() {
        let world = world(&[]);

        let error = unlink_with_stale(&world, &["vim"], false, vec![world.record("vim", ".vimrc")])
            .unwrap_err();

        assert!(matches!(error, PlanError::UnknownPackage(_)));
    }

    #[test]
    fn unlink_of_a_package_in_the_repo_also_removes_its_stale_links() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let current = world.record("zsh", ".zshrc");
        let removed = world.record("zsh", ".zshenv");
        world.symlink_in_home(".zshrc", &current.destination);
        world.symlink_in_home(".zshenv", &removed.destination);

        let plan = unlink_with_stale(&world, &["zsh"], false, vec![removed.clone()]).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(
            unlinked_paths(&plan),
            [current.link, removed.link],
            "its own link first, then the stale one"
        );
    }

    #[test]
    fn unlink_of_a_package_in_the_repo_with_nothing_stale_is_the_ordinary_plan() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let current = world.record("zsh", ".zshrc");
        world.symlink_in_home(".zshrc", &current.destination);

        let plan = unlink_with_stale(&world, &["zsh"], false, vec![]).unwrap();

        assert_eq!(unlinked_paths(&plan), [current.link]);
    }

    #[test]
    fn pruning_comes_after_the_stale_link_inside_the_directory_is_removed() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let removed = world.record("zsh", ".config/zsh/env");
        world.symlink_in_home(".config/zsh/env", &removed.destination);

        let plan = unlink_with_stale(&world, &["zsh"], true, vec![removed.clone()]).unwrap();

        let actions = &plan[0].actions;
        let unlink_at = actions
            .iter()
            .position(|action| matches!(action, Action::Unlink { .. }))
            .unwrap();
        let first_prune = actions
            .iter()
            .position(|action| matches!(action, Action::RemoveDirectoryIfEmpty { .. }))
            .unwrap();
        assert!(unlink_at < first_prune, "{actions:?}");
        assert!(actions.contains(&Action::RemoveDirectoryIfEmpty {
            path: world.in_home(".config/zsh")
        }));
    }

    #[test]
    fn a_package_gone_from_the_repo_is_never_pruned() {
        let world = world(&[]);
        let gone = world.record("vim", ".config/vim/vimrc");
        world.symlink_in_home(".config/vim/vimrc", &gone.destination);

        let plan = unlink_with_stale(&world, &["vim"], true, vec![gone]).unwrap();

        assert!(!plan[0]
            .actions
            .iter()
            .any(|action| matches!(action, Action::RemoveDirectoryIfEmpty { .. })));
    }

    /// A filesystem where nothing is writable, so any removal needs root.
    struct ReadOnly;

    impl FileSystemState for ReadOnly {
        fn state(&self, path: &Path) -> Result<PathState, crate::link::FileSystemStateError> {
            RealFileSystem.state(path)
        }

        fn is_writable(&self, _path: &Path) -> bool {
            false
        }
    }

    #[test]
    fn removing_a_link_in_an_unwritable_directory_needs_root() {
        let world = world(&[]);
        let gone = world.record("vim", ".vimrc");
        let plan = vec![PackageActions {
            package: name("vim"),
            actions: vec![Action::Unlink {
                link: gone.link.clone(),
                expected_destination: gone.destination,
            }],
        }];

        let reasons = root_reasons_for_stale(&plan, &ReadOnly);

        assert_eq!(
            reasons,
            [RootReason::TargetNotWritable {
                package: name("vim"),
                path: world.home.clone(),
            }]
        );
    }

    #[test]
    fn removing_a_link_in_a_writable_directory_needs_no_root() {
        let world = world(&[]);
        let gone = world.record("vim", ".vimrc");
        let plan = vec![PackageActions {
            package: name("vim"),
            actions: vec![Action::Unlink {
                link: gone.link,
                expected_destination: gone.destination,
            }],
        }];

        assert!(root_reasons_for_stale(&plan, &RealFileSystem).is_empty());
    }

    #[test]
    fn unlinking_a_package_with_nothing_stale_plans_nothing() {
        let world = world(&[("zsh", &[".zshrc"])]);
        let current = world.record("zsh", ".zshrc");
        world.symlink_in_home(".zshrc", &current.destination);

        let plan = plan_unlink_stale(
            &world.context(),
            &[name("zsh")],
            &remembered(vec![current]),
            &RealFileSystem,
        )
        .unwrap();

        assert!(plan.is_empty());
    }

    #[test]
    fn a_relink_forgets_then_remembers_so_the_record_ends_up_current() {
        let mut repo = repo_with(vec![record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]);
        let relink = outcomes(
            "zsh",
            vec![
                (unlink_action("/h/.zshrc", "/r/packages/zsh/.zshrc"), done()),
                (link_action("/r/packages/zsh/.zshrc", "/h/.zshrc"), done()),
            ],
        );

        forget_links(&mut repo, &relink);
        remember(&mut repo, &relink);

        assert_eq!(
            repo.links,
            [record("zsh", "/h/.zshrc", "/r/packages/zsh/.zshrc")]
        );
    }
}
