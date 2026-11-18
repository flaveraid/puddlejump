mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::{link_target, Sandbox};
use predicates::prelude::*;

fn zsh_sandbox() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "export A=1");
    sandbox
}

#[test]
fn link_creates_the_symlinks_and_exits_zero() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".zshrc"))
        .stdout(predicate::str::contains("[done]"));

    assert_eq!(
        link_target(&sandbox.home().join(".zshrc")),
        Some(sandbox.repo().join("packages/zsh/.zshrc"))
    );
}

#[test]
fn link_creates_missing_parent_directories_in_the_target() {
    let sandbox = Sandbox::new();
    sandbox.package_file("nvim", ".config/nvim/init.lua", "");

    sandbox
        .command()
        .args(["link", "nvim", "--yes"])
        .assert()
        .success();

    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
    assert!(sandbox.home().join(".config/nvim").is_dir());
}

#[test]
fn link_dry_run_prints_the_plan_and_changes_nothing() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .args(["link", "zsh", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("link"))
        .stdout(predicate::str::contains(".zshrc"));

    assert_eq!(fs::read_dir(sandbox.home()).unwrap().count(), 0);
}

#[test]
fn link_is_idempotent_and_reports_already_linked() {
    let sandbox = zsh_sandbox();
    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already linked"));
}

#[test]
fn link_into_a_target_override_leaves_the_real_home_alone() {
    let sandbox = zsh_sandbox();
    let stage = sandbox.home().join("stage");
    fs::create_dir(&stage).unwrap();

    sandbox
        .command()
        .args(["--target"])
        .arg(&stage)
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    assert!(stage.join(".zshrc").is_symlink());
    assert!(!sandbox.home().join(".zshrc").exists());
}

