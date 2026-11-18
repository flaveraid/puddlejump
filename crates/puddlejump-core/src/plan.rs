//! Planning: deciding what to do, without doing it.
//!
//! Every function here is pure apart from reading through [`FileSystemState`],
//! so a plan can be printed, reviewed, or applied later.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::graph::{DependencyGraph, GraphError, UnknownPackageError};
use crate::hook_state::{fingerprint, reason_to_run, HookReason};
use crate::link::{FileSystemState, FileSystemStateError, PathState};
use crate::manifest::Target;
use crate::package::{Package, PackageError, PackageName, Repo};
use crate::state::RepoState;
use crate::system::{plan_system, PackageManager, SystemError, SystemPlan};

/// Directory under the repo root that holds the packages; needed to tell which
/// package a link points into.
const PACKAGES_DIR_NAME: &str = "packages";

/// Facts about the environment that planning needs, passed in rather than read
/// so planning stays deterministic.
#[derive(Debug, Clone, Copy)]
pub struct PlanContext<'a> {
    /// The repo being planned.
    pub repo: &'a Repo,
    /// What `~` means in a manifest target.
    pub home: &'a Path,
    /// Replaces every package's target when set (the `--target` flag).
    pub target_override: Option<&'a Path>,
}

/// One step the planner wants taken on one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Action {
    /// Create `link` as a symlink to `source`.
    Link {
        /// The file in the package.
        source: PathBuf,
        /// Where the symlink is created.
        link: PathBuf,
    },
    /// Create `link` as a copy of a symlink that lives in a package.
    LinkSymlinkCopy {
        /// The destination the package's symlink points to.
        destination: PathBuf,
        /// Where the symlink is created.
        link: PathBuf,
    },
    /// Remove the symlink at `link`, provided it still points at
    /// `expected_destination` when this action runs.
    Unlink {
        /// The symlink to remove.
        link: PathBuf,
        /// Where the symlink must point for it to be removed.
        expected_destination: PathBuf,
    },
    /// Nothing to do for `link`.
    Skip {
        /// The path that was skipped.
        link: PathBuf,
        /// Why nothing needs doing.
        reason: SkipReason,
    },
    /// The path cannot be handled without user intervention.
    Conflict {
        /// The path in the way.
        path: PathBuf,
        /// What is in the way.
        kind: ConflictKind,
    },
    /// Remove `path` if it is empty when this action runs (`--prune`).
    RemoveDirectoryIfEmpty {
        /// The directory to remove.
        path: PathBuf,
    },
}

/// Why an action was skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SkipReason {
    /// The link already points where it should.
    AlreadyLinked,
    /// Nothing exists at the path, so there is nothing to unlink.
    NotLinked,
    /// The path is a symlink that does not point at this package's file.
    NotOurSymlink,
    /// The path is a real file or directory, which is never removed.
    NotASymlink,
    /// The package's symlink has a relative destination, which would mean
    /// something different once copied.
    RelativeSymlink,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            SkipReason::AlreadyLinked => "already linked",
            SkipReason::NotLinked => "not linked",
            SkipReason::NotOurSymlink => "symlink points somewhere else",
            SkipReason::NotASymlink => "not a symlink",
            SkipReason::RelativeSymlink => "relative symlink destination",
        })
    }
}

/// What stands in the way of creating a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ConflictKind {
    /// A real file exists at the path.
    RealFile,
    /// A real directory exists at the path.
    RealDirectory,
    /// A symlink to somewhere outside the repo's packages exists at the path.
    ForeignSymlink,
    /// A symlink whose destination does not exist is at the path.
    BrokenSymlink,
    /// A symlink into a different package of this repo is at the path.
    OtherPackage {
        /// The package the existing link points into.
        owner: PackageName,
    },
    /// An earlier package in this plan already claims the path.
    ProvidedTwice {
        /// The package that claimed it first.
        other: PackageName,
    },
    /// A parent directory of the path is a file or a broken symlink.
    ParentNotDirectory {
        /// The parent that is in the way.
        parent: PathBuf,
    },
}

/// The actions for one package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageActions {
    /// The package.
    pub package: PackageName,
    /// What to do, in order.
    pub actions: Vec<Action>,
}

/// Why root is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RootReason {
    /// System packages need installing, which needs root.
    SystemInstall,
    /// The package's manifest says `needs_root = true`.
    PackageDeclared {
        /// The package.
        package: PackageName,
    },
    /// The package's target directory is not writable by the current user.
    TargetNotWritable {
        /// The package.
        package: PackageName,
        /// The target directory.
        path: PathBuf,
    },
}

/// Something the user should know that does not stop the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PlanWarning {
    /// The target is not writable but the manifest does not say
    /// `needs_root = true`.
    NeedsRootUndeclared {
        /// The package.
        package: PackageName,
    },
    /// A symlink in the package has a relative destination and was skipped.
    RelativeSymlinkSkipped {
        /// The package.
        package: PackageName,
        /// The symlink's path relative to the package.
        path: PathBuf,
    },
}

/// The file part of a plan for linking packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilePlan {
    /// Per-package actions, in link order.
    pub packages: Vec<PackageActions>,
    /// Why root is needed, if it is.
    pub needs_root: Vec<RootReason>,
    /// Things worth telling the user.
    pub warnings: Vec<PlanWarning>,
}

/// A post-link hook to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookPlan {
    /// The package that declares the hook.
    pub package: PackageName,
    /// Why it is going to run.
    pub reason: HookReason,
    /// The hook script, inside the package directory.
    pub script: PathBuf,
    /// The package directory, which the hook runs in.
    pub package_dir: PathBuf,
    /// The directory the package is linked into.
    pub target_dir: PathBuf,
    /// The repo root.
    pub repo_root: PathBuf,
}

/// Everything `link` would do: link files, install system packages, then run
/// hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Per-package file actions, in link order.
    pub packages: Vec<PackageActions>,
    /// System packages to install.
    pub system: SystemPlan,
    /// Hooks to run after linking and installing, in dependency order.
    pub hooks: Vec<HookPlan>,
    /// Every reason root is needed. Empty means root is not needed.
    pub needs_root: Vec<RootReason>,
    /// Things worth telling the user.
    pub warnings: Vec<PlanWarning>,
}

