mod common;

use std::fs;
use std::os::unix::fs::symlink;

use common::Sandbox;
use predicates::prelude::*;

/// A package with a hook that appends to a log in the home directory.
fn hooked_sandbox() -> (Sandbox, std::path::PathBuf) {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let log = sandbox.home().join("hook-runs");
    sandbox.package_script("dwm", "build.sh", &format!("echo run >> {}", log.display()));
    (sandbox, log)
}

#[test]
fn hook_runs_after_confirmation() {
    let (sandbox, log) = hooked_sandbox();

    sandbox
        .command()
        .args(["hook", "dwm"])
        .write_stdin("y\n")
        .assert()
        .success()
        .stderr(predicate::str::contains("These hooks will run:"))
        .stderr(predicate::str::contains("Run them? [y/N]"))
        .stdout(predicate::str::contains("hook dwm: succeeded"));

    assert_eq!(fs::read_to_string(log).unwrap(), "run\n");
}

#[test]
fn hook_does_not_run_if_the_answer_is_no() {
    let (sandbox, log) = hooked_sandbox();

    sandbox
        .command()
        .args(["hook", "dwm"])
        .write_stdin("n\n")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("nothing was changed"));

    assert!(!log.exists());
}

#[test]
fn hook_does_not_run_with_no_terminal_input() {
    let (sandbox, log) = hooked_sandbox();

    sandbox
        .command()
        .args(["hook", "dwm"])
        .write_stdin("")
        .assert()
        .code(2);

    assert!(!log.exists());
}

#[test]
fn hook_with_yes_runs_without_asking() {
    let (sandbox, log) = hooked_sandbox();

    sandbox
        .command()
        .args(["hook", "dwm", "--yes"])
        .write_stdin("")
        .assert()
        .success();

    assert_eq!(fs::read_to_string(log).unwrap(), "run\n");
}

#[test]
fn hook_dry_run_lists_the_hooks_and_runs_nothing() {
    let (sandbox, log) = hooked_sandbox();

    sandbox
        .command()
        .args(["hook", "dwm", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hook: dwm"));

    assert!(!log.exists());
}

#[test]
fn hook_does_not_link_anything() {
    let (sandbox, _log) = hooked_sandbox();

    sandbox
        .command()
        .args(["hook", "dwm", "--yes"])
        .assert()
        .success();

    assert!(!sandbox.home().join("config.h").exists());
}

#[test]
fn hook_runs_only_the_named_packages_not_their_dependencies() {
    let (sandbox, log) = hooked_sandbox();
    sandbox.package_file("app", ".apprc", "");
    sandbox.manifest("app", "depends = [\"dwm\"]\nhook = \"hook.sh\"");
    let app_log = sandbox.home().join("app-runs");
    sandbox.package_script(
        "app",
        "hook.sh",
        &format!("echo app >> {}", app_log.display()),
    );

    sandbox
        .command()
        .args(["hook", "app", "--yes"])
        .assert()
        .success();

    assert!(app_log.exists());
    assert!(!log.exists(), "the dependency's hook was not run");
}

#[test]
fn hook_runs_named_packages_in_dependency_order() {
    let sandbox = Sandbox::new();
    let log = sandbox.home().join("order");
    for (package, depends) in [("base", ""), ("app", "depends = [\"base\"]\n")] {
        sandbox.package_file(package, ".rc", "");
        sandbox.manifest(package, &format!("{depends}hook = \"hook.sh\""));
        sandbox.package_script(
            package,
            "hook.sh",
            &format!("echo {package} >> {}", log.display()),
        );
    }

    sandbox
        .command()
        .args(["hook", "app", "base", "--yes"])
        .assert()
        .success();

    assert_eq!(fs::read_to_string(log).unwrap(), "base\napp\n");
}

#[test]
fn hook_refuses_when_none_of_the_packages_has_a_hook() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");

    sandbox
        .command()
        .args(["hook", "zsh", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "none of those packages has a hook",
        ));
}

#[test]
fn hook_of_an_unknown_package_exits_two() {
    let (sandbox, _log) = hooked_sandbox();

    sandbox
        .command()
        .args(["hook", "ghost", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("package ghost does not exist"));
}

#[test]
fn hook_requires_at_least_one_package() {
    let sandbox = Sandbox::new();

    sandbox.command().arg("hook").assert().code(2);
}

#[test]
fn a_failing_hook_exits_one_and_reports_it() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_script("dwm", "build.sh", "echo broke >&2\nexit 5");

    sandbox
        .command()
        .args(["hook", "dwm", "--yes"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("hook dwm: FAILED (exit status 5)"))
        .stderr(predicate::str::contains("[dwm] broke"));
}

#[test]
fn hook_with_json_prints_one_parseable_document() {
    let (sandbox, _log) = hooked_sandbox();

    let output = sandbox
        .command()
        .args(["--json", "hook", "dwm", "--yes"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let outcomes: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcomes[0]["package"], "dwm");
    assert_eq!(outcomes[0]["result"], "Succeeded");
}

#[test]
fn status_says_a_hook_has_never_run() {
    let (sandbox, _log) = hooked_sandbox();

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hook has never run"));
}

#[test]
fn status_is_quiet_about_a_hook_that_succeeded() {
    let (sandbox, _log) = hooked_sandbox();
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dwm: linked"))
        .stdout(predicate::str::contains("hook").not());
}

#[test]
fn status_reports_a_hook_that_failed_with_its_exit_code() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_script("dwm", "build.sh", "exit 5");
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .code(1);

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "hook FAILED last time (exit status 5, ",
        ))
        .stdout(predicate::str::contains("just now)"));
}

