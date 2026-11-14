//! Turning structured results into text. Pure functions that return strings,
//! so they can be tested without a terminal.

use std::fmt::Write;

use puddlejump_core::adopt::{AdoptAction, AdoptOutcome, AdoptPlan, AdoptRefusal, AdoptResult};
use puddlejump_core::apply::{ActionResult, PackageOutcome};
use puddlejump_core::execute::{InstallOutcome, Report};
use puddlejump_core::hook::{HookOutcome, HookResult};
use puddlejump_core::host::HostName;
use puddlejump_core::package::PackageName;
use puddlejump_core::plan::{Action, ConflictKind, PackageActions, Plan, PlanWarning, RootReason};
use puddlejump_core::status::{FileState, PackageState, PackageStatus, SystemStatus};
use puddlejump_core::system::{ManagerId, SystemNotice, SystemPlan};

/// Appends a formatted line. Writing to a `String` cannot fail.
macro_rules! line {
    ($out:expr) => { $out.push('\n') };
    ($out:expr, $($arg:tt)*) => {{
        let _ = writeln!($out, $($arg)*);
    }};
}

fn managers(managers: &[ManagerId]) -> String {
    managers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Describes one planned action in a line.
fn action_line(action: &Action) -> String {
    match action {
        Action::Link { source, link } => {
            format!("link    {} -> {}", link.display(), source.display())
        }
        Action::LinkSymlinkCopy { destination, link } => {
            format!("link    {} -> {}", link.display(), destination.display())
        }
        Action::Unlink { link, .. } => format!("unlink  {}", link.display()),
        Action::RemoveDirectoryIfEmpty { path } => {
            format!("rmdir   {} (if empty)", path.display())
        }
        Action::Conflict { path, kind } => {
            format!("CONFLICT {}: {}", path.display(), conflict(kind))
        }
        Action::Skip { link, reason } => format!("skip    {} ({reason})", link.display()),
    }
}

fn conflict(kind: &ConflictKind) -> String {
    match kind {
        ConflictKind::RealFile => "a real file is in the way".to_string(),
        ConflictKind::RealDirectory => "a real directory is in the way".to_string(),
        ConflictKind::ForeignSymlink => "a symlink to somewhere else is in the way".to_string(),
        ConflictKind::BrokenSymlink => "a broken symlink is in the way".to_string(),
        ConflictKind::OtherPackage { owner } => {
            format!("already linked by package {owner}")
        }
        ConflictKind::ProvidedTwice { other } => {
            format!("package {other} already provides this path")
        }
        ConflictKind::ParentNotDirectory { parent } => {
            format!("{} is not a directory", parent.display())
        }
    }
}

/// Describes why root is needed, for a refusal message.
pub fn root_reason(reason: &RootReason) -> String {
    match reason {
        RootReason::SystemInstall => "installing system packages".to_string(),
        RootReason::PackageDeclared { package } => {
            format!("package {package} declares needs_root")
        }
        RootReason::TargetNotWritable { package, path } => {
            format!("package {package}: {} is not writable", path.display())
        }
    }
}

fn warning(warning: &PlanWarning) -> String {
    match warning {
        PlanWarning::NeedsRootUndeclared { package } => {
            format!("package {package} needs root but its manifest does not say needs_root = true")
        }
        PlanWarning::RelativeSymlinkSkipped { package, path } => format!(
            "package {package}: symlink {} has a relative destination and was skipped",
            path.display()
        ),
    }
}

fn notice(notice: &SystemNotice) -> String {
    match notice {
        SystemNotice::NoManagerAvailable { package, declared } => format!(
            "package {package} declares system packages ({}) but no package manager is available",
            managers(declared)
        ),
        SystemNotice::NoListForManager { package, declared } => format!(
            "package {package} has system packages only for: {}",
            if declared.is_empty() {
                "(none)".to_string()
            } else {
                managers(declared)
            }
        ),
    }
}

fn file_actions(out: &mut String, packages: &[PackageActions]) {
    for package in packages {
        line!(out, "{}:", package.package);
        if package.actions.is_empty() {
            line!(out, "  (no files)");
        }
        for action in &package.actions {
            line!(out, "  {}", action_line(action));
        }
    }
}

fn system_section(out: &mut String, system: &SystemPlan) {
    if let Some(manager) = system.manager.filter(|_| !system.to_install.is_empty()) {
        line!(out, "install ({manager}): {}", system.to_install.join(" "));
    }
    for text in system.notices.iter().map(notice) {
        line!(out, "note: {text}");
    }
}

/// Renders everything `link` would do.
pub fn plan(plan: &Plan) -> String {
    let mut out = String::new();
    file_actions(&mut out, &plan.packages);
    system_section(&mut out, &plan.system);
    for hook in &plan.hooks {
        line!(out, "hook: {} ({})", hook.package, hook.script.display());
    }
    if !plan.needs_root.is_empty() {
        line!(out, "root required for:");
        for reason in &plan.needs_root {
            line!(out, "  - {}", root_reason(reason));
        }
    }
    for text in plan.warnings.iter().map(warning) {
        line!(out, "warning: {text}");
    }
    out
}

/// Renders only what the user should know before a run: warnings and system
/// package notices. The actions themselves appear in the report afterwards.
pub fn plan_notes(plan: &Plan) -> String {
    let mut out = String::new();
    for text in plan.warnings.iter().map(warning) {
        line!(out, "warning: {text}");
    }
    for text in plan.system.notices.iter().map(notice) {
        line!(out, "note: {text}");
    }
    out
}

/// Renders the per-package actions of an unlink or relink.
pub fn file_plan(packages: &[PackageActions]) -> String {
    let mut out = String::new();
    file_actions(&mut out, packages);
    out
}

/// The result column of an outcome line, or `None` when the action line
/// already says everything (a planned skip).
fn action_result(action: &Action, result: &ActionResult) -> Option<String> {
    match (action, result) {
        (Action::Skip { .. }, ActionResult::Skipped { .. }) => None,
        (_, ActionResult::Done) => Some("done".to_string()),
        (_, ActionResult::Skipped { reason }) => Some(format!("skipped: {reason}")),
        (_, ActionResult::Failed { reason }) => Some(format!("FAILED: {reason}")),
    }
}

fn file_outcomes(out: &mut String, packages: &[PackageOutcome]) {
    for package in packages {
        line!(out, "{}:", package.package);
        for outcome in &package.actions {
            let text = action_line(&outcome.action);
            match action_result(&outcome.action, &outcome.result) {
                Some(result) => line!(out, "  {text}  [{result}]"),
                None => line!(out, "  {text}"),
            }
        }
    }
}

fn install_outcome(outcome: &InstallOutcome) -> Option<String> {
    match outcome {
        InstallOutcome::NotNeeded => None,
        InstallOutcome::Skipped { packages } => {
            Some(format!("install skipped: {}", packages.join(" ")))
        }
        InstallOutcome::NoManager => {
            Some("install skipped: no package manager is available".to_string())
        }
        InstallOutcome::Installed { packages } => {
            Some(format!("installed: {}", packages.join(" ")))
        }
        InstallOutcome::Failed { reason } => Some(format!("install FAILED: {reason}")),
    }
}

fn hook_result(result: &HookResult) -> String {
    match result {
        HookResult::Succeeded => "succeeded".to_string(),
        HookResult::Failed {
            exit_code: Some(code),
        } => format!("FAILED (exit status {code})"),
        HookResult::Failed { exit_code: None } => "FAILED (killed by a signal)".to_string(),
        HookResult::NotRunnable { reason } => format!("FAILED to start: {reason}"),
    }
}

/// Renders the hook results. Output was already streamed live, so only the
/// verdict per hook is repeated here.
pub fn hook_outcomes(outcomes: &[HookOutcome]) -> String {
    let mut out = String::new();
    for outcome in outcomes {
        line!(
            out,
            "hook {}: {}",
            outcome.package,
            hook_result(&outcome.result)
        );
    }
    out
}

/// Renders what a `link` run did, except hook output (already streamed).
pub fn report(report: &Report) -> String {
    let mut out = String::new();
    file_outcomes(&mut out, &report.files);
    if let Some(text) = install_outcome(&report.install) {
        line!(out, "{text}");
    }
    out.push_str(&hook_outcomes(&report.hooks));
    out
}

/// Renders the results of an unlink or relink.
pub fn file_report(packages: &[PackageOutcome]) -> String {
    let mut out = String::new();
    file_outcomes(&mut out, packages);
    out
}

fn package_state(state: PackageState) -> &'static str {
    match state {
        PackageState::Empty => "empty",
        PackageState::Linked => "linked",
        PackageState::NotLinked => "not linked",
        PackageState::Partial => "partially linked",
        PackageState::Problem => "PROBLEM",
    }
}