/// Why a plan could not be built.
#[derive(Debug, Error)]
pub enum PlanError {
    /// System packages could not be checked.
    #[error(transparent)]
    System(#[from] SystemError),
    /// A package's files could not be listed.
    #[error(transparent)]
    Package(#[from] PackageError),
    /// A path could not be inspected.
    #[error(transparent)]
    FileSystem(#[from] FileSystemStateError),
    /// A package named in the request is not in the repo.
    #[error("package {0} does not exist")]
    UnknownPackage(PackageName),
    /// The dependency graph could not be built.
    #[error(transparent)]
    Graph(#[from] GraphError),
}

/// Plans everything `link` does for the packages in `graph`: file actions,
/// system package installs, and hooks, all in dependency order.
///
/// A hook runs when `remembered` says it needs to: it never ran, it failed
/// last time, or its script changed since it last succeeded. A hook that last
/// succeeded with the same script is left out, whether or not its package was
/// named. With `force_hooks`, every hook of the graph runs regardless.
///
/// Installing system packages adds [`RootReason::SystemInstall`].
///
/// Errors if a package's files cannot be listed, a path cannot be inspected,
/// or the package manager cannot be queried.
pub fn plan_full_link(
    context: &PlanContext,
    graph: &DependencyGraph,
    remembered: &RepoState,
    force_hooks: bool,
    state: &dyn FileSystemState,
    manager: Option<&dyn PackageManager>,
) -> Result<Plan, PlanError> {
    let files = plan_link(context, graph, state)?;
    let system = plan_system(context.repo, graph, manager)?;
    let mut needs_root = files.needs_root;
    if !system.to_install.is_empty() {
        needs_root.push(RootReason::SystemInstall);
    }
    let hooks = hooks_to_run(hooks_in_order(context, graph), remembered, force_hooks);
    Ok(Plan {
        packages: files.packages,
        hooks,
        system,
        needs_root,
        warnings: files.warnings,
    })
}

impl Plan {
    /// Whether applying this plan would create at least one link.
    pub fn links_anything(&self) -> bool {
        self.packages.iter().any(links_something)
    }
}

/// Keeps the hooks that need to run, each with the reason, according to what
/// was remembered about their last run.
///
/// With `force_hooks`, every hook is kept and its reason is
/// [`HookReason::Forced`].
fn hooks_to_run(hooks: Vec<HookPlan>, remembered: &RepoState, force_hooks: bool) -> Vec<HookPlan> {
    hooks
        .into_iter()
        .filter_map(|mut hook| {
            hook.reason = if force_hooks {
                HookReason::Forced
            } else {
                let record = remembered
                    .hooks
                    .iter()
                    .find(|record| record.package == hook.package);
                let current = fingerprint(&hook.script).ok();
                reason_to_run(record, current.as_deref())?
            };
            Some(hook)
        })
        .collect()
}

/// Whether applying `package` would create at least one link.
fn links_something(package: &PackageActions) -> bool {
    package
        .actions
        .iter()
        .any(|action| matches!(action, Action::Link { .. } | Action::LinkSymlinkCopy { .. }))
}

/// Plans running the hooks of exactly the named packages, in dependency order,
/// each with the reason [`HookReason::Forced`]: naming a hook explicitly is a
/// request to run it, whatever was remembered.
///
/// Dependencies that are not named are used only to order the named packages;
/// their own hooks are not run. Packages without a hook are left out.
///
/// Errors with [`PlanError::Graph`] if a package is unknown or the
/// dependencies loop.
pub fn plan_hooks(
    context: &PlanContext,
    packages: &[PackageName],
) -> Result<Vec<HookPlan>, PlanError> {
    let graph = DependencyGraph::build(context.repo, packages)?;
    let named: BTreeSet<&PackageName> = packages.iter().collect();
    Ok(hooks_in_order(context, &graph)
        .into_iter()
        .filter(|hook| named.contains(&hook.package))
        .collect())
}

/// The hooks declared by packages in `graph`, in link order.
fn hooks_in_order(context: &PlanContext, graph: &DependencyGraph) -> Vec<HookPlan> {
    graph
        .link_order()
        .into_iter()
        .filter_map(|name| {
            let package = context.repo.packages.get(name)?;
            let hook = package.manifest.hook.as_ref()?;
            Some(HookPlan {
                package: name.clone(),
                reason: HookReason::Forced,
                script: package.dir.join(hook),
                package_dir: package.dir.clone(),
                target_dir: target_dir(context, package),
                repo_root: context.repo.root.clone(),
            })
        })
        .collect()
}

/// Plans linking every package in `graph`, in dependency order.
///
/// Errors if a package's files cannot be listed or a path cannot be inspected.
/// Conflicts are not errors; they are [`Action::Conflict`] entries.
pub fn plan_link(
    context: &PlanContext,
    graph: &DependencyGraph,
    state: &dyn FileSystemState,
) -> Result<FilePlan, PlanError> {
    let mut claimed: BTreeMap<PathBuf, PackageName> = BTreeMap::new();
    let mut plan = FilePlan {
        packages: Vec::new(),
        needs_root: Vec::new(),
        warnings: Vec::new(),
    };
    for name in graph.link_order() {
        let package = package_in(context.repo, name)?;
        let target_dir = target_dir(context, package);
        note_root_needs(package, &target_dir, state, &mut plan);
        let actions = plan_link_package(
            context,
            package,
            &target_dir,
            state,
            &mut claimed,
            &mut plan.warnings,
        )?;
        plan.packages.push(PackageActions {
            package: name.clone(),
            actions,
        });
    }
    Ok(plan)
}

/// Plans removing the links of each named package, in the order given.
///
/// Only a symlink that points at the package's own file is removed. With
/// `prune`, directories on the path of removed links are also listed for
/// removal if they end up empty.
///
/// Errors if a package is unknown, its files cannot be listed, or a path
/// cannot be inspected.
pub fn plan_unlink(
    context: &PlanContext,
    packages: &[PackageName],
    prune: bool,
    state: &dyn FileSystemState,
) -> Result<Vec<PackageActions>, PlanError> {
    let mut plans = Vec::new();
    for name in packages {
        let package = package_in(context.repo, name)?;
        let target_dir = target_dir(context, package);
        let mut actions = Vec::new();
        for relative in package.files()? {
            let link = target_dir.join(&relative);
            actions.push(unlink_action(package, &relative, link, state)?);
        }
        if prune {
            actions.extend(prune_actions(&target_dir, &actions));
        }
        plans.push(PackageActions {
            package: name.clone(),
            actions,
        });
    }
    Ok(plans)
}

/// The file part of a relink: removals first, then links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelinkPlan {
    /// Removing the current links of the named packages.
    pub unlink: Vec<PackageActions>,
    /// Linking the named packages again. Planned as if the removals had
    /// already happened, so it does not report the old links as in the way.
    pub link: FilePlan,
}

/// Plans removing and re-creating the links of exactly the named packages,
/// without resolving their dependencies.
///
/// The link half is planned against the state after the removals: a path
/// the unlink half will remove is treated as missing.
///
/// Errors if a package is unknown, its files cannot be listed, or a path
/// cannot be inspected.
pub fn plan_relink(
    context: &PlanContext,
    packages: &[PackageName],
    prune: bool,
    state: &dyn FileSystemState,
) -> Result<RelinkPlan, PlanError> {
    let unlink = plan_unlink(context, packages, prune, state)?;
    let removed: BTreeSet<PathBuf> = unlink
        .iter()
        .flat_map(|plan| &plan.actions)
        .filter_map(|action| match action {
            Action::Unlink { link, .. } => Some(link.clone()),
            _ => None,
        })
        .collect();
    let graph = DependencyGraph::without_dependencies(context.repo, packages)
        .map_err(|UnknownPackageError(package)| PlanError::UnknownPackage(package))?;
    let after_removal = AfterRemoval { state, removed };
    let link = plan_link(context, &graph, &after_removal)?;
    Ok(RelinkPlan { unlink, link })
}

/// A view of the filesystem as it will be once some links are removed.
struct AfterRemoval<'a> {
    state: &'a dyn FileSystemState,
    removed: BTreeSet<PathBuf>,
}

impl FileSystemState for AfterRemoval<'_> {
    fn state(&self, path: &Path) -> Result<PathState, FileSystemStateError> {
        if self.removed.contains(path) {
            return Ok(PathState::Missing);
        }
        self.state.state(path)
    }

    fn is_writable(&self, path: &Path) -> bool {
        self.state.is_writable(path)
    }
}

pub(crate) fn package_in<'a>(repo: &'a Repo, name: &PackageName) -> Result<&'a Package, PlanError> {
    repo.packages
        .get(name)
        .ok_or_else(|| PlanError::UnknownPackage(name.clone()))
}

pub(crate) fn target_dir(context: &PlanContext, package: &Package) -> PathBuf {
    if let Some(target) = context.target_override {
        return target.to_path_buf();
    }
    match &package.manifest.target {
        Target::Home => context.home.to_path_buf(),
        Target::Absolute(path) => path.clone(),
    }
}

/// Why `package` needs root to change `target_dir`, and a warning if it does
/// but its manifest does not say so.
fn root_needs(
    package: &Package,
    target_dir: &Path,
    state: &dyn FileSystemState,
) -> (Vec<RootReason>, Option<PlanWarning>) {
    let declared = package.manifest.needs_root;
    let not_writable = !state.is_writable(target_dir);
    let mut reasons = Vec::new();
    if declared {
        reasons.push(RootReason::PackageDeclared {
            package: package.name.clone(),
        });
    }
    if not_writable {
        reasons.push(RootReason::TargetNotWritable {
            package: package.name.clone(),
            path: target_dir.to_path_buf(),
        });
    }
    let warning = (not_writable && !declared).then(|| PlanWarning::NeedsRootUndeclared {
        package: package.name.clone(),
    });
    (reasons, warning)
}

fn note_root_needs(
    package: &Package,
    target_dir: &Path,
    state: &dyn FileSystemState,
    plan: &mut FilePlan,
) {
    let (reasons, warning) = root_needs(package, target_dir, state);
    plan.needs_root.extend(reasons);
    plan.warnings.extend(warning);
}

/// Why changing the links of each named package needs root, in the order
/// given. Used by operations that change links without planning a whole
/// `link`, such as unlink, relink, and adopt.
///
/// Errors if a package is unknown.
pub fn root_reasons_for(
    context: &PlanContext,
    packages: &[PackageName],
    state: &dyn FileSystemState,
) -> Result<Vec<RootReason>, PlanError> {
    let mut reasons = Vec::new();
    for name in packages {
        let package = package_in(context.repo, name)?;
        let (needed, _warning) = root_needs(package, &target_dir(context, package), state);
        reasons.extend(needed);
    }
    Ok(reasons)
}

