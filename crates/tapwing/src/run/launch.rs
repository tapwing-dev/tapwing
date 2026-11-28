//! Pure launch-routing decision and black-box demotion formatting.

use std::time::Duration;

use crate::driver::human_duration;
use crate::driver_probe::SkipReason;
use crate::native_lane::{BlackboxDemotion, InputMode, STICKY_BLACKBOX_THRESHOLD};

use super::hybrid::HANDSHAKE_FALLBACK_TIMEOUT;

/// Default handshake accept timeout when native fallback is unavailable or disabled.
pub(super) const DEFAULT_ACCEPT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long `--input auto` waits in total for the hello of a build the probe
/// proved to contain a dialing driver, while its process is not known to be dead.
///
/// 60 s is the window in which the driver itself dials every 150 ms; after it
/// the driver slows to one dial per second (`TapwingServer._fastRetryWindow`
/// in `tapwing_driver`). It is four times [`HANDSHAKE_FALLBACK_TIMEOUT`], which
/// two of twelve launches missed on a host with a load average near 300 while
/// the other ten made it, and no longer than the longest startup the host
/// already waits for (the iOS XCUITest runner, 60 s).
pub(super) const SLOW_START_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long `launchApp` waits for the driver's hello when the native lane could take over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HandshakeTiming {
    /// The wait every such launch gets first.
    pub base: Duration,
    /// The whole wait of a build proven to dial, `base` included.
    pub slow_start: Duration,
    /// How often the part after `base` checks that the app process is alive.
    pub liveness_every: Duration,
}

impl Default for HandshakeTiming {
    fn default() -> Self {
        Self {
            base: HANDSHAKE_FALLBACK_TIMEOUT,
            slow_start: SLOW_START_HANDSHAKE_TIMEOUT,
            liveness_every: crate::driver::LIVENESS_PROBE_EVERY,
        }
    }
}

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
    /// The probe finished and found a marker only a dialing driver leaves. The
    /// wait is the usual one, but running out of it is a slow start or a
    /// fault, never a build without the driver (see [`decide_handshake_miss`]).
    Proven,
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

/// Pure inputs to [`decide_handshake_miss`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HandshakeMiss {
    /// What the probe knows now that the wait has run out.
    pub driver: DriverKnowledge,
    /// Whether the launched process is alive: `Some(false)` only when it is certainly gone.
    pub alive: Option<bool>,
    /// An earlier launch of this app in this shard already waited
    /// [`HandshakeTiming::slow_start`] and got no handshake.
    pub slow_start_spent: bool,
    /// How long this launch has waited for the hello so far.
    pub waited: Duration,
    pub timing: HandshakeTiming,
}

/// What a launch does once [`HandshakeTiming::base`] has passed without a hello.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MissDecision {
    /// The build is not proven to dial: drive it black-box and count the miss, as before the probe.
    BlackBox,
    /// The build is proven to dial and may only be slow: wait this much longer.
    WaitLonger { more: Duration },
    /// The build is proven to dial and did not: the step fails and says so.
    Fail(MissFailure),
}

/// Why a launch of a build proven to dial fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MissFailure {
    /// The app process is gone: a crash, which the crash diagnosis reports.
    ProcessDied,
    /// [`HandshakeTiming::slow_start`] ran out.
    NeverConnected,
    /// [`HandshakeTiming::base`] ran out and the longer wait was already spent in this shard.
    NeverConnectedShortWait,
}

/// Decides what a missed handshake means. Only a build the probe proved to
/// dial leaves the old path: it is never driven black-box and never demoted. It
/// gets the longer wait once per shard until a handshake restores it, so a
/// setup that cannot connect pays that wait once and the short one afterwards.
pub(super) fn decide_handshake_miss(miss: &HandshakeMiss) -> MissDecision {
    if miss.driver != DriverKnowledge::Proven {
        return MissDecision::BlackBox;
    }
    if miss.alive == Some(false) {
        return MissDecision::Fail(MissFailure::ProcessDied);
    }
    if miss.slow_start_spent {
        return MissDecision::Fail(MissFailure::NeverConnectedShortWait);
    }
    match miss.timing.slow_start.checked_sub(miss.waited).filter(|more| !more.is_zero()) {
        Some(more) => MissDecision::WaitLonger { more },
        None => MissDecision::Fail(MissFailure::NeverConnected),
    }
}

