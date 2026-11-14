//! Carrying out a whole `link` plan: files, then system packages, then hooks.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

use crate::adopt::{apply_adopt, moved_links, AdoptOutcome, AdoptPlan, AdoptResult};
use crate::apply::{apply_files, refuse_conflicts, ApplyError, PackageOutcome, RealFileWriter};
use crate::hook::{run_hooks, HookObserver, HookOutcome};
use crate::link::FileSystemState;
use crate::package::PackageName;
use crate::plan::{ConflictKind, PackageActions, Plan, RelinkPlan, RootReason};
use crate::system::command::CommandRunner;
use crate::system::privilege::Privilege;
use crate::system::privileged_writer::PrivilegedFileWriter;
use crate::system::PackageManager;

/// What the caller chose for this run.
#[derive(Debug, Clone, Copy)]
pub struct ExecuteOptions {
    /// Install missing system packages (false for `--no-install`).
    pub install: bool,
    /// Run hooks (false for `--no-hooks`).
    pub hooks: bool,
    /// How to get root, or `None` if the process is not root and `sudo` is not
    /// available.
    pub privilege: Option<Privilege>,
}

/// How the system package install turned out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InstallOutcome {
    /// Nothing needed installing.
    NotNeeded,
    /// Installing was turned off, so these packages were not installed.
    Skipped {
        /// What would have been installed.
        packages: Vec<String>,
    },
    /// No package manager is available, so nothing was installed.
    NoManager,
    /// These packages were installed in one call.
    Installed {
        /// What was installed.
        packages: Vec<String>,
    },
    /// The install failed. Links already made stay, and hooks did not run.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

/// What a whole run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Per-package file outcomes, in link order.
    pub files: Vec<PackageOutcome>,
    /// What happened to the system package install.
    pub install: InstallOutcome,
    /// Hook outcomes in dependency order. Empty if hooks were turned off or an
    /// install failure stopped them.
    pub hooks: Vec<HookOutcome>,
}

/// How a finished run should be judged, which frontends turn into an exit
/// status or a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Verdict {
    /// Everything went as asked.
    Success,
    /// Something failed while running: a file action, the install, or a hook.
    Failed,
    /// The request did nothing because every part of it was refused.
    Refused,
}

/// Whether any file action in `packages` failed.
pub fn any_file_action_failed(packages: &[PackageOutcome]) -> bool {
    packages
        .iter()
        .flat_map(|package| &package.actions)
        .any(|outcome| matches!(outcome.result, crate::apply::ActionResult::Failed { .. }))
}

/// The verdict for a run that only changed files, such as unlink or relink.
pub fn file_verdict(packages: &[PackageOutcome]) -> Verdict {
    if any_file_action_failed(packages) {
        Verdict::Failed
    } else {
        Verdict::Success
    }
}

impl Report {
    /// Whether the run succeeded or something in it failed.
    pub fn verdict(&self) -> Verdict {
        let hook_failed = self
            .hooks
            .iter()
            .any(|outcome| outcome.result != crate::hook::HookResult::Succeeded);
        let install_failed = matches!(self.install, InstallOutcome::Failed { .. });
        if any_file_action_failed(&self.files) || install_failed || hook_failed {
            Verdict::Failed
        } else {
            Verdict::Success
        }
    }
}

/// What adopting did: which files moved, and the links made for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdoptReport {
    /// What happened to each requested file.
    pub moved: Vec<AdoptOutcome>,
    /// The links created back from where each moved file came.
    pub linked: Vec<PackageOutcome>,
}

impl AdoptReport {
    /// Failed if any move or link failed. Refused if nothing was moved (so a
    /// script can tell a request that did nothing from one that worked), and
    /// successful if at least one file moved, even if others were refused.
    pub fn verdict(&self) -> Verdict {
        let move_failed = self
            .moved
            .iter()
            .any(|outcome| matches!(outcome.result, AdoptResult::Failed { .. }));
        if move_failed || any_file_action_failed(&self.linked) {
            Verdict::Failed
        } else if self
            .moved
            .iter()
            .any(|outcome| outcome.result == AdoptResult::Moved)
        {
            Verdict::Success
        } else {
            Verdict::Refused
        }
    }
}

/// Why a plan was refused before anything was changed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RefusalError {
    /// The plan has conflicts, which must be resolved first.
    #[error("{} conflict(s) must be resolved first", .0.len())]
    Conflicts(Vec<(PathBuf, ConflictKind)>),
    /// Root is needed but the process is not root and `sudo` is not available.
    #[error("root is required but neither root nor sudo is available ({} reason(s))", .0.len())]
    RootUnavailable(Vec<RootReason>),
    /// Adopting would have to move a file out of a directory that needs root,
    /// which puddlejump does not do: it never half-moves a file.
    #[error("package {0} needs root, and adopting files into it is not supported")]
    AdoptNeedsRoot(PackageName),
}

impl From<ApplyError> for RefusalError {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Conflicts(conflicts) => RefusalError::Conflicts(conflicts),
        }
    }
}

