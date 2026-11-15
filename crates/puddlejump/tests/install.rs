mod common;

use std::fs;

use common::Sandbox;
use predicates::prelude::*;

/// A package that declares xbps packages.
fn dwm_sandbox(xbps: &[&str]) -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    let list: Vec<String> = xbps.iter().map(|name| format!("{name:?}")).collect();
    sandbox.manifest("dwm", &format!("[packages]\nxbps = [{}]", list.join(", ")));
    sandbox
}

#[test]
fn missing_packages_are_installed_in_one_batched_call_through_sudo() {
    let sandbox = dwm_sandbox(&["gcc", "make", "git"]);
    sandbox.fake_xbps(&["git"]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed: gcc make"));

    // The fake sudo logs itself and then runs the fake xbps-install, which
    // logs too. So one real install is one `sudo ...` line followed by one
    // `xbps-install ...` line, and the tool must have run exactly once.
    let calls = sandbox.calls();
    let tool_runs = calls
        .iter()
        .filter(|call| call.starts_with("xbps-install"))
        .count();
    assert_eq!(tool_runs, 1, "exactly one install call: {calls:?}");
    assert!(
        calls.contains(&"sudo xbps-install -y gcc make".to_string()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"xbps-install -y gcc make".to_string()),
        "{calls:?}"
    );
}

#[test]
fn each_package_is_checked_with_the_exact_xbps_query_command() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    assert!(sandbox
        .calls()
        .contains(&"xbps-query -p pkgver gcc".to_string()));
}

#[test]
fn nothing_is_installed_when_everything_is_already_present() {
    let sandbox = dwm_sandbox(&["gcc", "make"]);
    sandbox.fake_xbps(&["gcc", "make"]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    assert!(
        !sandbox
            .calls()
            .iter()
            .any(|call| call.contains("xbps-install")),
        "{:?}",
        sandbox.calls()
    );
    assert!(sandbox.home().join("config.h").is_symlink());
}

#[test]
fn packages_wanted_by_several_packages_are_installed_once() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.package_file("st", ".strc", "");
    sandbox.manifest("st", "[packages]\nxbps = [\"gcc\", \"harfbuzz\"]");
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "st", "--yes"])
        .assert()
        .success();

    assert!(sandbox
        .calls()
        .contains(&"sudo xbps-install -y gcc harfbuzz".to_string()));
}

#[test]
fn the_install_runs_as_the_invoking_user_through_sudo_not_as_root_directly() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    let calls = sandbox.calls();
    let sudo_at = calls
        .iter()
        .position(|c| c.starts_with("sudo xbps-install"));
    let query_at = calls.iter().position(|c| c.starts_with("xbps-query"));
    assert!(sudo_at.is_some());
    assert!(
        query_at.unwrap() < sudo_at.unwrap(),
        "queries run unprivileged, before the install: {calls:?}"
    );
    assert!(!calls.iter().any(|c| c.starts_with("sudo xbps-query")));
}

#[test]
fn files_are_linked_even_though_the_install_happens_after_them() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    assert!(sandbox.home().join("config.h").is_symlink());
}

#[test]
fn a_dry_run_queries_but_never_installs() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("install (xbps): gcc"))
        .stdout(predicate::str::contains("root required for"))
        .stdout(predicate::str::contains("installing system packages"));

    let calls = sandbox.calls();
    assert!(calls.iter().any(|c| c.starts_with("xbps-query")));
    assert!(
        !calls.iter().any(|c| c.contains("xbps-install")),
        "{calls:?}"
    );
    assert!(!sandbox.home().join("config.h").exists());
}

#[test]
fn no_install_skips_the_install_and_does_not_need_root() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.fake_tool("xbps-query", "exit 2");
    sandbox.fake_tool("xbps-install", "exit 0");
    // No sudo is available at all.

    sandbox
        .command()
        .args(["link", "dwm", "--no-install", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("install skipped: gcc"));

    assert!(sandbox.home().join("config.h").is_symlink());
}

