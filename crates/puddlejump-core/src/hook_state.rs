//! Deciding whether a hook needs to run, from what was remembered about its
//! last run.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::hook::{HookOutcome, HookResult};
use crate::plan::HookPlan;
use crate::state::{HookRecord, HookStatus, RepoState};

/// FNV-1a's offset basis and prime for 64 bits. Chosen over `DefaultHasher`
/// because that is not stable across Rust versions, and a fingerprint stored on
/// disk must mean the same thing next year. It only has to notice an edit, not
/// resist an attacker.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A fingerprint of the script at `script`: 16 hex digits that change when its
/// contents do.
///
/// Errors if the script cannot be read.
pub fn fingerprint(script: &Path) -> io::Result<String> {
    let bytes = fs::read(script)?;
    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    Ok(format!("{hash:016x}"))
}

/// Why a hook is going to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HookReason {
    /// It has never run before.
    NeverRun,
    /// It ran last time and failed, so it is retried.
    PreviouslyFailed,
    /// Its script was edited since it last succeeded.
    ScriptChanged,
    /// The user asked for it to run regardless of what is remembered
    /// (`--force-hooks`, or the `hook` command).
    Forced,
}

impl std::fmt::Display for HookReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            HookReason::NeverRun => "never run",
            HookReason::PreviouslyFailed => "failed last time",
            HookReason::ScriptChanged => "script changed since it last succeeded",
            HookReason::Forced => "requested",
        })
    }
}

/// Whether a hook needs to run, given its last record and the script's
/// fingerprint now.
///
/// `None` means it last succeeded with this exact script, so there is nothing
/// to do. A script that cannot be fingerprinted (`current` is `None`) is
/// treated as changed: running it will report the real problem.
pub fn reason_to_run(record: Option<&HookRecord>, current: Option<&str>) -> Option<HookReason> {
    let Some(record) = record else {
        return Some(HookReason::NeverRun);
    };
    if record.status == HookStatus::Failed {
        return Some(HookReason::PreviouslyFailed);
    }
    if current != Some(record.fingerprint.as_str()) {
        return Some(HookReason::ScriptChanged);
    }
    None
}

/// The record to remember for `outcome`, stamped with `now`.
///
/// A script that cannot be read (a hook that was not runnable) is recorded
/// with an empty fingerprint, which never matches a real one. A clock before
/// the Unix epoch is recorded as time zero rather than failing the run.
pub fn record_for(hook: &HookPlan, outcome: &HookOutcome, now: SystemTime) -> HookRecord {
    let exit_code = match &outcome.result {
        HookResult::Failed { exit_code } => *exit_code,
        HookResult::Succeeded => Some(0),
        HookResult::NotRunnable { .. } => None,
    };
    HookRecord {
        package: hook.package.clone(),
        status: HookStatus::from(&outcome.result),
        exit_code,
        fingerprint: fingerprint(&hook.script).unwrap_or_default(),
        ran_at: now
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default(),
    }
}