fn plan_link_package(
    context: &PlanContext,
    package: &Package,
    target_dir: &Path,
    state: &dyn FileSystemState,
    claimed: &mut BTreeMap<PathBuf, PackageName>,
    warnings: &mut Vec<PlanWarning>,
) -> Result<Vec<Action>, PlanError> {
    let mut actions = Vec::new();
    for relative in package.files()? {
        let link = target_dir.join(&relative);
        if let Some(other) = claimed.get(&link) {
            actions.push(Action::Conflict {
                path: link,
                kind: ConflictKind::ProvidedTwice {
                    other: other.clone(),
                },
            });
            continue;
        }
        claimed.insert(link.clone(), package.name.clone());
        let source = package.dir.join(&relative);
        let action = match package_file_kind(&source, state)? {
            PackageFile::Regular => {
                let wanted = WantedLink {
                    link,
                    destination: source,
                    copies_package_symlink: false,
                };
                link_action(context.repo, package, wanted, target_dir, state)?
            }
            PackageFile::AbsoluteSymlink(destination) => {
                let wanted = WantedLink {
                    link,
                    destination,
                    copies_package_symlink: true,
                };
                link_action(context.repo, package, wanted, target_dir, state)?
            }
            PackageFile::RelativeSymlink => {
                warnings.push(PlanWarning::RelativeSymlinkSkipped {
                    package: package.name.clone(),
                    path: relative,
                });
                Action::Skip {
                    link,
                    reason: SkipReason::RelativeSymlink,
                }
            }
        };
        actions.push(action);
    }
    Ok(actions)
}

enum PackageFile {
    Regular,
    AbsoluteSymlink(PathBuf),
    RelativeSymlink,
}

fn package_file_kind(source: &Path, state: &dyn FileSystemState) -> Result<PackageFile, PlanError> {
    Ok(match state.state(source)? {
        PathState::Symlink { destination } | PathState::BrokenSymlink { destination } => {
            if destination.is_absolute() {
                PackageFile::AbsoluteSymlink(destination)
            } else {
                PackageFile::RelativeSymlink
            }
        }
        _ => PackageFile::Regular,
    })
}

/// A link to create for one package file.
struct WantedLink {
    /// Where the symlink goes.
    link: PathBuf,
    /// What it points at.
    destination: PathBuf,
    /// Whether it is a copy of a symlink that lives in the package, rather
    /// than a link to a regular file.
    copies_package_symlink: bool,
}

impl WantedLink {
    /// The action that creates this link.
    fn create(self) -> Action {
        if self.copies_package_symlink {
            Action::LinkSymlinkCopy {
                destination: self.destination,
                link: self.link,
            }
        } else {
            Action::Link {
                source: self.destination,
                link: self.link,
            }
        }
    }

    /// Whether a symlink already at the path is this link.
    ///
    /// A copy of a package symlink may legitimately point at something that
    /// does not exist, so a broken link with the right destination counts. A
    /// link to a regular file in the repo that is broken does not: the file
    /// is there, so a broken link to it is wrong.
    fn is_already_in_place(&self, existing: &PathState) -> bool {
        match existing {
            PathState::Symlink { destination } => *destination == self.destination,
            PathState::BrokenSymlink { destination } => {
                self.copies_package_symlink && *destination == self.destination
            }
            PathState::Missing | PathState::File | PathState::Directory => false,
        }
    }
}

/// Decides what to do about `wanted`: create it, skip it if it is already in
/// place, or report what is in the way.
fn link_action(
    repo: &Repo,
    package: &Package,
    wanted: WantedLink,
    target_dir: &Path,
    state: &dyn FileSystemState,
) -> Result<Action, PlanError> {
    if let Some(kind) = blocked_ancestor(&wanted.link, target_dir, state)? {
        return Ok(Action::Conflict {
            path: wanted.link,
            kind,
        });
    }
    let existing = state.state(&wanted.link)?;
    Ok(if existing == PathState::Missing {
        wanted.create()
    } else if wanted.is_already_in_place(&existing) {
        Action::Skip {
            link: wanted.link,
            reason: SkipReason::AlreadyLinked,
        }
    } else {
        Action::Conflict {
            kind: conflict_kind(repo, package, &existing),
            path: wanted.link,
        }
    })
}

/// Classifies something already at a link path that is not the link we want.
fn conflict_kind(repo: &Repo, package: &Package, existing: &PathState) -> ConflictKind {
    match existing {
        PathState::File => ConflictKind::RealFile,
        PathState::Directory => ConflictKind::RealDirectory,
        PathState::BrokenSymlink { .. } => ConflictKind::BrokenSymlink,
        PathState::Symlink { destination } => match owning_package(repo, destination) {
            Some(owner) if owner != package.name => ConflictKind::OtherPackage { owner },
            _ => ConflictKind::ForeignSymlink,
        },
        // Missing is handled by callers before asking for a conflict.
        PathState::Missing => ConflictKind::RealFile,
    }
}

/// The package whose directory contains `destination`, if any.
fn owning_package(repo: &Repo, destination: &Path) -> Option<PackageName> {
    let packages_dir = repo.root.join(PACKAGES_DIR_NAME);
    let relative = destination.strip_prefix(packages_dir).ok()?;
    let first = relative.components().next()?;
    PackageName::new(first.as_os_str().to_str()?).ok()
}

/// Finds an ancestor of `link` below `target_dir` that is not a directory (or
/// a symlink to one), which would stop the link from being created.
fn blocked_ancestor(
    link: &Path,
    target_dir: &Path,
    state: &dyn FileSystemState,
) -> Result<Option<ConflictKind>, PlanError> {
    let ancestors: Vec<&Path> = link
        .ancestors()
        .skip(1)
        .take_while(|ancestor| *ancestor != target_dir)
        .collect();
    // Check from the top down so the outermost problem is the one reported.
    for ancestor in ancestors.into_iter().rev() {
        match state.state(ancestor)? {
            PathState::Missing | PathState::Directory | PathState::Symlink { .. } => {}
            PathState::File | PathState::BrokenSymlink { .. } => {
                return Ok(Some(ConflictKind::ParentNotDirectory {
                    parent: ancestor.to_path_buf(),
                }));
            }
        }
    }
    Ok(None)
}

fn unlink_action(
    package: &Package,
    relative: &Path,
    link: PathBuf,
    state: &dyn FileSystemState,
) -> Result<Action, PlanError> {
    let source = package.dir.join(relative);
    let ours = match package_file_kind(&source, state)? {
        PackageFile::AbsoluteSymlink(destination) => destination,
        PackageFile::Regular | PackageFile::RelativeSymlink => source,
    };
    Ok(match state.state(&link)? {
        PathState::Missing => Action::Skip {
            link,
            reason: SkipReason::NotLinked,
        },
        PathState::Symlink { destination } | PathState::BrokenSymlink { destination }
            if destination == ours =>
        {
            Action::Unlink {
                link,
                expected_destination: ours,
            }
        }
        PathState::Symlink { .. } | PathState::BrokenSymlink { .. } => Action::Skip {
            link,
            reason: SkipReason::NotOurSymlink,
        },
        PathState::File | PathState::Directory => Action::Skip {
            link,
            reason: SkipReason::NotASymlink,
        },
    })
}

