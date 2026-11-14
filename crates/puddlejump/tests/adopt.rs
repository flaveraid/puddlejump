mod common;

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use common::{link_target, Sandbox};
use predicates::prelude::*;

fn zsh_sandbox() -> Sandbox {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.repo().join("packages/zsh")).unwrap();
    sandbox
}

#[test]
fn adopt_moves_the_file_into_the_package_and_links_it_back() {
    let sandbox = zsh_sandbox();
    let file = sandbox.home_file(".zshrc", "export A=1");

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("move"))
        .stdout(predicate::str::contains("[done]"));

    let in_package = sandbox.repo().join("packages/zsh/.zshrc");
    assert_eq!(fs::read_to_string(&in_package).unwrap(), "export A=1");
    assert_eq!(link_target(&file), Some(in_package));
    assert_eq!(fs::read_to_string(&file).unwrap(), "export A=1");
}

#[test]
fn adopt_keeps_the_relative_path_for_nested_files() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.repo().join("packages/nvim")).unwrap();
    let file = sandbox.home_file(".config/nvim/init.lua", "x");

    sandbox
        .command()
        .args(["adopt", "nvim"])
        .arg(&file)
        .assert()
        .success();

    assert!(sandbox
        .repo()
        .join("packages/nvim/.config/nvim/init.lua")
        .is_file());
    assert!(file.is_symlink());
}

#[test]
fn adopt_expands_a_directory_to_its_files() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.repo().join("packages/nvim")).unwrap();
    sandbox.home_file(".config/nvim/init.lua", "a");
    sandbox.home_file(".config/nvim/lua/b.lua", "b");

    sandbox
        .command()
        .args(["adopt", "nvim"])
        .arg(sandbox.home().join(".config/nvim"))
        .assert()
        .success();

    assert!(sandbox.home().join(".config/nvim/init.lua").is_symlink());
    assert!(sandbox.home().join(".config/nvim/lua/b.lua").is_symlink());
    assert!(sandbox
        .repo()
        .join("packages/nvim/.config/nvim/lua/b.lua")
        .is_file());
}

#[test]
fn adopt_preserves_the_file_mode() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.repo().join("packages/ssh")).unwrap();
    let file = sandbox.home_file(".ssh/config", "Host *");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

    sandbox
        .command()
        .args(["adopt", "ssh"])
        .arg(&file)
        .assert()
        .success();

    let moved = sandbox.repo().join("packages/ssh/.ssh/config");
    assert_eq!(
        fs::metadata(moved).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn adopt_dry_run_changes_nothing() {
    let sandbox = zsh_sandbox();
    let file = sandbox.home_file(".zshrc", "mine");

    sandbox
        .command()
        .args(["adopt", "zsh", "--dry-run"])
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("move"));

    assert!(!file.is_symlink());
    assert_eq!(fs::read_to_string(&file).unwrap(), "mine");
    assert!(!sandbox.repo().join("packages/zsh/.zshrc").exists());
}

#[test]
fn adopt_refuses_to_overwrite_a_file_already_in_the_package() {
    let sandbox = zsh_sandbox();
    sandbox.package_file("zsh", ".zshrc", "already in the repo");
    let file = sandbox.home_file(".zshrc", "mine");

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(&file)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("REFUSED"))
        .stdout(predicate::str::contains("the package already has"));

    assert_eq!(fs::read_to_string(&file).unwrap(), "mine");
    assert_eq!(
        fs::read_to_string(sandbox.repo().join("packages/zsh/.zshrc")).unwrap(),
        "already in the repo"
    );
}

#[test]
fn adopt_refuses_a_symlink() {
    let sandbox = zsh_sandbox();
    let real = sandbox.home().join("real");
    fs::write(&real, "").unwrap();
    let link = sandbox.home().join(".zshrc");
    symlink(&real, &link).unwrap();

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(&link)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("is a symlink"));

    assert_eq!(link_target(&link), Some(real));
}