#[test]
fn needing_root_with_no_sudo_refuses_up_front_and_links_nothing() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.fake_tool("xbps-query", "exit 2");
    sandbox.fake_tool("xbps-install", "exit 0");

    sandbox
        .command()
        .args(["link", "dwm", "zsh", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("root is required"))
        .stderr(predicate::str::contains("installing system packages"));

    assert!(!sandbox.home().join("config.h").exists());
    assert!(
        !sandbox.home().join(".zshrc").exists(),
        "even the package that needs no root was not linked"
    );
}

#[test]
fn a_failing_install_exits_one_keeps_the_links_and_skips_hooks() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.manifest("dwm", "hook = \"build.sh\"\n[packages]\nxbps = [\"gcc\"]");
    let marker = sandbox.home().join("hook-ran");
    sandbox.package_script("dwm", "build.sh", &format!("touch {}", marker.display()));
    sandbox.fake_tool("xbps-query", "exit 2");
    sandbox.fake_tool("xbps-install", "echo 'no space left on device' >&2\nexit 1");
    sandbox.fake_sudo();

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("install FAILED"))
        .stdout(predicate::str::contains("no space left on device"))
        .stderr(predicate::str::contains(
            "[install] no space left on device",
        ));

    assert!(sandbox.home().join("config.h").is_symlink(), "links stay");
    assert!(!marker.exists(), "hooks did not run");
}

#[test]
fn the_output_of_xbps_install_is_shown_as_it_runs() {
    let sandbox = dwm_sandbox(&["btop"]);
    sandbox.fake_tool("xbps-query", "exit 2");
    sandbox.fake_tool(
        "xbps-install",
        "echo '[*] Downloading btop'\necho 'warning: slow mirror' >&2",
    );
    sandbox.fake_sudo();

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[install] [*] Downloading btop"))
        .stderr(predicate::str::contains("[install] warning: slow mirror"))
        .stdout(predicate::str::contains("installed: btop"));
}

#[test]
fn xbps_install_output_comes_before_the_install_result_and_the_hooks() {
    let sandbox = dwm_sandbox(&["btop"]);
    sandbox.manifest("dwm", "hook = \"build.sh\"\n[packages]\nxbps = [\"btop\"]");
    sandbox.package_script("dwm", "build.sh", "echo hook ran");
    sandbox.fake_tool("xbps-query", "exit 2");
    sandbox.fake_tool("xbps-install", "echo installing btop");
    sandbox.fake_sudo();

    let output = sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let installing = stdout.find("installing btop").unwrap();
    let hook = stdout.find("hook ran").unwrap();
    assert!(installing < hook, "{stdout}");
}

#[test]
fn nothing_is_shown_from_xbps_when_nothing_needs_installing() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.fake_tool("xbps-query", "echo query noise; exit 0");
    sandbox.fake_tool("xbps-install", "echo install noise");
    sandbox.fake_sudo();

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("noise").not())
        .stderr(predicate::str::contains("noise").not());
}

#[test]
fn xbps_query_output_is_never_shown() {
    let sandbox = dwm_sandbox(&["btop"]);
    sandbox.fake_tool("xbps-query", "echo query noise >&2; exit 2");
    sandbox.fake_tool("xbps-install", "true");
    sandbox.fake_sudo();

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("query noise").not())
        .stderr(predicate::str::contains("query noise").not());
}

#[test]
fn with_json_the_install_output_goes_to_stderr_and_stdout_stays_one_document() {
    let sandbox = dwm_sandbox(&["btop"]);
    sandbox.fake_tool("xbps-query", "exit 2");
    sandbox.fake_tool("xbps-install", "echo '[*] Downloading btop'");
    sandbox.fake_sudo();

    let output = sandbox
        .command()
        .args(["--json", "link", "dwm", "--yes"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report.get("install").is_some());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("[*] Downloading btop"), "{stderr}");
}