#[test]
fn link_with_a_conflict_exits_two_names_the_path_and_changes_nothing() {
    let sandbox = zsh_sandbox();
    sandbox.package_file("nvim", ".nvimrc", "");
    sandbox.home_file(".zshrc", "my own zshrc");

    sandbox
        .command()
        .args(["link", "zsh", "nvim", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("conflict"))
        .stderr(predicate::str::contains(".zshrc"));

    assert_eq!(
        fs::read_to_string(sandbox.home().join(".zshrc")).unwrap(),
        "my own zshrc"
    );
    assert!(
        !sandbox.home().join(".nvimrc").exists(),
        "a package without conflicts was not linked either"
    );
}

#[test]
fn link_never_overwrites_a_real_directory() {
    let sandbox = zsh_sandbox();
    fs::create_dir(sandbox.home().join(".zshrc")).unwrap();

    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .code(2);

    assert!(sandbox.home().join(".zshrc").is_dir());
}

#[test]
fn link_links_dependencies_first() {
    let sandbox = Sandbox::new();
    sandbox.package_file("base", ".baserc", "");
    sandbox.package_file("app", ".apprc", "");
    sandbox.manifest("app", "depends = [\"base\"]");

    let output = sandbox
        .command()
        .args(["link", "app", "--yes"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let base = stdout.find("base:").expect("base is in the output");
    let app = stdout.find("app:").expect("app is in the output");
    assert!(base < app, "base must be linked before app:\n{stdout}");
    assert!(sandbox.home().join(".baserc").is_symlink());
    assert!(sandbox.home().join(".apprc").is_symlink());
}

#[test]
fn link_reports_a_dependency_cycle_and_exits_two() {
    let sandbox = Sandbox::new();
    sandbox.package_file("a", ".a", "");
    sandbox.package_file("b", ".b", "");
    sandbox.manifest("a", "depends = [\"b\"]");
    sandbox.manifest("b", "depends = [\"a\"]");

    sandbox
        .command()
        .args(["link", "a", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("dependency cycle: a -> b -> a"));

    assert_eq!(fs::read_dir(sandbox.home()).unwrap().count(), 0);
}

#[test]
fn link_reports_a_missing_package_and_exits_two() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .args(["link", "ghost", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("package ghost does not exist"));
}

#[test]
fn link_reports_a_missing_dependency_and_names_who_needs_it() {
    let sandbox = zsh_sandbox();
    sandbox.manifest("zsh", "depends = [\"ghost\"]");

    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ghost (required by zsh)"));
}

#[test]
fn link_rejects_an_invalid_package_name() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .args(["link", "a/b", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("path separator"));
}

#[test]
fn link_reports_an_invalid_manifest_and_exits_two() {
    let sandbox = zsh_sandbox();
    sandbox.manifest("zsh", "target = \"relative\"");

    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("manifest.toml"));
}

#[test]
fn link_outside_a_repo_exits_two() {
    let sandbox = Sandbox::new();
    let nowhere = sandbox.home().join("not-a-repo");
    fs::create_dir(&nowhere).unwrap();

    sandbox
        .command_without_repo()
        .args(["--repo"])
        .arg(&nowhere)
        .args(["link", "zsh", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot open the repo"));
}

#[test]
fn link_uses_the_current_directory_when_no_repo_is_given() {
    let sandbox = zsh_sandbox();

    sandbox
        .command_without_repo()
        .current_dir(sandbox.repo())
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn link_uses_the_repo_from_the_environment() {
    let sandbox = zsh_sandbox();

    sandbox
        .command_without_repo()
        .env("PUDDLEJUMP_REPO", sandbox.repo())
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn the_repo_flag_beats_the_environment() {
    let sandbox = zsh_sandbox();
    let decoy = sandbox.home().join("decoy");
    fs::create_dir(&decoy).unwrap();

    sandbox
        .command_without_repo()
        .env("PUDDLEJUMP_REPO", &decoy)
        .arg("--repo")
        .arg(sandbox.repo())
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn link_dry_run_with_json_prints_a_single_parseable_plan() {
    let sandbox = zsh_sandbox();

    let output = sandbox
        .command()
        .args(["--json", "link", "zsh", "--dry-run"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(plan["packages"][0]["actions"][0]["Link"].is_object());
}

#[test]
fn link_with_json_prints_a_single_parseable_report() {
    let sandbox = zsh_sandbox();

    let output = sandbox
        .command()
        .args(["--json", "link", "zsh", "--yes"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["files"][0]["package"], "zsh");
    assert_eq!(report["files"][0]["actions"][0]["result"], "Done");
}

#[test]
fn link_with_no_packages_and_no_host_asks_before_linking_everything() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .arg("link")
        .write_stdin("n\n")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("links all 1 packages"))
        .stderr(predicate::str::contains("Link all of them? [y/N]"))
        .stderr(predicate::str::contains("nothing was changed"));

    assert_eq!(fs::read_dir(sandbox.home()).unwrap().count(), 0);
}

#[test]
fn link_with_no_packages_and_no_host_links_everything_when_confirmed() {
    let sandbox = zsh_sandbox();
    sandbox.package_file("nvim", ".nvimrc", "");

    sandbox
        .command()
        .arg("link")
        .write_stdin("y\n")
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
    assert!(sandbox.home().join(".nvimrc").is_symlink());
}

#[test]
fn link_with_no_packages_and_no_host_links_everything_with_yes() {
    let sandbox = zsh_sandbox();

    sandbox.command().args(["link", "--yes"]).assert().success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn link_with_no_terminal_input_does_not_link_everything_by_default() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .arg("link")
        .write_stdin("")
        .assert()
        .code(2);

    assert!(!sandbox.home().join(".zshrc").exists());
}

#[test]
fn the_everything_prompt_calls_out_packages_that_need_root() {
    let sandbox = zsh_sandbox();
    sandbox.package_file("sysctl", "a.conf", "");
    sandbox.manifest("sysctl", "needs_root = true\ntarget = \"/etc\"");

    sandbox
        .command()
        .arg("link")
        .write_stdin("n\n")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("sysctl  (needs root)"));
}

#[test]
fn link_uses_the_host_profile_when_no_packages_are_named() {
    let sandbox = zsh_sandbox();
    sandbox.package_file("nvim", ".nvimrc", "");
    sandbox.repo_file("hosts/workstation.toml", "packages = [\"zsh\"]");

    sandbox
        .command()
        .args(["--host", "workstation", "link"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
    assert!(
        !sandbox.home().join(".nvimrc").exists(),
        "packages outside the profile are not linked"
    );
}

#[test]
fn link_with_an_unknown_host_exits_two() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .args(["--host", "ghost", "link", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no host profile named ghost"));
}

#[test]
fn link_picks_the_host_from_the_environment() {
    let sandbox = zsh_sandbox();
    sandbox.repo_file("hosts/workstation.toml", "packages = [\"zsh\"]");

    sandbox
        .command()
        .env("PUDDLEJUMP_HOST", "workstation")
        .arg("link")
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn a_hook_runs_after_linking_and_its_output_is_shown() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_script("dwm", "build.sh", "echo building\necho warning >&2");

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[dwm] building"))
        .stdout(predicate::str::contains("hook dwm: succeeded"))
        .stderr(predicate::str::contains("[dwm] warning"));
}

#[test]
fn the_hook_script_itself_is_not_linked_into_the_target() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_script("dwm", "build.sh", "true");

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    assert!(sandbox.home().join("config.h").is_symlink());
    assert!(!sandbox.home().join("build.sh").exists());
    assert!(!sandbox.home().join("manifest.toml").exists());
}

#[test]
fn a_hook_sees_the_package_target_and_repo_in_its_environment() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_script(
        "dwm",
        "build.sh",
        "echo \"$PUDDLEJUMP_PACKAGE\"\necho \"$PUDDLEJUMP_TARGET\"\necho \"$PUDDLEJUMP_REPO\"",
    );

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[dwm] dwm"))
        .stdout(predicate::str::contains(format!(
            "[dwm] {}",
            sandbox.home().display()
        )))
        .stdout(predicate::str::contains(format!(
            "[dwm] {}",
            sandbox.repo().display()
        )));
}

#[test]
fn a_failing_hook_exits_one_keeps_the_links_and_runs_later_hooks() {
    let sandbox = Sandbox::new();
    sandbox.package_file("first", ".first", "");
    sandbox.manifest("first", "hook = \"hook.sh\"");
    sandbox.package_script("first", "hook.sh", "echo first ran\nexit 3");
    sandbox.package_file("second", ".second", "");
    sandbox.manifest("second", "hook = \"hook.sh\"");
    sandbox.package_script("second", "hook.sh", "echo second ran");

    sandbox
        .command()
        .args(["link", "first", "second", "--yes"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "hook first: FAILED (exit status 3)",
        ))
        .stdout(predicate::str::contains("hook second: succeeded"))
        .stdout(predicate::str::contains("[second] second ran"));

    assert!(sandbox.home().join(".first").is_symlink());
    assert!(sandbox.home().join(".second").is_symlink());
}

#[test]
fn hooks_run_in_dependency_order() {
    let sandbox = Sandbox::new();
    sandbox.package_file("base", ".base", "");
    sandbox.manifest("base", "hook = \"hook.sh\"");
    sandbox.package_script("base", "hook.sh", "echo base");
    sandbox.package_file("app", ".app", "");
    sandbox.manifest("app", "depends = [\"base\"]\nhook = \"hook.sh\"");
    sandbox.package_script("app", "hook.sh", "echo app");

    let output = sandbox
        .command()
        .args(["link", "app", "--yes"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.find("[base] base").unwrap() < stdout.find("[app] app").unwrap());
}

/// `app` depends on `base`; both have hooks that append to a log in the home
/// directory, so a test can count how often each ran.
fn app_and_base_with_logging_hooks() -> (Sandbox, std::path::PathBuf) {
    let sandbox = Sandbox::new();
    let log = sandbox.home().join("hook-runs");
    for package in ["base", "app"] {
        sandbox.package_file(package, &format!(".{package}rc"), "");
        sandbox.package_script(
            package,
            "hook.sh",
            &format!("echo {package} >> {}", log.display()),
        );
    }
    sandbox.manifest("base", "hook = \"hook.sh\"");
    sandbox.manifest("app", "depends = [\"base\"]\nhook = \"hook.sh\"");
    (sandbox, log)
}

#[test]
fn linking_a_package_does_not_re_run_the_hook_of_an_already_linked_dependency() {
    let (sandbox, log) = app_and_base_with_logging_hooks();
    sandbox
        .command()
        .args(["link", "base", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["link", "app"])
        .write_stdin("")
        .assert()
        .success();

    assert_eq!(fs::read_to_string(log).unwrap(), "base\napp\n");
}

#[test]
fn linking_a_package_still_runs_the_hook_of_a_dependency_that_is_not_linked_yet() {
    let (sandbox, log) = app_and_base_with_logging_hooks();

    sandbox
        .command()
        .args(["link", "app"])
        .write_stdin("")
        .assert()
        .success();

    assert_eq!(fs::read_to_string(log).unwrap(), "base\napp\n");
}

#[test]
fn a_hook_that_is_not_executable_is_reported_and_exits_one() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_file("dwm", "build.sh", "#!/bin/sh\n");

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("is not executable"));
}

#[test]
fn no_hooks_skips_the_hooks() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let marker = sandbox.home().join("hook-ran");
    sandbox.package_script("dwm", "build.sh", &format!("touch {}", marker.display()));

    sandbox
        .command()
        .args(["link", "dwm", "--no-hooks", "--yes"])
        .assert()
        .success();

    assert!(!marker.exists());
    assert!(sandbox.home().join("config.h").is_symlink());
}

#[test]
fn a_dry_run_does_not_run_hooks() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let marker = sandbox.home().join("hook-ran");
    sandbox.package_script("dwm", "build.sh", &format!("touch {}", marker.display()));

    sandbox
        .command()
        .args(["link", "dwm", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hook: dwm"));

    assert!(!marker.exists());
}

/// A `dwm` package with a hook that appends a line to a log in the home
/// directory, so a test can count how often it ran.
fn dwm_with_logging_hook() -> (Sandbox, std::path::PathBuf) {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let log = sandbox.home().join("hook-runs");
    sandbox.package_script("dwm", "build.sh", &format!("echo run >> {}", log.display()));
    (sandbox, log)
}

fn link_dwm(sandbox: &Sandbox) -> assert_cmd::assert::Assert {
    sandbox
        .command()
        .args(["link", "dwm"])
        .write_stdin("")
        .assert()
}

#[test]
fn running_link_again_does_not_re_run_a_hook_that_already_succeeded() {
    let (sandbox, log) = dwm_with_logging_hook();
    link_dwm(&sandbox).success();

    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\n");
}

#[test]
fn running_link_again_never_asks_whether_to_re_run_hooks() {
    let (sandbox, _log) = dwm_with_logging_hook();
    link_dwm(&sandbox).success();

    link_dwm(&sandbox)
        .success()
        .stderr(predicate::str::contains("Run them?").not());
}

#[test]
fn force_hooks_re_runs_a_hook_that_already_succeeded() {
    let (sandbox, log) = dwm_with_logging_hook();
    link_dwm(&sandbox).success();

    sandbox
        .command()
        .args(["link", "dwm", "--force-hooks"])
        .write_stdin("")
        .assert()
        .success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\nrun\n");
}

#[test]
fn a_failed_hook_is_retried_on_the_next_link_without_asking() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let log = sandbox.home().join("hook-runs");
    let script = format!("echo run >> {}\nexit 3", log.display());
    sandbox.package_script("dwm", "build.sh", &script);
    link_dwm(&sandbox).code(1);

    link_dwm(&sandbox).code(1);

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\nrun\n");
}

#[test]
fn a_hook_that_was_fixed_runs_again_then_stops_being_retried() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let log = sandbox.home().join("hook-runs");
    sandbox.package_script("dwm", "build.sh", "exit 3");
    link_dwm(&sandbox).code(1);
    sandbox.package_script(
        "dwm",
        "build.sh",
        &format!("echo fixed >> {}", log.display()),
    );
    link_dwm(&sandbox).success();

    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "fixed\n");
}

#[test]
fn editing_a_hook_that_succeeded_makes_it_run_again() {
    let (sandbox, log) = dwm_with_logging_hook();
    link_dwm(&sandbox).success();
    sandbox.package_script(
        "dwm",
        "build.sh",
        &format!("echo edited >> {}", log.display()),
    );

    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\nedited\n");
}

#[test]
fn the_hook_command_runs_even_when_the_hook_already_succeeded() {
    let (sandbox, log) = dwm_with_logging_hook();
    link_dwm(&sandbox).success();

    sandbox
        .command()
        .args(["hook", "dwm", "--yes"])
        .assert()
        .success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\nrun\n");
}

#[test]
fn the_hook_command_records_its_result_so_link_does_not_repeat_it() {
    let (sandbox, log) = dwm_with_logging_hook();
    sandbox
        .command()
        .args(["hook", "dwm", "--yes"])
        .assert()
        .success();

    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\n");
}

#[test]
fn a_dry_run_does_not_remember_anything() {
    let (sandbox, log) = dwm_with_logging_hook();
    sandbox
        .command()
        .args(["link", "dwm", "--dry-run"])
        .assert()
        .success();
    assert!(!state_file(&sandbox).exists());

    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\n");
}

#[test]
fn no_hooks_does_not_remember_a_hook_that_never_ran() {
    let (sandbox, log) = dwm_with_logging_hook();
    sandbox
        .command()
        .args(["link", "dwm", "--no-hooks"])
        .assert()
        .success();

    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\n");
}

#[test]
fn a_failed_install_does_not_remember_hooks_that_never_ran() {
    let (sandbox, log) = dwm_with_logging_hook();
    sandbox.manifest("dwm", "hook = \"build.sh\"\n[packages]\nxbps = [\"gcc\"]");
    sandbox.fake_tool("xbps-query", "exit 2");
    sandbox.fake_tool("xbps-install", "exit 1");
    sandbox.fake_sudo();
    link_dwm(&sandbox).code(1);
    assert!(!log.exists(), "the hook did not run");

    sandbox.fake_tool("xbps-install", "true");
    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\n");
}

#[test]
fn the_plan_says_why_each_hook_will_run() {
    let (sandbox, _log) = dwm_with_logging_hook();

    sandbox
        .command()
        .args(["link", "dwm", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hook: dwm"))
        .stdout(predicate::str::contains("[never run]"));
}

#[test]
fn a_dry_run_after_success_lists_no_hook() {
    let (sandbox, _log) = dwm_with_logging_hook();
    link_dwm(&sandbox).success();

    sandbox
        .command()
        .args(["link", "dwm", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hook:").not());
}

#[test]
fn the_json_plan_carries_the_hook_reason() {
    let (sandbox, _log) = dwm_with_logging_hook();

    let output = sandbox
        .command()
        .args(["--json", "link", "dwm", "--dry-run"])
        .output()
        .unwrap();

    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["hooks"][0]["reason"], "NeverRun");
}

/// Where the state file lives in a sandbox that sets no `XDG_STATE_HOME`.
fn state_file(sandbox: &Sandbox) -> std::path::PathBuf {
    sandbox.home().join(".local/state/puddlejump/state.toml")
}

#[test]
fn the_state_file_is_written_under_the_home_directory_by_default() {
    let (sandbox, _log) = dwm_with_logging_hook();

    link_dwm(&sandbox).success();

    let text = fs::read_to_string(state_file(&sandbox)).unwrap();
    assert!(text.contains("version = 1"), "{text}");
    assert!(text.contains("package = \"dwm\""), "{text}");
    assert!(text.contains("status = \"succeeded\""), "{text}");
}

#[test]
fn xdg_state_home_moves_the_state_file() {
    let (sandbox, log) = dwm_with_logging_hook();
    let xdg = sandbox.home().join("xdg-state");
    fs::create_dir_all(&xdg).unwrap();
    let link = |sandbox: &Sandbox| {
        sandbox
            .command()
            .env("XDG_STATE_HOME", &xdg)
            .args(["link", "dwm"])
            .write_stdin("")
            .assert()
            .success();
    };
    link(&sandbox);

    link(&sandbox);

    assert!(xdg.join("puddlejump/state.toml").is_file());
    assert!(!state_file(&sandbox).exists());
    assert_eq!(fs::read_to_string(&log).unwrap(), "run\n");
}

#[test]
fn a_corrupt_state_file_refuses_the_link_before_changing_anything() {
    let (sandbox, log) = dwm_with_logging_hook();
    sandbox.home_file(".local/state/puddlejump/state.toml", "not [valid");

    link_dwm(&sandbox)
        .code(2)
        .stderr(predicate::str::contains("invalid state file"));

    assert!(!log.exists());
    assert!(!sandbox.home().join("config.h").exists());
}

#[test]
fn a_corrupt_state_file_refuses_the_hook_command_before_running() {
    let (sandbox, log) = dwm_with_logging_hook();
    sandbox.home_file(".local/state/puddlejump/state.toml", "not [valid");

    sandbox
        .command()
        .args(["hook", "dwm", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid state file"));

    assert!(!log.exists());
}

#[test]
fn a_state_file_from_a_newer_version_is_refused_not_overwritten() {
    let (sandbox, log) = dwm_with_logging_hook();
    let path = sandbox.home_file(".local/state/puddlejump/state.toml", "version = 99\n");

    link_dwm(&sandbox)
        .code(2)
        .stderr(predicate::str::contains("format version 99"));

    assert_eq!(fs::read_to_string(path).unwrap(), "version = 99\n");
    assert!(!log.exists());
}

#[test]
fn a_state_file_that_cannot_be_written_warns_but_does_not_fail_the_run() {
    let (sandbox, log) = dwm_with_logging_hook();
    // A read-only state directory reads as empty but cannot be written to.
    let directory = sandbox.home().join(".local/state/puddlejump");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();

    let result = sandbox
        .command()
        .args(["link", "dwm"])
        .write_stdin("")
        .assert();

    // Restore write access first so the temp directory can be cleaned up even
    // if an assertion below fails.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    result
        .success()
        .stderr(predicate::str::contains("hook results were not remembered"));
    assert_eq!(fs::read_to_string(&log).unwrap(), "run\n");
}

#[test]
fn deleting_the_state_file_only_loses_history() {
    let (sandbox, log) = dwm_with_logging_hook();
    link_dwm(&sandbox).success();
    fs::remove_file(state_file(&sandbox)).unwrap();

    link_dwm(&sandbox).success();

    assert_eq!(fs::read_to_string(&log).unwrap(), "run\nrun\n");
}

#[test]
fn the_first_link_runs_hooks_without_asking() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    sandbox.package_script("dwm", "build.sh", "echo ran");

    sandbox
        .command()
        .args(["link", "dwm"])
        .write_stdin("")
        .assert()
        .success()
        .stdout(predicate::str::contains("[dwm] ran"));
}
