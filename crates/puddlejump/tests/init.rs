mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::Sandbox;
use predicates::prelude::*;

/// Where a new repo goes: a path that does not exist yet.
fn new_repo(sandbox: &Sandbox) -> PathBuf {
    sandbox.home().join("dotfiles")
}

/// The host profile files created under `repo/hosts`.
fn host_profiles(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(repo.join("hosts"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn init_creates_the_repo_directories_config_and_a_host_profile() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("[done]"));

    assert!(repo.join("puddlejump.toml").is_file());
    assert!(repo.join("hosts").is_dir());
    assert!(repo.join("packages").is_dir());
    let profiles = host_profiles(&repo);
    assert_eq!(
        profiles.len(),
        1,
        "one profile for this machine: {profiles:?}"
    );
    assert!(profiles[0].ends_with(".toml"));
}

#[test]
fn init_requires_a_path() {
    let sandbox = Sandbox::new();

    sandbox
        .command_without_repo()
        .arg("init")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("<PATH>"));

    // Nothing is created in the current directory, which is the sandbox root.
    assert!(!sandbox.home().join("packages").exists());
}

#[test]
fn init_takes_exactly_one_path() {
    let sandbox = Sandbox::new();

    sandbox
        .command_without_repo()
        .args(["init", "a", "b"])
        .assert()
        .code(2);
}

#[test]
fn init_creates_missing_parent_directories() {
    let sandbox = Sandbox::new();
    let repo = sandbox.home().join("a/b/c/dotfiles");

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    assert!(repo.join("packages").is_dir());
}

#[test]
fn init_accepts_a_relative_path() {
    let sandbox = Sandbox::new();

    sandbox
        .command_without_repo()
        .args(["init", "relative-repo"])
        .assert()
        .success();

    // The sandbox runs commands from its own root.
    let repo = sandbox.home().parent().unwrap().join("relative-repo");
    assert!(repo.join("hosts").is_dir());
}

#[test]
fn init_dry_run_prints_the_plan_and_creates_nothing() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("create"))
        .stdout(predicate::str::contains("puddlejump.toml"))
        .stdout(predicate::str::contains("[done]").not());

    assert!(!repo.exists());
}

#[test]
fn init_ignores_the_repo_flag() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);

    sandbox
        .command_without_repo()
        .args(["--repo", "/does/not/exist", "init"])
        .arg(&repo)
        .assert()
        .success();

    assert!(repo.join("packages").is_dir());
}

#[test]
fn init_does_not_need_an_existing_repo_or_environment_repo() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);

    sandbox
        .command_without_repo()
        .env("PUDDLEJUMP_REPO", "/does/not/exist")
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    assert!(repo.join("hosts").is_dir());
}

#[test]
fn the_generated_files_are_the_documented_starters() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    let config = fs::read_to_string(repo.join("puddlejump.toml")).unwrap();
    let profile = fs::read_to_string(repo.join("hosts").join(&host_profiles(&repo)[0])).unwrap();

    assert_eq!(config, puddlejump_core::init::STARTER_CONFIG);
    assert_eq!(profile, puddlejump_core::init::STARTER_HOST_PROFILE);
}

#[test]
fn a_repo_made_by_init_works_with_list_status_and_link() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();
    fs::create_dir_all(repo.join("packages/zsh")).unwrap();
    fs::write(repo.join("packages/zsh/.zshrc"), "export A=1").unwrap();

    sandbox
        .command_without_repo()
        .arg("--repo")
        .arg(&repo)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh"));
    sandbox
        .command_without_repo()
        .arg("--repo")
        .arg(&repo)
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh: not linked"));
    sandbox
        .command_without_repo()
        .arg("--repo")
        .arg(&repo)
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn running_init_again_changes_nothing_and_reports_everything_as_existing() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();
    fs::write(repo.join("puddlejump.toml"), "package_manager = \"xbps\"\n").unwrap();

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("exists"))
        .stdout(predicate::str::contains("[done]").not());

    assert_eq!(
        fs::read_to_string(repo.join("puddlejump.toml")).unwrap(),
        "package_manager = \"xbps\"\n",
        "an edited config is never overwritten"
    );
}

#[test]
fn init_in_an_existing_git_style_directory_leaves_its_files_alone() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join("README.md"), "my readme").unwrap();

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(repo.join("README.md")).unwrap(),
        "my readme"
    );
    assert!(repo.join(".git").is_dir());
    assert!(repo.join("packages").is_dir());
}

