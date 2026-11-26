//! Pure launch-routing decision and black-box demotion formatting.

use std::time::Duration;

use crate::driver_probe::SkipReason;
use crate::native_lane::{BlackboxDemotion, InputMode, STICKY_BLACKBOX_THRESHOLD};

use super::hybrid::HANDSHAKE_FALLBACK_TIMEOUT;

/// Default handshake accept timeout when native fallback is unavailable or disabled.
pub(super) const DEFAULT_ACCEPT_TIMEOUT: Duration = Duration::from_secs(20);

/// What the pre-launch driver probe (see [`crate::driver_probe`]) knows when
/// the launch is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DriverKnowledge {
    /// No probe ran: `--input flutter` or `native`, nothing to wait for, or the
    /// probe could not be started. The launch behaves as it always did.
    NotProbed,
    /// The probe is still reading. The wait for the handshake watches it and
    /// gives up as soon as it proves the build has no driver.
    Pending,
    /// The probe finished and the build may still dial in: wait as usual.
    Wait,
    /// The probe finished and the wait would be pure loss.
    Skip(SkipReason),
}

/// Pure inputs to [`decide_launch_plan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LaunchPlanInput<'a> {
    pub lane_mode: InputMode,
    pub blackbox: bool,
    pub native_usable: bool,
    pub stop_app: bool,
    pub clear_state: bool,
    pub app_changed: bool,
    pub was_connected: bool,
    pub previous_pid: Option<i64>,
    pub launch_pid: Option<i64>,
    pub old_pids: &'a [i64],
    pub driver: DriverKnowledge,
}

/// Pure launch-phase decision after `Device::prepare_and_launch` completes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LaunchPlan {
    /// `--input native` or sticky black-box app: skip the Flutter driver.
    Native { reason: &'static str },
    /// Wait for a fresh `tapwing_driver` handshake on the host TCP listener.
    AcceptHandshake {
        /// `true` when `stopApp: false` still resulted in a new process PID
        /// (e.g. a revoked permission killed the app on Android or iOS), so the
        /// stale connection must be closed before accepting the new process.
        relaunched: bool,
        expected_pid: Option<i64>,
        reject_pids: Vec<i64>,
        can_fall_back: bool,
        accept_timeout: Duration,
    },
    /// The installed build provably (or, from earlier runs, evidently) has no
    /// driver: skip the handshake wait and take the native launch path at once.
    SkipHandshake { reason: SkipReason },
    /// Same app process stayed alive: call `waitIdle` on the existing socket.
    ReuseConnection,
}

/// Computes how [`super::DeviceFlutterBackend::launch_app`] should attach to
/// (or bypass) the in-app driver after `prepare_and_launch`.
///
/// Covers both iOS and Android uniformly: whenever `stopApp: false` causes the
/// OS to kill the running process (e.g. Android runtime permission revoke or
/// iOS `simctl privacy`) and `launch_pid` differs from `previous_pid`, the
/// launch is treated as a relaunch so we reconnect instead of calling
/// `waitIdle` on the dead socket.
pub(super) fn decide_launch_plan(input: &LaunchPlanInput<'_>) -> LaunchPlan {
    if input.lane_mode == InputMode::Native {
        return LaunchPlan::Native { reason: "--input native: Flutter driver not used" };
    }
    if input.lane_mode == InputMode::Auto && input.blackbox {
        return LaunchPlan::Native { reason: "app demoted to black-box after consecutive missed handshakes" };
    }
    let restarting = input.stop_app || input.clear_state || input.app_changed || !input.was_connected;
    let relaunched = !restarting
        && input.launch_pid.is_some()
        && input.previous_pid.is_some()
        && input.launch_pid != input.previous_pid;
    if !(restarting || relaunched) {
        return LaunchPlan::ReuseConnection;
    }

    let mut reject_pids = input.old_pids.to_vec();
    if let Some(prev) = input.previous_pid
        && (input.stop_app || input.clear_state || relaunched || (input.app_changed && input.launch_pid != Some(prev)))
        && !reject_pids.contains(&prev)
    {
        reject_pids.push(prev);
    }
    let expected_pid = input.launch_pid.filter(|p| !reject_pids.contains(p));
    let can_fall_back = input.lane_mode == InputMode::Auto && input.native_usable;
    // Only `--input auto` with a native lane to hand over to may skip: `--input flutter`
    // is the documented way to insist on the driver and never gives up on it early.
    if can_fall_back && let DriverKnowledge::Skip(reason) = input.driver {
        return LaunchPlan::SkipHandshake { reason };
    }
    let accept_timeout = if can_fall_back { HANDSHAKE_FALLBACK_TIMEOUT } else { DEFAULT_ACCEPT_TIMEOUT };
    LaunchPlan::AcceptHandshake { relaunched, expected_pid, reject_pids, can_fall_back, accept_timeout }
}