/// Updates `repo` with the result of every hook in `outcomes`, which ran from
/// the plans in `hooks`.
///
/// A hook that ran replaces its package's earlier record; hooks that did not
/// run this time keep theirs. Outcomes with no matching plan are ignored.
pub fn remember_outcomes(
    repo: &mut RepoState,
    hooks: &[HookPlan],
    outcomes: &[HookOutcome],
    now: SystemTime,
) {
    for outcome in outcomes {
        let Some(hook) = hooks.iter().find(|hook| hook.package == outcome.package) else {
            continue;
        };
        repo.hooks
            .retain(|record| record.package != outcome.package);
        repo.hooks.push(record_for(hook, outcome, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::PackageName;
    use std::path::PathBuf;
    use std::time::Duration;

    fn record(status: HookStatus, fingerprint: &str) -> HookRecord {
        HookRecord {
            package: PackageName::new("dwm").unwrap(),
            status,
            exit_code: None,
            fingerprint: fingerprint.to_string(),
            ran_at: 0,
        }
    }

    fn script_with(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("build.sh");
        fs::write(&path, contents).unwrap();
        (directory, path)
    }

    fn hook_plan(script: PathBuf) -> HookPlan {
        HookPlan {
            package: PackageName::new("dwm").unwrap(),
            reason: HookReason::NeverRun,
            script,
            package_dir: PathBuf::from("/repo/packages/dwm"),
            target_dir: PathBuf::from("/home/me"),
            repo_root: PathBuf::from("/repo"),
        }
    }

    fn outcome(result: HookResult) -> HookOutcome {
        HookOutcome {
            package: PackageName::new("dwm").unwrap(),
            result,
            output: Vec::new(),
        }
    }

    #[test]
    fn a_fingerprint_is_sixteen_hex_digits() {
        let (_directory, script) = script_with("echo hi");

        let text = fingerprint(&script).unwrap();

        assert_eq!(text.len(), 16);
        assert!(text.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_fingerprint_matches_the_known_fnv_1a_value() {
        // FNV-1a 64 of "a" is af63dc4c8601ec8c in the reference test vectors.
        let (_directory, script) = script_with("a");

        assert_eq!(fingerprint(&script).unwrap(), "af63dc4c8601ec8c");
    }

    #[test]
    fn the_fingerprint_of_an_empty_script_is_the_offset_basis() {
        let (_directory, script) = script_with("");

        assert_eq!(fingerprint(&script).unwrap(), "cbf29ce484222325");
    }

    #[test]
    fn editing_a_script_changes_its_fingerprint() {
        let (_first, one) = script_with("echo one");
        let (_second, two) = script_with("echo two");

        assert_ne!(fingerprint(&one).unwrap(), fingerprint(&two).unwrap());
    }

    #[test]
    fn identical_scripts_have_the_same_fingerprint() {
        let (_first, one) = script_with("echo same");
        let (_second, two) = script_with("echo same");

        assert_eq!(fingerprint(&one).unwrap(), fingerprint(&two).unwrap());
    }

    #[test]
    fn fingerprinting_a_missing_script_is_an_error() {
        let directory = tempfile::tempdir().unwrap();

        assert!(fingerprint(&directory.path().join("gone.sh")).is_err());
    }

    #[test]
    fn a_hook_with_no_record_has_never_run() {
        assert_eq!(reason_to_run(None, Some("abc")), Some(HookReason::NeverRun));
    }

    #[test]
    fn a_hook_that_failed_last_time_is_retried_even_if_unchanged() {
        let record = record(HookStatus::Failed, "abc");

        assert_eq!(
            reason_to_run(Some(&record), Some("abc")),
            Some(HookReason::PreviouslyFailed)
        );
    }

    #[test]
    fn a_hook_that_succeeded_with_a_different_script_runs_again() {
        let record = record(HookStatus::Succeeded, "abc");

        assert_eq!(
            reason_to_run(Some(&record), Some("def")),
            Some(HookReason::ScriptChanged)
        );
    }

    #[test]
    fn a_hook_that_succeeded_with_the_same_script_is_up_to_date() {
        let record = record(HookStatus::Succeeded, "abc");

        assert_eq!(reason_to_run(Some(&record), Some("abc")), None);
    }

    #[test]
    fn a_script_that_cannot_be_read_counts_as_changed() {
        let record = record(HookStatus::Succeeded, "abc");

        assert_eq!(
            reason_to_run(Some(&record), None),
            Some(HookReason::ScriptChanged)
        );
    }

    #[test]
    fn a_successful_outcome_is_recorded_with_exit_code_zero_and_the_fingerprint() {
        let (_directory, script) = script_with("echo hi");
        let now = UNIX_EPOCH + Duration::from_secs(500);

        let record = record_for(
            &hook_plan(script.clone()),
            &outcome(HookResult::Succeeded),
            now,
        );

        assert_eq!(record.status, HookStatus::Succeeded);
        assert_eq!(record.exit_code, Some(0));
        assert_eq!(record.fingerprint, fingerprint(&script).unwrap());
        assert_eq!(record.ran_at, 500);
    }

    #[test]
    fn a_failed_outcome_is_recorded_with_its_exit_code() {
        let (_directory, script) = script_with("exit 3");

        let record = record_for(
            &hook_plan(script),
            &outcome(HookResult::Failed { exit_code: Some(3) }),
            UNIX_EPOCH,
        );

        assert_eq!(record.status, HookStatus::Failed);
        assert_eq!(record.exit_code, Some(3));
    }

    #[test]
    fn a_hook_killed_by_a_signal_is_recorded_as_failed_without_an_exit_code() {
        let (_directory, script) = script_with("kill -9 $$");

        let record = record_for(
            &hook_plan(script),
            &outcome(HookResult::Failed { exit_code: None }),
            UNIX_EPOCH,
        );

        assert_eq!(record.status, HookStatus::Failed);
        assert_eq!(record.exit_code, None);
    }

    #[test]
    fn a_hook_that_could_not_start_is_recorded_as_failed_with_an_empty_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("gone.sh");

        let record = record_for(
            &hook_plan(missing),
            &outcome(HookResult::NotRunnable {
                reason: "does not exist".to_string(),
            }),
            UNIX_EPOCH,
        );

        assert_eq!(record.status, HookStatus::Failed);
        assert_eq!(record.exit_code, None);
        assert_eq!(record.fingerprint, "");
    }

    fn outcome_for(package: &str, result: HookResult) -> HookOutcome {
        HookOutcome {
            package: PackageName::new(package).unwrap(),
            result,
            output: Vec::new(),
        }
    }

    fn plan_for(package: &str, script: &Path) -> HookPlan {
        HookPlan {
            package: PackageName::new(package).unwrap(),
            ..hook_plan(script.to_path_buf())
        }
    }

    #[test]
    fn remembering_an_outcome_adds_a_record_for_its_package() {
        let (_directory, script) = script_with("true");
        let mut repo = RepoState::default();

        remember_outcomes(
            &mut repo,
            &[plan_for("dwm", &script)],
            &[outcome_for("dwm", HookResult::Succeeded)],
            UNIX_EPOCH,
        );

        assert_eq!(repo.hooks.len(), 1);
        assert_eq!(repo.hooks[0].status, HookStatus::Succeeded);
    }

    #[test]
    fn remembering_replaces_the_earlier_record_of_the_same_package() {
        let (_directory, script) = script_with("true");
        let mut repo = RepoState {
            hooks: vec![record(HookStatus::Failed, "old")],
            ..RepoState::default()
        };

        remember_outcomes(
            &mut repo,
            &[plan_for("dwm", &script)],
            &[outcome_for("dwm", HookResult::Succeeded)],
            UNIX_EPOCH,
        );

        assert_eq!(repo.hooks.len(), 1);
        assert_eq!(repo.hooks[0].status, HookStatus::Succeeded);
    }

    #[test]
    fn hooks_that_did_not_run_keep_their_records() {
        let (_directory, script) = script_with("true");
        let mut other = record(HookStatus::Succeeded, "kept");
        other.package = PackageName::new("other").unwrap();
        let mut repo = RepoState {
            hooks: vec![other.clone()],
            ..RepoState::default()
        };

        remember_outcomes(
            &mut repo,
            &[plan_for("dwm", &script)],
            &[outcome_for("dwm", HookResult::Succeeded)],
            UNIX_EPOCH,
        );

        assert!(repo.hooks.contains(&other));
    }

    #[test]
    fn an_outcome_without_a_matching_plan_is_ignored() {
        let mut repo = RepoState::default();

        remember_outcomes(
            &mut repo,
            &[],
            &[outcome_for("ghost", HookResult::Succeeded)],
            UNIX_EPOCH,
        );

        assert!(repo.hooks.is_empty());
    }

    #[test]
    fn remembering_a_failure_makes_the_next_plan_retry_it() {
        let (_directory, script) = script_with("exit 1");
        let mut repo = RepoState::default();
        remember_outcomes(
            &mut repo,
            &[plan_for("dwm", &script)],
            &[outcome_for(
                "dwm",
                HookResult::Failed { exit_code: Some(1) },
            )],
            UNIX_EPOCH,
        );

        let now = fingerprint(&script).unwrap();

        assert_eq!(
            reason_to_run(repo.hooks.first(), Some(&now)),
            Some(HookReason::PreviouslyFailed)
        );
    }

    #[test]
    fn remembering_a_success_makes_the_next_plan_skip_it_until_the_script_changes() {
        let (_directory, script) = script_with("true");
        let mut repo = RepoState::default();
        remember_outcomes(
            &mut repo,
            &[plan_for("dwm", &script)],
            &[outcome_for("dwm", HookResult::Succeeded)],
            UNIX_EPOCH,
        );
        let unchanged = fingerprint(&script).unwrap();
        fs::write(&script, "echo edited").unwrap();
        let edited = fingerprint(&script).unwrap();

        assert_eq!(reason_to_run(repo.hooks.first(), Some(&unchanged)), None);
        assert_eq!(
            reason_to_run(repo.hooks.first(), Some(&edited)),
            Some(HookReason::ScriptChanged)
        );
    }

    #[test]
    fn a_clock_before_the_epoch_is_recorded_as_zero_rather_than_failing() {
        let (_directory, script) = script_with("true");
        let before = UNIX_EPOCH - Duration::from_secs(10);

        let record = record_for(&hook_plan(script), &outcome(HookResult::Succeeded), before);

        assert_eq!(record.ran_at, 0);
    }
}
