use assert_cmd::Command;
use predicates::prelude::*;

fn puddlejump() -> Command {
    Command::cargo_bin("puddlejump").unwrap()
}

#[test]
fn version_flag_prints_the_crate_version() {
    puddlejump()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_flag_exits_with_usage_error() {
    puddlejump()
        .arg("--no-such-flag")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--no-such-flag"));
}
