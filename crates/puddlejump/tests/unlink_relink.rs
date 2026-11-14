mod common;

use std::fs;
use std::os::unix::fs::symlink;

use common::{link_target, Sandbox};
use predicates::prelude::*;

/// A sandbox with `nvim` linked into the home directory.
fn linked_nvim() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.package_file("nvim", ".config/nvim/init.lua", "vim.o.number = true");
    sandbox
        .command()
        .args(["link", "nvim", "--yes"])
        .assert()
        .success();
    sandbox
}

#[test]
fn unlink_removes_the_links_and_keeps_the_package_files() {
    let sandbox = linked_nvim();

    sandbox
        .command()
        .args(["unlink", "nvim"])
        .assert()
        .success()
        .stdout(predicate::str::contains("unlink"))
        .stdout(predicate::str::contains("[done]"));

    assert!(!sandbox.home().join(".config/nvim/init.lua").exists());
    assert!(sandbox
        .repo()
        .join("packages/nvim/.config/nvim/init.lua")
        .is_file());
}

#[test]
fn unlink_leaves_empty_directories_by_default() {
    let sandbox = linked_nvim();

    sandbox
        .command()
        .args(["unlink", "nvim"])
        .assert()
        .success();

    assert!(sandbox.home().join(".config/nvim").is_dir());
}

