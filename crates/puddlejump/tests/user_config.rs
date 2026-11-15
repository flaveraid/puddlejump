mod common;

use std::fs;

use common::Sandbox;
use predicates::prelude::*;

/// A sandbox whose repo has a `zsh` package and a `workstation` host profile
/// that lists it, so linking with no arguments proves which host was chosen.
fn host_sandbox() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.package_file("nvim", ".nvimrc", "");
    sandbox.repo_file("hosts/workstation.toml", "packages = [\"zsh\"]");
    sandbox
}

#[test]
fn the_config_repo_is_used_when_no_repo_is_given() {
    let sandbox = host_sandbox();
    sandbox.user_config(&format!("repo = {:?}", sandbox.repo()));

    sandbox
        .command_without_repo()
        .args(["link", "zsh"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn a_tilde_repo_in_the_config_means_the_home_directory() {
    let sandbox = Sandbox::new();
    sandbox.home_file("dotfiles/packages/zsh/.zshrc", "");
    sandbox.user_config("repo = \"~/dotfiles\"");

    sandbox
        .command_without_repo()
        .args(["link", "zsh"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn the_repo_flag_beats_the_config_repo() {
    let sandbox = host_sandbox();
    sandbox.user_config("repo = \"/does/not/exist\"");

    sandbox.command().args(["link", "zsh"]).assert().success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn the_repo_environment_variable_beats_the_config_repo() {
    let sandbox = host_sandbox();
    sandbox.user_config("repo = \"/does/not/exist\"");

    sandbox
        .command_without_repo()
        .env("PUDDLEJUMP_REPO", sandbox.repo())
        .args(["link", "zsh"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn the_config_host_is_used_when_no_host_is_given() {
    let sandbox = host_sandbox();
    sandbox.user_config("host = \"workstation\"");

    sandbox.command().arg("link").assert().success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
    assert!(!sandbox.home().join(".nvimrc").exists());
}

#[test]
fn the_host_flag_beats_the_config_host() {
    let sandbox = host_sandbox();
    sandbox.repo_file("hosts/other.toml", "packages = [\"nvim\"]");
    sandbox.user_config("host = \"workstation\"");

    sandbox
        .command()
        .args(["--host", "other", "link"])
        .assert()
        .success();

    assert!(sandbox.home().join(".nvimrc").is_symlink());
    assert!(!sandbox.home().join(".zshrc").exists());
}

#[test]
fn the_host_environment_variable_beats_the_config_host() {
    let sandbox = host_sandbox();
    sandbox.repo_file("hosts/other.toml", "packages = [\"nvim\"]");
    sandbox.user_config("host = \"workstation\"");

    sandbox
        .command()
        .env("PUDDLEJUMP_HOST", "other")
        .arg("link")
        .assert()
        .success();

    assert!(sandbox.home().join(".nvimrc").is_symlink());
    assert!(!sandbox.home().join(".zshrc").exists());
}

#[test]
fn a_config_host_with_no_profile_exits_two() {
    let sandbox = host_sandbox();
    sandbox.user_config("host = \"ghost\"");

    sandbox
        .command()
        .args(["link", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no host profile named ghost"));
}

#[test]
fn xdg_config_home_moves_the_config() {
    let sandbox = host_sandbox();
    let xdg = sandbox.home().join("xdg");
    fs::create_dir_all(xdg.join("puddlejump")).unwrap();
    fs::write(xdg.join("puddlejump/config.toml"), "host = \"workstation\"").unwrap();

    sandbox
        .command()
        .env("XDG_CONFIG_HOME", &xdg)
        .arg("link")
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
    assert!(!sandbox.home().join(".nvimrc").exists());
}

#[test]
fn the_default_config_is_ignored_when_xdg_config_home_is_set() {
    let sandbox = host_sandbox();
    sandbox.user_config("host = \"ghost\"");
    let xdg = sandbox.home().join("xdg");
    fs::create_dir_all(&xdg).unwrap();

    sandbox
        .command()
        .env("XDG_CONFIG_HOME", &xdg)
        .args(["link", "zsh"])
        .assert()
        .success();
}

#[test]
fn an_invalid_config_exits_two_naming_the_file() {
    let sandbox = host_sandbox();
    sandbox.user_config("colour = \"red\"");

    sandbox
        .command()
        .arg("list")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("config.toml"));
}

#[test]
fn a_relative_config_repo_exits_two() {
    let sandbox = host_sandbox();
    sandbox.user_config("repo = \"dotfiles\"");

    sandbox
        .command_without_repo()
        .arg("list")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("repo must be an absolute path"));
}

#[test]
fn init_ignores_the_user_config() {
    let sandbox = Sandbox::new();
    sandbox.user_config("colour = \"red\"");
    let repo = sandbox.home().join("new-repo");

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    assert!(repo.join("packages").is_dir());
}

#[test]
fn no_config_file_changes_nothing() {
    let sandbox = host_sandbox();

    sandbox.command().args(["link", "zsh"]).assert().success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}
