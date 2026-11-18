mod common;

use std::fs;

use common::{link_target, Sandbox};
use predicates::prelude::*;

/// Where the state file lives in a sandbox that sets no `XDG_STATE_HOME`.
fn state_file(sandbox: &Sandbox) -> std::path::PathBuf {
    sandbox.home().join(".local/state/puddlejump/state.toml")
}

fn state_text(sandbox: &Sandbox) -> String {
    fs::read_to_string(state_file(sandbox)).unwrap_or_default()
}

fn link(sandbox: &Sandbox, package: &str) {
    sandbox
        .command()
        .args(["link", package, "--yes"])
        .assert()
        .success();
}

/// Seconds in a day, for aging a remembered time by a whole number of days.
const DAY: u64 = 24 * 60 * 60;

/// Rewrites every `key = <seconds>` line in the state file to be `days_ago`
/// days before now, so the age shown by `status` is known without a clock.
///
/// Rounded up by an hour, so the run's own few seconds can never tip it into
/// the next unit down.
fn age_state(sandbox: &Sandbox, key: &str, days_ago: u64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let then = now - days_ago * DAY - 60 * 60;
    let aged: Vec<String> = state_text(sandbox)
        .lines()
        .map(|line| {
            if line.trim_start().starts_with(&format!("{key} = ")) {
                format!("  {key} = {then}")
            } else {
                line.to_string()
            }
        })
        .collect();
    fs::write(state_file(sandbox), aged.join("\n") + "\n").unwrap();
}

/// A sandbox with `zsh` (two files) linked.
fn linked_zsh() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "export A=1");
    sandbox.package_file("zsh", ".zshenv", "export B=2");
    link(&sandbox, "zsh");
    sandbox
}

#[test]
fn link_remembers_each_link_it_creates() {
    let sandbox = linked_zsh();

    let text = state_text(&sandbox);

    assert!(text.contains("[[repo.link]]"), "{text}");
    assert!(text.contains("package = \"zsh\""), "{text}");
    assert!(
        text.contains(&sandbox.home().join(".zshrc").display().to_string()),
        "{text}"
    );
    assert!(
        text.contains(&sandbox.home().join(".zshenv").display().to_string()),
        "{text}"
    );
}

#[test]
fn link_does_not_remember_a_link_that_was_already_in_place() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    std::os::unix::fs::symlink(
        sandbox.repo().join("packages/zsh/.zshrc"),
        sandbox.home().join(".zshrc"),
    )
    .unwrap();

    link(&sandbox, "zsh");

    assert!(!state_file(&sandbox).exists());
}

#[test]
fn link_dry_run_remembers_nothing() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");

    sandbox
        .command()
        .args(["link", "zsh", "--dry-run"])
        .assert()
        .success();

    assert!(!state_file(&sandbox).exists());
}

#[test]
fn xdg_state_home_moves_the_remembered_links() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    let xdg = sandbox.home().join("xdg-state");
    fs::create_dir_all(&xdg).unwrap();

    sandbox
        .command()
        .env("XDG_STATE_HOME", &xdg)
        .args(["link", "zsh", "--yes"])
        .assert()
        .success();

    let text = fs::read_to_string(xdg.join("puddlejump/state.toml")).unwrap();
    assert!(text.contains("[[repo.link]]"), "{text}");
    assert!(!state_file(&sandbox).exists());
}

#[test]
fn unlink_forgets_the_links_it_removes() {
    let sandbox = linked_zsh();

    sandbox.command().args(["unlink", "zsh"]).assert().success();

    assert!(!state_text(&sandbox).contains("[[repo.link]]"));
}

#[test]
fn unlink_dry_run_keeps_the_remembered_links() {
    let sandbox = linked_zsh();
    let before = state_text(&sandbox);

    sandbox
        .command()
        .args(["unlink", "zsh", "--dry-run"])
        .assert()
        .success();

    assert_eq!(state_text(&sandbox), before);
}