/// Seconds in a day.
const DAY: u64 = 24 * 60 * 60;

/// Rewrites the remembered `ran_at` to `days_ago` days before now (plus an
/// hour, so the run's own few seconds cannot tip it into the next unit down).
fn age_hook_runs(sandbox: &Sandbox, days_ago: u64) {
    let path = sandbox.home().join(".local/state/puddlejump/state.toml");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let then = now - days_ago * DAY - 60 * 60;
    let aged: String = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("ran_at = ") {
                format!("  ran_at = {then}\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    fs::write(path, aged).unwrap();
}

#[test]
fn status_says_how_long_ago_a_failed_hook_ran() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_script("dwm", "build.sh", "exit 5");
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .code(1);
    age_hook_runs(&sandbox, 2);

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "hook FAILED last time (exit status 5, 2 days ago)",
        ));
}

#[test]
fn status_says_how_long_ago_an_edited_hook_last_ran() {
    let (sandbox, log) = hooked_sandbox();
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();
    age_hook_runs(&sandbox, 5);
    sandbox.package_script("dwm", "build.sh", &format!("echo new >> {}", log.display()));

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "hook script changed since it last ran (5 days ago)",
        ));
}

#[test]
fn status_json_carries_when_a_hook_last_ran_in_seconds() {
    let (sandbox, _log) = hooked_sandbox();
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    let output = sandbox
        .command()
        .args(["--json", "status", "dwm"])
        .output()
        .unwrap();

    let statuses: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(statuses[0]["hook_ran_at"].as_u64().unwrap() > 0);
    assert_eq!(statuses[0]["hook"], "UpToDate");
}

#[test]
fn status_json_has_no_hook_time_for_a_hook_that_never_ran() {
    let (sandbox, _log) = hooked_sandbox();

    let output = sandbox
        .command()
        .args(["--json", "status", "dwm"])
        .output()
        .unwrap();

    let statuses: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(statuses[0]["hook_ran_at"].is_null());
    assert_eq!(statuses[0]["hook"], "NeverRun");
}

#[test]
fn status_reports_a_hook_edited_since_it_succeeded() {
    let (sandbox, log) = hooked_sandbox();
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();
    sandbox.package_script("dwm", "build.sh", &format!("echo new >> {}", log.display()));

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "hook script changed since it last ran",
        ));
}

#[test]
fn status_never_runs_a_hook_or_writes_state() {
    let (sandbox, log) = hooked_sandbox();

    sandbox.command().args(["status", "dwm"]).assert().success();

    assert!(!log.exists());
    assert!(!sandbox
        .home()
        .join(".local/state/puddlejump/state.toml")
        .exists());
}

#[test]
fn status_with_json_carries_the_hook_summary() {
    let (sandbox, _log) = hooked_sandbox();

    let output = sandbox
        .command()
        .args(["--json", "status", "dwm"])
        .output()
        .unwrap();

    let statuses: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(statuses[0]["hook"], "NeverRun");
}

#[test]
fn status_refuses_a_corrupt_state_file() {
    let (sandbox, _log) = hooked_sandbox();
    sandbox.home_file(".local/state/puddlejump/state.toml", "not [valid");

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid state file"));
}

#[test]
fn status_reports_not_linked_then_linked() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh: not linked"));

    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh: linked"));
}

#[test]
fn status_reports_a_partially_linked_package() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.package_file("zsh", ".zshenv", "");
    symlink(
        sandbox.repo().join("packages/zsh/.zshenv"),
        sandbox.home().join(".zshenv"),
    )
    .unwrap();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh: partially linked"))
        .stdout(predicate::str::contains(".zshrc  not linked"));
}