fn file_state(state: &FileState) -> String {
    match state {
        FileState::Linked => "linked".to_string(),
        FileState::NotLinked => "not linked".to_string(),
        FileState::Conflict(kind) => format!("CONFLICT: {}", conflict(kind)),
        FileState::RelativeSymlink => "relative symlink (cannot be copied)".to_string(),
    }
}

fn system_status(status: &SystemStatus) -> Option<String> {
    match status {
        SystemStatus::NothingDeclared | SystemStatus::AllInstalled => None,
        SystemStatus::Missing { packages } => {
            Some(format!("missing system packages: {}", packages.join(" ")))
        }
        SystemStatus::Unknown => {
            Some("system packages declared but cannot be checked (no package manager)".to_string())
        }
    }
}

/// Renders the status of each package. Files are listed only when they are not
/// simply linked, so a healthy package is one line.
pub fn statuses(statuses: &[PackageStatus]) -> String {
    let mut out = String::new();
    for status in statuses {
        line!(out, "{}: {}", status.package, package_state(status.state));
        for file in &status.files {
            if file.state != FileState::Linked || status.state == PackageState::Problem {
                line!(
                    out,
                    "  {}  {}",
                    file.link.display(),
                    file_state(&file.state)
                );
            }
        }
        if let Some(text) = system_status(&status.system) {
            line!(out, "  {text}");
        }
    }
    out
}

