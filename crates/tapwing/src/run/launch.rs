//! Pure launch-routing decision and black-box demotion formatting.

use std::time::Duration;

use crate::native_lane::{BlackboxDemotion, InputMode, STICKY_BLACKBOX_THRESHOLD};

use super::hybrid::HANDSHAKE_FALLBACK_TIMEOUT;

/// Default handshake accept timeout when native fallback is unavailable or disabled.
pub(super) const DEFAULT_ACCEPT_TIMEOUT: Duration = Duration::from_secs(20);

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