#[test]
fn unlink_prune_removes_the_directories_it_emptied_but_not_the_home() {
    let sandbox = linked_nvim();

    sandbox
        .command()
        .args(["unlink", "nvim", "--prune"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rmdir"));

    assert!(!sandbox.home().join(".config").exists());
    assert!(sandbox.home().is_dir());
}

#[test]
fn unlink_prune_keeps_a_directory_that_still_has_other_files() {
    let sandbox = linked_nvim();
    sandbox.home_file(".config/nvim/my-own-notes.txt", "keep me");

    sandbox
        .command()
        .args(["unlink", "nvim", "--prune"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped: not empty"));

    assert_eq!(
        fs::read_to_string(sandbox.home().join(".config/nvim/my-own-notes.txt")).unwrap(),
        "keep me"
    );
}

#[test]
fn unlink_dry_run_changes_nothing() {
    let sandbox = linked_nvim();

    sandbox
        .command()
        .args(["unlink", "nvim", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("unlink"));

    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
}

#[test]
fn unlink_leaves_a_symlink_that_points_somewhere_else() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    let elsewhere = sandbox.home().join("elsewhere");
    fs::write(&elsewhere, "").unwrap();
    symlink(&elsewhere, sandbox.home().join(".zshrc")).unwrap();

    sandbox
        .command()
        .args(["unlink", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("symlink points somewhere else"));

    assert_eq!(link_target(&sandbox.home().join(".zshrc")), Some(elsewhere));
}

#[test]
fn unlink_never_removes_a_real_file() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.home_file(".zshrc", "my own");

    sandbox
        .command()
        .args(["unlink", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not a symlink"));

    assert_eq!(
        fs::read_to_string(sandbox.home().join(".zshrc")).unwrap(),
        "my own"
    );
}

#[test]
fn unlink_leaves_a_link_that_belongs_to_another_package() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".rc", "");
    sandbox.package_file("bash", ".rc", "");
    sandbox
        .command()
        .args(["link", "bash", "--yes"])
        .assert()
        .success();

    sandbox.command().args(["unlink", "zsh"]).assert().success();

    assert_eq!(
        link_target(&sandbox.home().join(".rc")),
        Some(sandbox.repo().join("packages/bash/.rc"))
    );
}

#[test]
fn unlink_only_touches_the_named_packages() {
    let sandbox = linked_nvim();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    sandbox.command().args(["unlink", "zsh"]).assert().success();

    assert!(!sandbox.home().join(".zshrc").exists());
    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
}

#[test]
fn unlink_does_not_uninstall_or_touch_the_package_manager() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "[packages]\nxbps = [\"gcc\"]");
    sandbox.fake_xbps(&[]);
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();
    let before = sandbox.calls().len();

    sandbox.command().args(["unlink", "dwm"]).assert().success();

    assert_eq!(sandbox.calls().len(), before, "no package manager call");
}

#[test]
fn unlink_with_no_packages_and_no_host_refuses_rather_than_unlinking_everything() {
    let sandbox = linked_nvim();

    sandbox
        .command()
        .arg("unlink")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no packages named"));

    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
}

#[test]
fn unlink_uses_the_host_profile_when_no_packages_are_named() {
    let sandbox = linked_nvim();
    sandbox.repo_file("hosts/workstation.toml", "packages = [\"nvim\"]");

    sandbox
        .command()
        .args(["--host", "workstation", "unlink"])
        .assert()
        .success();

    assert!(!sandbox.home().join(".config/nvim/init.lua").exists());
}

#[test]
fn unlink_of_an_unknown_package_exits_two() {
    let sandbox = linked_nvim();

    sandbox
        .command()
        .args(["unlink", "ghost"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("package ghost does not exist"));
}

#[test]
fn unlink_with_json_prints_one_parseable_document() {
    let sandbox = linked_nvim();

    let output = sandbox
        .command()
        .args(["--json", "unlink", "nvim"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let outcomes: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcomes[0]["package"], "nvim");
}

#[test]
fn relink_picks_up_a_file_added_to_the_package() {
    let sandbox = linked_nvim();
    sandbox.package_file("nvim", ".config/nvim/lua/plugins.lua", "return {}");

    sandbox
        .command()
        .args(["relink", "nvim"])
        .assert()
        .success();

    assert!(sandbox
        .home()
        .join(".config/nvim/lua/plugins.lua")
        .is_symlink());
    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
}

#[test]
fn relink_dry_run_shows_the_removals_and_the_links_and_changes_nothing() {
    let sandbox = linked_nvim();
    sandbox.package_file("nvim", ".newfile", "");

    sandbox
        .command()
        .args(["relink", "nvim", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("unlink"))
        .stdout(predicate::str::contains("link    "));

    assert!(!sandbox.home().join(".newfile").exists());
}

#[test]
fn relink_never_installs_or_runs_hooks() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "hook = \"build.sh\"\n[packages]\nxbps = [\"gcc\"]");
    let marker = sandbox.home().join("hook-ran");
    sandbox.package_script("dwm", "build.sh", &format!("touch {}", marker.display()));
    sandbox.fake_xbps(&[]);
    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();
    fs::remove_file(&marker).unwrap();
    let calls_before = sandbox.calls().len();

    sandbox.command().args(["relink", "dwm"]).assert().success();

    assert!(!marker.exists(), "relink must not run hooks");
    assert_eq!(
        sandbox.calls().len(),
        calls_before,
        "relink must not install"
    );
}

#[test]
fn relink_does_not_pull_in_dependencies() {
    let sandbox = Sandbox::new();
    sandbox.package_file("base", ".baserc", "");
    sandbox.package_file("app", ".apprc", "");
    sandbox.manifest("app", "depends = [\"base\"]");

    sandbox.command().args(["relink", "app"]).assert().success();

    assert!(sandbox.home().join(".apprc").is_symlink());
    assert!(
        !sandbox.home().join(".baserc").exists(),
        "dependencies are not linked by relink"
    );
}

#[test]
fn relink_reports_a_conflict_with_a_real_file_exits_two_and_keeps_the_file() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.home_file(".zshrc", "mine");

    sandbox
        .command()
        .args(["relink", "zsh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("conflict"));

    assert_eq!(
        fs::read_to_string(sandbox.home().join(".zshrc")).unwrap(),
        "mine"
    );
}

#[test]
fn relink_with_prune_removes_and_recreates_the_directories() {
    let sandbox = linked_nvim();

    sandbox
        .command()
        .args(["relink", "nvim", "--prune"])
        .assert()
        .success();

    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
}

#[test]
fn relink_with_no_packages_and_no_host_refuses() {
    let sandbox = linked_nvim();

    sandbox.command().arg("relink").assert().code(2);

    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
}

/// A package targeting a read-only directory, standing in for `/etc`: removing
/// or creating links there as the plain user fails, so only going through the
/// (fake) `sudo`, which runs as the plain user but is what puddlejump asked
/// for, is observable. The fake `rm`/`ln`/`mkdir` ignore the mode so the
/// sudo path can complete.
fn root_sandbox() -> (Sandbox, std::path::PathBuf) {
    let sandbox = Sandbox::new();
    let etc = sandbox.home().join("fake-etc");
    fs::create_dir(&etc).unwrap();
    sandbox.package_file("sysctl", "99.conf", "x");
    sandbox.manifest(
        "sysctl",
        &format!("needs_root = true\ntarget = \"{}\"", etc.display()),
    );
    sandbox.fake_sudo();
    for tool in ["ln", "rm", "mkdir", "rmdir"] {
        sandbox.fake_tool(
            tool,
            &format!(
                "echo \"{tool} $*\" >> {}\n/bin/{tool} \"$@\"",
                sandbox.calls_log().display()
            ),
        );
    }
    (sandbox, etc)
}

#[test]
fn unlink_of_a_package_that_needs_root_goes_through_sudo() {
    let (sandbox, etc) = root_sandbox();
    sandbox
        .command()
        .args(["link", "sysctl", "--yes"])
        .assert()
        .success();
    assert!(etc.join("99.conf").is_symlink());

    sandbox
        .command()
        .args(["unlink", "sysctl"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[done]"));

    assert!(!etc.join("99.conf").exists());
    let calls = sandbox.calls();
    assert!(
        calls.iter().any(|call| call.starts_with("sudo rm -- ")),
        "the removal must go through sudo: {calls:?}"
    );
}

#[test]
fn relink_of_a_package_that_needs_root_goes_through_sudo() {
    let (sandbox, etc) = root_sandbox();
    sandbox
        .command()
        .args(["link", "sysctl", "--yes"])
        .assert()
        .success();

    sandbox
        .command()
        .args(["relink", "sysctl"])
        .assert()
        .success();

    assert!(etc.join("99.conf").is_symlink());
    let calls = sandbox.calls();
    assert!(
        calls.iter().any(|call| call.starts_with("sudo rm -- ")),
        "{calls:?}"
    );
    let links = calls
        .iter()
        .filter(|call| call.starts_with("sudo ln -s -- "))
        .count();
    assert_eq!(
        links, 2,
        "one link from `link`, one from `relink`: {calls:?}"
    );
}

#[test]
fn unlink_of_a_package_that_needs_root_without_sudo_is_refused_and_changes_nothing() {
    let (sandbox, etc) = root_sandbox();
    sandbox
        .command()
        .args(["link", "sysctl", "--yes"])
        .assert()
        .success();
    // Remove sudo from the sandbox so root cannot be obtained.
    fs::remove_file(sandbox.bin_path().join("sudo")).unwrap();

    sandbox
        .command()
        .args(["unlink", "sysctl"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("root is required"))
        .stderr(predicate::str::contains(
            "package sysctl declares needs_root",
        ));

    assert!(etc.join("99.conf").is_symlink(), "nothing was removed");
}

#[test]
fn relink_with_a_conflict_in_the_link_half_leaves_the_existing_links_alone() {
    let sandbox = linked_nvim();
    // A file appears in the package that collides with a real file in the home.
    sandbox.package_file("nvim", ".config/nvim/extra.lua", "");
    sandbox.home_file(".config/nvim/extra.lua", "a real file of mine");

    sandbox
        .command()
        .args(["relink", "nvim"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("conflict"));

    assert!(
        sandbox.home().join(".config/nvim/init.lua").is_symlink(),
        "the existing link was not removed by a refused relink"
    );
    assert_eq!(
        fs::read_to_string(sandbox.home().join(".config/nvim/extra.lua")).unwrap(),
        "a real file of mine"
    );
}