#[test]
fn hooks_run_after_a_successful_install() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.manifest("dwm", "hook = \"build.sh\"\n[packages]\nxbps = [\"gcc\"]");
    let log = sandbox.calls_log();
    sandbox.package_script(
        "dwm",
        "build.sh",
        &format!("echo hook >> {}", log.display()),
    );
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    let calls = sandbox.calls();
    let install_at = calls
        .iter()
        .position(|c| c.contains("xbps-install"))
        .unwrap();
    let hook_at = calls.iter().position(|c| c == "hook").unwrap();
    assert!(install_at < hook_at, "{calls:?}");
}

#[test]
fn with_no_package_manager_linking_works_and_the_packages_are_reported() {
    let sandbox = dwm_sandbox(&["gcc"]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("install skipped").not());

    assert!(sandbox.home().join("config.h").is_symlink());
}

#[test]
fn with_no_package_manager_the_declared_packages_are_noted_as_unavailable() {
    let sandbox = dwm_sandbox(&["gcc"]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "package dwm declares system packages (xbps) but no package manager is available",
        ));
}

#[test]
fn a_manifest_with_no_list_for_the_manager_in_use_is_noted() {
    let sandbox = Sandbox::new();
    sandbox.package_file("dwm", "config.h", "");
    sandbox.manifest("dwm", "[packages]");
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "has system packages only for: (none)",
        ));
}

#[test]
fn a_package_without_a_packages_table_causes_no_query_or_notice() {
    let sandbox = Sandbox::new();
    sandbox.package_file("zsh", ".zshrc", "");
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "zsh", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());

    assert!(sandbox.calls().is_empty(), "{:?}", sandbox.calls());
}

#[test]
fn a_configured_package_manager_that_is_not_installed_is_an_error() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.repo_file("puddlejump.toml", "package_manager = \"xbps\"");

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "package manager xbps is configured but xbps-install was not found on PATH",
        ));

    assert!(!sandbox.home().join("config.h").exists());
}

#[test]
fn a_configured_package_manager_that_is_installed_is_used() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.repo_file("puddlejump.toml", "package_manager = \"xbps\"");
    sandbox.fake_xbps(&[]);

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .success();

    assert!(sandbox
        .calls()
        .contains(&"sudo xbps-install -y gcc".to_string()));
}

#[test]
fn an_invalid_repo_config_is_an_error() {
    let sandbox = dwm_sandbox(&["gcc"]);
    sandbox.repo_file("puddlejump.toml", "package_manager = \"pacman-ish\"");

    sandbox
        .command()
        .args(["link", "dwm", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pacman-ish"));
}

#[test]
fn a_system_package_for_a_root_target_needs_root_and_is_written_through_sudo() {
    let sandbox = Sandbox::new();
    let etc = sandbox.home().join("fake-etc");
    fs::create_dir(&etc).unwrap();
    sandbox.package_file("sysctl", "99.conf", "x");
    sandbox.manifest(
        "sysctl",
        &format!("needs_root = true\ntarget = \"{}\"", etc.display()),
    );
    sandbox.fake_sudo();
    sandbox.fake_tool("ln", "exit 0");
    sandbox.fake_tool("mkdir", "exit 0");

    sandbox
        .command()
        .args(["link", "sysctl", "--yes"])
        .assert()
        .success();

    let calls = sandbox.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("sudo ln -s -- ")),
        "the link was made through sudo: {calls:?}"
    );
}

#[test]
fn a_root_package_with_no_sudo_is_refused_naming_the_package() {
    let sandbox = Sandbox::new();
    sandbox.package_file("sysctl", "99.conf", "x");
    sandbox.manifest("sysctl", "needs_root = true");

    sandbox
        .command()
        .args(["link", "sysctl", "--yes"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "package sysctl declares needs_root",
        ));
}

#[test]
fn an_unwritable_target_without_a_declaration_warns_that_needs_root_is_missing() {
    let sandbox = Sandbox::new();
    sandbox.package_file("sysctl", "99.conf", "x");
    sandbox.manifest("sysctl", "target = \"/proc\"");
    sandbox.fake_sudo();
    sandbox.fake_tool("ln", "exit 0");
    sandbox.fake_tool("mkdir", "exit 0");

    sandbox
        .command()
        .args(["link", "sysctl", "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("does not say needs_root = true"));
}