#[test]
fn relink_leaves_the_links_remembered() {
    let sandbox = linked_zsh();

    sandbox.command().args(["relink", "zsh"]).assert().success();

    let text = state_text(&sandbox);
    assert!(text.contains(&sandbox.home().join(".zshrc").display().to_string()));
    assert!(text.contains(&sandbox.home().join(".zshenv").display().to_string()));
}

#[test]
fn adopt_remembers_the_link_it_creates() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.repo().join("packages/zsh")).unwrap();
    let file = sandbox.home_file(".zshrc", "export A=1");

    sandbox
        .command()
        .args(["adopt", "zsh"])
        .arg(&file)
        .assert()
        .success();

    let text = state_text(&sandbox);
    assert!(text.contains("package = \"zsh\""), "{text}");
    assert!(text.contains(&file.display().to_string()), "{text}");
}

#[test]
fn status_reports_a_link_whose_file_was_removed_from_the_package() {
    let sandbox = linked_zsh();
    fs::remove_file(sandbox.repo().join("packages/zsh/.zshenv")).unwrap();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".zshenv"))
        .stdout(predicate::str::contains(
            "no longer provided by the package",
        ));
}

#[test]
fn link_remembers_when_it_made_each_link() {
    let sandbox = linked_zsh();

    let text = state_text(&sandbox);

    assert!(text.contains("linked_at = "), "{text}");
    assert!(!text.contains("linked_at = 0"), "{text}");
}

#[test]
fn status_says_how_long_ago_a_stale_link_was_made() {
    let sandbox = linked_zsh();
    fs::remove_file(sandbox.repo().join("packages/zsh/.zshenv")).unwrap();
    age_state(&sandbox, "linked_at", 3);

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "no longer provided by the package (linked 3 days ago)",
        ));
}

#[test]
fn status_says_how_long_ago_a_gone_packages_link_was_made() {
    let sandbox = linked_zsh();
    fs::remove_dir_all(sandbox.repo().join("packages/zsh")).unwrap();
    sandbox.package_file("vim", ".vimrc", "");
    age_state(&sandbox, "linked_at", 10);

    sandbox
        .command()
        .args(["status", "vim"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "still linked (linked 10 days ago)",
        ));
}

#[test]
fn status_json_carries_the_time_a_stale_link_was_made_in_seconds() {
    let sandbox = linked_zsh();
    fs::remove_file(sandbox.repo().join("packages/zsh/.zshenv")).unwrap();

    let output = sandbox
        .command()
        .args(["--json", "status", "zsh"])
        .output()
        .unwrap();

    let statuses: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(statuses[0]["stale"][0]["linked_at"].as_u64().unwrap() > 0);
}

#[test]
fn status_leaves_out_the_time_of_a_link_recorded_before_times_were_kept() {
    let sandbox = linked_zsh();
    fs::remove_file(sandbox.repo().join("packages/zsh/.zshenv")).unwrap();
    let without_times: String = state_text(&sandbox)
        .lines()
        .filter(|line| !line.trim_start().starts_with("linked_at"))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(state_file(&sandbox), without_times).unwrap();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "no longer provided by the package\n",
        ));
}

#[test]
fn status_does_not_show_a_time_for_a_healthy_link() {
    let sandbox = linked_zsh();
    age_state(&sandbox, "linked_at", 30);

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ago").not());
}

#[test]
fn status_is_quiet_when_every_remembered_link_is_still_wanted() {
    let sandbox = linked_zsh();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no longer").not());
}

#[test]
fn status_json_lists_stale_links_on_the_package() {
    let sandbox = linked_zsh();
    fs::remove_file(sandbox.repo().join("packages/zsh/.zshenv")).unwrap();

    let output = sandbox
        .command()
        .args(["--json", "status", "zsh"])
        .output()
        .unwrap();

    let statuses: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let stale = statuses[0]["stale"].as_array().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(
        stale[0]["link"],
        sandbox.home().join(".zshenv").display().to_string()
    );
}