#[test]
fn init_never_overwrites_an_existing_host_profile() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();
    let profile = repo.join("hosts").join(&host_profiles(&repo)[0]);
    fs::write(&profile, "packages = [\"zsh\"]\n").unwrap();

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&profile).unwrap(),
        "packages = [\"zsh\"]\n"
    );
}

#[test]
fn init_at_a_path_that_is_a_file_is_refused_and_leaves_the_file() {
    let sandbox = Sandbox::new();
    let file = sandbox.home().join("dotfiles");
    fs::write(&file, "i am a file").unwrap();

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&file)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("1 path(s) are in the way"))
        .stderr(predicate::str::contains(file.display().to_string()))
        .stderr(predicate::str::contains("nothing was created"));

    assert_eq!(fs::read_to_string(&file).unwrap(), "i am a file");
}

#[test]
fn init_with_a_file_where_a_directory_is_needed_creates_nothing() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    fs::create_dir(&repo).unwrap();
    fs::write(repo.join("packages"), "i am a file").unwrap();

    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("packages"))
        .stderr(predicate::str::contains("nothing was created"))
        // Only the problem is listed, not the steps that did not happen.
        .stderr(predicate::str::contains("create  ").not());

    assert!(!repo.join("puddlejump.toml").exists());
    assert!(!repo.join("hosts").exists());
    assert_eq!(
        fs::read_to_string(repo.join("packages")).unwrap(),
        "i am a file"
    );
}

#[test]
fn init_into_a_read_only_parent_fails_without_creating_anything() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let locked = sandbox.home().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let repo = locked.join("dotfiles");

    let assert = sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert();

    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert
        .code(1)
        .stdout(predicate::str::contains("FAILED"))
        .stdout(predicate::str::contains("Permission denied"));
    assert!(!repo.exists());
}

#[test]
fn init_with_json_prints_one_parseable_report() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);

    let output = sandbox
        .command_without_repo()
        .arg("--json")
        .arg("init")
        .arg(&repo)
        .output()
        .unwrap();

    assert!(output.status.success());
    let outcomes: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcomes.as_array().unwrap().len(), 5);
    assert_eq!(outcomes[0]["result"], "Created");
}

#[test]
fn init_dry_run_with_json_prints_one_parseable_plan() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);

    let output = sandbox
        .command_without_repo()
        .args(["--json", "init", "--dry-run"])
        .arg(&repo)
        .output()
        .unwrap();

    assert!(output.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["root"], repo.display().to_string());
    assert_eq!(plan["steps"].as_array().unwrap().len(), 5);
    assert!(!repo.exists());
}

#[test]
fn the_host_profile_init_creates_is_selected_automatically_for_this_machine() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();
    let profile = host_profiles(&repo).remove(0);
    let name = profile.strip_suffix(".toml").unwrap().to_string();

    // No --host and no PUDDLEJUMP_HOST: only the machine's own hostname can
    // pick it, so this holds whatever the hostname is and however it was
    // converted to a filename.
    sandbox
        .command_without_repo()
        .arg("--repo")
        .arg(&repo)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}  (selected)")));
}

#[test]
fn the_host_profile_name_is_a_safe_filename() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    for profile in host_profiles(&repo) {
        let name = profile.strip_suffix(".toml").unwrap();
        assert!(!name.is_empty());
        assert!(!name.starts_with('.') && !name.starts_with('-'), "{name}");
        assert!(
            !name.contains('/') && !name.chars().any(char::is_whitespace),
            "{name}"
        );
    }
    assert!(repo
        .join("hosts")
        .read_dir()
        .unwrap()
        .all(|entry| entry.unwrap().path().starts_with(repo.join("hosts"))));
}

#[test]
fn the_starter_files_mention_every_key_so_they_can_be_edited_without_the_docs() {
    let sandbox = Sandbox::new();
    let repo = new_repo(&sandbox);
    sandbox
        .command_without_repo()
        .arg("init")
        .arg(&repo)
        .assert()
        .success();

    let config = fs::read_to_string(repo.join("puddlejump.toml")).unwrap();
    let profile = fs::read_to_string(repo.join("hosts").join(&host_profiles(&repo)[0])).unwrap();

    for key in puddlejump_core::config::CONFIG_KEYS {
        assert!(config.contains(key), "puddlejump.toml never mentions {key}");
    }
    for key in puddlejump_core::host::PROFILE_KEYS {
        assert!(
            profile.contains(key),
            "the host profile never mentions {key}"
        );
    }
}