/// Moves the adoptable files of `plan` into the package and links them back.
///
/// Moving a file out of a directory that needs root would need a privileged
/// move, which could leave a file half-moved if it failed, so this refuses
/// such a package up front instead (`reasons` says whether it needs root).
/// Otherwise each file is moved with its own re-check, and the links for the
/// files that moved are created through the same writer choice as `link`.
///
/// Errors with [`RefusalError::AdoptNeedsRoot`] before changing anything if
/// the package needs root.
pub fn execute_adopt(
    plan: &AdoptPlan,
    reasons: &[RootReason],
    state: &dyn FileSystemState,
    runner: &dyn CommandRunner,
    privilege: Option<Privilege>,
) -> Result<AdoptReport, RefusalError> {
    if !privileged_packages(reasons).is_empty() {
        return Err(RefusalError::AdoptNeedsRoot(plan.package.clone()));
    }
    let moved = apply_adopt(plan, state);
    let links = [PackageActions {
        package: plan.package.clone(),
        actions: moved_links(&moved),
    }];
    let linked = apply_packages(&links, reasons, privilege, state, runner)?;
    Ok(AdoptReport { moved, linked })
}

/// The packages whose files must be written as root.
fn privileged_packages(reasons: &[RootReason]) -> BTreeSet<&PackageName> {
    reasons
        .iter()
        .filter_map(|reason| match reason {
            RootReason::PackageDeclared { package }
            | RootReason::TargetNotWritable { package, .. } => Some(package),
            RootReason::SystemInstall => None,
        })
        .collect()
}

/// The root reasons that still apply once the options are taken into account:
/// turning installs off removes the system install reason.
fn effective_root_reasons(plan: &Plan, options: &ExecuteOptions) -> Vec<RootReason> {
    plan.needs_root
        .iter()
        .filter(|reason| options.install || **reason != RootReason::SystemInstall)
        .cloned()
        .collect()
}

/// Carries out `plan`: links files, installs system packages, then runs hooks.
///
/// Before changing anything it refuses the plan if it has conflicts, or if
/// root is needed and `options.privilege` is `None`, reporting every reason.
/// Packages that need root have their files written through `runner` with the
/// chosen privilege; the rest are written directly as the current user.
///
/// A failed file action is recorded and the run continues. A failed install
/// stops the run before hooks; links already made stay in place. A failed hook
/// does not stop later hooks.
///
/// The caller decides how to get root once and should build both `manager`
/// and `options.privilege` from that one decision; this function does not
/// check that they agree.
///
/// Errors only with a [`RefusalError`], and only before anything is changed.
pub fn execute_link(
    plan: &Plan,
    options: &ExecuteOptions,
    state: &dyn FileSystemState,
    runner: &dyn CommandRunner,
    manager: Option<&dyn PackageManager>,
    observer: &mut dyn HookObserver,
) -> Result<Report, RefusalError> {
    let reasons = effective_root_reasons(plan, options);
    refuse_if_root_unavailable(&reasons, options.privilege)?;
    let files = apply_packages(&plan.packages, &reasons, options.privilege, state, runner)?;
    let install = install_packages(plan, options, manager);
    let install_failed = matches!(install, InstallOutcome::Failed { .. });
    let hooks = if options.hooks && !install_failed {
        run_hooks(&plan.hooks, observer)
    } else {
        Vec::new()
    };
    Ok(Report {
        files,
        install,
        hooks,
    })
}

/// Refuses if root is needed for any of `reasons` but there is no way to get
/// it.
fn refuse_if_root_unavailable(
    reasons: &[RootReason],
    privilege: Option<Privilege>,
) -> Result<(), RefusalError> {
    if reasons.is_empty() || privilege.is_some() {
        Ok(())
    } else {
        Err(RefusalError::RootUnavailable(reasons.to_vec()))
    }
}

/// Applies each package's file actions with the writer it needs, keeping the
/// packages in the order given. A package is written through `privilege` if
/// `reasons` say it needs root, and directly as the current user otherwise.
///
/// Refuses all of `packages`, changing nothing, if any action is a conflict.
fn apply_packages(
    packages: &[PackageActions],
    reasons: &[RootReason],
    privilege: Option<Privilege>,
    state: &dyn FileSystemState,
    runner: &dyn CommandRunner,
) -> Result<Vec<PackageOutcome>, RefusalError> {
    // Conflicts anywhere refuse the whole plan, so check before applying any
    // package rather than after the first few have been linked.
    refuse_conflicts(packages)?;
    let privileged = privileged_packages(reasons);
    let mut outcomes = Vec::new();
    for package in packages {
        let single = std::slice::from_ref(package);
        let applied = match (privileged.contains(&package.package), privilege) {
            (true, Some(privilege)) => {
                let mut writer = PrivilegedFileWriter::new(runner, privilege);
                apply_files(single, state, &mut writer)?
            }
            _ => apply_files(single, state, &mut RealFileWriter)?,
        };
        outcomes.extend(applied);
    }
    Ok(outcomes)
}