#[test]
fn status_reports_a_conflict_and_still_exits_zero() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.home_file(".zshrc", "mine");

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh: PROBLEM"))
        .stdout(predicate::str::contains("a real file is in the way"));
}

#[test]
fn status_reports_a_broken_link() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    symlink("/gone/for/good", sandbox.home().join(".zshrc")).unwrap();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("a broken symlink is in the way"));
}

#[test]
fn status_reports_a_link_into_another_package() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".rc", "");
    sandbox.package_file("bash", ".rc", "");
    sandbox
        .command()
        .args(["link", "bash", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already linked by package bash"));
}

#[test]
fn status_reports_missing_system_packages() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "[packages]\nxbps = [\"gcc\", \"make\"]");
    sandbox.fake_xbps(&["make"]);

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("missing system packages: gcc"));
}

#[test]
fn status_does_not_report_system_packages_that_are_installed() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "[packages]\nxbps = [\"gcc\"]");
    sandbox.fake_xbps(&["gcc"]);

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("missing").not());
}

#[test]
fn status_says_unknown_not_missing_when_there_is_no_package_manager() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "[packages]\nxbps = [\"gcc\"]");

    sandbox
        .command()
        .args(["status", "dwm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cannot be checked"))
        .stdout(predicate::str::contains("missing").not());
}

#[test]
fn status_never_installs_anything() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "[packages]\nxbps = [\"gcc\"]");
    sandbox.fake_xbps(&[]);

    sandbox.command().args(["status", "dwm"]).assert().success();

    assert!(!sandbox.calls().iter().any(|c| c.contains("xbps-install")));
    assert!(!sandbox.home().join("config.h").exists());
}

#[test]
fn status_with_no_packages_reports_every_package_when_there_is_no_host() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.package_file("nvim", ".nvimrc", "");

    sandbox
        .command()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("nvim: not linked"))
        .stdout(predicate::str::contains("zsh: not linked"));
}

#[test]
fn status_with_a_host_reports_only_the_profile_packages() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.package_file("nvim", ".nvimrc", "");
    sandbox.repo_file("hosts/workstation.toml", "packages = [\"zsh\"]");

    sandbox
        .command()
        .args(["--host", "workstation", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh:"))
        .stdout(predicate::str::contains("nvim:").not());
}

#[test]
fn status_of_an_unknown_package_exits_two() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");

    sandbox
        .command()
        .args(["status", "ghost"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("package ghost does not exist"));
}

#[test]
fn status_with_json_prints_one_parseable_document() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");

    let output = sandbox
        .command()
        .args(["--json", "status", "zsh"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let statuses: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(statuses[0]["package"], "zsh");
    assert_eq!(statuses[0]["state"], "NotLinked");
    assert_eq!(statuses[0]["system"], "NothingDeclared");
}

#[test]
fn list_shows_packages_and_hosts_sorted() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.package_file("nvim", ".nvimrc", "");
    sandbox.repo_file("hosts/zeta.toml", "");
    sandbox.repo_file("hosts/alpha.toml", "");

    let output = sandbox.command().arg("list").output().unwrap();

    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.find("nvim").unwrap() < text.find("zsh").unwrap(),
        "{text}"
    );
    assert!(
        text.find("alpha").unwrap() < text.find("zeta").unwrap(),
        "{text}"
    );
}

#[test]
fn list_marks_the_selected_host() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.repo_file("hosts/workstation.toml", "");
    sandbox.repo_file("hosts/other.toml", "");

    sandbox
        .command()
        .args(["--host", "workstation", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("workstation  (selected)"))
        .stdout(predicate::str::contains("other  (selected)").not());
}

#[test]
fn list_of_an_empty_repo_says_none() {
    let sandbox = Sandbox::new();

    sandbox
        .command()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("(none)"));
}

#[test]
fn list_with_json_prints_one_parseable_document() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.repo_file("hosts/workstation.toml", "");

    let output = sandbox.command().args(["--json", "list"]).output().unwrap();

    assert!(output.status.success());
    let listing: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listing["packages"][0], "zsh");
    assert_eq!(listing["hosts"][0], "workstation");
}

#[test]
fn list_in_a_directory_that_is_not_a_repo_exits_two() {
    let sandbox = Sandbox::new();
    let nowhere = sandbox.home().join("nowhere");
    fs::create_dir(&nowhere).unwrap();

    sandbox
        .command_without_repo()
        .arg("--repo")
        .arg(&nowhere)
        .arg("list")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot open the repo"));
}