#[test]
fn adopt_refuses_a_path_that_does_not_exist() {
    let sandbox = zsh_sandbox();

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(sandbox.home().join(".nothing"))
        .assert()
        .code(2)
        .stdout(predicate::str::contains("does not exist"));
}

#[test]
fn adopt_refuses_a_path_outside_the_target() {
    let sandbox = zsh_sandbox();
    let outside = sandbox.repo().join("README.md");
    fs::write(&outside, "").unwrap();

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(&outside)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("not inside the package's target"));

    assert!(outside.is_file());
}

#[test]
fn adopt_refuses_a_path_that_climbs_out_of_the_target() {
    let sandbox = zsh_sandbox();
    let outside = sandbox.repo().join("secret");
    fs::write(&outside, "").unwrap();
    let sneaky = sandbox.home().join("../repo/secret");

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(&sneaky)
        .assert()
        .code(2);

    assert!(outside.is_file());
}

#[test]
fn adopt_moves_what_it_can_and_exits_zero_even_if_some_paths_were_refused() {
    let sandbox = zsh_sandbox();
    let good = sandbox.home_file(".zshrc", "x");

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(sandbox.home().join(".nothing"))
        .arg(&good)
        .assert()
        .success()
        .stdout(predicate::str::contains("REFUSED"));

    assert!(good.is_symlink());
}

#[test]
fn adopt_into_an_unknown_package_exits_two() {
    let sandbox = zsh_sandbox();
    let file = sandbox.home_file(".zshrc", "mine");

    sandbox
        .command()
        .args(["adopt", "ghost"])
        .arg(&file)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("package ghost does not exist"));

    assert_eq!(fs::read_to_string(&file).unwrap(), "mine");
}

#[test]
fn adopt_needs_a_package_and_a_path() {
    let sandbox = zsh_sandbox();

    sandbox.command().arg("adopt").assert().code(2);
    sandbox.command().args(["adopt", "zsh"]).assert().code(2);
}

#[test]
fn adopted_files_are_reported_as_linked_by_status() {
    let sandbox = zsh_sandbox();
    let file = sandbox.home_file(".zshrc", "x");
    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(&file)
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
fn adopt_with_json_prints_one_parseable_document() {
    let sandbox = zsh_sandbox();
    let file = sandbox.home_file(".zshrc", "x");

    let output = sandbox
        .command()
        .args(["--json", "adopt", "zsh"])
        .arg(&file)
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["moved"][0]["result"], "Moved");
    assert_eq!(result["linked"][0]["actions"][0]["result"], "Done");
}

#[test]
fn adopt_into_a_target_override_uses_that_directory() {
    let sandbox = zsh_sandbox();
    let stage = sandbox.home().join("stage");
    fs::create_dir(&stage).unwrap();
    let file = stage.join(".zshrc");
    fs::write(&file, "staged").unwrap();

    sandbox
        .command()
        .arg("--target")
        .arg(&stage)
        .args(["adopt", "zsh"])
        .arg(&file)
        .assert()
        .success();

    assert!(sandbox.repo().join("packages/zsh/.zshrc").is_file());
    assert!(file.is_symlink());
}

#[test]
fn adopt_into_a_package_that_needs_root_is_refused_and_moves_nothing() {
    let sandbox = Sandbox::new();
    let etc = sandbox.home().join("fake-etc");
    fs::create_dir(&etc).unwrap();
    fs::create_dir_all(sandbox.repo().join("packages/sysctl")).unwrap();
    sandbox.manifest(
        "sysctl",
        &format!("needs_root = true\ntarget = \"{}\"", etc.display()),
    );
    sandbox.fake_sudo();
    let file = etc.join("99.conf");
    fs::write(&file, "mine").unwrap();

    sandbox
        .command()
        .args(["adopt", "sysctl"])
        .arg(&file)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("needs root"))
        .stderr(predicate::str::contains("not supported"));

    assert_eq!(fs::read_to_string(&file).unwrap(), "mine");
    assert!(!file.is_symlink());
    assert!(!sandbox.repo().join("packages/sysctl/99.conf").exists());
}