/// Renders the packages and host profiles in the repo.
pub fn listing(
    packages: &[&PackageName],
    hosts: &[HostName],
    selected: Option<&HostName>,
) -> String {
    let mut out = String::new();
    line!(out, "packages:");
    if packages.is_empty() {
        line!(out, "  (none)");
    }
    for package in packages {
        line!(out, "  {package}");
    }
    line!(out, "hosts:");
    if hosts.is_empty() {
        line!(out, "  (none)");
    }
    for host in hosts {
        let marker = if Some(host) == selected {
            "  (selected)"
        } else {
            ""
        };
        line!(out, "  {host}{marker}");
    }
    out
}

fn adopt_refusal(refusal: &AdoptRefusal) -> String {
    match refusal {
        AdoptRefusal::NotInTarget => "not inside the package's target directory".to_string(),
        AdoptRefusal::Missing => "does not exist".to_string(),
        AdoptRefusal::IsSymlink => "is a symlink".to_string(),
        AdoptRefusal::NotARegularFile => "is not a regular file".to_string(),
        AdoptRefusal::DestinationExists { into } => {
            format!("the package already has {}", into.display())
        }
        AdoptRefusal::Duplicate => "was named more than once".to_string(),
    }
}

fn adopt_line(action: &AdoptAction) -> String {
    match action {
        AdoptAction::Move { from, into } => {
            format!("move    {} -> {}", from.display(), into.display())
        }
        AdoptAction::Refuse { path, reason } => {
            format!("REFUSED {}: {}", path.display(), adopt_refusal(reason))
        }
    }
}