/// What a message about a missed handshake of a build proven to dial names.
#[derive(Clone, Copy, Debug)]
pub(super) struct ProvenMiss<'a> {
    pub app_id: &'a str,
    /// What the probe found, in its own words.
    pub evidence: &'a str,
    pub pid: Option<i64>,
    pub alive: Option<bool>,
    pub timing: HandshakeTiming,
}

impl ProvenMiss<'_> {
    fn process(&self) -> String {
        match (self.pid, self.alive) {
            (Some(pid), Some(true)) => format!("the app process {pid} is running"),
            (Some(pid), Some(false)) => format!("the app process {pid} is gone"),
            (Some(pid), None) => format!("the app process {pid} could not be checked"),
            (None, _) => "the app's process id is unknown".to_string(),
        }
    }
}

const TROUBLESHOOTING_POINTER: &str = "See \"App Fails to Connect to tapwing_driver\" in docs/troubleshooting.md";

/// Stderr warning when the longer wait for a slow start begins.
pub(super) fn format_slow_start_warning(log_prefix: &str, miss: &ProvenMiss<'_>) -> String {
    let (base, total) = (human_duration(miss.timing.base), human_duration(miss.timing.slow_start));
    let more = miss.timing.slow_start.saturating_sub(miss.timing.base);
    format!(
        "{log_prefix}warning: launchApp {}: no tapwing_driver handshake after {base}, but the installed build \
         contains the driver ({}) and {}; waiting up to {} more for a slow start ({total} in total), \
         with no black-box fallback",
        miss.app_id,
        miss.evidence,
        miss.process(),
        human_duration(more),
    )
}

/// The error of a `launchApp` whose build is proven to dial and did not.
pub(super) fn format_driver_never_connected(miss: &ProvenMiss<'_>, failure: MissFailure) -> String {
    let (app_id, evidence, process) = (miss.app_id, miss.evidence, miss.process());
    let (base, total) = (human_duration(miss.timing.base), human_duration(miss.timing.slow_start));
    let no_blackbox = "The app is not driven black-box, because a build with the driver belongs on the Flutter lane \
                       (pass --input native to drive it black-box on purpose)";
    match failure {
        MissFailure::ProcessDied => {
            let who = miss.pid.map_or_else(|| "the app process".to_string(), |pid| format!("the app process {pid}"));
            format!(
                "launchApp {app_id}: {who} died before tapwing_driver connected \
                 (the installed build contains the driver: {evidence})"
            )
        }
        MissFailure::NeverConnected => format!(
            "launchApp {app_id}: the installed build contains tapwing_driver ({evidence}) but the driver never \
             connected: no handshake within {total}, {process}. {no_blackbox}. {TROUBLESHOOTING_POINTER}"
        ),
        MissFailure::NeverConnectedShortWait => format!(
            "launchApp {app_id}: the installed build contains tapwing_driver ({evidence}) but the driver never \
             connected: no handshake within {base}, {process}. The longer wait for a slow start ({total}) was not \
             repeated, because an earlier launchApp of this app in this run already waited that long and got no \
             handshake (the next handshake restores it). {no_blackbox}. {TROUBLESHOOTING_POINTER}"
        ),
    }
}

/// The launch step's `warning` when the driver connected during the longer wait.
pub(super) fn slow_start_note(evidence: &str, waited: Duration, timing: HandshakeTiming) -> String {
    format!(
        "tapwing_driver connected after {:.1} s, past the usual {}: the installed build contains the driver \
         ({evidence}), so the launch waited for a slow start and did not drive the app black-box",
        waited.as_secs_f64(),
        human_duration(timing.base),
    )
}