/// Removes the links of the packages in `plan`.
///
/// A package that needs root (per `reasons`) is changed through `privilege`;
/// the rest directly as the current user. Each action re-checks its path, and
/// a failed action is recorded while the rest still run.
///
/// Errors with [`RefusalError::RootUnavailable`] before changing anything if
/// root is needed and `privilege` is `None`.
pub fn execute_unlink(
    plan: &[PackageActions],
    reasons: &[RootReason],
    privilege: Option<Privilege>,
    state: &dyn FileSystemState,
    runner: &dyn CommandRunner,
) -> Result<Vec<PackageOutcome>, RefusalError> {
    refuse_if_root_unavailable(reasons, privilege)?;
    apply_packages(plan, reasons, privilege, state, runner)
}

/// Removes the links of the packages in `plan`, then creates them again.
///
/// Refuses the whole relink, changing nothing, if the link half has a
/// conflict or root is needed and unavailable: checking both halves first
/// means a conflict cannot be found only after the links were removed.
///
/// Errors with a [`RefusalError`] before changing anything.
pub fn execute_relink(
    plan: &RelinkPlan,
    reasons: &[RootReason],
    privilege: Option<Privilege>,
    state: &dyn FileSystemState,
    runner: &dyn CommandRunner,
) -> Result<Vec<PackageOutcome>, RefusalError> {
    refuse_if_root_unavailable(reasons, privilege)?;
    refuse_conflicts(&plan.link.packages)?;
    let mut outcomes = apply_packages(&plan.unlink, reasons, privilege, state, runner)?;
    outcomes.extend(apply_packages(
        &plan.link.packages,
        reasons,
        privilege,
        state,
        runner,
    )?);
    Ok(outcomes)
}

