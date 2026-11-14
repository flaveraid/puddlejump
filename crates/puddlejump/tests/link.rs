mod common;

use std::fs;

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

#[test]
fn running_link_again_asks_before_re_running_hooks_and_refuses_by_default() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let marker = sandbox.home().join("hook-runs");
    sandbox.package_script(
        "dwm",
        "build.sh",
        &format!("echo run >> {}", marker.display()),
    );
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["link", "dwm"])
        .write_stdin("")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "already linked, but these hooks would run again",
        ));

    assert_eq!(fs::read_to_string(&marker).unwrap(), "run\n");
}

#[test]
fn running_link_again_re_runs_hooks_when_confirmed() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"");
    let marker = sandbox.home().join("hook-runs");
    sandbox.package_script(
        "dwm",
        "build.sh",
        &format!("echo run >> {}", marker.display()),
    );
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["link", "dwm"])
        .write_stdin("y\n")
        .assert()
        .success();

    assert_eq!(fs::read_to_string(&marker).unwrap(), "run\nrun\n");
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