/// Directories on the path of removed links, deepest first, never the target
/// root itself.
fn prune_actions(target_dir: &Path, actions: &[Action]) -> Vec<Action> {
    let mut directories = BTreeSet::new();
    for action in actions {
        if let Action::Unlink { link, .. } = action {
            directories.extend(
                link.ancestors()
                    .skip(1)
                    .take_while(|ancestor| *ancestor != target_dir)
                    .map(Path::to_path_buf),
            );
        }
    }
    let mut directories: Vec<PathBuf> = directories.into_iter().collect();
    // Deepest first: a parent can only be empty once its children are gone.
    directories.sort_by_key(|directory| std::cmp::Reverse(directory.components().count()));
    directories
        .into_iter()
        .map(|path| Action::RemoveDirectoryIfEmpty { path })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::state::{HookRecord, HookStatus};
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    const HOME: &str = "/home/me";

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn names(texts: &[&str]) -> Vec<PackageName> {
        texts.iter().map(|text| name(text)).collect()
    }

    /// An in-memory filesystem: paths map to their state, anything absent is
    /// missing, and `read_only` lists directories that are not writable.
    #[derive(Default)]
    struct FakeFileSystem {
        states: RefCell<BTreeMap<PathBuf, PathState>>,
        read_only: BTreeSet<PathBuf>,
    }

    impl FakeFileSystem {
        fn with(self, path: &str, state: PathState) -> Self {
            self.states.borrow_mut().insert(PathBuf::from(path), state);
            self
        }

        fn file(self, path: &str) -> Self {
            self.with(path, PathState::File)
        }

        fn directory(self, path: &str) -> Self {
            self.with(path, PathState::Directory)
        }

        fn symlink(self, path: &str, destination: &str) -> Self {
            self.with(
                path,
                PathState::Symlink {
                    destination: PathBuf::from(destination),
                },
            )
        }

        fn broken_symlink(self, path: &str, destination: &str) -> Self {
            self.with(
                path,
                PathState::BrokenSymlink {
                    destination: PathBuf::from(destination),
                },
            )
        }

        fn read_only(mut self, path: &str) -> Self {
            self.read_only.insert(PathBuf::from(path));
            self
        }
    }

    impl FileSystemState for FakeFileSystem {
        fn state(&self, path: &Path) -> Result<PathState, FileSystemStateError> {
            Ok(self
                .states
                .borrow()
                .get(path)
                .cloned()
                .unwrap_or(PathState::Missing))
        }

        fn is_writable(&self, path: &Path) -> bool {
            !path
                .ancestors()
                .any(|ancestor| self.read_only.contains(ancestor))
        }
    }

    /// Describes a package: its name, manifest, and the files in it. A
    /// `files` entry is a path relative to the package.
    struct PackageSpec {
        name: &'static str,
        manifest: Manifest,
        files: Vec<&'static str>,
    }

    fn package(name: &'static str, files: &[&'static str]) -> PackageSpec {
        PackageSpec {
            name,
            manifest: Manifest::default(),
            files: files.to_vec(),
        }
    }

    impl PackageSpec {
        fn manifest(mut self, manifest: Manifest) -> Self {
            self.manifest = manifest;
            self
        }
    }

    /// Writes the packages to a temp directory and discovers them, returning
    /// the repo. The temp directory is returned so it outlives the repo.
    fn repo_on_disk(specs: &[PackageSpec]) -> (tempfile::TempDir, Repo) {
        let root = tempfile::tempdir().unwrap();
        for spec in specs {
            let dir = root.path().join("packages").join(spec.name);
            std::fs::create_dir_all(&dir).unwrap();
            for file in &spec.files {
                let path = dir.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "").unwrap();
            }
        }
        let mut repo = Repo::discover(root.path()).unwrap();
        for spec in specs {
            repo.packages.get_mut(&name(spec.name)).unwrap().manifest = spec.manifest.clone();
        }
        (root, repo)
    }

    fn context<'a>(repo: &'a Repo, home: &'a Path) -> PlanContext<'a> {
        PlanContext {
            repo,
            home,
            target_override: None,
        }
    }

    fn source(repo: &Repo, package: &str, file: &str) -> PathBuf {
        repo.root.join("packages").join(package).join(file)
    }

    fn home_path(relative: &str) -> PathBuf {
        Path::new(HOME).join(relative)
    }

    fn link_plan(repo: &Repo, roots: &[&str], filesystem: &FakeFileSystem) -> FilePlan {
        let graph = DependencyGraph::build(repo, &names(roots)).unwrap();
        plan_link(&context(repo, Path::new(HOME)), &graph, filesystem).unwrap()
    }

    fn actions_of<'a>(plan: &'a FilePlan, package: &str) -> &'a [Action] {
        &plan
            .packages
            .iter()
            .find(|entry| entry.package == name(package))
            .unwrap()
            .actions
    }

    #[test]
    fn linking_into_an_empty_target_links_every_file_in_sorted_order() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc", ".config/zsh/aliases"])]);

        let plan = link_plan(&repo, &["zsh"], &FakeFileSystem::default());

        assert_eq!(
            actions_of(&plan, "zsh"),
            [
                Action::Link {
                    source: source(&repo, "zsh", ".config/zsh/aliases"),
                    link: home_path(".config/zsh/aliases"),
                },
                Action::Link {
                    source: source(&repo, "zsh", ".zshrc"),
                    link: home_path(".zshrc"),
                },
            ]
        );
    }

    #[test]
    fn packages_are_planned_in_dependency_order() {
        let app = Manifest {
            depends: names(&["base"]),
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[
            package("app", &[".apprc"]).manifest(app),
            package("base", &[".baserc"]),
        ]);

        let plan = link_plan(&repo, &["app"], &FakeFileSystem::default());

        let order: Vec<&str> = plan.packages.iter().map(|p| p.package.as_str()).collect();
        assert_eq!(order, ["base", "app"]);
    }

    #[test]
    fn an_empty_package_has_no_actions() {
        let (_root, repo) = repo_on_disk(&[package("empty", &[])]);

        let plan = link_plan(&repo, &["empty"], &FakeFileSystem::default());

        assert!(actions_of(&plan, "empty").is_empty());
    }

    #[test]
    fn a_link_that_already_points_at_the_file_is_skipped() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().symlink(
            home_path(".zshrc").to_str().unwrap(),
            source(&repo, "zsh", ".zshrc").to_str().unwrap(),
        );

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert_eq!(
            actions_of(&plan, "zsh"),
            [Action::Skip {
                link: home_path(".zshrc"),
                reason: SkipReason::AlreadyLinked
            }]
        );
    }

    fn conflict_for(existing: FakeFileSystem) -> ConflictKind {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let plan = link_plan(&repo, &["zsh"], &existing);
        match &actions_of(&plan, "zsh")[0] {
            Action::Conflict { path, kind } => {
                assert_eq!(path, &home_path(".zshrc"));
                kind.clone()
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_real_file_in_the_way_is_a_conflict() {
        let filesystem = FakeFileSystem::default().file("/home/me/.zshrc");
        assert_eq!(conflict_for(filesystem), ConflictKind::RealFile);
    }

    #[test]
    fn a_real_directory_in_the_way_is_a_conflict() {
        let filesystem = FakeFileSystem::default().directory("/home/me/.zshrc");
        assert_eq!(conflict_for(filesystem), ConflictKind::RealDirectory);
    }

    #[test]
    fn a_symlink_elsewhere_in_the_way_is_a_foreign_symlink_conflict() {
        let filesystem = FakeFileSystem::default().symlink("/home/me/.zshrc", "/somewhere/else");
        assert_eq!(conflict_for(filesystem), ConflictKind::ForeignSymlink);
    }

    #[test]
    fn a_broken_symlink_in_the_way_is_a_conflict() {
        let filesystem = FakeFileSystem::default().broken_symlink("/home/me/.zshrc", "/gone");
        assert_eq!(conflict_for(filesystem), ConflictKind::BrokenSymlink);
    }

    #[test]
    fn a_symlink_into_another_package_names_that_package() {
        let (_root, repo) =
            repo_on_disk(&[package("zsh", &[".zshrc"]), package("bash", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.zshrc",
            source(&repo, "bash", ".zshrc").to_str().unwrap(),
        );

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert_eq!(
            actions_of(&plan, "zsh"),
            [Action::Conflict {
                path: home_path(".zshrc"),
                kind: ConflictKind::OtherPackage {
                    owner: name("bash")
                }
            }]
        );
    }

    #[test]
    fn a_symlink_into_the_same_package_but_a_different_file_is_foreign() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc", ".zshenv"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.zshrc",
            source(&repo, "zsh", ".zshenv").to_str().unwrap(),
        );

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert!(matches!(
            actions_of(&plan, "zsh")[1],
            Action::Conflict {
                kind: ConflictKind::ForeignSymlink,
                ..
            }
        ));
    }

    #[test]
    fn two_packages_providing_the_same_path_conflict_for_the_later_one() {
        let (_root, repo) = repo_on_disk(&[package("a", &[".rc"]), package("b", &[".rc"])]);

        let plan = link_plan(&repo, &["a", "b"], &FakeFileSystem::default());

        assert!(matches!(actions_of(&plan, "a")[0], Action::Link { .. }));
        assert_eq!(
            actions_of(&plan, "b"),
            [Action::Conflict {
                path: home_path(".rc"),
                kind: ConflictKind::ProvidedTwice { other: name("a") }
            }]
        );
    }

    #[test]
    fn a_parent_that_is_a_file_is_a_conflict() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/nvim/init.lua"])]);
        let filesystem = FakeFileSystem::default().file("/home/me/.config");

        let plan = link_plan(&repo, &["nvim"], &filesystem);

        assert_eq!(
            actions_of(&plan, "nvim"),
            [Action::Conflict {
                path: home_path(".config/nvim/init.lua"),
                kind: ConflictKind::ParentNotDirectory {
                    parent: home_path(".config")
                }
            }]
        );
    }

    #[test]
    fn the_outermost_blocked_parent_is_reported() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/nvim/init.lua"])]);
        let filesystem = FakeFileSystem::default()
            .file("/home/me/.config")
            .file("/home/me/.config/nvim");

        let plan = link_plan(&repo, &["nvim"], &filesystem);

        assert!(matches!(
            &actions_of(&plan, "nvim")[0],
            Action::Conflict {
                kind: ConflictKind::ParentNotDirectory { parent },
                ..
            } if parent == &home_path(".config")
        ));
    }

    #[test]
    fn a_parent_that_is_a_broken_symlink_is_a_conflict() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/nvim/init.lua"])]);
        let filesystem = FakeFileSystem::default().broken_symlink("/home/me/.config", "/gone");

        let plan = link_plan(&repo, &["nvim"], &filesystem);

        assert!(matches!(
            actions_of(&plan, "nvim")[0],
            Action::Conflict {
                kind: ConflictKind::ParentNotDirectory { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_parent_that_is_a_symlink_to_a_directory_is_allowed() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/nvim/init.lua"])]);
        let filesystem = FakeFileSystem::default().symlink("/home/me/.config", "/data/config");

        let plan = link_plan(&repo, &["nvim"], &filesystem);

        assert!(matches!(actions_of(&plan, "nvim")[0], Action::Link { .. }));
    }

    #[test]
    fn the_target_directory_itself_is_never_checked_as_a_parent() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().file(HOME);

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert!(matches!(actions_of(&plan, "zsh")[0], Action::Link { .. }));
    }

    fn package_symlink(
        repo: &Repo,
        package: &str,
        file: &str,
        destination: &str,
    ) -> FakeFileSystem {
        FakeFileSystem::default()
            .symlink(source(repo, package, file).to_str().unwrap(), destination)
    }

    #[test]
    fn a_package_symlink_with_an_absolute_destination_is_copied() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let filesystem = package_symlink(&repo, "zsh", "current", "/opt/zsh/5.9");

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert_eq!(
            actions_of(&plan, "zsh"),
            [Action::LinkSymlinkCopy {
                destination: PathBuf::from("/opt/zsh/5.9"),
                link: home_path("current")
            }]
        );
    }

    #[test]
    fn a_copied_symlink_already_in_place_is_skipped() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let filesystem = package_symlink(&repo, "zsh", "current", "/opt/zsh/5.9")
            .symlink("/home/me/current", "/opt/zsh/5.9");

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert_eq!(
            actions_of(&plan, "zsh"),
            [Action::Skip {
                link: home_path("current"),
                reason: SkipReason::AlreadyLinked
            }]
        );
    }

    #[test]
    fn a_copied_symlink_that_is_broken_but_already_in_place_is_skipped() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let filesystem = FakeFileSystem::default()
            .broken_symlink(
                source(&repo, "zsh", "current").to_str().unwrap(),
                "/opt/gone",
            )
            .broken_symlink("/home/me/current", "/opt/gone");

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert_eq!(
            actions_of(&plan, "zsh"),
            [Action::Skip {
                link: home_path("current"),
                reason: SkipReason::AlreadyLinked
            }]
        );
    }

    #[test]
    fn a_broken_link_to_a_regular_package_file_is_a_conflict_even_with_the_right_destination() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        // The repo file exists, so a link to it that does not resolve is wrong.
        let filesystem = FakeFileSystem::default().broken_symlink(
            "/home/me/.zshrc",
            source(&repo, "zsh", ".zshrc").to_str().unwrap(),
        );

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert_eq!(
            actions_of(&plan, "zsh"),
            [Action::Conflict {
                path: home_path(".zshrc"),
                kind: ConflictKind::BrokenSymlink
            }]
        );
    }

    #[test]
    fn a_broken_package_symlink_with_an_absolute_destination_is_still_copied() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let filesystem = FakeFileSystem::default().broken_symlink(
            source(&repo, "zsh", "current").to_str().unwrap(),
            "/opt/gone",
        );

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert!(matches!(
            actions_of(&plan, "zsh")[0],
            Action::LinkSymlinkCopy { .. }
        ));
    }

    #[test]
    fn a_copied_symlink_over_a_different_symlink_is_a_conflict() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let filesystem = package_symlink(&repo, "zsh", "current", "/opt/zsh/5.9")
            .symlink("/home/me/current", "/opt/zsh/5.8");

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert!(matches!(
            actions_of(&plan, "zsh")[0],
            Action::Conflict {
                kind: ConflictKind::ForeignSymlink,
                ..
            }
        ));
    }

    #[test]
    fn a_package_symlink_with_a_relative_destination_is_skipped_with_a_warning() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let filesystem = package_symlink(&repo, "zsh", "current", "../shared/zsh");

        let plan = link_plan(&repo, &["zsh"], &filesystem);

        assert_eq!(
            actions_of(&plan, "zsh"),
            [Action::Skip {
                link: home_path("current"),
                reason: SkipReason::RelativeSymlink
            }]
        );
        assert_eq!(
            plan.warnings,
            [PlanWarning::RelativeSymlinkSkipped {
                package: name("zsh"),
                path: PathBuf::from("current")
            }]
        );
    }

    #[test]
    fn an_absolute_manifest_target_is_used_for_link_paths() {
        let manifest = Manifest {
            target: Target::Absolute(PathBuf::from("/etc")),
            ..Manifest::default()
        };
        let (_root, repo) =
            repo_on_disk(&[package("sysctl", &["sysctl.d/99.conf"]).manifest(manifest)]);

        let plan = link_plan(&repo, &["sysctl"], &FakeFileSystem::default());

        assert!(matches!(
            &actions_of(&plan, "sysctl")[0],
            Action::Link { link, .. } if link == Path::new("/etc/sysctl.d/99.conf")
        ));
    }

    #[test]
    fn the_target_override_replaces_every_package_target() {
        let manifest = Manifest {
            target: Target::Absolute(PathBuf::from("/etc")),
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[
            package("sysctl", &["a.conf"]).manifest(manifest),
            package("zsh", &[".zshrc"]),
        ]);
        let graph = DependencyGraph::build(&repo, &names(&["sysctl", "zsh"])).unwrap();
        let context = PlanContext {
            repo: &repo,
            home: Path::new(HOME),
            target_override: Some(Path::new("/stage")),
        };

        let plan = plan_link(&context, &graph, &FakeFileSystem::default()).unwrap();

        assert!(matches!(
            &actions_of(&plan, "sysctl")[0],
            Action::Link { link, .. } if link == Path::new("/stage/a.conf")
        ));
        assert!(matches!(
            &actions_of(&plan, "zsh")[0],
            Action::Link { link, .. } if link == Path::new("/stage/.zshrc")
        ));
    }

    #[test]
    fn no_root_is_needed_for_a_writable_home() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);

        let plan = link_plan(&repo, &["zsh"], &FakeFileSystem::default());

        assert!(plan.needs_root.is_empty());
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn a_declared_needs_root_is_a_root_reason() {
        let manifest = Manifest {
            needs_root: true,
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"]).manifest(manifest)]);

        let plan = link_plan(&repo, &["zsh"], &FakeFileSystem::default());

        assert_eq!(
            plan.needs_root,
            [RootReason::PackageDeclared {
                package: name("zsh")
            }]
        );
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn an_unwritable_target_is_a_root_reason_with_a_warning_when_undeclared() {
        let manifest = Manifest {
            target: Target::Absolute(PathBuf::from("/etc")),
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[package("sysctl", &["a.conf"]).manifest(manifest)]);
        let filesystem = FakeFileSystem::default().read_only("/etc");

        let plan = link_plan(&repo, &["sysctl"], &filesystem);

        assert_eq!(
            plan.needs_root,
            [RootReason::TargetNotWritable {
                package: name("sysctl"),
                path: PathBuf::from("/etc")
            }]
        );
        assert_eq!(
            plan.warnings,
            [PlanWarning::NeedsRootUndeclared {
                package: name("sysctl")
            }]
        );
    }

    #[test]
    fn an_unwritable_target_that_is_declared_has_both_reasons_and_no_warning() {
        let manifest = Manifest {
            target: Target::Absolute(PathBuf::from("/etc")),
            needs_root: true,
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[package("sysctl", &["a.conf"]).manifest(manifest)]);
        let filesystem = FakeFileSystem::default().read_only("/etc");

        let plan = link_plan(&repo, &["sysctl"], &filesystem);

        assert_eq!(plan.needs_root.len(), 2);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn a_writable_staging_target_needs_no_root_even_for_an_etc_package() {
        let manifest = Manifest {
            target: Target::Absolute(PathBuf::from("/etc")),
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[package("sysctl", &["a.conf"]).manifest(manifest)]);
        let graph = DependencyGraph::build(&repo, &names(&["sysctl"])).unwrap();
        let context = PlanContext {
            repo: &repo,
            home: Path::new(HOME),
            target_override: Some(Path::new("/stage")),
        };
        let filesystem = FakeFileSystem::default().read_only("/etc");

        let plan = plan_link(&context, &graph, &filesystem).unwrap();

        assert!(plan.needs_root.is_empty());
    }

    fn unlink_plan(
        repo: &Repo,
        packages: &[&str],
        prune: bool,
        filesystem: &FakeFileSystem,
    ) -> Vec<PackageActions> {
        plan_unlink(
            &context(repo, Path::new(HOME)),
            &names(packages),
            prune,
            filesystem,
        )
        .unwrap()
    }

    #[test]
    fn unlink_removes_a_link_that_points_at_the_packages_file() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.zshrc",
            source(&repo, "zsh", ".zshrc").to_str().unwrap(),
        );

        let plans = unlink_plan(&repo, &["zsh"], false, &filesystem);

        assert_eq!(
            plans[0].actions,
            [Action::Unlink {
                link: home_path(".zshrc"),
                expected_destination: source(&repo, "zsh", ".zshrc"),
            }]
        );
    }

    fn unlink_reason(filesystem: FakeFileSystem) -> SkipReason {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        match &unlink_plan(&repo, &["zsh"], false, &filesystem)[0].actions[0] {
            Action::Skip { reason, .. } => reason.clone(),
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    #[test]
    fn unlink_skips_a_path_that_does_not_exist() {
        assert_eq!(
            unlink_reason(FakeFileSystem::default()),
            SkipReason::NotLinked
        );
    }

    #[test]
    fn unlink_skips_a_foreign_symlink() {
        let filesystem = FakeFileSystem::default().symlink("/home/me/.zshrc", "/somewhere/else");
        assert_eq!(unlink_reason(filesystem), SkipReason::NotOurSymlink);
    }

    #[test]
    fn unlink_skips_a_symlink_into_another_package() {
        let (_root, repo) =
            repo_on_disk(&[package("zsh", &[".zshrc"]), package("bash", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.zshrc",
            source(&repo, "bash", ".zshrc").to_str().unwrap(),
        );

        let plans = unlink_plan(&repo, &["zsh"], false, &filesystem);

        assert_eq!(
            plans[0].actions,
            [Action::Skip {
                link: home_path(".zshrc"),
                reason: SkipReason::NotOurSymlink
            }]
        );
    }

    #[test]
    fn unlink_never_removes_a_real_file_or_directory() {
        assert_eq!(
            unlink_reason(FakeFileSystem::default().file("/home/me/.zshrc")),
            SkipReason::NotASymlink
        );
        assert_eq!(
            unlink_reason(FakeFileSystem::default().directory("/home/me/.zshrc")),
            SkipReason::NotASymlink
        );
    }

    #[test]
    fn unlink_removes_a_copied_symlink_that_has_the_packages_destination() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let filesystem = package_symlink(&repo, "zsh", "current", "/opt/zsh/5.9")
            .symlink("/home/me/current", "/opt/zsh/5.9");

        let plans = unlink_plan(&repo, &["zsh"], false, &filesystem);

        assert_eq!(
            plans[0].actions,
            [Action::Unlink {
                link: home_path("current"),
                expected_destination: PathBuf::from("/opt/zsh/5.9"),
            }]
        );
    }

    #[test]
    fn unlink_only_touches_the_named_packages() {
        let (_root, repo) =
            repo_on_disk(&[package("zsh", &[".zshrc"]), package("nvim", &[".nvimrc"])]);

        let plans = unlink_plan(&repo, &["zsh"], false, &FakeFileSystem::default());

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].package, name("zsh"));
    }

    #[test]
    fn unlink_of_an_unknown_package_is_an_error() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);

        let error = plan_unlink(
            &context(&repo, Path::new(HOME)),
            &names(&["nope"]),
            false,
            &FakeFileSystem::default(),
        )
        .unwrap_err();

        assert!(matches!(error, PlanError::UnknownPackage(package) if package == name("nope")));
    }

    #[test]
    fn prune_lists_directories_on_the_path_of_removed_links_deepest_first() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/nvim/lua/a.lua"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.config/nvim/lua/a.lua",
            source(&repo, "nvim", ".config/nvim/lua/a.lua")
                .to_str()
                .unwrap(),
        );

        let plans = unlink_plan(&repo, &["nvim"], true, &filesystem);

        assert_eq!(
            plans[0].actions[1..],
            [
                Action::RemoveDirectoryIfEmpty {
                    path: home_path(".config/nvim/lua")
                },
                Action::RemoveDirectoryIfEmpty {
                    path: home_path(".config/nvim")
                },
                Action::RemoveDirectoryIfEmpty {
                    path: home_path(".config")
                },
            ]
        );
    }

    #[test]
    fn prune_lists_each_shared_directory_once() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/a", ".config/b"])]);
        let filesystem = FakeFileSystem::default()
            .symlink(
                "/home/me/.config/a",
                source(&repo, "nvim", ".config/a").to_str().unwrap(),
            )
            .symlink(
                "/home/me/.config/b",
                source(&repo, "nvim", ".config/b").to_str().unwrap(),
            );

        let plans = unlink_plan(&repo, &["nvim"], true, &filesystem);

        let removals: Vec<_> = plans[0]
            .actions
            .iter()
            .filter(|action| matches!(action, Action::RemoveDirectoryIfEmpty { .. }))
            .collect();
        assert_eq!(removals.len(), 1);
    }

    #[test]
    fn prune_never_lists_the_target_root() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.zshrc",
            source(&repo, "zsh", ".zshrc").to_str().unwrap(),
        );

        let plans = unlink_plan(&repo, &["zsh"], true, &filesystem);

        assert_eq!(plans[0].actions.len(), 1);
    }

    #[test]
    fn prune_ignores_links_that_are_not_removed() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/nvim/init.lua"])]);
        let filesystem = FakeFileSystem::default().file("/home/me/.config/nvim/init.lua");

        let plans = unlink_plan(&repo, &["nvim"], true, &filesystem);

        assert_eq!(plans[0].actions.len(), 1);
        assert!(matches!(plans[0].actions[0], Action::Skip { .. }));
    }

    #[test]
    fn without_prune_no_directories_are_listed() {
        let (_root, repo) = repo_on_disk(&[package("nvim", &[".config/nvim/init.lua"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.config/nvim/init.lua",
            source(&repo, "nvim", ".config/nvim/init.lua")
                .to_str()
                .unwrap(),
        );

        let plans = unlink_plan(&repo, &["nvim"], false, &filesystem);

        assert_eq!(plans[0].actions.len(), 1);
    }

    #[test]
    fn relink_removes_existing_links_then_plans_them_again() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().symlink(
            "/home/me/.zshrc",
            source(&repo, "zsh", ".zshrc").to_str().unwrap(),
        );

        let plan = plan_relink(
            &context(&repo, Path::new(HOME)),
            &names(&["zsh"]),
            false,
            &filesystem,
        )
        .unwrap();

        assert_eq!(
            plan.unlink[0].actions,
            [Action::Unlink {
                link: home_path(".zshrc"),
                expected_destination: source(&repo, "zsh", ".zshrc"),
            }]
        );
        assert_eq!(
            actions_of(&plan.link, "zsh"),
            [Action::Link {
                source: source(&repo, "zsh", ".zshrc"),
                link: home_path(".zshrc"),
            }]
        );
    }

    #[test]
    fn relink_still_reports_conflicts_with_real_files() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let filesystem = FakeFileSystem::default().file("/home/me/.zshrc");

        let plan = plan_relink(
            &context(&repo, Path::new(HOME)),
            &names(&["zsh"]),
            false,
            &filesystem,
        )
        .unwrap();

        assert!(matches!(
            actions_of(&plan.link, "zsh")[0],
            Action::Conflict {
                kind: ConflictKind::RealFile,
                ..
            }
        ));
    }

    #[test]
    fn relink_does_not_pull_in_dependencies() {
        let app = Manifest {
            depends: names(&["base"]),
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[
            package("app", &[".apprc"]).manifest(app),
            package("base", &[".baserc"]),
        ]);

        let plan = plan_relink(
            &context(&repo, Path::new(HOME)),
            &names(&["app"]),
            false,
            &FakeFileSystem::default(),
        )
        .unwrap();

        assert_eq!(plan.unlink.len(), 1);
        assert_eq!(plan.link.packages.len(), 1);
        assert_eq!(plan.link.packages[0].package, name("app"));
    }

    #[test]
    fn relink_of_an_unknown_package_is_an_error() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);

        let error = plan_relink(
            &context(&repo, Path::new(HOME)),
            &names(&["nope"]),
            false,
            &FakeFileSystem::default(),
        )
        .unwrap_err();

        assert!(matches!(error, PlanError::UnknownPackage(_)));
    }

    use crate::system::ManagerId;

    /// A package manager with a fixed set of installed packages.
    struct FakeManager {
        installed: Vec<&'static str>,
    }

    impl PackageManager for FakeManager {
        fn id(&self) -> ManagerId {
            ManagerId::Xbps
        }

        fn is_installed(&self, package: &str) -> Result<bool, SystemError> {
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

    fn xbps(packages: &[&str]) -> Manifest {
        Manifest {
            system_packages: Some(BTreeMap::from([(
                ManagerId::Xbps,
                packages.iter().map(|p| p.to_string()).collect(),
            )])),
            ..Manifest::default()
        }
    }

    fn full_plan(repo: &Repo, roots: &[&str], manager: Option<&dyn PackageManager>) -> Plan {
        let graph = DependencyGraph::build(repo, &names(roots)).unwrap();
        plan_full_link(
            &context(repo, Path::new(HOME)),
            &graph,
            &RepoState::default(),
            false,
            &FakeFileSystem::default(),
            manager,
        )
        .unwrap()
    }

    #[test]
    fn the_full_plan_combines_files_system_packages_and_hooks() {
        let dwm = Manifest {
            hook: Some(PathBuf::from("build.sh")),
            ..xbps(&["base-devel"])
        };
        let (_root, repo) = repo_on_disk(&[package("dwm", &[".dwmrc", "build.sh"]).manifest(dwm)]);
        let manager = FakeManager { installed: vec![] };

        let plan = full_plan(&repo, &["dwm"], Some(&manager));

        assert_eq!(
            plan.packages,
            [PackageActions {
                package: name("dwm"),
                actions: vec![Action::Link {
                    source: source(&repo, "dwm", ".dwmrc"),
                    link: home_path(".dwmrc"),
                }]
            }]
        );
        assert_eq!(plan.system.to_install, ["base-devel".to_string()]);
        assert_eq!(
            plan.hooks,
            [HookPlan {
                package: name("dwm"),
                reason: HookReason::NeverRun,
                script: source(&repo, "dwm", "build.sh"),
                package_dir: repo.root.join("packages/dwm"),
                target_dir: PathBuf::from(HOME),
                repo_root: repo.root.clone(),
            }]
        );
        assert_eq!(plan.needs_root, [RootReason::SystemInstall]);
    }

    #[test]
    fn hooks_are_listed_in_dependency_order_and_only_for_packages_that_have_one() {
        let hook = Some(PathBuf::from("setup.sh"));
        let app = Manifest {
            depends: names(&["base"]),
            hook: hook.clone(),
            ..Manifest::default()
        };
        let base = Manifest {
            hook,
            ..Manifest::default()
        };
        let (_root, repo) = repo_on_disk(&[
            package("app", &[]).manifest(app),
            package("base", &[]).manifest(base),
            package("plain", &[".plainrc"]),
        ]);

        let plan = full_plan(&repo, &["app", "plain"], None);

        let hooked: Vec<&str> = plan.hooks.iter().map(|h| h.package.as_str()).collect();
        assert_eq!(hooked, ["base", "app"]);
    }

    #[test]
    fn a_package_without_a_hook_adds_no_hook_entry() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);

        let plan = full_plan(&repo, &["zsh"], None);

        assert!(plan.hooks.is_empty());
    }

    #[test]
    fn system_install_is_a_root_reason_only_when_something_needs_installing() {
        let (_root, repo) =
            repo_on_disk(&[package("dwm", &[".dwmrc"]).manifest(xbps(&["base-devel"]))]);

        let missing = FakeManager { installed: vec![] };
        let present = FakeManager {
            installed: vec!["base-devel"],
        };

        assert_eq!(
            full_plan(&repo, &["dwm"], Some(&missing)).needs_root,
            [RootReason::SystemInstall]
        );
        assert!(full_plan(&repo, &["dwm"], Some(&present))
            .needs_root
            .is_empty());
        assert!(full_plan(&repo, &["dwm"], None).needs_root.is_empty());
    }

    #[test]
    fn file_root_reasons_come_before_the_system_install_reason() {
        let manifest = Manifest {
            needs_root: true,
            ..xbps(&["base-devel"])
        };
        let (_root, repo) = repo_on_disk(&[package("dwm", &[".dwmrc"]).manifest(manifest)]);
        let manager = FakeManager { installed: vec![] };

        let plan = full_plan(&repo, &["dwm"], Some(&manager));

        assert_eq!(
            plan.needs_root,
            [
                RootReason::PackageDeclared {
                    package: name("dwm")
                },
                RootReason::SystemInstall
            ]
        );
    }

    /// A full plan for just `zsh` against `filesystem`, with no package manager.
    fn plan_zsh(repo: &Repo, graph: &DependencyGraph, filesystem: &FakeFileSystem) -> Plan {
        plan_full_link(
            &context(repo, Path::new(HOME)),
            graph,
            &RepoState::default(),
            false,
            filesystem,
            None,
        )
        .unwrap()
    }

    #[test]
    fn file_warnings_are_carried_into_the_full_plan() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let graph = DependencyGraph::build(&repo, &names(&["zsh"])).unwrap();
        let filesystem = package_symlink(&repo, "zsh", "current", "../relative");

        let plan = plan_zsh(&repo, &graph, &filesystem);

        assert_eq!(plan.warnings.len(), 1);
    }

    #[test]
    fn system_notices_appear_in_the_full_plan_when_no_manager_is_available() {
        let (_root, repo) = repo_on_disk(&[package("dwm", &[".dwmrc"]).manifest(xbps(&["gcc"]))]);

        let plan = full_plan(&repo, &["dwm"], None);

        assert_eq!(plan.system.manager, None);
        assert_eq!(plan.system.notices.len(), 1);
    }

    fn hooked(depends: &[&str]) -> Manifest {
        Manifest {
            depends: names(depends),
            hook: Some(PathBuf::from("setup.sh")),
            ..Manifest::default()
        }
    }

    fn hook_plan(repo: &Repo, packages: &[&str]) -> Result<Vec<HookPlan>, PlanError> {
        plan_hooks(&context(repo, Path::new(HOME)), &names(packages))
    }

    fn hook_packages(hooks: &[HookPlan]) -> Vec<&str> {
        hooks.iter().map(|hook| hook.package.as_str()).collect()
    }

    #[test]
    fn a_plan_with_a_link_to_create_links_something() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);

        let plan = full_plan(&repo, &["zsh"], None);

        assert!(plan.links_anything());
    }

    #[test]
    fn a_plan_where_everything_is_already_linked_links_nothing() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);
        let graph = DependencyGraph::build(&repo, &names(&["zsh"])).unwrap();
        let filesystem = FakeFileSystem::default().symlink(
            home_path(".zshrc").to_str().unwrap(),
            source(&repo, "zsh", ".zshrc").to_str().unwrap(),
        );

        let plan = plan_zsh(&repo, &graph, &filesystem);

        assert!(!plan.links_anything());
    }

    #[test]
    fn a_plan_with_a_copied_symlink_to_create_links_something() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &["current"])]);
        let graph = DependencyGraph::build(&repo, &names(&["zsh"])).unwrap();
        let filesystem = package_symlink(&repo, "zsh", "current", "/opt/zsh");

        let plan = plan_zsh(&repo, &graph, &filesystem);

        assert!(plan.links_anything());
    }

    #[test]
    fn a_plan_with_no_packages_links_nothing() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[".zshrc"])]);

        assert!(!full_plan(&repo, &[], None).links_anything());
    }

    #[test]
    fn hooks_of_named_packages_are_planned_in_dependency_order() {
        let (_root, repo) = repo_on_disk(&[
            package("app", &[]).manifest(hooked(&["base"])),
            package("base", &[]).manifest(hooked(&[])),
        ]);

        let hooks = hook_plan(&repo, &["app", "base"]).unwrap();

        assert_eq!(hook_packages(&hooks), ["base", "app"]);
    }

    #[test]
    fn a_dependency_that_is_not_named_orders_but_does_not_run() {
        let (_root, repo) = repo_on_disk(&[
            package("app", &[]).manifest(hooked(&["base"])),
            package("base", &[]).manifest(hooked(&[])),
        ]);

        let hooks = hook_plan(&repo, &["app"]).unwrap();

        assert_eq!(hook_packages(&hooks), ["app"]);
    }

    #[test]
    fn a_named_package_without_a_hook_is_left_out() {
        let (_root, repo) = repo_on_disk(&[
            package("plain", &[".plainrc"]),
            package("dwm", &[]).manifest(hooked(&[])),
        ]);

        let hooks = hook_plan(&repo, &["plain", "dwm"]).unwrap();

        assert_eq!(hook_packages(&hooks), ["dwm"]);
    }

    /// A repo where `app` depends on `base`, both with a real `setup.sh` on
    /// disk so their fingerprints can be read.
    fn hooked_app_and_base() -> (tempfile::TempDir, Repo) {
        let (root, repo) = repo_on_disk(&[
            package("app", &[".apprc", "setup.sh"]).manifest(hooked(&["base"])),
            package("base", &[".baserc", "setup.sh"]).manifest(hooked(&[])),
        ]);
        (root, repo)
    }

    /// What was remembered after `package`'s hook last ran with `status`, using
    /// the script's real fingerprint unless `fingerprint_override` is given.
    fn remembered_hook(
        repo: &Repo,
        package: &str,
        status: HookStatus,
        fingerprint_override: Option<&str>,
    ) -> HookRecord {
        let script = source(repo, package, "setup.sh");
        HookRecord {
            package: name(package),
            status,
            exit_code: None,
            fingerprint: fingerprint_override
                .map(str::to_string)
                .unwrap_or_else(|| fingerprint(&script).unwrap()),
            ran_at: 0,
        }
    }

    fn remembering(records: Vec<HookRecord>) -> RepoState {
        RepoState {
            hooks: records,
            ..RepoState::default()
        }
    }

    fn plan_remembering(repo: &Repo, named: &[&str], remembered: &RepoState, force: bool) -> Plan {
        let graph = DependencyGraph::build(repo, &names(named)).unwrap();
        plan_full_link(
            &context(repo, Path::new(HOME)),
            &graph,
            remembered,
            force,
            &FakeFileSystem::default(),
            None,
        )
        .unwrap()
    }

    fn reasons(plan: &Plan) -> Vec<(&str, HookReason)> {
        plan.hooks
            .iter()
            .map(|hook| (hook.package.as_str(), hook.reason))
            .collect()
    }

    #[test]
    fn hooks_that_never_ran_are_planned_with_that_reason() {
        let (_root, repo) = hooked_app_and_base();

        let plan = plan_remembering(&repo, &["app"], &RepoState::default(), false);

        assert_eq!(
            reasons(&plan),
            [
                ("base", HookReason::NeverRun),
                ("app", HookReason::NeverRun)
            ]
        );
    }

    #[test]
    fn a_hook_that_succeeded_with_the_same_script_is_left_out() {
        let (_root, repo) = hooked_app_and_base();
        let remembered = remembering(vec![remembered_hook(
            &repo,
            "base",
            HookStatus::Succeeded,
            None,
        )]);

        let plan = plan_remembering(&repo, &["app"], &remembered, false);

        assert_eq!(reasons(&plan), [("app", HookReason::NeverRun)]);
    }

    #[test]
    fn a_named_package_whose_hook_is_up_to_date_is_left_out_too() {
        let (_root, repo) = hooked_app_and_base();
        let remembered = remembering(vec![
            remembered_hook(&repo, "base", HookStatus::Succeeded, None),
            remembered_hook(&repo, "app", HookStatus::Succeeded, None),
        ]);

        let plan = plan_remembering(&repo, &["app", "base"], &remembered, false);

        assert!(plan.hooks.is_empty());
    }

    #[test]
    fn a_hook_that_failed_last_time_is_planned_for_retry() {
        let (_root, repo) = hooked_app_and_base();
        let remembered = remembering(vec![
            remembered_hook(&repo, "base", HookStatus::Failed, None),
            remembered_hook(&repo, "app", HookStatus::Succeeded, None),
        ]);

        let plan = plan_remembering(&repo, &["app"], &remembered, false);

        assert_eq!(reasons(&plan), [("base", HookReason::PreviouslyFailed)]);
    }

    #[test]
    fn a_hook_whose_script_changed_is_planned_again() {
        let (_root, repo) = hooked_app_and_base();
        let remembered = remembering(vec![
            remembered_hook(&repo, "base", HookStatus::Succeeded, Some("stale")),
            remembered_hook(&repo, "app", HookStatus::Succeeded, None),
        ]);

        let plan = plan_remembering(&repo, &["app"], &remembered, false);

        assert_eq!(reasons(&plan), [("base", HookReason::ScriptChanged)]);
    }

    #[test]
    fn a_hook_only_dependency_that_succeeded_is_not_run_again() {
        // Without memory a hook-only package could never be told apart from one
        // that had not run, so it re-ran every time.
        let (_root, repo) = repo_on_disk(&[
            package("app", &[".apprc", "setup.sh"]).manifest(hooked(&["setup"])),
            package("setup", &["setup.sh"]).manifest(hooked(&[])),
        ]);
        let remembered = remembering(vec![remembered_hook(
            &repo,
            "setup",
            HookStatus::Succeeded,
            None,
        )]);

        let plan = plan_remembering(&repo, &["app"], &remembered, false);

        assert_eq!(reasons(&plan), [("app", HookReason::NeverRun)]);
    }

    #[test]
    fn forcing_plans_every_hook_even_when_up_to_date() {
        let (_root, repo) = hooked_app_and_base();
        let remembered = remembering(vec![
            remembered_hook(&repo, "base", HookStatus::Succeeded, None),
            remembered_hook(&repo, "app", HookStatus::Succeeded, None),
        ]);

        let plan = plan_remembering(&repo, &["app"], &remembered, true);

        assert_eq!(
            reasons(&plan),
            [("base", HookReason::Forced), ("app", HookReason::Forced)]
        );
    }

    #[test]
    fn a_hook_whose_script_is_missing_is_planned_so_the_problem_is_reported() {
        let (_root, repo) = repo_on_disk(&[package("dwm", &[".dwmrc"]).manifest(hooked(&[]))]);
        let remembered = remembering(vec![HookRecord {
            package: name("dwm"),
            status: HookStatus::Succeeded,
            exit_code: Some(0),
            fingerprint: "was-here-once".to_string(),
            ran_at: 0,
        }]);

        let plan = plan_remembering(&repo, &["dwm"], &remembered, false);

        assert_eq!(reasons(&plan), [("dwm", HookReason::ScriptChanged)]);
    }

    #[test]
    fn what_is_remembered_about_another_package_does_not_apply() {
        let (_root, repo) = hooked_app_and_base();
        let remembered = remembering(vec![remembered_hook(
            &repo,
            "app",
            HookStatus::Succeeded,
            None,
        )]);

        let plan = plan_remembering(&repo, &["app"], &remembered, false);

        assert_eq!(reasons(&plan), [("base", HookReason::NeverRun)]);
    }

    #[test]
    fn explicitly_named_hooks_are_planned_as_forced() {
        let (_root, repo) = hooked_app_and_base();

        let hooks = hook_plan(&repo, &["app"]).unwrap();

        assert_eq!(hooks[0].reason, HookReason::Forced);
    }

    #[test]
    fn hook_plans_carry_the_script_directories_and_target() {
        let (_root, repo) = repo_on_disk(&[package("dwm", &[]).manifest(hooked(&[]))]);

        let hooks = hook_plan(&repo, &["dwm"]).unwrap();

        assert_eq!(hooks[0].script, source(&repo, "dwm", "setup.sh"));
        assert_eq!(hooks[0].package_dir, repo.root.join("packages/dwm"));
        assert_eq!(hooks[0].target_dir, PathBuf::from(HOME));
    }

    #[test]
    fn hooks_of_an_unknown_package_is_an_error() {
        let (_root, repo) = repo_on_disk(&[package("zsh", &[])]);

        let error = hook_plan(&repo, &["nope"]).unwrap_err();

        assert!(matches!(
            error,
            PlanError::Graph(GraphError::MissingPackage { .. })
        ));
    }

    #[test]
    fn hooks_of_packages_that_depend_on_each_other_in_a_loop_is_an_error() {
        let (_root, repo) = repo_on_disk(&[
            package("a", &[]).manifest(hooked(&["b"])),
            package("b", &[]).manifest(hooked(&["a"])),
        ]);

        let error = hook_plan(&repo, &["a"]).unwrap_err();

        assert!(matches!(error, PlanError::Graph(GraphError::Cycle { .. })));
    }

    #[test]
    fn hooks_with_no_packages_named_is_empty() {
        let (_root, repo) = repo_on_disk(&[package("dwm", &[]).manifest(hooked(&[]))]);

        assert!(hook_plan(&repo, &[]).unwrap().is_empty());
    }

    #[test]
    fn a_failing_package_manager_query_fails_planning() {
        struct Failing;
        impl PackageManager for Failing {
            fn id(&self) -> ManagerId {
                ManagerId::Xbps
            }
            fn is_installed(&self, package: &str) -> Result<bool, SystemError> {
                Err(SystemError::Query {
                    package: package.to_string(),
                    reason: "boom".to_string(),
                })
            }
            fn install(
                &self,
                _packages: &[String],
                _on_line: crate::system::command::LineCallback,
            ) -> Result<(), SystemError> {
                Ok(())
            }
        }
        let (_root, repo) = repo_on_disk(&[package("dwm", &[".dwmrc"]).manifest(xbps(&["gcc"]))]);
        let graph = DependencyGraph::build(&repo, &names(&["dwm"])).unwrap();

        let error = plan_full_link(
            &context(&repo, Path::new(HOME)),
            &graph,
            &RepoState::default(),
            false,
            &FakeFileSystem::default(),
            Some(&Failing),
        )
        .unwrap_err();

        assert!(matches!(error, PlanError::System(_)));
    }
}