#[test]
fn status_reports_links_of_a_package_that_is_gone_from_the_repo() {
    let sandbox = linked_zsh();
    fs::remove_dir_all(sandbox.repo().join("packages/zsh")).unwrap();
    sandbox.package_file("vim", ".vimrc", "");

    sandbox
        .command()
        .args(["status", "vim"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zsh: no longer in the repo"))
        .stdout(predicate::str::contains("puddlejump unlink zsh"));
}

#[test]
fn status_does_not_claim_a_link_the_user_has_since_replaced() {
    let sandbox = linked_zsh();
    fs::remove_file(sandbox.repo().join("packages/zsh/.zshenv")).unwrap();
    let link = sandbox.home().join(".zshenv");
    fs::remove_file(&link).unwrap();
    fs::write(&link, "mine").unwrap();

    sandbox
        .command()
        .args(["status", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no longer provided").not());
}

#[test]
fn unlink_removes_a_stale_link_of_a_package_that_still_exists() {
    let sandbox = linked_zsh();
    fs::remove_file(sandbox.repo().join("packages/zsh/.zshenv")).unwrap();

    sandbox.command().args(["unlink", "zsh"]).assert().success();

    assert!(!sandbox.home().join(".zshenv").exists());
    assert!(!sandbox.home().join(".zshrc").exists());
    assert!(!state_text(&sandbox).contains("[[repo.link]]"));
}

#[test]
fn unlink_removes_the_links_of_a_package_that_is_gone_from_the_repo() {
    let sandbox = linked_zsh();
    fs::remove_dir_all(sandbox.repo().join("packages/zsh")).unwrap();

    sandbox
        .command()
        .args(["unlink", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[done]"));

    assert!(link_target(&sandbox.home().join(".zshrc")).is_none());
    assert!(link_target(&sandbox.home().join(".zshenv")).is_none());
    assert!(!state_text(&sandbox).contains("[[repo.link]]"));
}

#[test]
fn unlink_of_a_gone_package_leaves_a_link_the_user_replaced() {
    let sandbox = linked_zsh();
    fs::remove_dir_all(sandbox.repo().join("packages/zsh")).unwrap();
    let replaced = sandbox.home().join(".zshenv");
    fs::remove_file(&replaced).unwrap();
    fs::write(&replaced, "mine").unwrap();

    sandbox.command().args(["unlink", "zsh"]).assert().success();

    assert_eq!(fs::read_to_string(&replaced).unwrap(), "mine");
    assert!(link_target(&sandbox.home().join(".zshrc")).is_none());
}

#[test]
fn unlink_of_a_gone_package_dry_run_changes_nothing() {
    let sandbox = linked_zsh();
    fs::remove_dir_all(sandbox.repo().join("packages/zsh")).unwrap();
    let before = state_text(&sandbox);

    sandbox
        .command()
        .args(["unlink", "zsh", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".zshrc"));

    assert!(sandbox.home().join(".zshrc").is_symlink());
    assert_eq!(state_text(&sandbox), before);
}

#[test]
fn unlink_of_a_package_that_never_existed_is_still_an_error() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");

    sandbox
        .command()
        .args(["unlink", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("package nope does not exist"));
}

#[test]
fn a_corrupt_state_file_refuses_unlink_before_removing_anything() {
    let sandbox = linked_zsh();
    fs::write(state_file(&sandbox), "not [valid").unwrap();

    sandbox
        .command()
        .args(["unlink", "zsh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid state file"));

    assert!(sandbox.home().join(".zshrc").is_symlink());
}

#[test]
fn unlink_of_a_package_in_the_repo_works_without_a_state_file() {
    let sandbox = linked_zsh();
    fs::remove_file(state_file(&sandbox)).unwrap();

    sandbox.command().args(["unlink", "zsh"]).assert().success();

    assert!(link_target(&sandbox.home().join(".zshrc")).is_none());
}

#[test]
fn deleting_the_state_file_loses_the_ability_to_unlink_a_gone_package() {
    let sandbox = linked_zsh();
    fs::remove_dir_all(sandbox.repo().join("packages/zsh")).unwrap();
    fs::remove_file(state_file(&sandbox)).unwrap();

    sandbox
        .command()
        .args(["unlink", "zsh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("package zsh does not exist"));

    assert!(sandbox.home().join(".zshrc").is_symlink());
}
