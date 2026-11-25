//! Per-session identity of the iOS runner.
//!
//! A `/status` answer on the expected port does not prove that the runner just
//! started is the one answering. A runner left over from a hard-killed run (its
//! teardown never ran) or another Tapwing process keeps listening on a fixed
//! `$TAPWING_IOS_NATIVE_PORT`, and `xcodebuild` needs seconds before it replaces
//! or fails to replace it, while the first readiness probe arrives at once. So
//! every session starts its runner with a fresh id (`TEST_RUNNER_TAPWING_SESSION`)
//! and readiness waits until `/status` echoes it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::sha256_hex;

/// `xcodebuild` passes `TEST_RUNNER_<NAME>` to the runner as `<NAME>`.
pub(crate) const RUNNER_ENV: &str = "TEST_RUNNER_TAPWING_SESSION";

/// A new session id: unique per process, session and start time. It tells
/// sessions apart; it is not a secret.
pub(crate) fn new_id(seq: u64, port: u16) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let seed = format!("{}-{seq}-{port}-{nanos}", std::process::id());
    sha256_hex(&[seed.as_bytes()]).chars().take(16).collect()
}

/// Appended to a startup failure when another runner kept answering the probes.
pub(crate) fn foreign_hint(foreign: Option<&str>, port_env: &str, runner_bundle_id: &str) -> String {
    match foreign {
        None => String::new(),
        Some(other) => format!(
            ": the port is answered by another Tapwing iOS runner (session `{other}`), left over from an earlier \
             run or started by another Tapwing process on the same port; stop it (`xcrun simctl terminate <udid> \
             {runner_bundle_id}`) or unset ${port_env}"
        ),
    }
}

/// Shown once when the runner does not report a session id at all.
pub(crate) fn unverified_notice() -> String {
    "the iOS native driver does not report a session id (it was built before Tapwing checked it), so a runner \
     left over from an earlier run cannot be told apart; rebuild it with scripts/build-ios-driver.sh"
        .to_string()
}

/// Returns [`unverified_notice`] at most once per process (later shards and
/// restarts stay quiet).
pub(crate) fn unverified_notice_once() -> Option<String> {
    static NOTICED: AtomicBool = AtomicBool::new(false);
    (!NOTICED.swap(true, Ordering::Relaxed)).then(unverified_notice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_short_hex_and_differ_per_session() {
        let a = new_id(0, 50_000);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, new_id(1, 50_000));
        assert_ne!(a, new_id(0, 50_001));
    }

    #[test]
    fn the_hint_names_the_foreign_session_and_the_way_out() {
        assert_eq!(foreign_hint(None, "P", "b"), "");
        let hint = foreign_hint(Some("abc"), "TAPWING_IOS_NATIVE_PORT", "dev.tapwing.x");
        assert!(hint.contains("session `abc`") && hint.contains("$TAPWING_IOS_NATIVE_PORT"), "{hint}");
        assert!(hint.contains("simctl terminate <udid> dev.tapwing.x"), "{hint}");
    }
}