/// Formats the loud stderr warning when `launchApp` misses the Flutter
/// handshake and falls back to the native lane, clearly distinguishing a
/// single-flow fallback from a sticky shard-wide demotion.
pub(super) fn format_blackbox_demotion_warning(
    log_prefix: &str,
    app_id: &str,
    reason: &str,
    demotion: BlackboxDemotion,
) -> String {
    match demotion {
        BlackboxDemotion::FlowOnly { consecutive_misses } => format!(
            "{log_prefix}warning: launchApp {app_id}: no tapwing_driver handshake ({reason}); \
             falling back to black-box native mode for this flow only \
             (miss {consecutive_misses}/{STICKY_BLACKBOX_THRESHOLD} — the next launchApp will retry the Flutter driver)"
        ),
        BlackboxDemotion::StickyShard { consecutive_misses } => format!(
            "{log_prefix}warning: launchApp {app_id}: no tapwing_driver handshake ({reason}); \
             demoting {app_id} to sticky black-box native mode for the rest of this shard \
             after {consecutive_misses} consecutive handshake misses"
        ),
    }
}

/// Loud stderr warning for a launch that skips the handshake wait because the
/// installed build is known to have no driver. Names what was inspected and how
/// to insist on the driver instead.
pub(super) fn format_driver_skip_warning(log_prefix: &str, app_id: &str, reason: SkipReason, evidence: &str) -> String {
    let secs = HANDSHAKE_FALLBACK_TIMEOUT.as_secs();
    match reason {
        SkipReason::NoDriver { cached } => {
            let from = if cached { "; verdict remembered from an earlier run of this install" } else { "" };
            format!(
                "{log_prefix}warning: launchApp {app_id}: the installed build contains no tapwing_driver \
                 ({evidence}{from}); not waiting {secs} s for a handshake that cannot come, driving it \
                 black-box via the native lane (pass --input flutter to wait for the driver anyway)"
            )
        }
        SkipReason::LearnedMisses { misses } => format!(
            "{log_prefix}warning: launchApp {app_id}: the last {misses} launches of this installed build waited \
             {secs} s for a tapwing_driver handshake with the app running and got none, and its payload could \
             not be shown to hold the driver ({evidence}); not waiting again, driving it black-box via the \
             native lane (pass --input flutter to wait for the driver anyway; a handshake clears this)"
        ),
    }
}

/// The launch step's `note`: what happened, in one sentence.
pub(super) fn driver_skip_note(reason: SkipReason, evidence: &str) -> String {
    match reason {
        SkipReason::NoDriver { .. } => {
            format!(
                "no tapwing_driver in the installed build ({evidence}); driving the app black-box via the native lane"
            )
        }
        SkipReason::LearnedMisses { misses } => format!(
            "no tapwing_driver handshake in the last {misses} launches of this installed build; \
             driving the app black-box via the native lane without waiting"
        ),
    }
}

/// Why a flow ran black-box without waiting, as the reports carry it
/// (`blackboxReason` in run.json and the trace, the HTML flow card, JUnit `<system-err>`).
pub(super) fn driver_skip_report_reason(reason: SkipReason, evidence: &str) -> String {
    match reason {
        SkipReason::NoDriver { cached: false } => format!("no tapwing_driver in the installed build: {evidence}"),
        SkipReason::NoDriver { cached: true } => {
            format!("no tapwing_driver in the installed build (remembered): {evidence}")
        }
        SkipReason::LearnedMisses { misses } => format!(
            "no tapwing_driver handshake in the last {misses} launches of this installed build (learned): {evidence}"
        ),
    }
}