fn install_packages(
    plan: &Plan,
    options: &ExecuteOptions,
    manager: Option<&dyn PackageManager>,
) -> InstallOutcome {
    let packages = &plan.system.to_install;
    if packages.is_empty() {
        return InstallOutcome::NotNeeded;
    }
    if !options.install {
        return InstallOutcome::Skipped {
            packages: packages.clone(),
        };
    }
    let Some(manager) = manager else {
        return InstallOutcome::NoManager;
    };
    match manager.install(packages) {
        Ok(()) => InstallOutcome::Installed {
            packages: packages.clone(),
        },
        Err(error) => InstallOutcome::Failed {
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::{ActionResult, PackageOutcome};
    use crate::hook::{HookResult, SilentObserver};
    use crate::link::RealFileSystem;
    use crate::plan::{Action, HookPlan, PackageActions};
    use crate::system::command::fake::FakeCommandRunner;
    use crate::system::{ManagerId, SystemError, SystemPlan};
    use crate::test_scripts::write_script;
    use std::cell::RefCell;
    use std::fs;
    use std::path::Path;

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn strings(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    /// A package manager that records installs and can be told to fail.
    struct FakeManager {
        installs: RefCell<Vec<Vec<String>>>,
        fail_with: Option<&'static str>,
    }

    fn manager() -> FakeManager {
        FakeManager {
            installs: RefCell::new(Vec::new()),
            fail_with: None,
        }
    }

    impl PackageManager for FakeManager {
        fn id(&self) -> ManagerId {
            ManagerId::Xbps
        }

        fn is_installed(&self, _package: &str) -> Result<bool, SystemError> {
            Ok(false)
        }

        fn install(&self, packages: &[String]) -> Result<(), SystemError> {
            self.installs.borrow_mut().push(packages.to_vec());
            match self.fail_with {
                Some(reason) => Err(SystemError::Install {
                    packages: packages.to_vec(),
                    reason: reason.to_string(),
                }),
                None => Ok(()),
            }
        }
    }

    fn system_plan(to_install: &[&str]) -> SystemPlan {
        SystemPlan {
            manager: Some(ManagerId::Xbps),
            to_install: strings(to_install),
            already_installed: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn plan(
        packages: Vec<PackageActions>,
        system: SystemPlan,
        hooks: Vec<HookPlan>,
        needs_root: Vec<RootReason>,
    ) -> Plan {
        Plan {
            packages,
            system,
            hooks,
            needs_root,
            warnings: Vec::new(),
        }
    }

    fn link_action(source: &Path, link: &Path) -> Action {
        Action::Link {
            source: source.to_path_buf(),
            link: link.to_path_buf(),
        }
    }

    fn package(package: &str, actions: Vec<Action>) -> PackageActions {
        PackageActions {
            package: name(package),
            actions,
        }
    }

    fn options(privilege: Option<Privilege>) -> ExecuteOptions {
        ExecuteOptions {
            install: true,
            hooks: true,
            privilege,
        }
    }

    fn run(
        plan: &Plan,
        options: &ExecuteOptions,
        runner: &FakeCommandRunner,
        manager: Option<&FakeManager>,
    ) -> Result<Report, RefusalError> {
        execute_link(
            plan,
            options,
            &RealFileSystem,
            runner,
            manager.map(|manager| manager as &dyn PackageManager),
            &mut SilentObserver,
        )
    }

    /// A hook that appends its package name to `log` when it runs.
    fn logging_hook(root: &Path, package: &str, log: &Path, exit: i32) -> HookPlan {
        let package_dir = root.join("packages").join(package);
        fs::create_dir_all(&package_dir).unwrap();
        let body = format!("echo {package} >> {}\nexit {exit}", log.display());
        let script = write_script(&package_dir, "hook.sh", &body, 0o755);
        HookPlan {
            package: name(package),
            script,
            package_dir,
            target_dir: root.join("home"),
            repo_root: root.to_path_buf(),
        }
    }

    fn files_outcomes(report: &Report) -> Vec<&ActionResult> {
        report
            .files
            .iter()
            .flat_map(|package: &PackageOutcome| &package.actions)
            .map(|outcome| &outcome.result)
            .collect()
    }

    #[test]
    fn a_plain_plan_links_files_and_reports_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        let plan = plan(
            vec![package("zsh", vec![link_action(&source, &link)])],
            system_plan(&[]),
            vec![],
            vec![],
        );

        let report = run(&plan, &options(None), &FakeCommandRunner::new(), None).unwrap();

        assert_eq!(files_outcomes(&report), [&ActionResult::Done]);
        assert_eq!(report.install, InstallOutcome::NotNeeded);
        assert!(report.hooks.is_empty());
        assert_eq!(fs::read_link(&link).unwrap(), source);
    }

    #[test]
    fn a_plan_with_a_conflict_is_refused_and_nothing_runs() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let fine = root.path().join("fine");
        let log = root.path().join("log");
        fs::write(&source, "").unwrap();
        let clash = root.path().join("clash");
        let manager = manager();
        let plan = plan(
            vec![
                package("a", vec![link_action(&source, &fine)]),
                package(
                    "b",
                    vec![Action::Conflict {
                        path: clash.clone(),
                        kind: ConflictKind::RealFile,
                    }],
                ),
            ],
            system_plan(&["git"]),
            vec![logging_hook(root.path(), "a", &log, 0)],
            vec![],
        );

        let error = run(
            &plan,
            &options(None),
            &FakeCommandRunner::new(),
            Some(&manager),
        )
        .unwrap_err();

        assert_eq!(
            error,
            RefusalError::Conflicts(vec![(clash, ConflictKind::RealFile)])
        );
        assert!(fs::symlink_metadata(&fine).is_err(), "no file was linked");
        assert!(
            manager.installs.borrow().is_empty(),
            "nothing was installed"
        );
        assert!(!log.exists(), "no hook ran");
    }

    #[test]
    fn root_needed_without_a_way_to_get_it_is_refused_with_every_reason() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        let reasons = vec![
            RootReason::PackageDeclared {
                package: name("sysctl"),
            },
            RootReason::SystemInstall,
        ];
        let plan = plan(
            vec![package("home", vec![link_action(&source, &link)])],
            system_plan(&["git"]),
            vec![],
            reasons.clone(),
        );

        let error = run(
            &plan,
            &options(None),
            &FakeCommandRunner::new(),
            Some(&manager()),
        )
        .unwrap_err();

        assert_eq!(error, RefusalError::RootUnavailable(reasons));
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "even the package that needs no root was not linked"
        );
    }

    #[test]
    fn turning_installs_off_drops_the_system_install_root_reason() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        let plan = plan(
            vec![package("zsh", vec![link_action(&source, &link)])],
            system_plan(&["git"]),
            vec![],
            vec![RootReason::SystemInstall],
        );
        let no_install = ExecuteOptions {
            install: false,
            ..options(None)
        };

        let report = run(
            &plan,
            &no_install,
            &FakeCommandRunner::new(),
            Some(&manager()),
        )
        .unwrap();

        assert_eq!(
            report.install,
            InstallOutcome::Skipped {
                packages: strings(&["git"])
            }
        );
        assert!(link.is_symlink());
    }

    #[test]
    fn a_package_that_needs_root_is_written_through_sudo() {
        let runner = FakeCommandRunner::new();
        let plan = plan(
            vec![package(
                "sysctl",
                vec![link_action(
                    Path::new("/repo/a.conf"),
                    Path::new("/etc/a.conf"),
                )],
            )],
            system_plan(&[]),
            vec![],
            vec![RootReason::PackageDeclared {
                package: name("sysctl"),
            }],
        );

        // /etc/a.conf does not exist, so the re-check passes and the fake runs.
        let report = run(&plan, &options(Some(Privilege::UseSudo)), &runner, None).unwrap();

        assert_eq!(files_outcomes(&report), [&ActionResult::Done]);
        assert_eq!(
            runner.lines(),
            [
                "sudo mkdir -p -- /etc",
                "sudo ln -s -- /repo/a.conf /etc/a.conf"
            ]
        );
    }

    #[test]
    fn a_package_that_needs_no_root_is_written_directly_not_through_sudo() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        let runner = FakeCommandRunner::new();
        let plan = plan(
            vec![package("zsh", vec![link_action(&source, &link)])],
            system_plan(&[]),
            vec![],
            vec![RootReason::PackageDeclared {
                package: name("someone-else"),
            }],
        );

        run(&plan, &options(Some(Privilege::UseSudo)), &runner, None).unwrap();

        assert!(runner.lines().is_empty());
        assert!(link.is_symlink());
    }

    #[test]
    fn only_the_package_that_needs_root_uses_sudo_in_a_mixed_plan() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let home_link = root.path().join("home-link");
        fs::write(&source, "").unwrap();
        let runner = FakeCommandRunner::new();
        let plan = plan(
            vec![
                package("zsh", vec![link_action(&source, &home_link)]),
                package(
                    "sysctl",
                    vec![link_action(Path::new("/repo/a"), Path::new("/etc/a"))],
                ),
            ],
            system_plan(&[]),
            vec![],
            vec![RootReason::TargetNotWritable {
                package: name("sysctl"),
                path: "/etc".into(),
            }],
        );

        run(&plan, &options(Some(Privilege::UseSudo)), &runner, None).unwrap();

        assert!(home_link.is_symlink());
        assert_eq!(runner.lines().len(), 2);
        assert!(runner.lines().iter().all(|line| line.contains("/etc")));
    }

    #[test]
    fn a_privileged_package_run_as_root_runs_commands_without_sudo() {
        let runner = FakeCommandRunner::new();
        let plan = plan(
            vec![package(
                "sysctl",
                vec![link_action(Path::new("/repo/a"), Path::new("/etc/a"))],
            )],
            system_plan(&[]),
            vec![],
            vec![RootReason::PackageDeclared {
                package: name("sysctl"),
            }],
        );

        run(&plan, &options(Some(Privilege::AlreadyRoot)), &runner, None).unwrap();

        assert_eq!(
            runner.lines(),
            ["mkdir -p -- /etc", "ln -s -- /repo/a /etc/a"]
        );
    }

    #[test]
    fn missing_packages_are_installed_in_one_call_after_files_are_linked() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        let manager = manager();
        let plan = plan(
            vec![package("dwm", vec![link_action(&source, &link)])],
            system_plan(&["base-devel", "fd"]),
            vec![],
            vec![RootReason::SystemInstall],
        );

        let report = run(
            &plan,
            &options(Some(Privilege::UseSudo)),
            &FakeCommandRunner::new(),
            Some(&manager),
        )
        .unwrap();

        assert_eq!(
            report.install,
            InstallOutcome::Installed {
                packages: strings(&["base-devel", "fd"])
            }
        );
        assert_eq!(*manager.installs.borrow(), [strings(&["base-devel", "fd"])]);
        assert!(link.is_symlink());
    }

    #[test]
    fn nothing_to_install_never_calls_the_manager() {
        let manager = manager();
        let plan = plan(vec![], system_plan(&[]), vec![], vec![]);

        let report = run(
            &plan,
            &options(None),
            &FakeCommandRunner::new(),
            Some(&manager),
        )
        .unwrap();

        assert_eq!(report.install, InstallOutcome::NotNeeded);
        assert!(manager.installs.borrow().is_empty());
    }

    #[test]
    fn no_manager_reports_that_the_install_was_not_possible() {
        let plan = plan(vec![], system_plan(&["git"]), vec![], vec![]);

        let report = run(&plan, &options(None), &FakeCommandRunner::new(), None).unwrap();

        assert_eq!(report.install, InstallOutcome::NoManager);
    }

    #[test]
    fn a_failed_install_keeps_the_links_and_skips_every_hook() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        let log = root.path().join("log");
        fs::write(&source, "").unwrap();
        let manager = FakeManager {
            fail_with: Some("no space left"),
            ..manager()
        };
        let plan = plan(
            vec![package("dwm", vec![link_action(&source, &link)])],
            system_plan(&["gcc"]),
            vec![logging_hook(root.path(), "dwm", &log, 0)],
            vec![],
        );

        let report = run(
            &plan,
            &options(None),
            &FakeCommandRunner::new(),
            Some(&manager),
        )
        .unwrap();

        assert!(matches!(
            report.install,
            InstallOutcome::Failed { ref reason } if reason.contains("no space left")
        ));
        assert!(link.is_symlink(), "links made before the failure stay");
        assert!(report.hooks.is_empty());
        assert!(!log.exists(), "no hook ran");
    }

    #[test]
    fn hooks_run_after_files_and_install_in_the_planned_order() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("log");
        let plan = plan(
            vec![],
            system_plan(&[]),
            vec![
                logging_hook(root.path(), "base", &log, 0),
                logging_hook(root.path(), "app", &log, 0),
            ],
            vec![],
        );

        let report = run(&plan, &options(None), &FakeCommandRunner::new(), None).unwrap();

        assert_eq!(fs::read_to_string(&log).unwrap(), "base\napp\n");
        let order: Vec<&str> = report.hooks.iter().map(|h| h.package.as_str()).collect();
        assert_eq!(order, ["base", "app"]);
    }

    #[test]
    fn a_failing_hook_is_reported_and_later_hooks_still_run() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("log");
        let plan = plan(
            vec![],
            system_plan(&[]),
            vec![
                logging_hook(root.path(), "first", &log, 3),
                logging_hook(root.path(), "second", &log, 0),
            ],
            vec![],
        );

        let report = run(&plan, &options(None), &FakeCommandRunner::new(), None).unwrap();

        assert_eq!(
            report.hooks[0].result,
            HookResult::Failed { exit_code: Some(3) }
        );
        assert_eq!(report.hooks[1].result, HookResult::Succeeded);
        assert_eq!(fs::read_to_string(&log).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn turning_hooks_off_skips_them() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("log");
        let plan = plan(
            vec![],
            system_plan(&[]),
            vec![logging_hook(root.path(), "dwm", &log, 0)],
            vec![],
        );
        let no_hooks = ExecuteOptions {
            hooks: false,
            ..options(None)
        };

        let report = run(&plan, &no_hooks, &FakeCommandRunner::new(), None).unwrap();

        assert!(report.hooks.is_empty());
        assert!(!log.exists());
    }

    #[test]
    fn a_failed_file_action_does_not_stop_the_install_or_the_hooks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let blocked = root.path().join("blocked");
        let log = root.path().join("log");
        fs::write(&source, "").unwrap();
        fs::write(&blocked, "a real file that appeared after planning").unwrap();
        let manager = manager();
        let plan = plan(
            vec![package("dwm", vec![link_action(&source, &blocked)])],
            system_plan(&["gcc"]),
            vec![logging_hook(root.path(), "dwm", &log, 0)],
            vec![],
        );

        let report = run(
            &plan,
            &options(None),
            &FakeCommandRunner::new(),
            Some(&manager),
        )
        .unwrap();

        assert!(matches!(
            files_outcomes(&report)[0],
            ActionResult::Failed { .. }
        ));
        assert_eq!(manager.installs.borrow().len(), 1);
        assert!(log.exists());
        assert_eq!(
            fs::read_to_string(&blocked).unwrap(),
            "a real file that appeared after planning"
        );
    }

    #[test]
    fn refusal_errors_describe_themselves() {
        let conflicts = RefusalError::Conflicts(vec![("/x".into(), ConflictKind::RealFile)]);
        let root = RefusalError::RootUnavailable(vec![RootReason::SystemInstall]);

        assert_eq!(
            conflicts.to_string(),
            "1 conflict(s) must be resolved first"
        );
        assert_eq!(
            root.to_string(),
            "root is required but neither root nor sudo is available (1 reason(s))"
        );
    }

    fn root_for(package: &str) -> Vec<RootReason> {
        vec![RootReason::PackageDeclared {
            package: name(package),
        }]
    }

    fn unlink_action(link: &Path, expected: &Path) -> Action {
        Action::Unlink {
            link: link.to_path_buf(),
            expected_destination: expected.to_path_buf(),
        }
    }

    #[test]
    fn unlinking_a_package_that_needs_root_goes_through_sudo() {
        let runner = FakeCommandRunner::new();
        let plan = vec![package(
            "sysctl",
            vec![unlink_action(
                Path::new("/etc/a.conf"),
                Path::new("/repo/a.conf"),
            )],
        )];

        let outcomes = execute_unlink(
            &plan,
            &root_for("sysctl"),
            Some(Privilege::UseSudo),
            &FakeState::linked("/etc/a.conf", "/repo/a.conf"),
            &runner,
        )
        .unwrap();

        assert_eq!(runner.lines(), ["sudo rm -- /etc/a.conf"]);
        assert_eq!(files_outcomes_of(&outcomes), [&ActionResult::Done]);
    }

    #[test]
    fn unlinking_a_package_that_needs_no_root_does_not_use_the_runner() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let runner = FakeCommandRunner::new();
        let plan = vec![package("zsh", vec![unlink_action(&link, &source)])];

        execute_unlink(
            &plan,
            &[],
            Some(Privilege::UseSudo),
            &RealFileSystem,
            &runner,
        )
        .unwrap();

        assert!(runner.lines().is_empty());
        assert!(fs::symlink_metadata(&link).is_err());
    }

    #[test]
    fn unlinking_that_needs_root_without_a_way_to_get_it_is_refused_and_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let plan = vec![package("zsh", vec![unlink_action(&link, &source)])];

        let error = execute_unlink(
            &plan,
            &root_for("zsh"),
            None,
            &RealFileSystem,
            &FakeCommandRunner::new(),
        )
        .unwrap_err();

        assert_eq!(error, RefusalError::RootUnavailable(root_for("zsh")));
        assert!(link.is_symlink());
    }

    /// A relink plan for one package: remove a link, then link it again.
    fn relink_plan(source: &Path, link: &Path, link_half: Vec<Action>) -> RelinkPlan {
        RelinkPlan {
            unlink: vec![package("zsh", vec![unlink_action(link, source)])],
            link: crate::plan::FilePlan {
                packages: vec![package("zsh", link_half)],
                needs_root: Vec::new(),
                warnings: Vec::new(),
            },
        }
    }

    #[test]
    fn relinking_removes_the_old_link_then_creates_it_again() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let plan = relink_plan(&source, &link, vec![link_action(&source, &link)]);

        let outcomes =
            execute_relink(&plan, &[], None, &RealFileSystem, &FakeCommandRunner::new()).unwrap();

        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].actions[0].result, ActionResult::Done);
        // The link half was planned as if the removal had happened, but the
        // real filesystem still had the link when it was checked, so the file
        // action sequence matters: unlink ran first and the link is recreated.
        assert_eq!(fs::read_link(&link).unwrap(), source);
    }

    #[test]
    fn a_conflict_in_the_link_half_refuses_before_anything_is_unlinked() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let clash = root.path().join("clash");
        let plan = relink_plan(
            &source,
            &link,
            vec![Action::Conflict {
                path: clash.clone(),
                kind: ConflictKind::RealFile,
            }],
        );

        let error = execute_relink(&plan, &[], None, &RealFileSystem, &FakeCommandRunner::new())
            .unwrap_err();

        assert_eq!(
            error,
            RefusalError::Conflicts(vec![(clash, ConflictKind::RealFile)])
        );
        assert!(
            link.is_symlink(),
            "the existing link must survive a refused relink"
        );
    }

    #[test]
    fn relinking_that_needs_root_without_a_way_to_get_it_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        fs::write(&source, "").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let plan = relink_plan(&source, &link, vec![link_action(&source, &link)]);

        let error = execute_relink(
            &plan,
            &root_for("zsh"),
            None,
            &RealFileSystem,
            &FakeCommandRunner::new(),
        )
        .unwrap_err();

        assert!(matches!(error, RefusalError::RootUnavailable(_)));
        assert!(link.is_symlink());
    }

    fn adopt_plan_for(root: &Path, package: &str) -> (AdoptPlan, PathBuf, PathBuf) {
        let from = root.join("home/.zshrc");
        let into = root.join("repo/packages").join(package).join(".zshrc");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, "mine").unwrap();
        let plan = AdoptPlan {
            package: name(package),
            moves: vec![crate::adopt::AdoptAction::Move {
                from: from.clone(),
                into: into.clone(),
            }],
            links: Vec::new(),
        };
        (plan, from, into)
    }

    #[test]
    fn adopting_moves_the_file_and_links_it_back() {
        let root = tempfile::tempdir().unwrap();
        let (plan, from, into) = adopt_plan_for(root.path(), "zsh");

        let report =
            execute_adopt(&plan, &[], &RealFileSystem, &FakeCommandRunner::new(), None).unwrap();

        assert_eq!(fs::read_to_string(&into).unwrap(), "mine");
        assert_eq!(fs::read_link(&from).unwrap(), into);
        assert_eq!(report.verdict(), Verdict::Success);
    }

    #[test]
    fn adopting_into_a_package_that_needs_root_is_refused_and_moves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (plan, from, into) = adopt_plan_for(root.path(), "sysctl");

        let error = execute_adopt(
            &plan,
            &root_for("sysctl"),
            &RealFileSystem,
            &FakeCommandRunner::new(),
            Some(Privilege::UseSudo),
        )
        .unwrap_err();

        assert_eq!(error, RefusalError::AdoptNeedsRoot(name("sysctl")));
        assert_eq!(fs::read_to_string(&from).unwrap(), "mine");
        assert!(!into.exists());
    }

    #[test]
    fn adopting_links_only_the_files_that_moved() {
        let root = tempfile::tempdir().unwrap();
        let (mut plan, from, into) = adopt_plan_for(root.path(), "zsh");
        let blocked_from = root.path().join("home/.zshenv");
        fs::write(&blocked_from, "also mine").unwrap();
        let blocked_into = root.path().join("repo/packages/zsh/.zshenv");
        fs::create_dir_all(blocked_into.parent().unwrap()).unwrap();
        fs::write(&blocked_into, "already in the repo").unwrap();
        plan.moves.push(crate::adopt::AdoptAction::Move {
            from: blocked_from.clone(),
            into: blocked_into,
        });

        let report =
            execute_adopt(&plan, &[], &RealFileSystem, &FakeCommandRunner::new(), None).unwrap();

        assert_eq!(fs::read_link(&from).unwrap(), into);
        assert!(!blocked_from.is_symlink());
        assert_eq!(fs::read_to_string(&blocked_from).unwrap(), "also mine");
        assert_eq!(report.verdict(), Verdict::Failed);
    }

    fn adopt_report(moved: Vec<AdoptResult>, link_failed: bool) -> AdoptReport {
        let action = || crate::adopt::AdoptAction::Move {
            from: "/h/x".into(),
            into: "/r/x".into(),
        };
        let linked_result = if link_failed {
            ActionResult::Failed {
                reason: "no".to_string(),
            }
        } else {
            ActionResult::Done
        };
        AdoptReport {
            moved: moved
                .into_iter()
                .map(|result| AdoptOutcome {
                    action: action(),
                    result,
                })
                .collect(),
            linked: vec![PackageOutcome {
                package: name("zsh"),
                actions: vec![crate::apply::ActionOutcome {
                    action: link_action(Path::new("/r/x"), Path::new("/h/x")),
                    result: linked_result,
                }],
            }],
        }
    }

    #[test]
    fn an_adopt_report_with_a_moved_file_succeeds_even_if_others_were_refused() {
        let report = adopt_report(vec![AdoptResult::Moved, AdoptResult::Refused], false);
        assert_eq!(report.verdict(), Verdict::Success);
    }

    #[test]
    fn an_adopt_report_where_everything_was_refused_is_refused() {
        let report = adopt_report(vec![AdoptResult::Refused, AdoptResult::Refused], false);
        assert_eq!(report.verdict(), Verdict::Refused);
    }

    #[test]
    fn an_adopt_report_with_nothing_requested_is_refused() {
        assert_eq!(adopt_report(vec![], false).verdict(), Verdict::Refused);
    }

    #[test]
    fn an_adopt_report_with_a_failed_move_fails_even_if_another_moved() {
        let failed = AdoptResult::Failed {
            reason: "no".to_string(),
        };
        let report = adopt_report(vec![AdoptResult::Moved, failed], false);
        assert_eq!(report.verdict(), Verdict::Failed);
    }

    #[test]
    fn an_adopt_report_with_a_failed_link_fails() {
        let report = adopt_report(vec![AdoptResult::Moved], true);
        assert_eq!(report.verdict(), Verdict::Failed);
    }

    #[test]
    fn a_run_report_fails_if_a_file_action_the_install_or_a_hook_failed() {
        let ok = Report {
            files: Vec::new(),
            install: InstallOutcome::NotNeeded,
            hooks: Vec::new(),
        };
        assert_eq!(ok.verdict(), Verdict::Success);

        let install_failed = Report {
            install: InstallOutcome::Failed {
                reason: "x".to_string(),
            },
            ..ok.clone()
        };
        assert_eq!(install_failed.verdict(), Verdict::Failed);

        let hook_failed = Report {
            hooks: vec![HookOutcome {
                package: name("dwm"),
                result: HookResult::Failed { exit_code: Some(1) },
                output: Vec::new(),
            }],
            ..ok.clone()
        };
        assert_eq!(hook_failed.verdict(), Verdict::Failed);

        let hook_not_runnable = Report {
            hooks: vec![HookOutcome {
                package: name("dwm"),
                result: HookResult::NotRunnable {
                    reason: "x".to_string(),
                },
                output: Vec::new(),
            }],
            ..ok.clone()
        };
        assert_eq!(hook_not_runnable.verdict(), Verdict::Failed);

        let file_failed = Report {
            files: vec![PackageOutcome {
                package: name("zsh"),
                actions: vec![crate::apply::ActionOutcome {
                    action: link_action(Path::new("/a"), Path::new("/b")),
                    result: ActionResult::Failed {
                        reason: "x".to_string(),
                    },
                }],
            }],
            ..ok
        };
        assert_eq!(file_failed.verdict(), Verdict::Failed);
    }

    #[test]
    fn a_skipped_file_action_is_not_a_failure() {
        let outcomes = vec![PackageOutcome {
            package: name("zsh"),
            actions: vec![crate::apply::ActionOutcome {
                action: link_action(Path::new("/a"), Path::new("/b")),
                result: ActionResult::Skipped {
                    reason: "already linked".to_string(),
                },
            }],
        }];

        assert_eq!(file_verdict(&outcomes), Verdict::Success);
    }

    #[test]
    fn the_adopt_refusal_describes_itself() {
        assert_eq!(
            RefusalError::AdoptNeedsRoot(name("sysctl")).to_string(),
            "package sysctl needs root, and adopting files into it is not supported"
        );
    }

    /// A filesystem view with one symlink in it, for tests that must not touch
    /// the real filesystem (the paths are like `/etc/a.conf`).
    struct FakeState {
        link: PathBuf,
        destination: PathBuf,
    }

    impl FakeState {
        fn linked(link: &str, destination: &str) -> Self {
            Self {
                link: link.into(),
                destination: destination.into(),
            }
        }
    }

    impl FileSystemState for FakeState {
        fn state(
            &self,
            path: &Path,
        ) -> Result<crate::link::PathState, crate::link::FileSystemStateError> {
            Ok(if path == self.link {
                crate::link::PathState::Symlink {
                    destination: self.destination.clone(),
                }
            } else {
                crate::link::PathState::Missing
            })
        }

        fn is_writable(&self, _path: &Path) -> bool {
            true
        }
    }

    fn files_outcomes_of(packages: &[PackageOutcome]) -> Vec<&ActionResult> {
        packages
            .iter()
            .flat_map(|package| &package.actions)
            .map(|outcome| &outcome.result)
            .collect()
    }
}