/// Renders what adopting would do.
pub fn adopt_plan(plan: &AdoptPlan) -> String {
    let mut out = String::new();
    line!(out, "{}:", plan.package);
    if plan.moves.is_empty() {
        line!(out, "  (nothing to adopt)");
    }
    for action in &plan.moves {
        line!(out, "  {}", adopt_line(action));
    }
    out
}

/// Renders what adopting did.
pub fn adopt_outcomes(outcomes: &[AdoptOutcome]) -> String {
    let mut out = String::new();
    for outcome in outcomes {
        let result = match &outcome.result {
            AdoptResult::Moved => "done".to_string(),
            AdoptResult::Refused => "refused".to_string(),
            AdoptResult::Failed { reason } => format!("FAILED: {reason}"),
        };
        line!(out, "  {}  [{result}]", adopt_line(&outcome.action));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use puddlejump_core::apply::ActionOutcome;
    use puddlejump_core::hook::OutputLine;
    use puddlejump_core::plan::{HookPlan, SkipReason};
    use puddlejump_core::status::FileStatus;
    use std::path::PathBuf;

    fn name(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn path(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    fn link(source: &str, link: &str) -> Action {
        Action::Link {
            source: path(source),
            link: path(link),
        }
    }

    fn done() -> ActionResult {
        ActionResult::Done
    }

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn every_action_is_described_in_one_line() {
        let cases = [
            (link("/r/a", "/h/a"), "link    /h/a -> /r/a"),
            (
                Action::LinkSymlinkCopy {
                    destination: path("/opt/x"),
                    link: path("/h/x"),
                },
                "link    /h/x -> /opt/x",
            ),
            (
                Action::Unlink {
                    link: path("/h/a"),
                    expected_destination: path("/r/a"),
                },
                "unlink  /h/a",
            ),
            (
                Action::RemoveDirectoryIfEmpty { path: path("/h/d") },
                "rmdir   /h/d (if empty)",
            ),
            (
                Action::Skip {
                    link: path("/h/a"),
                    reason: SkipReason::AlreadyLinked,
                },
                "skip    /h/a (already linked)",
            ),
            (
                Action::Conflict {
                    path: path("/h/a"),
                    kind: ConflictKind::RealFile,
                },
                "CONFLICT /h/a: a real file is in the way",
            ),
        ];

        for (action, expected) in cases {
            assert_eq!(action_line(&action), expected);
        }
    }

    #[test]
    fn every_kind_of_conflict_has_its_own_wording() {
        let cases = [
            (ConflictKind::RealFile, "a real file is in the way"),
            (
                ConflictKind::RealDirectory,
                "a real directory is in the way",
            ),
            (
                ConflictKind::ForeignSymlink,
                "a symlink to somewhere else is in the way",
            ),
            (
                ConflictKind::BrokenSymlink,
                "a broken symlink is in the way",
            ),
            (
                ConflictKind::OtherPackage {
                    owner: name("bash"),
                },
                "already linked by package bash",
            ),
            (
                ConflictKind::ProvidedTwice { other: name("a") },
                "package a already provides this path",
            ),
            (
                ConflictKind::ParentNotDirectory {
                    parent: path("/h/.config"),
                },
                "/h/.config is not a directory",
            ),
        ];

        for (kind, expected) in cases {
            assert_eq!(conflict(&kind), expected);
        }
    }

    #[test]
    fn every_root_reason_says_who_needs_root_and_why() {
        assert_eq!(
            root_reason(&RootReason::SystemInstall),
            "installing system packages"
        );
        assert_eq!(
            root_reason(&RootReason::PackageDeclared {
                package: name("sysctl")
            }),
            "package sysctl declares needs_root"
        );
        assert_eq!(
            root_reason(&RootReason::TargetNotWritable {
                package: name("sysctl"),
                path: path("/etc"),
            }),
            "package sysctl: /etc is not writable"
        );
    }

    fn plan_with(
        actions: Vec<Action>,
        system: SystemPlan,
        hooks: Vec<HookPlan>,
        needs_root: Vec<RootReason>,
        warnings: Vec<PlanWarning>,
    ) -> Plan {
        Plan {
            packages: vec![PackageActions {
                package: name("dwm"),
                actions,
            }],
            system,
            hooks,
            needs_root,
            warnings,
        }
    }

    fn no_system() -> SystemPlan {
        SystemPlan {
            manager: None,
            to_install: Vec::new(),
            already_installed: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn hook(package: &str) -> HookPlan {
        HookPlan {
            package: name(package),
            script: path("/r/build.sh"),
            package_dir: path("/r"),
            target_dir: path("/h"),
            repo_root: path("/r"),
        }
    }

    #[test]
    fn a_plan_lists_actions_installs_hooks_root_reasons_and_warnings_in_order() {
        let system = SystemPlan {
            manager: Some(ManagerId::Xbps),
            to_install: vec!["gcc".to_string(), "make".to_string()],
            already_installed: Vec::new(),
            notices: Vec::new(),
        };
        let plan = plan_with(
            vec![link("/r/a", "/h/a")],
            system,
            vec![hook("dwm")],
            vec![RootReason::SystemInstall],
            vec![PlanWarning::NeedsRootUndeclared {
                package: name("dwm"),
            }],
        );

        assert_eq!(
            lines(&super::plan(&plan)),
            [
                "dwm:",
                "  link    /h/a -> /r/a",
                "install (xbps): gcc make",
                "hook: dwm (/r/build.sh)",
                "root required for:",
                "  - installing system packages",
                "warning: package dwm needs root but its manifest does not say needs_root = true",
            ]
        );
    }

    #[test]
    fn a_plan_with_nothing_to_install_does_not_mention_installing() {
        let system = SystemPlan {
            manager: Some(ManagerId::Xbps),
            ..no_system()
        };
        let plan = plan_with(vec![link("/r/a", "/h/a")], system, vec![], vec![], vec![]);

        assert!(!super::plan(&plan).contains("install"));
    }

    #[test]
    fn a_package_with_no_files_says_so() {
        let plan = plan_with(vec![], no_system(), vec![], vec![], vec![]);

        assert_eq!(lines(&super::plan(&plan)), ["dwm:", "  (no files)"]);
    }

    #[test]
    fn plan_notes_shows_warnings_and_system_notices_but_not_actions() {
        let mut system = no_system();
        system.notices = vec![SystemNotice::NoManagerAvailable {
            package: name("dwm"),
            declared: vec![ManagerId::Xbps],
        }];
        let plan = plan_with(
            vec![link("/r/a", "/h/a")],
            system,
            vec![hook("dwm")],
            vec![RootReason::SystemInstall],
            vec![PlanWarning::RelativeSymlinkSkipped {
                package: name("dwm"),
                path: path("current"),
            }],
        );

        assert_eq!(
            lines(&plan_notes(&plan)),
            [
                "warning: package dwm: symlink current has a relative destination and was skipped",
                "note: package dwm declares system packages (xbps) but no package manager is available",
            ]
        );
    }

    #[test]
    fn plan_notes_is_empty_when_there_is_nothing_to_say() {
        let plan = plan_with(vec![], no_system(), vec![], vec![], vec![]);

        assert_eq!(plan_notes(&plan), "");
    }

    #[test]
    fn a_notice_for_a_manifest_with_no_list_for_this_manager_names_what_it_has() {
        let none = SystemNotice::NoListForManager {
            package: name("dwm"),
            declared: vec![],
        };
        let some = SystemNotice::NoListForManager {
            package: name("dwm"),
            declared: vec![ManagerId::Xbps],
        };

        assert_eq!(
            notice(&none),
            "package dwm has system packages only for: (none)"
        );
        assert_eq!(
            notice(&some),
            "package dwm has system packages only for: xbps"
        );
    }

    #[test]
    fn a_file_plan_lists_each_package_with_its_actions() {
        let packages = vec![
            PackageActions {
                package: name("a"),
                actions: vec![link("/r/a", "/h/a")],
            },
            PackageActions {
                package: name("b"),
                actions: vec![],
            },
        ];

        assert_eq!(
            lines(&file_plan(&packages)),
            ["a:", "  link    /h/a -> /r/a", "b:", "  (no files)"]
        );
    }

    fn outcome(action: Action, result: ActionResult) -> ActionOutcome {
        ActionOutcome { action, result }
    }

    fn outcomes_of(actions: Vec<ActionOutcome>) -> Vec<PackageOutcome> {
        vec![PackageOutcome {
            package: name("zsh"),
            actions,
        }]
    }

    #[test]
    fn a_report_shows_the_result_of_each_action() {
        let packages = outcomes_of(vec![
            outcome(link("/r/a", "/h/a"), done()),
            outcome(
                link("/r/b", "/h/b"),
                ActionResult::Failed {
                    reason: "Permission denied".to_string(),
                },
            ),
            outcome(
                link("/r/c", "/h/c"),
                ActionResult::Skipped {
                    reason: "already linked".to_string(),
                },
            ),
        ]);

        assert_eq!(
            lines(&file_report(&packages)),
            [
                "zsh:",
                "  link    /h/a -> /r/a  [done]",
                "  link    /h/b -> /r/b  [FAILED: Permission denied]",
                "  link    /h/c -> /r/c  [skipped: already linked]",
            ]
        );
    }

    #[test]
    fn a_planned_skip_is_not_annotated_twice() {
        let packages = outcomes_of(vec![outcome(
            Action::Skip {
                link: path("/h/a"),
                reason: SkipReason::AlreadyLinked,
            },
            ActionResult::Skipped {
                reason: "already linked".to_string(),
            },
        )]);

        assert_eq!(
            lines(&file_report(&packages)),
            ["zsh:", "  skip    /h/a (already linked)"]
        );
    }

    #[test]
    fn every_install_outcome_has_wording_except_not_needed() {
        let packages = vec!["gcc".to_string(), "make".to_string()];
        let cases = [
            (InstallOutcome::NotNeeded, None),
            (
                InstallOutcome::Skipped {
                    packages: packages.clone(),
                },
                Some("install skipped: gcc make"),
            ),
            (
                InstallOutcome::NoManager,
                Some("install skipped: no package manager is available"),
            ),
            (
                InstallOutcome::Installed { packages },
                Some("installed: gcc make"),
            ),
            (
                InstallOutcome::Failed {
                    reason: "no space".to_string(),
                },
                Some("install FAILED: no space"),
            ),
        ];

        for (outcome, expected) in cases {
            assert_eq!(install_outcome(&outcome).as_deref(), expected);
        }
    }

    fn hook_outcome(package: &str, result: HookResult) -> HookOutcome {
        HookOutcome {
            package: name(package),
            result,
            output: vec![OutputLine {
                stream: puddlejump_core::hook::HookOutputStream::Stdout,
                text: "never repeated here".to_string(),
            }],
        }
    }

    #[test]
    fn every_hook_result_has_its_own_wording_and_output_is_not_repeated() {
        let outcomes = [
            hook_outcome("a", HookResult::Succeeded),
            hook_outcome("b", HookResult::Failed { exit_code: Some(3) }),
            hook_outcome("c", HookResult::Failed { exit_code: None }),
            hook_outcome(
                "d",
                HookResult::NotRunnable {
                    reason: "gone".to_string(),
                },
            ),
        ];

        assert_eq!(
            lines(&hook_outcomes(&outcomes)),
            [
                "hook a: succeeded",
                "hook b: FAILED (exit status 3)",
                "hook c: FAILED (killed by a signal)",
                "hook d: FAILED to start: gone",
            ]
        );
    }

    #[test]
    fn a_run_report_shows_files_then_the_install_then_hooks() {
        let report = Report {
            files: outcomes_of(vec![outcome(link("/r/a", "/h/a"), done())]),
            install: InstallOutcome::Installed {
                packages: vec!["gcc".to_string()],
            },
            hooks: vec![hook_outcome("dwm", HookResult::Succeeded)],
        };

        assert_eq!(
            lines(&super::report(&report)),
            [
                "zsh:",
                "  link    /h/a -> /r/a  [done]",
                "installed: gcc",
                "hook dwm: succeeded",
            ]
        );
    }

    #[test]
    fn a_run_report_with_nothing_to_install_says_nothing_about_it() {
        let report = Report {
            files: vec![],
            install: InstallOutcome::NotNeeded,
            hooks: vec![],
        };

        assert_eq!(super::report(&report), "");
    }

    fn status(
        package: &str,
        state: PackageState,
        files: Vec<FileStatus>,
        system: SystemStatus,
    ) -> PackageStatus {
        PackageStatus {
            package: name(package),
            state,
            files,
            system,
        }
    }

    fn file(link: &str, state: FileState) -> FileStatus {
        FileStatus {
            link: path(link),
            state,
        }
    }

    #[test]
    fn every_package_state_has_its_own_word() {
        let cases = [
            (PackageState::Empty, "empty"),
            (PackageState::Linked, "linked"),
            (PackageState::NotLinked, "not linked"),
            (PackageState::Partial, "partially linked"),
            (PackageState::Problem, "PROBLEM"),
        ];

        for (state, expected) in cases {
            assert_eq!(package_state(state), expected);
        }
    }

    #[test]
    fn a_healthy_package_is_one_line() {
        let statuses = [status(
            "zsh",
            PackageState::Linked,
            vec![file("/h/.zshrc", FileState::Linked)],
            SystemStatus::NothingDeclared,
        )];

        assert_eq!(lines(&super::statuses(&statuses)), ["zsh: linked"]);
    }

    #[test]
    fn files_that_are_not_linked_are_listed_under_their_package() {
        let statuses = [status(
            "zsh",
            PackageState::Partial,
            vec![
                file("/h/.zshenv", FileState::Linked),
                file("/h/.zshrc", FileState::NotLinked),
            ],
            SystemStatus::NothingDeclared,
        )];

        assert_eq!(
            lines(&super::statuses(&statuses)),
            ["zsh: partially linked", "  /h/.zshrc  not linked"]
        );
    }

    #[test]
    fn a_problem_package_lists_every_file_including_the_linked_ones() {
        let statuses = [status(
            "zsh",
            PackageState::Problem,
            vec![
                file("/h/.zshenv", FileState::Linked),
                file("/h/.zshrc", FileState::Conflict(ConflictKind::RealFile)),
            ],
            SystemStatus::NothingDeclared,
        )];

        assert_eq!(
            lines(&super::statuses(&statuses)),
            [
                "zsh: PROBLEM",
                "  /h/.zshenv  linked",
                "  /h/.zshrc  CONFLICT: a real file is in the way",
            ]
        );
    }

    #[test]
    fn a_relative_symlink_is_explained_in_status() {
        let statuses = [status(
            "zsh",
            PackageState::NotLinked,
            vec![file("/h/current", FileState::RelativeSymlink)],
            SystemStatus::NothingDeclared,
        )];

        assert!(super::statuses(&statuses).contains("relative symlink (cannot be copied)"));
    }

    #[test]
    fn system_status_says_nothing_unless_something_is_missing_or_unknown() {
        assert_eq!(system_status(&SystemStatus::NothingDeclared), None);
        assert_eq!(system_status(&SystemStatus::AllInstalled), None);
        assert_eq!(
            system_status(&SystemStatus::Missing {
                packages: vec!["gcc".to_string(), "make".to_string()]
            })
            .as_deref(),
            Some("missing system packages: gcc make")
        );
        assert_eq!(
            system_status(&SystemStatus::Unknown).as_deref(),
            Some("system packages declared but cannot be checked (no package manager)")
        );
    }

    #[test]
    fn missing_system_packages_are_shown_under_the_package() {
        let statuses = [status(
            "dwm",
            PackageState::Linked,
            vec![],
            SystemStatus::Missing {
                packages: vec!["gcc".to_string()],
            },
        )];

        assert_eq!(
            lines(&super::statuses(&statuses)),
            ["dwm: linked", "  missing system packages: gcc"]
        );
    }

    #[test]
    fn a_listing_shows_packages_and_hosts_and_marks_the_selected_host() {
        let (zsh, nvim) = (name("zsh"), name("nvim"));
        let hosts = [
            HostName::new("laptop").unwrap(),
            HostName::new("desktop").unwrap(),
        ];

        let text = listing(&[&nvim, &zsh], &hosts, Some(&hosts[0]));

        assert_eq!(
            lines(&text),
            [
                "packages:",
                "  nvim",
                "  zsh",
                "hosts:",
                "  laptop  (selected)",
                "  desktop",
            ]
        );
    }

    #[test]
    fn an_empty_listing_says_none_for_both() {
        assert_eq!(
            lines(&listing(&[], &[], None)),
            ["packages:", "  (none)", "hosts:", "  (none)"]
        );
    }

    #[test]
    fn every_adopt_refusal_has_its_own_wording() {
        let cases = [
            (
                AdoptRefusal::NotInTarget,
                "not inside the package's target directory",
            ),
            (AdoptRefusal::Missing, "does not exist"),
            (AdoptRefusal::IsSymlink, "is a symlink"),
            (AdoptRefusal::NotARegularFile, "is not a regular file"),
            (
                AdoptRefusal::DestinationExists {
                    into: path("/r/p/.zshrc"),
                },
                "the package already has /r/p/.zshrc",
            ),
            (AdoptRefusal::Duplicate, "was named more than once"),
        ];

        for (refusal, expected) in cases {
            assert_eq!(adopt_refusal(&refusal), expected);
        }
    }

    fn mv(from: &str, into: &str) -> AdoptAction {
        AdoptAction::Move {
            from: path(from),
            into: path(into),
        }
    }

    #[test]
    fn an_adopt_plan_lists_moves_and_refusals() {
        let plan = AdoptPlan {
            package: name("zsh"),
            moves: vec![
                mv("/h/.zshrc", "/r/zsh/.zshrc"),
                AdoptAction::Refuse {
                    path: path("/h/gone"),
                    reason: AdoptRefusal::Missing,
                },
            ],
            links: vec![],
        };

        assert_eq!(
            lines(&adopt_plan(&plan)),
            [
                "zsh:",
                "  move    /h/.zshrc -> /r/zsh/.zshrc",
                "  REFUSED /h/gone: does not exist",
            ]
        );
    }

    #[test]
    fn an_adopt_plan_with_nothing_says_so() {
        let plan = AdoptPlan {
            package: name("zsh"),
            moves: vec![],
            links: vec![],
        };

        assert_eq!(lines(&adopt_plan(&plan)), ["zsh:", "  (nothing to adopt)"]);
    }

    #[test]
    fn adopt_outcomes_show_each_result() {
        let outcomes = [
            AdoptOutcome {
                action: mv("/h/a", "/r/a"),
                result: AdoptResult::Moved,
            },
            AdoptOutcome {
                action: AdoptAction::Refuse {
                    path: path("/h/b"),
                    reason: AdoptRefusal::IsSymlink,
                },
                result: AdoptResult::Refused,
            },
            AdoptOutcome {
                action: mv("/h/c", "/r/c"),
                result: AdoptResult::Failed {
                    reason: "disk full".to_string(),
                },
            },
        ];

        assert_eq!(
            lines(&adopt_outcomes(&outcomes)),
            [
                "  move    /h/a -> /r/a  [done]",
                "  REFUSED /h/b: is a symlink  [refused]",
                "  move    /h/c -> /r/c  [FAILED: disk full]",
            ]
        );
    }
}