/// The launch step's `driverProbe` object: how the host knew, for tools that read run.json.
pub(super) fn driver_probe_json(reason: SkipReason, report: &crate::driver_probe::Report) -> serde_json::Value {
    let (verdict, source) = match reason {
        SkipReason::NoDriver { cached } => ("absent", if cached { "cache" } else { "scan" }),
        SkipReason::LearnedMisses { .. } => (report.verdict.presence.as_str(), "learnedMisses"),
    };
    let mut out = serde_json::json!({
        "verdict": verdict,
        "source": source,
        "evidence": report.verdict.evidence,
        "handshakeWaitSkippedMs": HANDSHAKE_FALLBACK_TIMEOUT.as_millis() as u64,
    });
    if let SkipReason::LearnedMisses { misses } = reason {
        out["misses"] = serde_json::json!(misses);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_launch_plan_table() {
        struct Case {
            name: &'static str,
            input: LaunchPlanInput<'static>,
            expected: LaunchPlan,
        }

        let cases = [
            Case {
                name: "native mode always skips Flutter even when connected",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Native,
                    blackbox: false,
                    native_usable: true,
                    stop_app: false,
                    clear_state: false,
                    app_changed: false,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: Some(100),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::Native { reason: "--input native: Flutter driver not used" },
            },
            Case {
                name: "auto mode with sticky blackbox skips Flutter",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: true,
                    native_usable: true,
                    stop_app: true,
                    clear_state: false,
                    app_changed: false,
                    was_connected: false,
                    previous_pid: None,
                    launch_pid: Some(200),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::Native { reason: "app demoted to black-box after consecutive missed handshakes" },
            },
            Case {
                name: "flutter mode ignores blackbox flag and never falls back",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Flutter,
                    blackbox: true,
                    native_usable: true,
                    stop_app: true,
                    clear_state: false,
                    app_changed: false,
                    was_connected: false,
                    previous_pid: None,
                    launch_pid: Some(200),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(200),
                    reject_pids: vec![],
                    can_fall_back: false,
                    accept_timeout: DEFAULT_ACCEPT_TIMEOUT,
                },
            },
            Case {
                name: "cold launch with auto + usable native uses fallback timeout",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: true,
                    stop_app: true,
                    clear_state: false,
                    app_changed: false,
                    was_connected: false,
                    previous_pid: None,
                    launch_pid: Some(300),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(300),
                    reject_pids: vec![],
                    can_fall_back: true,
                    accept_timeout: HANDSHAKE_FALLBACK_TIMEOUT,
                },
            },
            Case {
                name: "stopApp true rejects previous_pid and old_pids without duplicates",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: false,
                    stop_app: true,
                    clear_state: false,
                    app_changed: false,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: Some(200),
                    old_pids: &[100, 99],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(200),
                    reject_pids: vec![100, 99],
                    can_fall_back: false,
                    accept_timeout: DEFAULT_ACCEPT_TIMEOUT,
                },
            },
            Case {
                name: "clearState filters launch_pid out of expected_pid if it matches old_pids",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: true,
                    stop_app: false,
                    clear_state: true,
                    app_changed: false,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: Some(100),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: None,
                    reject_pids: vec![100],
                    can_fall_back: true,
                    accept_timeout: HANDSHAKE_FALLBACK_TIMEOUT,
                },
            },
            Case {
                name: "stopApp false with same PID reuses existing connection",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: true,
                    stop_app: false,
                    clear_state: false,
                    app_changed: false,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: Some(100),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::ReuseConnection,
            },
            Case {
                name: "stopApp false with None launch_pid reuses existing connection",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: true,
                    stop_app: false,
                    clear_state: false,
                    app_changed: false,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: None,
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::ReuseConnection,
            },
            Case {
                name: "Android/iOS stopApp false + permission revoke kills app and starts new PID -> relaunched",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: true,
                    stop_app: false,
                    clear_state: false,
                    app_changed: false,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: Some(200),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: true,
                    expected_pid: Some(200),
                    reject_pids: vec![100],
                    can_fall_back: true,
                    accept_timeout: HANDSHAKE_FALLBACK_TIMEOUT,
                },
            },
            Case {
                name: "stopApp false after earlier disconnect reconnects without rejecting previous_pid",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: true,
                    stop_app: false,
                    clear_state: false,
                    app_changed: false,
                    was_connected: false,
                    previous_pid: Some(100),
                    launch_pid: Some(100),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(100),
                    reject_pids: vec![],
                    can_fall_back: true,
                    accept_timeout: HANDSHAKE_FALLBACK_TIMEOUT,
                },
            },
            Case {
                name: "app_changed with stopApp false restarts and rejects old app PID",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Auto,
                    blackbox: false,
                    native_usable: true,
                    stop_app: false,
                    clear_state: false,
                    app_changed: true,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: Some(250),
                    old_pids: &[],
                    driver: DriverKnowledge::NotProbed,
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(250),
                    reject_pids: vec![100],
                    can_fall_back: true,
                    accept_timeout: HANDSHAKE_FALLBACK_TIMEOUT,
                },
            },
        ];

        for c in cases {
            assert_eq!(decide_launch_plan(&c.input), c.expected, "case failed: {}", c.name);
        }
    }

    #[test]
    fn decide_launch_plan_driver_knowledge_table() {
        const NO_DRIVER: SkipReason = SkipReason::NoDriver { cached: false };
        let cold_launch = LaunchPlanInput {
            lane_mode: InputMode::Auto,
            blackbox: false,
            native_usable: true,
            stop_app: true,
            clear_state: false,
            app_changed: false,
            was_connected: false,
            previous_pid: None,
            launch_pid: Some(300),
            old_pids: &[],
            driver: DriverKnowledge::Skip(NO_DRIVER),
        };
        let waits = LaunchPlan::AcceptHandshake {
            relaunched: false,
            expected_pid: Some(300),
            reject_pids: vec![],
            can_fall_back: true,
            accept_timeout: HANDSHAKE_FALLBACK_TIMEOUT,
        };
        struct Case {
            name: &'static str,
            input: LaunchPlanInput<'static>,
            expected: LaunchPlan,
        }
        let cases = [
            Case {
                name: "auto + usable native lane + probe says no driver: skip the wait",
                input: cold_launch,
                expected: LaunchPlan::SkipHandshake { reason: NO_DRIVER },
            },
            Case {
                name: "a remembered verdict skips the same way",
                input: LaunchPlanInput {
                    driver: DriverKnowledge::Skip(SkipReason::NoDriver { cached: true }),
                    ..cold_launch
                },
                expected: LaunchPlan::SkipHandshake { reason: SkipReason::NoDriver { cached: true } },
            },
            Case {
                name: "learned misses skip too",
                input: LaunchPlanInput {
                    driver: DriverKnowledge::Skip(SkipReason::LearnedMisses { misses: 2 }),
                    ..cold_launch
                },
                expected: LaunchPlan::SkipHandshake { reason: SkipReason::LearnedMisses { misses: 2 } },
            },
            Case {
                name: "a probe that is still reading keeps the full wait (which watches the probe)",
                input: LaunchPlanInput { driver: DriverKnowledge::Pending, ..cold_launch },
                expected: waits.clone(),
            },
            Case {
                name: "a build that may dial in waits as before",
                input: LaunchPlanInput { driver: DriverKnowledge::Wait, ..cold_launch },
                expected: waits.clone(),
            },
            Case {
                name: "no probe waits as before",
                input: LaunchPlanInput { driver: DriverKnowledge::NotProbed, ..cold_launch },
                expected: waits,
            },
            Case {
                name: "--input flutter insists on the driver whatever a probe says",
                input: LaunchPlanInput { lane_mode: InputMode::Flutter, ..cold_launch },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(300),
                    reject_pids: vec![],
                    can_fall_back: false,
                    accept_timeout: DEFAULT_ACCEPT_TIMEOUT,
                },
            },
            Case {
                name: "without a usable native lane there is nothing to hand over to",
                input: LaunchPlanInput { native_usable: false, ..cold_launch },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(300),
                    reject_pids: vec![],
                    can_fall_back: false,
                    accept_timeout: DEFAULT_ACCEPT_TIMEOUT,
                },
            },
            Case {
                name: "--input native never reaches the driver",
                input: LaunchPlanInput { lane_mode: InputMode::Native, ..cold_launch },
                expected: LaunchPlan::Native { reason: "--input native: Flutter driver not used" },
            },
            Case {
                name: "an app already demoted for the shard keeps its own reason",
                input: LaunchPlanInput { blackbox: true, ..cold_launch },
                expected: LaunchPlan::Native { reason: "app demoted to black-box after consecutive missed handshakes" },
            },
            Case {
                name: "no handshake is awaited when the connection is reused",
                input: LaunchPlanInput { stop_app: false, was_connected: true, previous_pid: Some(300), ..cold_launch },
                expected: LaunchPlan::ReuseConnection,
            },
            Case {
                name: "a process that was killed and restarted skips too",
                input: LaunchPlanInput {
                    stop_app: false,
                    was_connected: true,
                    previous_pid: Some(100),
                    launch_pid: Some(300),
                    ..cold_launch
                },
                expected: LaunchPlan::SkipHandshake { reason: NO_DRIVER },
            },
        ];
        for c in cases {
            assert_eq!(decide_launch_plan(&c.input), c.expected, "case failed: {}", c.name);
        }
    }

    #[test]
    fn driver_skip_texts_name_the_app_the_evidence_and_the_override() {
        let evidence = "read base.apk: lib/arm64-v8a/libapp.so (7.6 MB) completely: no tapwing_driver marker";
        let fresh = format_driver_skip_warning(
            "[shard 2] ",
            "com.example.app",
            SkipReason::NoDriver { cached: false },
            evidence,
        );
        assert!(fresh.starts_with(
            "[shard 2] warning: launchApp com.example.app: the installed build contains no tapwing_driver"
        ));
        assert!(
            fresh.contains(evidence) && fresh.contains("not waiting 15 s") && fresh.contains("--input flutter"),
            "{fresh}"
        );
        assert!(!fresh.contains("remembered"));
        let cached = format_driver_skip_warning("", "com.example.app", SkipReason::NoDriver { cached: true }, evidence);
        assert!(cached.contains("remembered from an earlier run"), "{cached}");
        let learned = format_driver_skip_warning(
            "",
            "com.example.app",
            SkipReason::LearnedMisses { misses: 2 },
            "payload not readable",
        );
        assert!(learned.contains("last 2 launches") && learned.contains("a handshake clears this"), "{learned}");

        assert!(driver_skip_note(SkipReason::NoDriver { cached: false }, evidence).contains(evidence));
        assert_eq!(
            driver_skip_report_reason(SkipReason::NoDriver { cached: true }, "x"),
            "no tapwing_driver in the installed build (remembered): x"
        );
        assert!(driver_skip_report_reason(SkipReason::LearnedMisses { misses: 3 }, "x").contains("last 3 launches"));
    }

    #[test]
    fn driver_probe_json_says_how_the_host_knew() {
        use crate::driver_probe::{Presence, Report, Verdict, VerdictSource};
        let report = |presence, source| Report {
            verdict: Verdict { presence, dials: false, evidence: "e".into() },
            source,
            fingerprint: Some("f".into()),
            learned_misses: Some(2),
        };
        let scan =
            driver_probe_json(SkipReason::NoDriver { cached: false }, &report(Presence::Absent, VerdictSource::Scan));
        assert_eq!(scan["verdict"], "absent");
        assert_eq!(scan["source"], "scan");
        assert_eq!(scan["handshakeWaitSkippedMs"], 15_000);
        let cache =
            driver_probe_json(SkipReason::NoDriver { cached: true }, &report(Presence::Absent, VerdictSource::Cache));
        assert_eq!(cache["source"], "cache");
        let learned =
            driver_probe_json(SkipReason::LearnedMisses { misses: 2 }, &report(Presence::Unknown, VerdictSource::Scan));
        assert_eq!(
            (learned["verdict"].as_str(), learned["source"].as_str(), learned["misses"].as_u64()),
            (Some("unknown"), Some("learnedMisses"), Some(2))
        );
    }

    #[test]
    fn demotion_warnings_distinguish_flow_only_vs_sticky_shard() {
        let single = format_blackbox_demotion_warning(
            "[shard 1] ",
            "com.example.app",
            "timed out",
            BlackboxDemotion::FlowOnly { consecutive_misses: 1 },
        );
        assert!(single.starts_with("[shard 1] warning: launchApp com.example.app:"));
        assert!(single.contains("for this flow only"));
        assert!(single.contains("miss 1/2"));
        assert!(single.contains("next launchApp will retry"));

        let sticky = format_blackbox_demotion_warning(
            "",
            "com.example.app",
            "timed out",
            BlackboxDemotion::StickyShard { consecutive_misses: 2 },
        );
        assert!(sticky.starts_with("warning: launchApp com.example.app:"));
        assert!(sticky.contains("sticky black-box native mode"));
        assert!(sticky.contains("2 consecutive handshake misses"));
    }
}