/// The launch step's `driverProbe` object for a build proven to dial whose
/// handshake did not come in [`HandshakeTiming::base`]. `failure` is `None`
/// when the driver connected during the longer wait.
pub(super) fn driver_present_json(
    report: &crate::driver_probe::Report,
    waited: Duration,
    alive: Option<bool>,
    failure: Option<MissFailure>,
) -> serde_json::Value {
    let source = if report.source == crate::driver_probe::VerdictSource::Cache { "cache" } else { "scan" };
    let slow_start = if failure == Some(MissFailure::NeverConnectedShortWait) { "notRepeated" } else { "used" };
    let mut out = serde_json::json!({
        "verdict": report.verdict.presence.as_str(),
        "source": source,
        "evidence": report.verdict.evidence,
        "handshakeWaitedMs": waited.as_millis() as u64,
        "slowStartWait": slow_start,
    });
    if let Some(failure) = failure {
        out["connected"] = serde_json::json!(false);
        // `null` when the liveness check could not tell.
        out["processAlive"] = serde_json::json!(if failure == MissFailure::ProcessDied { Some(false) } else { alive });
    }
    out
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
             {secs} s for a tapwing_driver handshake with the app running and got none, and the build cannot be \
             shown to start the driver ({evidence}); not waiting again, driving it black-box via the \
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
                name: "a build proven to dial waits the same time (what a miss means is decided later)",
                input: LaunchPlanInput { driver: DriverKnowledge::Proven, ..cold_launch },
                expected: waits.clone(),
            },
            Case {
                name: "--input flutter is the same for a build proven to dial",
                input: LaunchPlanInput {
                    lane_mode: InputMode::Flutter,
                    driver: DriverKnowledge::Proven,
                    ..cold_launch
                },
                expected: LaunchPlan::AcceptHandshake {
                    relaunched: false,
                    expected_pid: Some(300),
                    reject_pids: vec![],
                    can_fall_back: false,
                    accept_timeout: DEFAULT_ACCEPT_TIMEOUT,
                },
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
    fn handshake_timing_defaults_are_the_documented_deadlines() {
        let t = HandshakeTiming::default();
        assert_eq!((t.base, t.slow_start), (Duration::from_secs(15), Duration::from_secs(60)));
        assert_eq!(t.liveness_every, Duration::from_secs(1));
    }

    #[test]
    fn decide_handshake_miss_table() {
        let timing = HandshakeTiming::default();
        let proven = HandshakeMiss {
            driver: DriverKnowledge::Proven,
            alive: Some(true),
            slow_start_spent: false,
            waited: Duration::from_secs(15),
            timing,
        };
        struct Case {
            name: &'static str,
            input: HandshakeMiss,
            expected: MissDecision,
        }
        let cases = [
            Case {
                name: "proven, alive, first miss: wait out the rest of the slow-start deadline",
                input: proven,
                expected: MissDecision::WaitLonger { more: Duration::from_secs(45) },
            },
            Case {
                name: "proven, liveness unknown: still bounded by the same deadline",
                input: HandshakeMiss { alive: None, ..proven },
                expected: MissDecision::WaitLonger { more: Duration::from_secs(45) },
            },
            Case {
                name: "proven, process gone: fail at once, whatever is left of the wait",
                input: HandshakeMiss { alive: Some(false), ..proven },
                expected: MissDecision::Fail(MissFailure::ProcessDied),
            },
            Case {
                name: "proven, process gone, long wait spent: the crash is still the cause",
                input: HandshakeMiss { alive: Some(false), slow_start_spent: true, ..proven },
                expected: MissDecision::Fail(MissFailure::ProcessDied),
            },
            Case {
                name: "proven, long wait already spent in this shard: fail after the short wait",
                input: HandshakeMiss { slow_start_spent: true, ..proven },
                expected: MissDecision::Fail(MissFailure::NeverConnectedShortWait),
            },
            Case {
                name: "proven, the whole slow-start deadline has passed",
                input: HandshakeMiss { waited: Duration::from_secs(60), ..proven },
                expected: MissDecision::Fail(MissFailure::NeverConnected),
            },
            Case {
                name: "proven, waited past the deadline",
                input: HandshakeMiss { waited: Duration::from_secs(61), ..proven },
                expected: MissDecision::Fail(MissFailure::NeverConnected),
            },
            Case {
                name: "unknown or driver linked without a dial path: black-box as before",
                input: HandshakeMiss { driver: DriverKnowledge::Wait, ..proven },
                expected: MissDecision::BlackBox,
            },
            Case {
                name: "unknown with the long wait spent by an earlier proven launch: still black-box",
                input: HandshakeMiss {
                    driver: DriverKnowledge::Wait,
                    slow_start_spent: true,
                    alive: Some(false),
                    ..proven
                },
                expected: MissDecision::BlackBox,
            },
            Case {
                name: "probe still reading when the wait ran out: black-box as before",
                input: HandshakeMiss { driver: DriverKnowledge::Pending, ..proven },
                expected: MissDecision::BlackBox,
            },
            Case {
                name: "no probe: black-box as before",
                input: HandshakeMiss { driver: DriverKnowledge::NotProbed, ..proven },
                expected: MissDecision::BlackBox,
            },
            Case {
                name: "a skip verdict that arrived too late to skip: black-box as before",
                input: HandshakeMiss {
                    driver: DriverKnowledge::Skip(SkipReason::NoDriver { cached: false }),
                    ..proven
                },
                expected: MissDecision::BlackBox,
            },
        ];
        for c in cases {
            assert_eq!(decide_handshake_miss(&c.input), c.expected, "case failed: {}", c.name);
        }
    }

    #[test]
    fn proven_miss_texts_name_the_evidence_the_process_the_deadline_and_the_way_out() {
        let evidence = "found tapwing_driver_hello_v1 in base.apk: lib/arm64-v8a/libapp.so";
        let miss = ProvenMiss {
            app_id: "com.example.app",
            evidence,
            pid: Some(4321),
            alive: Some(true),
            timing: HandshakeTiming::default(),
        };
        let waiting = format_slow_start_warning("[shard 1] ", &miss);
        assert!(
            waiting.starts_with("[shard 1] warning: launchApp com.example.app: no tapwing_driver handshake after 15 s")
        );
        for part in
            [evidence, "the app process 4321 is running", "up to 45 s more", "60 s in total", "no black-box fallback"]
        {
            assert!(waiting.contains(part), "missing {part:?} in {waiting}");
        }

        let never = format_driver_never_connected(&miss, MissFailure::NeverConnected);
        for part in [
            "the installed build contains tapwing_driver",
            evidence,
            "never connected",
            "no handshake within 60 s",
            "the app process 4321 is running",
            "--input native",
            "docs/troubleshooting.md",
        ] {
            assert!(never.contains(part), "missing {part:?} in {never}");
        }
        assert!(!never.contains("not repeated"), "{never}");

        let short = format_driver_never_connected(&miss, MissFailure::NeverConnectedShortWait);
        for part in ["no handshake within 15 s", "(60 s) was not repeated", "already waited that long", evidence] {
            assert!(short.contains(part), "missing {part:?} in {short}");
        }

        let unknown = ProvenMiss { alive: None, ..miss };
        assert!(
            format_driver_never_connected(&unknown, MissFailure::NeverConnected)
                .contains("the app process 4321 could not be checked")
        );
        let no_pid = ProvenMiss { pid: None, alive: None, ..miss };
        assert!(
            format_driver_never_connected(&no_pid, MissFailure::NeverConnected)
                .contains("the app's process id is unknown")
        );

        let died = format_driver_never_connected(&ProvenMiss { alive: Some(false), ..miss }, MissFailure::ProcessDied);
        assert!(died.contains("the app process 4321 died before tapwing_driver connected"), "{died}");
        assert!(died.contains(evidence) && !died.contains("troubleshooting"), "{died}");

        let note = slow_start_note(evidence, Duration::from_millis(23_400), HandshakeTiming::default());
        assert!(note.starts_with("tapwing_driver connected after 23.4 s, past the usual 15 s"), "{note}");
        assert!(note.contains(evidence), "{note}");
    }

    #[test]
    fn driver_present_json_says_what_the_wait_was_and_how_it_ended() {
        use crate::driver_probe::{Presence, Report, Verdict, VerdictSource};
        let report = |source| Report {
            verdict: Verdict { presence: Presence::Present, dials: true, evidence: "e".into() },
            source,
            fingerprint: Some("f".into()),
            learned_misses: None,
        };
        let late = driver_present_json(&report(VerdictSource::Scan), Duration::from_millis(23_400), Some(true), None);
        assert_eq!(
            late,
            serde_json::json!({
                "verdict": "present", "source": "scan", "evidence": "e",
                "handshakeWaitedMs": 23_400, "slowStartWait": "used",
            })
        );
        let never = driver_present_json(
            &report(VerdictSource::Cache),
            Duration::from_secs(60),
            Some(true),
            Some(MissFailure::NeverConnected),
        );
        assert_eq!(
            (never["source"].as_str(), never["slowStartWait"].as_str(), &never["connected"], &never["processAlive"]),
            (Some("cache"), Some("used"), &serde_json::json!(false), &serde_json::json!(true))
        );
        let short = driver_present_json(
            &report(VerdictSource::Scan),
            Duration::from_secs(15),
            None,
            Some(MissFailure::NeverConnectedShortWait),
        );
        assert_eq!(short["slowStartWait"], "notRepeated");
        assert!(short["processAlive"].is_null() && short.get("processAlive").is_some(), "{short}");
        let died = driver_present_json(
            &report(VerdictSource::Scan),
            Duration::from_secs(15),
            None,
            Some(MissFailure::ProcessDied),
        );
        assert_eq!(died["processAlive"], false);
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
