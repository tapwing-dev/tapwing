//! Hybrid routing between the in-app Flutter driver and the platform native
//! lanes (Android UiAutomator gRPC and iOS Simulator XCTest HTTP).

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::backend::{ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest};
use crate::device::Platform;
use crate::driver::DriverError;
use crate::flow::ParsedFlow;
use crate::flow::ir::{ElementSelector, KeyCode, MaestroCommand};
use crate::ios_native_lane::{IosNativeClient, IosNativeLaneHandle, IosNativeOps, SPRINGBOARD_BUNDLE_ID};
use crate::native_lane::{
    BlackboxDemotion, BlackboxTracker, ColdStart, FOCUS_PROBE, InputMode, Lane, NativeLaneHandle, NativeOps, RouteCtx,
    decide_lane, is_external_window, parse_focused_package,
};

use super::DeviceFlutterBackend;
use super::mapping::{android_keycode, ios_native_key_name};

/// Reactive Flutter wait before checking who owns the screen (`auto` mode).
pub(super) const FLUTTER_PROBE_MS: u64 = 400;
/// TTL for cached Android `mCurrentFocus` probes. Invalidated immediately on
/// every mutating command so only consecutive read-only checks share a probe.
const FOCUS_CACHE_TTL: Duration = Duration::from_millis(200);
/// Longer Flutter probe on iOS when `xcodebuild` has not been started yet
/// (lazy mode), so normal async Flutter transitions (~600 ms logins, spinners)
/// never spawn `xcodebuild` unnecessarily.
const IOS_LAZY_PROBE_MS: u64 = 2000;
/// How long a command may wait for the Android native lane's background warm-up
/// (first run installs the driver APKs).
const NATIVE_WAIT: Duration = Duration::from_secs(45);
/// How long a command may wait for the iOS XCTest runner's warm-up.
const IOS_NATIVE_WAIT: Duration = Duration::from_secs(65);
/// Native look at an element the Flutter driver cannot see (platform views, WebViews).
const NATIVE_FALLBACK_MS: u64 = 1000;
/// With the iOS overlay hint set, how long a Flutter miss looks natively before
/// the Flutter lane gets the rest of the budget.
const OVERLAY_NATIVE_SLICE_MS: u64 = 2500;
/// How long to wait for `tapwing_driver`'s handshake before treating the app
/// as uninstrumented, when the native lane can take over.
pub(super) const HANDSHAKE_FALLBACK_TIMEOUT: Duration = Duration::from_secs(15);
/// Cold-start retry of the native lane: the one extra window the first
/// required lookup after `launchApp` gets when the app never showed up.
pub(super) const COLD_START_EXTRA: Duration = Duration::from_secs(10);

/// Optional iOS Simulator XCTest native lane slot.
///
/// Stays [`IosNativeSlot::Lazy`] on pure-Flutter iOS runs so `xcodebuild` is
/// never spawned (0 ms startup, 0 CPU contention, 0 ms `simctl terminate` on
/// drop). Transitions to [`IosNativeSlot::Started`] on first native need, or
/// starts eagerly when `--input native` / static native commands are present.
pub enum IosNativeSlot {
    Lazy { udid: String, blackbox: BlackboxTracker },
    Started(IosNativeLaneHandle),
}

impl IosNativeSlot {
    pub fn lazy(udid: impl Into<String>) -> Self {
        Self::Lazy { udid: udid.into(), blackbox: BlackboxTracker::default() }
    }

    pub fn spawn(udid: &str) -> Self {
        Self::Started(IosNativeLaneHandle::spawn(udid))
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Started(IosNativeLaneHandle::unavailable(reason))
    }

    pub fn is_started(&self) -> bool {
        matches!(self, Self::Started(_))
    }

    pub fn is_usable(&self) -> bool {
        match self {
            Self::Lazy { .. } => true,
            Self::Started(h) => h.is_usable(),
        }
    }

    pub fn failure(&self) -> Option<&str> {
        match self {
            Self::Lazy { .. } => None,
            Self::Started(h) => h.failure(),
        }
    }

    pub fn take_failure_notice(&mut self) -> Option<String> {
        match self {
            Self::Lazy { .. } => None,
            Self::Started(h) => h.take_failure_notice(),
        }
    }

    pub fn is_sticky_blackbox(&self, app_id: &str) -> bool {
        match self {
            Self::Lazy { blackbox, .. } => blackbox.is_sticky(app_id),
            Self::Started(h) => h.is_sticky_blackbox(app_id),
        }
    }

    pub fn record_handshake_miss(&mut self, app_id: &str) -> BlackboxDemotion {
        match self {
            Self::Lazy { blackbox, .. } => blackbox.record_handshake_miss(app_id),
            Self::Started(h) => h.record_handshake_miss(app_id),
        }
    }

    pub fn record_handshake_success(&mut self, app_id: &str) {
        match self {
            Self::Lazy { blackbox, .. } => blackbox.record_handshake_success(app_id),
            Self::Started(h) => h.record_handshake_success(app_id),
        }
    }

    pub fn ensure_started(&mut self) -> &mut IosNativeLaneHandle {
        if let Self::Lazy { udid, blackbox } = self {
            let mut handle = IosNativeLaneHandle::spawn(udid);
            handle.set_blackbox_tracker(std::mem::take(blackbox));
            *self = Self::Started(handle);
        }
        match self {
            Self::Started(h) => h,
            Self::Lazy { .. } => unreachable!(),
        }
    }

    pub fn try_get(&mut self) -> Option<&mut IosNativeClient> {
        match self {
            Self::Lazy { .. } => None,
            Self::Started(h) => h.try_get(),
        }
    }

    pub fn get_or_wait(&mut self, timeout: Duration) -> Result<&mut IosNativeClient> {
        self.ensure_started().get_or_wait(timeout)
    }

    /// One line per time the runner was lost and restarted since the last call.
    pub fn take_restart_notices(&mut self) -> Vec<String> {
        match self {
            Self::Lazy { .. } => Vec::new(),
            Self::Started(h) => h.take_restart_notices(),
        }
    }
}

/// Returns `true` when any command in `flows` statically requires the iOS
/// XCTest native lane (`setOrientation`, `pressKey: Home | Lock`), so the CLI
/// can pre-warm `xcodebuild` in the background before the first step runs.
pub fn flows_need_ios_native(flows: &[ParsedFlow]) -> bool {
    fn scan(cmds: &[MaestroCommand]) -> bool {
        cmds.iter().any(|c| match c {
            MaestroCommand::SetOrientation(_) => true,
            MaestroCommand::PressKey(pk) => matches!(pk.code, KeyCode::Home | KeyCode::Lock),
            MaestroCommand::RunFlow(rf) => scan(&rf.commands),
            MaestroCommand::Repeat(r) => scan(&r.commands),
            MaestroCommand::Retry(r) => scan(&r.commands),
            _ => false,
        })
    }
    flows.iter().any(|f| {
        scan(&f.commands)
            || f.config.on_flow_start.as_ref().is_some_and(|h| scan(&h.commands))
            || f.config.on_flow_complete.as_ref().is_some_and(|h| scan(&h.commands))
    })
}

/// Unified native-lane command executor across Android (UiAutomator gRPC) and
/// iOS Simulator (XCTest HTTP).
pub(super) enum PlatformNativeOps<'a> {
    Android(NativeOps<'a>),
    Ios(IosNativeOps<'a>),
}

impl PlatformNativeOps<'_> {
    pub(super) fn tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.tap_on_element(req),
            Self::Ios(ops) => ops.tap_on_element(req),
        }
    }

    pub(super) fn tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.tap_on_point(req),
            Self::Ios(ops) => ops.tap_on_point(req),
        }
    }

    pub(super) fn assert_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.assert_visible(sel, timeout_ms),
            Self::Ios(ops) => ops.assert_visible(sel, timeout_ms),
        }
    }

    pub(super) fn assert_not_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.assert_not_visible(sel, timeout_ms),
            Self::Ios(ops) => ops.assert_not_visible(sel, timeout_ms),
        }
    }

    pub(super) fn find_element_bounds(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<[i64; 4]> {
        match self {
            Self::Android(ops) => ops.find_element_bounds(sel, timeout_ms),
            Self::Ios(ops) => ops.find_element_bounds(sel, timeout_ms),
        }
    }

    pub(super) fn copy_text_from(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<(String, Value)> {
        match self {
            Self::Android(ops) => ops.copy_text_from(sel, timeout_ms),
            Self::Ios(ops) => ops.copy_text_from(sel, timeout_ms),
        }
    }

    pub(super) fn input_text(&mut self, text: &str) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.input_text(text),
            Self::Ios(ops) => ops.input_text(text),
        }
    }

    pub(super) fn erase_text(&mut self, characters: i32) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.erase_text(characters),
            Self::Ios(ops) => ops.erase_text(characters),
        }
    }

    pub(super) fn back_press(&mut self) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.back_press(),
            Self::Ios(_) => Ok(json!({ "noop": true, "lane": "native", "note": "IOSDriver.backPress() is a no-op" })),
        }
    }

    pub(super) fn hide_keyboard(&mut self, strict: bool) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.hide_keyboard(),
            Self::Ios(ops) => ops.hide_keyboard_with(strict),
        }
    }

    pub(super) fn scroll(&mut self) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.scroll(),
            Self::Ios(ops) => ops.scroll(),
        }
    }

    pub(super) fn swipe(&mut self, req: &SwipeRequest) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.swipe(req),
            Self::Ios(ops) => ops.swipe(req),
        }
    }

    pub(super) fn scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.scroll_until_visible(req),
            Self::Ios(ops) => ops.scroll_until_visible(req),
        }
    }

    pub(super) fn wait_for_animation_to_end(&mut self, timeout_ms: Option<u64>) -> Result<Value> {
        match self {
            Self::Android(ops) => ops.wait_for_animation_to_end(timeout_ms),
            Self::Ios(ops) => ops.wait_for_animation_to_end(timeout_ms),
        }
    }

    /// Settles after a launch; `false` when, on Android, `app_id` shows
    /// nothing on screen yet (always `true` on iOS, which has no cold-start retry).
    pub(super) fn settle_launch(&mut self, app_id: &str) -> Result<bool> {
        match self {
            Self::Android(ops) => ops.settle(None, Some(3000)).map(|h| h.shows_package(app_id)),
            Self::Ios(ops) => ops.settle(None, Some(3000)).map(|_| true),
        }
    }
}

/// Commands executed per lane in one flow (reported as `lane` / `laneStats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaneStats {
    pub flutter: u32,
    pub native: u32,
    /// Bounded cold-start retries taken in this flow (a launch handshake
    /// that was re-sent, or a first native lookup that got its extra window).
    pub cold_start_retries: u32,
}

impl LaneStats {
    /// `flutter`, `native`, or `hybrid` when both lanes ran commands.
    pub fn lane_name(&self) -> &'static str {
        match (self.flutter, self.native) {
            (_, 0) => "flutter",
            (0, _) => "native",
            _ => "hybrid",
        }
    }
}

/// Error raised by [`DeviceFlutterBackend::ensure_connected`] when a step runs
/// without an active `tapwing_driver` connection (e.g. a flow starting without
/// `launchApp` on an uninstrumented or external app).
#[derive(Debug)]
pub(super) struct DriverNotAttachedError(pub(super) String);

impl std::fmt::Display for DriverNotAttachedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DriverNotAttachedError {}

pub(super) fn is_driver_not_attached(e: &anyhow::Error) -> bool {
    e.is::<DriverNotAttachedError>()
}

/// Driver misses that may mean "the element is in another window": the
/// Flutter lane's `ELEMENT_NOT_FOUND` and a failed assertion.
pub(super) fn is_lookup_miss(e: &anyhow::Error) -> bool {
    e.downcast_ref::<DriverError>().is_some_and(|d| matches!(d.code.as_str(), "ELEMENT_NOT_FOUND" | "ASSERTION"))
}

pub(super) fn is_flutter_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<DriverError>().is_some_and(|d| d.code == "ELEMENT_NOT_FOUND")
}

/// Result of probing which window currently owns input focus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FocusProbe {
    /// A system dialog or foreign app owns focus.
    External(String),
    /// The flow's own app affirmatively owns focus.
    InApp,
    /// Focus could not be determined (e.g. lazy iOS slot not started,
    /// `mCurrentFocus=null`, or probe failure).
    Unknown,
}

/// Classifies a reported foreground package/bundle against `flow_app`.
pub(super) fn classify_focused_package(focused: &str, flow_app: &str) -> FocusProbe {
    if focused.is_empty() {
        FocusProbe::Unknown
    } else if is_external_window(focused, flow_app) {
        FocusProbe::External(focused.to_string())
    } else {
        FocusProbe::InApp
    }
}

impl DeviceFlutterBackend<'_> {
    pub(super) fn lane_mode(&self) -> InputMode {
        self.opts.input_mode
    }

    pub(super) fn native_usable(&self) -> bool {
        match self.platform() {
            Platform::Android => self.native.as_deref().is_some_and(NativeLaneHandle::is_usable),
            Platform::Ios => self.ios_native.as_deref().is_some_and(IosNativeSlot::is_usable),
        }
    }

    /// Whether `app_id` was promoted to sticky shard-wide black-box after
    /// consecutive missed `launchApp` handshakes.
    pub(super) fn is_sticky_blackbox_app(&self, app_id: &str) -> bool {
        match self.platform() {
            Platform::Android => self.native.as_deref().is_some_and(|n| n.is_sticky_blackbox(app_id)),
            Platform::Ios => self.ios_native.as_deref().is_some_and(|s| s.is_sticky_blackbox(app_id)),
        }
    }

    /// Whether `app_id` is currently black-box (either for this flow or sticky
    /// for the rest of the shard).
    pub(super) fn is_blackbox_app(&self, app_id: &str) -> bool {
        self.flow_blackbox_apps.contains(app_id) || self.is_sticky_blackbox_app(app_id)
    }

    /// Whether the active flow app is currently black-box.
    pub(super) fn is_blackbox(&self) -> bool {
        self.is_blackbox_app(&self.flow_app)
    }

    pub(super) fn mark_flow_blackbox(&mut self, app_id: &str) {
        if !app_id.is_empty() {
            self.flow_blackbox_apps.insert(app_id.to_string());
        }
    }

    pub(super) fn clear_flow_blackbox(&mut self, app_id: &str) {
        self.flow_blackbox_apps.remove(app_id);
    }

    /// Marks `app_id` black-box for this flow and updates the shard-wide
    /// consecutive handshake-miss counter.
    pub(super) fn record_handshake_miss(&mut self, app_id: &str) -> BlackboxDemotion {
        self.mark_flow_blackbox(app_id);
        match self.platform() {
            Platform::Android => self
                .native
                .as_deref_mut()
                .map_or(BlackboxDemotion::FlowOnly { consecutive_misses: 1 }, |n| n.record_handshake_miss(app_id)),
            Platform::Ios => self
                .ios_native
                .as_deref_mut()
                .map_or(BlackboxDemotion::FlowOnly { consecutive_misses: 1 }, |s| s.record_handshake_miss(app_id)),
        }
    }

    /// Clears flow-scoped black-box and resets the shard-wide consecutive-miss
    /// counter after a healthy Flutter handshake.
    pub(super) fn record_handshake_success(&mut self, app_id: &str) {
        self.clear_flow_blackbox(app_id);
        match self.platform() {
            Platform::Android => {
                if let Some(n) = self.native.as_deref_mut() {
                    n.record_handshake_success(app_id);
                }
            }
            Platform::Ios => {
                if let Some(s) = self.ios_native.as_deref_mut() {
                    s.record_handshake_success(app_id);
                }
            }
        }
    }

    fn ios_app_ids(&self) -> Vec<String> {
        let mut ids = Vec::with_capacity(2);
        if !self.flow_app.is_empty() {
            ids.push(self.flow_app.clone());
        }
        ids.push(SPRINGBOARD_BUNDLE_ID.to_string());
        ids
    }

    /// Clears any cached Android `mCurrentFocus` probe (called on every
    /// mutating command and before final post-miss re-checks).
    pub(super) fn invalidate_focus_cache(&mut self) {
        self.focus_cache = None;
    }

    fn android_focus_probe(&mut self) -> FocusProbe {
        if let Some((cached, at)) = &self.focus_cache
            && at.elapsed() < FOCUS_CACHE_TTL
        {
            return cached.clone();
        }
        let probe = (|| {
            let android = self.dev.as_android()?;
            let out = android.shell(FOCUS_PROBE).ok()?;
            let pkg = parse_focused_package(&out)?;
            Some(classify_focused_package(&pkg, &self.flow_app))
        })()
        .unwrap_or(FocusProbe::Unknown);
        // Only cache affirmative results (`InApp` / `External`), never transient
        // `Unknown` states (`mCurrentFocus=null` mid-transition or adb hiccup).
        if probe != FocusProbe::Unknown {
            self.focus_cache = Some((probe.clone(), Instant::now()));
        }
        probe
    }

    /// Queries the iOS XCTest runner for a SpringBoard alert or foreground app.
    /// When `allow_lazy_spawn` is `true`, starts a `Lazy` runner on demand and
    /// waits up to `max_wait`; otherwise only checks an already-`Started`
    /// runner, waiting up to `max_wait` (or polling non-blockingly via
    /// `try_get()` when `max_wait.is_zero()`).
    fn ios_focus_probe(&mut self, max_wait: Duration, allow_lazy_spawn: bool) -> FocusProbe {
        let app_ids = self.ios_app_ids();
        let flow_app = self.flow_app.clone();
        let Some(slot) = self.ios_native.as_deref_mut() else {
            return FocusProbe::Unknown;
        };
        let Some(client) = (if allow_lazy_spawn || (!max_wait.is_zero() && slot.is_started()) {
            slot.get_or_wait(max_wait).ok()
        } else {
            slot.try_get()
        }) else {
            return FocusProbe::Unknown;
        };
        let mut ops = IosNativeOps::new(client, &app_ids);
        let classify_fg = |ops: &mut IosNativeOps<'_>| match ops.foreground_app_id() {
            Ok(fg) => classify_focused_package(&fg, &flow_app),
            Err(_) => FocusProbe::Unknown,
        };
        let probe = match ops.has_springboard_alert() {
            Ok(true) => FocusProbe::External(SPRINGBOARD_BUNDLE_ID.to_string()),
            Ok(false) => classify_fg(&mut ops),
            // The runner answered with an error (say, an accessibility snapshot that failed):
            // the foreground app can still be asked.
            Err(e) if crate::ios_native_lane::is_runner_answer(&e) => classify_fg(&mut ops),
            // A dead or hung runner: a second question would restart it again, or wait out
            // another timeout, for a probe that only routes a command.
            Err(_) => FocusProbe::Unknown,
        };
        self.report_native_restarts();
        probe
    }

    fn ios_external_focus(&mut self, max_wait: Duration, allow_lazy_spawn: bool) -> Option<String> {
        match self.ios_focus_probe(max_wait, allow_lazy_spawn) {
            FocusProbe::External(app) => Some(app),
            FocusProbe::InApp | FocusProbe::Unknown => None,
        }
    }

    /// Probes which window owns input focus without cold-spawning lazy iOS
    /// `xcodebuild`, bounding any wait on an already-started iOS runner to
    /// `ios_max_wait`.
    pub(super) fn probe_focus_within(&mut self, ios_max_wait: Duration) -> FocusProbe {
        match self.platform() {
            Platform::Android => self.android_focus_probe(),
            Platform::Ios => self.ios_focus_probe(ios_max_wait, false),
        }
    }

    /// Probes which window owns input focus without cold-spawning lazy iOS `xcodebuild`.
    pub(super) fn probe_focus(&mut self) -> FocusProbe {
        self.probe_focus_within(IOS_NATIVE_WAIT)
    }

    /// Package / bundle owning the focused window when it is not the flow's app,
    /// bounding any wait on an already-started iOS runner to `ios_max_wait`.
    fn external_focus_within(&mut self, ios_max_wait: Duration) -> Option<String> {
        match self.probe_focus_within(ios_max_wait) {
            FocusProbe::External(app) => Some(app),
            FocusProbe::InApp | FocusProbe::Unknown => None,
        }
    }

    /// Prints, once each, the times a native driver (Android driver or iOS
    /// runner) lost its connection and was restarted (a restart that worked
    /// never fails the run).
    fn report_native_restarts(&mut self) {
        let mut notices = self.native.as_deref_mut().map(NativeLaneHandle::take_restart_notices).unwrap_or_default();
        notices.extend(self.ios_native.as_deref_mut().map(IosNativeSlot::take_restart_notices).unwrap_or_default());
        for notice in notices {
            eprintln!("warning: {notice}");
        }
    }

    /// Prints the native lane's warm-up failure once (never fails the run).
    pub(super) fn warn_native_unavailable(&mut self) {
        let notice = match self.platform() {
            Platform::Android => self.native.as_deref_mut().and_then(NativeLaneHandle::take_failure_notice),
            Platform::Ios => self.ios_native.as_deref_mut().and_then(IosNativeSlot::take_failure_notice),
        };
        if let Some(why) = notice {
            eprintln!(
                "warning: native lane unavailable ({why}); system dialogs, WebViews and uninstrumented apps \
                 cannot be driven in this run"
            );
        }
    }

    /// Lane for a command without an element lookup. `check_focus` asks the
    /// platform which window has focus; otherwise only a remembered system
    /// window (`native_hint`) or an unattached Flutter driver triggers that
    /// look, so the connected Flutter fast path pays nothing.
    pub(super) fn decide(&mut self, check_focus: bool) -> Lane {
        let mode = self.lane_mode();
        if mode != InputMode::Auto || !self.native_usable() {
            return decide_lane(&RouteCtx { mode, blackbox: false, flow_app: &self.flow_app, focused_package: None });
        }
        let blackbox = self.is_blackbox();
        if !blackbox && self.ios_overlay_active() {
            return Lane::Native;
        }
        let should_probe_focus = !blackbox && (check_focus || self.native_hint || !self.drv.is_connected());
        let focused = if should_probe_focus { self.focused_package_now() } else { None };
        let lane =
            decide_lane(&RouteCtx { mode, blackbox, flow_app: &self.flow_app, focused_package: focused.as_deref() });
        self.native_hint = lane == Lane::Native && !blackbox;
        lane
    }

    /// iOS only: the last element lookup was served by the native lane alone
    /// (see [`DeviceFlutterBackend::overlay_hint`]) and the runner is warm.
    fn ios_overlay_active(&self) -> bool {
        self.overlay_hint
            && self.platform() == Platform::Ios
            && self.ios_native.as_deref().is_some_and(IosNativeSlot::is_started)
    }

    /// An element Flutter does not know was found by the native lane: on iOS
    /// remember it (see [`DeviceFlutterBackend::overlay_hint`]).
    fn note_native_fallback(&mut self) {
        if self.platform() == Platform::Ios {
            self.overlay_hint = true;
        }
    }

    /// Records what happened when a command ran without an attached Flutter
    /// driver (`DriverNotAttachedError`):
    /// - External window owns focus -> remember `native_hint`.
    /// - Our own app affirmatively owns focus -> mark flow-scoped black-box
    ///   (without touching the shard-wide consecutive `launchApp` miss counter).
    /// - Unknown focus (e.g. lazy iOS slot where focus isn't probed, or
    ///   `mCurrentFocus=null`) -> fall back for this command only, never mark black-box.
    fn record_unattached_fallback(&mut self) {
        match self.probe_focus() {
            FocusProbe::External(_) => {
                self.native_hint = true;
            }
            FocusProbe::InApp => {
                let app = self.flow_app.clone();
                self.mark_flow_blackbox(&app);
            }
            FocusProbe::Unknown => {}
        }
    }

    fn focused_package_now(&mut self) -> Option<String> {
        match self.platform() {
            Platform::Android => match self.android_focus_probe() {
                FocusProbe::External(pkg) => Some(pkg),
                FocusProbe::InApp => Some(self.flow_app.clone()),
                FocusProbe::Unknown => None,
            },
            // On iOS, never spawn `xcodebuild` just to probe focus; only join a
            // started runner when an external window was already remembered.
            Platform::Ios => {
                let max_wait = if self.native_hint { IOS_NATIVE_WAIT } else { Duration::ZERO };
                self.ios_external_focus(max_wait, false)
            }
        }
    }

    /// Runs `f` against the active platform's native lane, joining warm-up on first use.
    pub(super) fn run_native<T>(&mut self, f: impl FnOnce(&mut PlatformNativeOps<'_>) -> Result<T>) -> Result<T> {
        match self.platform() {
            Platform::Android => {
                let handle = self
                    .native
                    .as_deref_mut()
                    .ok_or_else(|| anyhow!("the native lane is not enabled in this session"))?;
                let adb = handle.adb().cloned().ok_or_else(|| {
                    anyhow!("the native lane is unavailable: {}", handle.failure().unwrap_or("no device"))
                })?;
                let client = handle.get_or_wait(NATIVE_WAIT)?;
                self.lanes.native += 1;
                let res = f(&mut PlatformNativeOps::Android(NativeOps::new(client, &adb)));
                self.report_native_restarts();
                res
            }
            Platform::Ios => {
                let app_ids = self.ios_app_ids();
                let slot = self
                    .ios_native
                    .as_deref_mut()
                    .ok_or_else(|| anyhow!("the iOS native lane is not enabled in this session"))?;
                let client = slot.get_or_wait(IOS_NATIVE_WAIT)?;
                self.lanes.native += 1;
                let res = f(&mut PlatformNativeOps::Ios(IosNativeOps::new(client, &app_ids)));
                self.report_native_restarts();
                res
            }
        }
    }

    /// Like [`Self::run_native`] but only when the lane is already warm.
    /// Counts toward `lanes.native` only when the probe succeeds, so a missed
    /// fallback on a pure-Flutter optional lookup does not misreport `hybrid`.
    fn try_native<T>(&mut self, f: impl FnOnce(&mut PlatformNativeOps<'_>) -> Result<T>) -> Option<Result<T>> {
        match self.platform() {
            Platform::Android => {
                let handle = self.native.as_deref_mut()?;
                let adb = handle.adb().cloned()?;
                let client = handle.try_get()?;
                let res = f(&mut PlatformNativeOps::Android(NativeOps::new(client, &adb)));
                if res.is_ok() {
                    self.lanes.native += 1;
                }
                self.report_native_restarts();
                Some(res)
            }
            Platform::Ios => {
                let app_ids = self.ios_app_ids();
                let slot = self.ios_native.as_deref_mut()?;
                let client = slot.try_get()?;
                let res = f(&mut PlatformNativeOps::Ios(IosNativeOps::new(client, &app_ids)));
                if res.is_ok() {
                    self.lanes.native += 1;
                }
                self.report_native_restarts();
                Some(res)
            }
        }
    }

    /// F8: captures the active lane's UI hierarchy for rich failure diagnostics
    /// without incrementing `lanes` stats or cold-spawning lazy iOS `xcodebuild`.
    pub(super) fn capture_failure_hierarchy_impl(&mut self) -> Option<(String, crate::fake_driver::TreeNode)> {
        let prefer_native =
            self.lane_mode() == InputMode::Native || self.is_blackbox() || self.native_hint || self.overlay_hint;
        if !prefer_native
            && self.drv.is_connected()
            && let Ok(snap) = self.drv.call("snapshot", json!({}))
            && let Some(root) = crate::failure::tree_from_flutter_snapshot(&snap)
        {
            return Some(("flutter".to_string(), root));
        }

        // Native fallback: query the active native lane when already warm (or when
        // explicitly driving native/blackbox), never cold-spawning lazy iOS XCTest.
        let native_root = match self.platform() {
            Platform::Android => {
                let root = {
                    let handle = self.native.as_deref_mut()?;
                    let adb = handle.adb().cloned()?;
                    let client =
                        if prefer_native { handle.get_or_wait(NATIVE_WAIT).ok()? } else { handle.try_get()? };
                    NativeOps::new(client, &adb).hierarchy().ok().map(|h| h.root)
                };
                self.report_native_restarts();
                root
            }
            Platform::Ios => {
                let root = {
                    let app_ids = self.ios_app_ids();
                    let slot = self.ios_native.as_deref_mut()?;
                    let client = if prefer_native && slot.is_started() {
                        slot.get_or_wait(IOS_NATIVE_WAIT).ok()?
                    } else {
                        slot.try_get()?
                    };
                    IosNativeOps::new(client, &app_ids).hierarchy().ok().map(|h| h.root)
                };
                self.report_native_restarts();
                root
            }
        };
        if let Some(root) = native_root {
            return Some(("native".to_string(), root));
        }

        // Last resort if native was preferred but unavailable while Flutter is still connected.
        if prefer_native
            && self.drv.is_connected()
            && let Ok(snap) = self.drv.call("snapshot", json!({}))
            && let Some(root) = crate::failure::tree_from_flutter_snapshot(&snap)
        {
            return Some(("flutter".to_string(), root));
        }
        None
    }

    /// Runs an Android-only native operation (e.g. physical-device `setLocation`).
    pub(super) fn run_android_native<T>(&mut self, f: impl FnOnce(&mut NativeOps<'_>) -> Result<T>) -> Result<T> {
        let handle =
            self.native.as_deref_mut().ok_or_else(|| anyhow!("the native lane is not enabled in this session"))?;
        let adb = handle
            .adb()
            .cloned()
            .ok_or_else(|| anyhow!("the native lane is unavailable: {}", handle.failure().unwrap_or("no device")))?;
        let client = handle.get_or_wait(NATIVE_WAIT)?;
        self.lanes.native += 1;
        let res = f(&mut NativeOps::new(client, &adb));
        self.report_native_restarts();
        res
    }

    /// Runs an iOS-only native operation (`setOrientation`, `Home`/`Lock`).
    pub(super) fn run_ios_native<T>(&mut self, f: impl FnOnce(&mut IosNativeOps<'_>) -> Result<T>) -> Result<T> {
        let app_ids = self.ios_app_ids();
        let slot = self
            .ios_native
            .as_deref_mut()
            .ok_or_else(|| anyhow!("the iOS native lane is not enabled in this session"))?;
        let client = slot.get_or_wait(IOS_NATIVE_WAIT)?;
        self.lanes.native += 1;
        let res = f(&mut IosNativeOps::new(client, &app_ids));
        self.report_native_restarts();
        res
    }

    /// Element command with the hybrid lookup policy.
    ///
    /// * `flutter`: Flutter driver only. `native`: native lane only.
    /// * `auto`: Flutter answers first (a short reactive probe, so visible
    ///   elements cost nothing extra). On a miss, one focus lookup decides:
    ///   a foreign window (permission dialog, another app) goes native; an
    ///   in-app miss keeps waiting on Flutter, then takes one short native
    ///   look for platform views / WebViews when the lane is warm.
    /// * On iOS when `xcodebuild` is still `Lazy` (pure-Flutter flows), the
    ///   initial Flutter probe extends to `IOS_LAZY_PROBE_MS` (2000 ms) so
    ///   ordinary async Flutter transitions never pay a cold `xcodebuild` start.
    pub(super) fn route_selector<T>(
        &mut self,
        timeout_ms: u64,
        negative: bool,
        flutter: impl Fn(&mut Self, u64) -> Result<T>,
        native: impl Fn(&mut PlatformNativeOps<'_>, u64) -> Result<T>,
    ) -> Result<T> {
        self.route_selector_opt(timeout_ms, negative, false, flutter, native)
    }

    /// [`Self::route_selector`] for a lookup that may be `optional`: an optional
    /// lookup never uses up the launch's one cold-start retry.
    pub(super) fn route_selector_opt<T>(
        &mut self,
        timeout_ms: u64,
        negative: bool,
        optional: bool,
        flutter: impl Fn(&mut Self, u64) -> Result<T>,
        native: impl Fn(&mut PlatformNativeOps<'_>, u64) -> Result<T>,
    ) -> Result<T> {
        match self.lane_mode() {
            InputMode::Flutter => {
                self.lanes.flutter += 1;
                return flutter(self, timeout_ms);
            }
            InputMode::Native => return self.native_lookup(timeout_ms, negative, optional, native),
            InputMode::Auto => {}
        }
        if !self.native_usable() {
            self.lanes.flutter += 1;
            return flutter(self, timeout_ms);
        }
        if self.is_blackbox() {
            return self.native_lookup(timeout_ms, negative, optional, native);
        }
        // (Not under the iOS overlay hint: Flutter looks first, so Flutter
        // elements are found again, and clear the hint, once the sheet is gone.)
        // For `assertNotVisible` (`negative == true`) with a connected Flutter
        // driver and no `native_hint`, ask Flutter first: if it immediately
        // reports absence (`Ok`), one cached non-blocking focus check catches
        // system dialogs without paying a pre-Flutter probe on waiting checks.
        if (self.native_hint || !self.drv.is_connected())
            && !self.ios_overlay_active()
            && self.decide(true) == Lane::Native
        {
            return self.run_native(|ops| native(ops, timeout_ms));
        }

        let ios_lazy = self.platform() == Platform::Ios && self.ios_native.as_deref().is_some_and(|s| !s.is_started());
        let probe = timeout_ms.min(if ios_lazy { IOS_LAZY_PROBE_MS } else { FLUTTER_PROBE_MS });
        let started = Instant::now();
        let left = |started: Instant| timeout_ms.saturating_sub(started.elapsed().as_millis() as u64);
        self.lanes.flutter += 1;
        let first = flutter(self, probe);
        let Err(miss) = first else {
            if negative && !self.ios_overlay_active() {
                let remaining = left(started);
                if self.external_focus_within(Duration::ZERO).is_some() {
                    self.lanes.flutter = self.lanes.flutter.saturating_sub(1);
                    self.native_hint = true;
                    return self.run_native(|ops| native(ops, remaining));
                }
            }
            // Flutter owns the screen again.
            self.overlay_hint = false;
            return first;
        };
        if is_driver_not_attached(&miss) {
            self.lanes.flutter = self.lanes.flutter.saturating_sub(1);
            let remaining = left(started);
            self.record_unattached_fallback();
            return self.run_native(|ops| native(ops, remaining));
        }
        if !is_lookup_miss(&miss) {
            return Err(miss);
        }
        // iOS: the element was last found natively and no system app is in
        // front (XCTest reports the app itself, e.g. under a web-auth sheet), so
        // look natively right away instead of waiting out the Flutter budget.
        if !negative && self.ios_overlay_active() {
            let slice = left(started).min(OVERLAY_NATIVE_SLICE_MS);
            match self.try_native(|ops| native(ops, slice)) {
                Some(Ok(v)) => return Ok(v),
                Some(Err(e)) if crate::interrupt::is_interrupted_error(&e) => return Err(e),
                _ => {}
            }
        }
        // iOS, lane still warming up (either Lazy about to spawn, or eagerly
        // Started and still Pending): overlap Flutter lookup slices with runner
        // warm-up so cold startup never blocks past the lookup budget.
        let ios_pending = self.platform() == Platform::Ios
            && self.ios_native.as_deref_mut().is_some_and(|s| s.is_started() && s.try_get().is_none() && s.is_usable());
        let prewarmed = self.platform() == Platform::Ios
            && !negative
            && ((ios_lazy && timeout_ms > IOS_LAZY_PROBE_MS) || ios_pending);
        if prewarmed {
            if ios_lazy && let Some(slot) = self.ios_native.as_deref_mut() {
                slot.ensure_started();
            }
            let warm = wait_on_flutter_while_warming(
                || left(started),
                |s: &mut Self| {
                    s.ios_native.as_deref_mut().is_none_or(|slot| slot.try_get().is_some() || !slot.is_usable())
                },
                self,
                &flutter,
            );
            match warm {
                WarmWait::Found(v) => {
                    self.overlay_hint = false;
                    return Ok(v);
                }
                WarmWait::Failed(e) => return Err(e),
                WarmWait::Settled => {}
            }
        }
        // Snapshot remaining budget BEFORE any focus probe that may wait on
        // native-lane warm-up, and cap the iOS focus-probe wait to that
        // remaining budget (never IOS_NATIVE_WAIT = 65 s) so a missing element
        // on a cold runner never stalls past the command's own timeout.
        let remaining_before_focus = left(started);
        let external = self.external_focus_within(Duration::from_millis(remaining_before_focus));
        if external.is_some() {
            self.native_hint = true;
            let budget =
                if prewarmed { remaining_before_focus.max(NATIVE_FALLBACK_MS) } else { remaining_before_focus };
            return self.run_native(|ops| native(ops, budget));
        }
        if probe >= timeout_ms || (prewarmed && remaining_before_focus == 0) {
            // Tight timeout (`timeout_ms <= probe`, e.g. `when: visible`), or
            // `wait_on_flutter_while_warming` already spent the full budget:
            // if the native lane is already warm, take one instant 0 ms look so
            // WebView / PlatformView elements match without adding latency to cold runs.
            if !negative && is_flutter_not_found(&miss) {
                match self.try_native(|ops| native(ops, 0)) {
                    Some(Ok(v)) => {
                        self.note_native_fallback();
                        return Ok(v);
                    }
                    Some(Err(e)) if crate::interrupt::is_interrupted_error(&e) => return Err(e),
                    _ => {}
                }
            }
            return Err(miss);
        }
        let second_budget = left(started);
        match flutter(self, second_budget) {
            Err(e) if is_lookup_miss(&e) => {
                let remaining_after_second = left(started);
                self.invalidate_focus_cache();
                if self.external_focus_within(Duration::from_millis(remaining_after_second)).is_some() {
                    self.native_hint = true;
                    return self.run_native(|ops| native(ops, remaining_after_second));
                }
                if !negative && is_flutter_not_found(&e) {
                    match self.try_native(|ops| native(ops, NATIVE_FALLBACK_MS)) {
                        Some(Ok(v)) => {
                            self.note_native_fallback();
                            return Ok(v);
                        }
                        Some(Err(int_err)) if crate::interrupt::is_interrupted_error(&int_err) => return Err(int_err),
                        _ => {}
                    }
                }
                Err(e)
            }
            other => {
                if other.is_ok() {
                    self.overlay_hint = false;
                }
                other
            }
        }
    }

    /// Element lookup on the native lane after a native `launchApp`. The first
    /// required lookup (not `optional`, longer than the 7 s optional budget, not negative)
    /// after a launch whose app showed nothing yet is armed with the bounded
    /// cold-start retry ([`NativeOps::arm_cold_start`]); `--maestro-compat=strict`
    /// never arms it (see [`Self::finish_native_launch`]).
    fn native_lookup<T>(
        &mut self,
        timeout_ms: u64,
        negative: bool,
        optional: bool,
        native: impl Fn(&mut PlatformNativeOps<'_>, u64) -> Result<T>,
    ) -> Result<T> {
        let eligible = !negative && !optional && timeout_ms > crate::backend::OPTIONAL_LOOKUP_TIMEOUT_MS;
        let cold = if eligible { self.cold_app.take() } else { None }.map(|app_id| ColdStart {
            app_id,
            extra: COLD_START_EXTRA,
            log_prefix: self.opts.log_prefix.clone(),
        });
        let mut retries = 0;
        let res = self.run_native(|ops| {
            if let (Some(c), PlatformNativeOps::Android(a)) = (cold, &mut *ops) {
                a.arm_cold_start(c);
            }
            let res = native(ops, timeout_ms);
            if let PlatformNativeOps::Android(a) = ops {
                retries = a.cold_start_retries();
            }
            res
        });
        self.lanes.cold_start_retries += retries;
        res
    }

    /// Command without an element lookup: lane by [`Self::decide`], falling back
    /// cleanly to the native lane if no Flutter driver is attached.
    pub(super) fn route_plain<T>(
        &mut self,
        check_focus: bool,
        flutter: impl FnOnce(&mut Self) -> Result<T>,
        native: impl FnOnce(&mut PlatformNativeOps<'_>) -> Result<T>,
    ) -> Result<T> {
        match self.decide(check_focus) {
            Lane::Native => self.run_native(native),
            Lane::Flutter => {
                self.lanes.flutter += 1;
                match flutter(self) {
                    Err(e)
                        if is_driver_not_attached(&e)
                            && self.lane_mode() == InputMode::Auto
                            && self.native_usable() =>
                    {
                        self.lanes.flutter = self.lanes.flutter.saturating_sub(1);
                        self.record_unattached_fallback();
                        self.run_native(native)
                    }
                    other => other,
                }
            }
        }
    }

    /// `pressKey` goes straight to the host/native lane when no Flutter driver
    /// is attached or when an external window owns focus.
    pub(super) fn keys_use_host(&mut self) -> bool {
        match self.lane_mode() {
            InputMode::Native => true,
            InputMode::Auto => self.is_blackbox() || (self.native_usable() && self.decide(false) == Lane::Native),
            InputMode::Flutter => false,
        }
    }

    pub(super) fn host_press_key(&mut self, code: KeyCode) -> Result<Value> {
        if self.platform() == Platform::Ios {
            let name = ios_native_key_name(code);
            if name.is_empty() {
                return Ok(json!({ "noop": true, "note": "IOSDriver.pressKey ignores this key" }));
            }
            return self.run_ios_native(|ops| ops.press_key(name));
        }
        let dev = self.dev.as_android().ok_or_else(|| anyhow!("pressKey: no Android device"))?;
        let keycode = android_keycode(code);
        if matches!(code, KeyCode::Power | KeyCode::Lock) {
            dev.keyevent_with_sleep(keycode)?;
        } else {
            dev.keyevent_fast(keycode)?;
        }
        self.lanes.native += 1;
        Ok(json!({ "keycode": keycode, "lane": "native" }))
    }
}

/// Outcome of [`wait_on_flutter_while_warming`].
enum WarmWait<T> {
    /// Flutter found the element while the lane warmed up.
    Found(T),
    /// A Flutter error that is not a lookup miss.
    Failed(anyhow::Error),
    /// The lane is ready (or failed to start) or the budget is spent.
    Settled,
}

/// How long one Flutter wait lasts while the native lane warms up.
const WARMUP_FLUTTER_SLICE_MS: u64 = 250;

/// Keeps asking Flutter in short slices until the native lane is ready (or
/// unusable) or the budget is spent, so a cold start overlaps with the wait
/// instead of following it.
fn wait_on_flutter_while_warming<C, T>(
    mut left: impl FnMut() -> u64,
    mut lane_settled: impl FnMut(&mut C) -> bool,
    ctx: &mut C,
    flutter: &impl Fn(&mut C, u64) -> Result<T>,
) -> WarmWait<T> {
    loop {
        if let Err(e) = crate::interrupt::check() {
            return WarmWait::Failed(e);
        }
        if lane_settled(ctx) {
            return WarmWait::Settled;
        }
        let remaining = left();
        if remaining == 0 {
            return WarmWait::Settled;
        }
        match flutter(ctx, remaining.min(WARMUP_FLUTTER_SLICE_MS)) {
            Ok(v) => return WarmWait::Found(v),
            Err(e) if is_lookup_miss(&e) => {}
            Err(e) => return WarmWait::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::backend::OPTIONAL_LOOKUP_TIMEOUT_MS;
    use crate::device::Device;
    use crate::driver::Driver;
    use crate::run::{Opts, Session};

    fn test_opts(mode: InputMode) -> Opts {
        Opts {
            settle_policy: "v2".into(),
            check_settle: false,
            settle_load: Default::default(),
            cli_env: HashMap::new(),
            test_output_dir: PathBuf::from("out"),
            strict_input: false,
            soft_keyboard_hint: None,
            maestro_compat_strict: false,
            http_agent: None,
            input_mode: mode,
            shard_index: None,
            log_prefix: String::new(),
            app_errors: crate::app_health::AppErrorsPolicy::Report,
            trace: false,
        }
    }

    fn miss(code: &str) -> anyhow::Error {
        anyhow::Error::new(DriverError { code: code.into(), message: "x".into(), details: Value::Null })
    }

    fn warm_native_handle() -> NativeLaneHandle {
        let client = crate::native_lane::NativeDriverClient::connect(1).unwrap();
        let adb = crate::native_lane::Adb::new("unreachable");
        NativeLaneHandle::ready_for_test(client, adb)
    }

    /// Runs one selector command through `route_selector` on a backend that has
    /// no usable native lane, returning the timeouts the Flutter leg saw.
    fn flutter_timeouts(
        mode: InputMode,
        native: Option<&mut NativeLaneHandle>,
        timeout_ms: u64,
    ) -> (Vec<u64>, Result<()>) {
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        let opts = test_opts(mode);
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        let seen = std::cell::RefCell::new(Vec::new());
        let res = backend.route_selector(
            timeout_ms,
            false,
            |_, t| {
                seen.borrow_mut().push(t);
                Err(miss("ELEMENT_NOT_FOUND"))
            },
            |_, _| Ok(()),
        );
        (seen.into_inner(), res)
    }

    #[test]
    fn lane_stats_name_the_lane() {
        assert_eq!(LaneStats::default().lane_name(), "flutter");
        assert_eq!(LaneStats { flutter: 5, native: 0, ..Default::default() }.lane_name(), "flutter");
        assert_eq!(LaneStats { flutter: 0, native: 3, ..Default::default() }.lane_name(), "native");
        assert_eq!(LaneStats { flutter: 4, native: 1, ..Default::default() }.lane_name(), "hybrid");
    }

    #[test]
    fn only_lookup_misses_may_leave_the_flutter_lane() {
        assert!(is_lookup_miss(&miss("ELEMENT_NOT_FOUND")));
        assert!(is_lookup_miss(&miss("ASSERTION")));
        assert!(!is_lookup_miss(&miss("INTERNAL")));
        assert!(!is_lookup_miss(&anyhow!("connection to app lost")));
        assert!(is_flutter_not_found(&miss("ELEMENT_NOT_FOUND")));
        assert!(!is_flutter_not_found(&miss("ASSERTION")));
    }

    #[test]
    fn flutter_mode_and_missing_native_lane_use_the_flutter_driver_alone() {
        // `--input flutter`: one full-timeout call, never a native detour, even with a lane.
        let mut lane = NativeLaneHandle::unavailable("irrelevant");
        let (seen, res) = flutter_timeouts(InputMode::Flutter, Some(&mut lane), 5000);
        assert_eq!(seen, vec![5000]);
        assert!(is_flutter_not_found(&res.unwrap_err()));
        // `auto` without a lane (APKs missing): plain Flutter, no probe split, no focus lookups.
        let (seen, res) = flutter_timeouts(InputMode::Auto, None, 5000);
        assert_eq!(seen, vec![5000]);
        assert!(res.is_err());
        let mut broken = NativeLaneHandle::unavailable("no Maestro");
        let (seen, _) = flutter_timeouts(InputMode::Auto, Some(&mut broken), 5000);
        assert_eq!(seen, vec![5000]);
    }

    #[test]
    fn native_mode_without_a_lane_fails_clearly() {
        let (seen, res) = flutter_timeouts(InputMode::Native, None, 5000);
        assert!(seen.is_empty(), "the Flutter driver is not used in native mode");
        assert!(res.unwrap_err().to_string().contains("native lane is not enabled"));
        let mut broken = NativeLaneHandle::unavailable("no Maestro");
        let (_, res) = flutter_timeouts(InputMode::Native, Some(&mut broken), 5000);
        assert!(res.unwrap_err().to_string().contains("no Maestro"));
    }

    #[test]
    fn driver_not_attached_error_is_classified_for_no_launch_app_fallback() {
        let err = anyhow::Error::new(DriverNotAttachedError("com.example is not running".into()));
        assert!(is_driver_not_attached(&err));
        assert!(!is_lookup_miss(&err));
        assert!(!is_driver_not_attached(&miss("ELEMENT_NOT_FOUND")));
    }

    #[test]
    fn hybrid_routing_handles_tight_timeouts_assert_not_visible_and_no_launch_app() {
        // 1. Tight timeout (`timeout_ms <= 400`, e.g. `when: visible`): when Flutter
        //    misses and the native lane is already warm, `try_native` runs with 0 ms
        //    so WebView / PlatformView elements match immediately.
        {
            let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
            let mut drv = Driver::listen(0).unwrap();
            let opts = test_opts(InputMode::Auto);
            let mut native = warm_native_handle();
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: Some(&mut native),
                ios_native: None,
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());

            let native_timeouts = std::cell::RefCell::new(Vec::new());
            let out = backend
                .route_selector(
                    0,
                    false,
                    |_, _| Err(miss("ELEMENT_NOT_FOUND")),
                    |_, t| {
                        native_timeouts.borrow_mut().push(t);
                        Ok("found-in-webview")
                    },
                )
                .unwrap();
            assert_eq!(out, "found-in-webview");
            assert_eq!(native_timeouts.into_inner(), vec![0]);
            assert_eq!(backend.lane_stats(), LaneStats { flutter: 1, native: 1, ..Default::default() });
        }

        // 2. `assertNotVisible` (`negative == true`):
        //    - Returns immediately in 1 Flutter call with 0 native calls when absent in Flutter.
        //    - Splits into an initial 400 ms probe + remaining wait when still visible in Flutter
        //      (never falling back to `try_native` when the element IS visible in Flutter).
        {
            let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
            let mut drv = Driver::listen(0).unwrap();
            let opts = test_opts(InputMode::Auto);
            let mut native = warm_native_handle();
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: Some(&mut native),
                ios_native: None,
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());

            let fast_ok = backend
                .route_selector(5000, true, |_, _| Ok("absent"), |_, _| panic!("must not dump native hierarchy"))
                .unwrap();
            assert_eq!(fast_ok, "absent");
            assert_eq!(backend.lane_stats(), LaneStats { flutter: 1, native: 0, ..Default::default() });

            let flutter_seen = std::cell::RefCell::new(Vec::new());
            let res: Result<()> = backend.route_selector(
                5000,
                true,
                |_, t| {
                    flutter_seen.borrow_mut().push(t);
                    Err(miss("ASSERTION"))
                },
                |_, _| panic!("must not override a real in-app Flutter assertion failure"),
            );
            assert!(res.is_err());
            let seen = flutter_seen.into_inner();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[0], FLUTTER_PROBE_MS);
            assert!(seen[1] > 4000 && seen[1] <= 5000, "unexpected second wait: {}", seen[1]);
        }

        // 3. Flow starting WITHOUT `launchApp` when focus is unknown (unreachable adb):
        //    `DriverNotAttachedError` in `route_selector` or `route_plain` falls back
        //    cleanly to the native lane without marking the app black-box.
        {
            let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
            let mut drv = Driver::listen(0).unwrap();
            let opts = test_opts(InputMode::Auto);
            let mut native = warm_native_handle();
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: Some(&mut native),
                ios_native: None,
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.uninstrumented.app".into());

            let sel_res = backend
                .route_selector(
                    1000,
                    false,
                    |_, _| Err(anyhow::Error::new(DriverNotAttachedError("not attached".into()))),
                    |_, _| Ok("native-selector-ok"),
                )
                .unwrap();
            assert_eq!(sel_res, "native-selector-ok");
            assert!(!backend.ran_blackbox(), "unknown focus must never mark an app black-box");
            assert_eq!(backend.lane_stats(), LaneStats { flutter: 0, native: 1, ..Default::default() });

            let plain_res = backend
                .route_plain(
                    true,
                    |_| Err(anyhow::Error::new(DriverNotAttachedError("not attached".into()))),
                    |_| Ok("native-scroll-ok"),
                )
                .unwrap();
            assert_eq!(plain_res, "native-scroll-ok");
            assert!(!backend.ran_blackbox());
            assert_eq!(backend.lane_stats(), LaneStats { flutter: 0, native: 2, ..Default::default() });
        }
    }

    #[test]
    fn warm_up_wait_overlaps_flutter_slices_and_stops_when_the_lane_is_ready() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let budget = Cell::new(1000u64);
        // The lane becomes ready after three Flutter slices.
        let out = wait_on_flutter_while_warming(
            || budget.get(),
            |_: &mut ()| calls.get() >= 3,
            &mut (),
            &|_: &mut (), t: u64| {
                assert!(t <= WARMUP_FLUTTER_SLICE_MS);
                calls.set(calls.get() + 1);
                budget.set(budget.get() - t);
                Err::<(), _>(miss("ELEMENT_NOT_FOUND"))
            },
        );
        assert!(matches!(out, WarmWait::Settled));
        assert_eq!(calls.get(), 3);
        assert_eq!(budget.get(), 1000 - 3 * WARMUP_FLUTTER_SLICE_MS);

        // Flutter finds the element mid-way: that answer wins, the lane is irrelevant.
        let n = Cell::new(0u32);
        let found = wait_on_flutter_while_warming(|| 1000, |_: &mut ()| false, &mut (), &|_: &mut (), _| {
            n.set(n.get() + 1);
            if n.get() == 2 { Ok("here") } else { Err(miss("ELEMENT_NOT_FOUND")) }
        });
        assert!(matches!(found, WarmWait::Found("here")));

        // A real error aborts; a spent budget settles without a Flutter call.
        let failed = wait_on_flutter_while_warming(|| 1000, |_: &mut ()| false, &mut (), &|_: &mut (), _| {
            Err::<(), _>(anyhow!("connection to app lost"))
        });
        assert!(matches!(failed, WarmWait::Failed(_)));
        let spent = wait_on_flutter_while_warming(|| 0, |_: &mut ()| false, &mut (), &|_: &mut (), _| -> Result<()> {
            panic!("no budget left")
        });
        assert!(matches!(spent, WarmWait::Settled));
    }

    #[test]
    fn ios_native_only_element_routes_plain_commands_natively_until_flutter_answers() {
        use crate::ios_native_lane::client::mock::{MockRunner, Resp};

        let runner = MockRunner::start(Box::new(|_| Resp::json("{}")));
        let mut dev = Device::Ios(crate::device::IosSim::new(crate::device::SimInfo {
            udid: "SIM-UDID".into(),
            name: "iPhone".into(),
        }));
        let mut drv = Driver::listen(0).unwrap();
        let opts = test_opts(InputMode::Auto);
        let mut slot = IosNativeSlot::Started(IosNativeLaneHandle::ready_for_test(
            crate::ios_native_lane::IosNativeClient::connect(runner.port),
        ));
        let mut session = Session {
            dev: &mut dev,
            drv: &mut drv,
            opts: &opts,
            units_scale: 1.0,
            native: None,
            ios_native: Some(&mut slot),
        };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        // Pretend the driver is attached so a Flutter miss is a real lookup miss.
        assert!(!backend.overlay_hint);

        // A web-auth sheet: Flutter does not know the field, XCTest still reports our app.
        let found =
            backend.route_selector(5000, false, |_, _| Err(miss("ELEMENT_NOT_FOUND")), |_, _| Ok("field")).unwrap();
        assert_eq!(found, "field");
        assert!(backend.overlay_hint, "a native-only hit sets the overlay hint");
        assert_eq!(backend.decide(false), Lane::Native, "inputText / pressKey / hideKeyboard now follow it");

        // The next lookup probes natively right after the short Flutter probe, not after the full budget.
        let flutter_seen = std::cell::RefCell::new(Vec::new());
        let again = backend
            .route_selector(
                5000,
                false,
                |_, t| {
                    flutter_seen.borrow_mut().push(t);
                    Err(miss("ELEMENT_NOT_FOUND"))
                },
                |_, t| {
                    assert!(t <= OVERLAY_NATIVE_SLICE_MS);
                    Ok("button")
                },
            )
            .unwrap();
        assert_eq!(again, "button");
        assert_eq!(flutter_seen.into_inner(), vec![FLUTTER_PROBE_MS], "no second full Flutter wait");

        // Flutter answers again: the hint is gone and plain commands return to the Flutter lane.
        backend.route_selector(5000, false, |_, _| Ok("home"), |_, _| panic!("Flutter answered")).unwrap();
        assert!(!backend.overlay_hint);
        assert_eq!(backend.decide(false), Lane::Flutter);
    }

    #[test]
    fn ios_ignored_host_key_does_not_touch_native_lane() {
        let mut dev = Device::Ios(crate::device::IosSim::new(crate::device::SimInfo {
            udid: "SIM-UDID".into(),
            name: "iPhone".into(),
        }));
        let mut drv = Driver::listen(0).unwrap();
        let opts = test_opts(InputMode::Native);
        let mut ios_slot = IosNativeSlot::lazy("SIM-UDID");
        let mut session = Session {
            dev: &mut dev,
            drv: &mut drv,
            opts: &opts,
            units_scale: 1.0,
            native: None,
            ios_native: Some(&mut ios_slot),
        };
        let mut backend = DeviceFlutterBackend::new(&mut session, "dev.tapwing.sampleApp".into());
        let res = backend.host_press_key(KeyCode::VolumeUp).unwrap();
        assert_eq!(res["noop"], true);
        assert_eq!(backend.lane_stats(), LaneStats { flutter: 0, native: 0, ..Default::default() });
        assert!(!backend.ios_native.as_deref().unwrap().is_started());
    }

    #[test]
    fn capture_failure_hierarchy_drains_native_restart_notices() {
        let dead_port = || std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        let opts = test_opts(InputMode::Native);
        let mut client = crate::native_lane::NativeDriverClient::connect(dead_port()).unwrap();
        client.set_restart(Box::new(move || crate::native_lane::NativeDriverClient::connect(dead_port())));
        let mut native = NativeLaneHandle::ready_for_test(client, crate::native_lane::Adb::new("unreachable"));
        {
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: Some(&mut native),
                ios_native: None,
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
            let _ = backend.capture_failure_hierarchy_impl();
        }
        assert!(
            native.take_restart_notices().is_empty(),
            "restart notices triggered during capture_failure_hierarchy_impl must be reported and drained"
        );
    }

    #[test]
    fn only_the_first_required_native_lookup_after_a_cold_launch_is_armed() {
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        let mut opts = test_opts(InputMode::Native);
        opts.log_prefix = "[shard 2] ".into();
        let mut native = warm_native_handle();
        let mut session = Session {
            dev: &mut dev,
            drv: &mut drv,
            opts: &opts,
            units_scale: 1.0,
            native: Some(&mut native),
            ios_native: None,
        };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        backend.cold_app = Some("com.example.app".into());

        // Each lookup reports what it was armed with; the third one "takes" the window.
        let lookup = |backend: &mut DeviceFlutterBackend<'_>, timeout_ms: u64, negative: bool| {
            let armed = std::cell::RefCell::new(None);
            backend
                .route_selector(
                    timeout_ms,
                    negative,
                    |_, _| -> Result<()> { unreachable!("native mode") },
                    |ops, _| {
                        let PlatformNativeOps::Android(a) = ops else { unreachable!() };
                        *armed.borrow_mut() = a.armed_cold_start().cloned();
                        if armed.borrow().is_some() {
                            a.note_cold_start_retry();
                        }
                        Ok(())
                    },
                )
                .unwrap();
            armed.into_inner()
        };
        // Optional (7 s) and conditional lookups, and negative ones, keep it armed.
        assert!(lookup(&mut backend, OPTIONAL_LOOKUP_TIMEOUT_MS, false).is_none());
        assert!(lookup(&mut backend, 0, false).is_none());
        assert!(lookup(&mut backend, 17_000, true).is_none());
        let armed = lookup(&mut backend, 17_000, false).expect("the first required lookup is armed");
        assert_eq!(armed.app_id, "com.example.app");
        assert_eq!(armed.extra, COLD_START_EXTRA);
        assert_eq!(armed.log_prefix, "[shard 2] ");
        assert!(lookup(&mut backend, 17_000, false).is_none(), "spent after one lookup");
        assert_eq!(backend.lane_stats().cold_start_retries, 1);
        assert_eq!(backend.lane_stats().native, 5);
    }

    /// Minimal loopback gRPC native driver for `finish_native_launch` / `FlowDriver` tests.
    struct StaticXmlDriver(String);

    #[tonic::async_trait]
    impl crate::native_lane::client::pb::maestro_driver_server::MaestroDriver for StaticXmlDriver {
        async fn device_info(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::DeviceInfoRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::DeviceInfo>, tonic::Status> {
            Ok(tonic::Response::new(crate::native_lane::client::pb::DeviceInfo {
                width_pixels: 1080,
                height_pixels: 2400,
            }))
        }
        async fn view_hierarchy(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::ViewHierarchyRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::ViewHierarchyResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::ViewHierarchyResponse {
                hierarchy: self.0.clone(),
            }))
        }
        async fn tap(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::TapRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::TapResponse>, tonic::Status> {
            Ok(tonic::Response::new(crate::native_lane::client::pb::TapResponse {}))
        }
        async fn erase_all_text(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::EraseAllTextRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::EraseAllTextResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::EraseAllTextResponse {}))
        }
        async fn input_text(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::InputTextRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::InputTextResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::InputTextResponse {}))
        }
        async fn screenshot(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::ScreenshotRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::ScreenshotResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::ScreenshotResponse { bytes: vec![] }))
        }
        async fn is_window_updating(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::CheckWindowUpdatingRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::native_lane::client::pb::CheckWindowUpdatingResponse>,
            tonic::Status,
        > {
            Ok(tonic::Response::new(crate::native_lane::client::pb::CheckWindowUpdatingResponse {
                is_window_updating: false,
            }))
        }
        async fn launch_app(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::LaunchAppRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::LaunchAppResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::LaunchAppResponse {}))
        }
        async fn set_location(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::SetLocationRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::SetLocationResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::SetLocationResponse {}))
        }
        async fn add_media(
            &self,
            _: tonic::Request<tonic::Streaming<crate::native_lane::client::pb::AddMediaRequest>>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::AddMediaResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::AddMediaResponse {}))
        }
        async fn enable_mock_location_providers(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::EmptyRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::EmptyResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::EmptyResponse {}))
        }
        async fn disable_location_updates(
            &self,
            _: tonic::Request<crate::native_lane::client::pb::EmptyRequest>,
        ) -> std::result::Result<tonic::Response<crate::native_lane::client::pb::EmptyResponse>, tonic::Status>
        {
            Ok(tonic::Response::new(crate::native_lane::client::pb::EmptyResponse {}))
        }
    }

    fn serve_static_native(
        xml: &str,
    ) -> (NativeLaneHandle, tokio::sync::oneshot::Sender<()>, std::thread::JoinHandle<()>) {
        use crate::native_lane::client::pb::maestro_driver_server::MaestroDriverServer;
        let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let xml = xml.to_string();
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                let incoming = tonic::transport::server::TcpIncoming::from(listener);
                let _ = port_tx.send(port);
                let _ = tonic::transport::Server::builder()
                    .add_service(MaestroDriverServer::new(StaticXmlDriver(xml)))
                    .serve_with_incoming_shutdown(incoming, async {
                        let _ = stop_rx.await;
                    })
                    .await;
            });
        });
        let port = port_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut client = crate::native_lane::NativeDriverClient::connect(port).unwrap();
        for _ in 0..100 {
            if client.device_info_with_timeout(Duration::from_millis(200)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let native = NativeLaneHandle::ready_for_test(client, crate::native_lane::Adb::new("unreachable"));
        (native, stop_tx, handle)
    }

    #[test]
    fn strict_maestro_compat_never_arms_native_cold_start_retry() {
        let launcher = r#"<hierarchy rotation="0">
  <node text="Phone" package="com.android.launcher3" enabled="true" bounds="[0,0][1080,2400]"/>
</hierarchy>"#;
        let metrics = crate::device::LaunchMetrics {
            pid: Some(123),
            old_pids: vec![],
            granted_permissions: 0,
            unsupported_permissions: vec![],
            stop_clear_ms: 0,
            permissions_ms: 0,
            start_ms: 1,
        };
        for strict in [false, true] {
            let (mut native, stop, srv) = serve_static_native(launcher);
            let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
            let mut drv = Driver::listen(0).unwrap();
            let mut opts = test_opts(InputMode::Native);
            opts.maestro_compat_strict = strict;
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: Some(&mut native),
                ios_native: None,
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
            backend.finish_native_launch(&metrics, "--input native").unwrap();
            assert_eq!(backend.cold_app.is_some(), !strict, "strict={strict}");
            drop(backend);
            drop(native);
            let _ = stop.send(());
            srv.join().unwrap();
        }
    }

    #[test]
    fn optional_assert_visible_must_not_consume_cold_start_arming() {
        use crate::backend::FlowDriver;
        // System dialog / launcher on screen while app is still cold.
        let xml = r#"<hierarchy rotation="0">
  <node text="Allow" package="com.google.android.permissioncontroller" enabled="true" bounds="[0,0][200,100]"/>
</hierarchy>"#;
        let (mut native, stop, srv) = serve_static_native(xml);
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        let opts = test_opts(InputMode::Native);
        let mut session = Session {
            dev: &mut dev,
            drv: &mut drv,
            opts: &opts,
            units_scale: 1.0,
            native: Some(&mut native),
            ios_native: None,
        };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        backend.cold_app = Some("com.example.app".into());

        // Orchestra calls assert_visible with LOOKUP_TIMEOUT_MS (17_000) even when optional=true.
        let sel = ElementSelector { text_regex: Some("Allow".into()), ..Default::default() };
        backend.assert_visible(&sel, crate::backend::LOOKUP_TIMEOUT_MS, true).unwrap();
        let still_armed = backend.cold_app.is_some();
        drop(backend);
        drop(native);
        let _ = stop.send(());
        srv.join().unwrap();

        assert!(
            still_armed,
            "optional assertVisible must not consume the cold-start arming reserved for required lookups"
        );
    }

    fn ios_backend_session_parts() -> (Device, Driver) {
        let dev = Device::Ios(crate::device::IosSim::new(crate::device::SimInfo {
            udid: "SIM-UDID".into(),
            name: "iPhone".into(),
        }));
        (dev, Driver::listen(0).unwrap())
    }

    /// A warm iOS slot whose runner is dead and restarts into another dead one.
    fn dead_ios_slot() -> IosNativeSlot {
        let dead_port = || std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut client = crate::ios_native_lane::IosNativeClient::connect(dead_port());
        client.set_restart(Box::new(move || Ok(crate::ios_native_lane::IosNativeClient::connect(dead_port()))));
        IosNativeSlot::Started(IosNativeLaneHandle::ready_for_test(client))
    }

    #[test]
    fn capture_failure_hierarchy_drains_ios_restart_notices() {
        let (mut dev, mut drv) = ios_backend_session_parts();
        let opts = test_opts(InputMode::Native);
        let mut slot = dead_ios_slot();
        {
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: None,
                ios_native: Some(&mut slot),
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
            assert!(backend.capture_failure_hierarchy_impl().is_none());
        }
        let IosNativeSlot::Started(h) = &mut slot else { panic!("slot stays started") };
        assert_eq!(h.get_or_wait(Duration::from_millis(1)).unwrap().restarts(), 1, "the capture restarted the runner");
        assert!(
            slot.take_restart_notices().is_empty(),
            "restart notices triggered during capture_failure_hierarchy_impl must be reported and drained"
        );
    }

    #[test]
    fn ios_native_commands_drain_restart_notices() {
        type Path = fn(&mut DeviceFlutterBackend<'_>);
        let paths: [(&str, Path); 4] = [
            ("run_native", |b| assert!(b.run_native(|ops| ops.input_text("x")).is_err())),
            ("run_ios_native", |b| assert!(b.run_ios_native(|ops| ops.hierarchy().map(drop)).is_err())),
            ("try_native", |b| assert!(b.try_native(|ops| ops.input_text("x")).is_some_and(|r| r.is_err()))),
            ("ios_external_focus", |b| assert!(b.ios_external_focus(IOS_NATIVE_WAIT, false).is_none())),
        ];
        for (name, path) in paths {
            let (mut dev, mut drv) = ios_backend_session_parts();
            let opts = test_opts(InputMode::Native);
            let mut slot = dead_ios_slot();
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: None,
                    ios_native: Some(&mut slot),
                };
                let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
                path(&mut backend);
            }
            let IosNativeSlot::Started(h) = &mut slot else { panic!("slot stays started") };
            assert!(h.get_or_wait(Duration::from_millis(1)).unwrap().restarts() >= 1, "{name} restarted the runner");
            assert!(slot.take_restart_notices().is_empty(), "{name} must report its restarts");
        }
    }

    #[test]
    fn lazy_ios_slot_never_spawns_on_focus_probe_failure_capture_or_notices() {
        for mode in [InputMode::Auto, InputMode::Native] {
            let (mut dev, mut drv) = ios_backend_session_parts();
            let opts = test_opts(mode);
            let mut slot = IosNativeSlot::lazy("SIM-UDID");
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: None,
                    ios_native: Some(&mut slot),
                };
                let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
                assert!(backend.ios_external_focus(Duration::ZERO, false).is_none());
                assert!(backend.ios_external_focus(IOS_NATIVE_WAIT, false).is_none());
                assert!(backend.focused_package_now().is_none());
                assert!(backend.capture_failure_hierarchy_impl().is_none());
            }
            assert!(!slot.is_started(), "{mode:?}: lazy slot must stay lazy");
            assert!(slot.take_restart_notices().is_empty());
            assert!(!slot.is_started(), "{mode:?}: taking notices must not start lazy slot");
        }
    }

    #[test]
    fn ios_external_focus_should_not_burn_two_restarts_in_one_probe() {
        let (mut dev, mut drv) = ios_backend_session_parts();
        let opts = test_opts(InputMode::Auto);
        let mut slot = dead_ios_slot();
        {
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: None,
                ios_native: Some(&mut slot),
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
            assert!(backend.ios_external_focus(IOS_NATIVE_WAIT, false).is_none());
        }
        let IosNativeSlot::Started(h) = &mut slot else { panic!("slot stays started") };
        assert_eq!(
            h.get_or_wait(Duration::from_millis(1)).unwrap().restarts(),
            1,
            "one focus probe should not fall through from a transport failure in has_springboard_alert() \
             into foreground_app_id() and trigger a second restart"
        );
    }

    #[test]
    fn classify_focused_package_distinguishes_unknown_in_app_and_external() {
        assert_eq!(classify_focused_package("", "com.example.app"), FocusProbe::Unknown);
        assert_eq!(classify_focused_package("com.example.app", "com.example.app"), FocusProbe::InApp);
        assert_eq!(
            classify_focused_package("com.apple.springboard", "com.example.app"),
            FocusProbe::External("com.apple.springboard".into())
        );
    }

    #[test]
    fn blackbox_state_transitions_are_flow_scoped_until_two_consecutive_misses() {
        let app = "com.example.app";
        // Verify on both Android (NativeLaneHandle) and iOS (IosNativeSlot::Lazy).
        for use_ios in [false, true] {
            let mut dev = if use_ios {
                Device::Ios(crate::device::IosSim::new(crate::device::SimInfo {
                    udid: "SIM-UDID".into(),
                    name: "iPhone".into(),
                }))
            } else {
                Device::Android(crate::device::Android::new("unreachable".into()))
            };
            let mut drv = Driver::listen(0).unwrap();
            let opts = test_opts(InputMode::Auto);
            let mut native = (!use_ios).then(warm_native_handle);
            let mut ios_slot = use_ios.then(|| IosNativeSlot::lazy("SIM-UDID"));

            // Flow 1: single miss -> flow-scoped black-box for Flow 1, NOT sticky shard-wide.
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: native.as_mut(),
                    ios_native: ios_slot.as_mut(),
                };
                let mut flow1 = DeviceFlutterBackend::new(&mut session, app.into());
                assert!(!flow1.is_blackbox());
                assert_eq!(flow1.record_handshake_miss(app), BlackboxDemotion::FlowOnly { consecutive_misses: 1 });
                assert!(flow1.ran_blackbox(), "current flow runs black-box after miss");
                assert!(!flow1.is_sticky_blackbox_app(app));

                // Next `launchApp` in the same flow clears the flow-scoped mark and retries Flutter.
                flow1.clear_flow_blackbox(app);
                assert!(!flow1.is_blackbox());
                // Put flow1 back in miss=1 state via another flow simulation below.
                flow1.mark_flow_blackbox(app);
            }

            // Flow 2: fresh backend starts non-black-box -> handshake succeeds -> resets miss counter!
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: native.as_mut(),
                    ios_native: ios_slot.as_mut(),
                };
                let mut flow2 = DeviceFlutterBackend::new(&mut session, app.into());
                assert!(!flow2.is_blackbox(), "flow 2 must retry the fast Flutter path");
                flow2.record_handshake_success(app);
                assert!(!flow2.is_blackbox());
                assert!(!flow2.is_sticky_blackbox_app(app));
            }

            // Flow 3: misses once (miss 1/2 because Flow 2 reset the counter!).
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: native.as_mut(),
                    ios_native: ios_slot.as_mut(),
                };
                let mut flow3 = DeviceFlutterBackend::new(&mut session, app.into());
                assert_eq!(flow3.record_handshake_miss(app), BlackboxDemotion::FlowOnly { consecutive_misses: 1 });
                assert!(flow3.ran_blackbox());
                assert!(!flow3.is_sticky_blackbox_app(app));
            }

            // Flow 4: misses a second time in a row -> sticky shard-wide black-box!
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: native.as_mut(),
                    ios_native: ios_slot.as_mut(),
                };
                let mut flow4 = DeviceFlutterBackend::new(&mut session, app.into());
                assert!(!flow4.is_blackbox(), "flow 4 still tries Flutter after 1 miss");
                assert_eq!(flow4.record_handshake_miss(app), BlackboxDemotion::StickyShard { consecutive_misses: 2 });
                assert!(flow4.is_sticky_blackbox_app(app));
                // Clearing flow-scoped mark on next launchApp no longer clears sticky black-box.
                flow4.clear_flow_blackbox(app);
                assert!(flow4.is_blackbox_app(app));
            }

            // Flow 5: starts sticky black-box immediately on a fresh DeviceFlutterBackend.
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: native.as_mut(),
                    ios_native: ios_slot.as_mut(),
                };
                let flow5 = DeviceFlutterBackend::new(&mut session, app.into());
                assert!(flow5.is_blackbox(), "sticky black-box persists across remaining flows in shard");
            }

            if let Some(slot) = ios_slot {
                assert!(!slot.is_started(), "black-box tracking must not spawn lazy iOS xcodebuild");
            }
        }
    }

    #[test]
    fn unattached_fallback_marks_flow_blackbox_only_when_focus_is_known_in_app() {
        use crate::ios_native_lane::client::mock::{MockRunner, Resp};

        // 1. Lazy iOS slot (focus unknown): never spawns xcodebuild and never marks black-box.
        {
            let (mut dev, mut drv) = ios_backend_session_parts();
            let opts = test_opts(InputMode::Auto);
            let mut slot = IosNativeSlot::lazy("SIM-UDID");
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: None,
                    ios_native: Some(&mut slot),
                };
                let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
                backend.record_unattached_fallback();
                assert!(!backend.ran_blackbox(), "lazy iOS slot has unknown focus");
                assert!(!backend.native_hint);
                assert!(!backend.is_sticky_blackbox_app("com.example.app"));
            }
            assert!(!slot.is_started(), "record_unattached_fallback must not spawn lazy xcodebuild");
        }

        // 2. Started iOS slot with transport error (focus unknown): never marks black-box.
        {
            let (mut dev, mut drv) = ios_backend_session_parts();
            let opts = test_opts(InputMode::Auto);
            let mut slot = dead_ios_slot();
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: None,
                ios_native: Some(&mut slot),
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
            backend.record_unattached_fallback();
            assert!(!backend.ran_blackbox());
            assert!(!backend.native_hint);
        }

        // 3. Started iOS slot reporting SpringBoard alert (external focus): sets native_hint only.
        {
            let runner = MockRunner::start(Box::new(|req| match req.path() {
                "/deviceInfo" => {
                    Resp::json(r#"{"widthPixels":1206,"heightPixels":2622,"widthPoints":402,"heightPoints":874}"#)
                }
                "/viewHierarchy" => Resp::json(r#"{"axElement":{"elementType":7,"label":"Allow?"},"depth":1}"#),
                _ => Resp::json("{}"),
            }));
            let (mut dev, mut drv) = ios_backend_session_parts();
            let opts = test_opts(InputMode::Auto);
            let mut slot = IosNativeSlot::Started(IosNativeLaneHandle::ready_for_test(
                crate::ios_native_lane::IosNativeClient::connect(runner.port),
            ));
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: None,
                ios_native: Some(&mut slot),
            };
            let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
            backend.record_unattached_fallback();
            assert!(backend.native_hint);
            assert!(!backend.ran_blackbox());
        }

        // 4. Started iOS slot affirmatively reporting our app in foreground: marks flow-scoped black-box only.
        {
            let runner = MockRunner::start(Box::new(|req| match req.path() {
                "/deviceInfo" => {
                    Resp::json(r#"{"widthPixels":1206,"heightPixels":2622,"widthPoints":402,"heightPoints":874}"#)
                }
                "/viewHierarchy" => Resp::json(r#"{"axElement":{"label":"x"},"depth":1}"#),
                "/runningApp" => Resp::json(r#"{"runningAppBundleId":"com.example.app"}"#),
                _ => Resp::json("{}"),
            }));
            let (mut dev, mut drv) = ios_backend_session_parts();
            let opts = test_opts(InputMode::Auto);
            let mut slot = IosNativeSlot::Started(IosNativeLaneHandle::ready_for_test(
                crate::ios_native_lane::IosNativeClient::connect(runner.port),
            ));
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: None,
                    ios_native: Some(&mut slot),
                };
                let mut flow1 = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
                flow1.record_unattached_fallback();
                assert!(flow1.ran_blackbox(), "known in-app unattached focus marks current flow black-box");
                assert!(!flow1.is_sticky_blackbox_app("com.example.app"), "must not promote shard counter");
            }
            {
                let mut session = Session {
                    dev: &mut dev,
                    drv: &mut drv,
                    opts: &opts,
                    units_scale: 1.0,
                    native: None,
                    ios_native: Some(&mut slot),
                };
                let flow2 = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
                assert!(!flow2.is_blackbox(), "next flow starts clean");
            }
        }
    }

    #[test]
    fn pending_ios_slot_caps_warm_up_wait_to_lookup_budget_on_miss() {
        use std::sync::atomic::Ordering;

        let (mut dev, mut drv) = ios_backend_session_parts();
        let opts = test_opts(InputMode::Auto);
        let mut slot = IosNativeSlot::Started(IosNativeLaneHandle::spawn_for_test(|cancel| {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            anyhow::bail!("cancelled")
        }));
        let mut session = Session {
            dev: &mut dev,
            drv: &mut drv,
            opts: &opts,
            units_scale: 1.0,
            native: None,
            ios_native: Some(&mut slot),
        };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());

        // 1. Optional 500 ms lookup miss while iOS runner is still Pending:
        //    overlaps warm-up and returns in ~500 ms instead of blocking 65 s.
        let t0 = Instant::now();
        let res: Result<()> = backend.route_selector_opt(
            500,
            false,
            true,
            |_, t| {
                std::thread::sleep(Duration::from_millis(t));
                Err(miss("ELEMENT_NOT_FOUND"))
            },
            |_, _| panic!("must not wait 65 s for a cold runner on a miss"),
        );
        let elapsed = t0.elapsed();
        assert!(is_flutter_not_found(&res.unwrap_err()));
        assert!(
            elapsed >= Duration::from_millis(450) && elapsed < Duration::from_secs(2),
            "expected ~500 ms lookup miss, took {elapsed:?}"
        );

        // 2. Non-blocking focus probe (`Duration::ZERO`) returns Unknown immediately.
        let t1 = Instant::now();
        assert_eq!(backend.ios_focus_probe(Duration::ZERO, false), FocusProbe::Unknown);
        assert!(t1.elapsed() < Duration::from_millis(200), "zero-wait probe stalled: {:?}", t1.elapsed());

        // 3. `assertNotVisible` when absent in Flutter does not wait on Pending runner.
        let t2 = Instant::now();
        let ok = backend
            .route_selector(5000, true, |_, _| Ok("absent"), |_, _| panic!("must not block on Pending runner"))
            .unwrap();
        assert_eq!(ok, "absent");
        assert!(t2.elapsed() < Duration::from_millis(200), "assertNotVisible stalled: {:?}", t2.elapsed());
    }

    #[test]
    fn assert_not_visible_caches_focus_probe_invalidates_on_tap_and_catches_external_dialog() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;

        use crate::backend::FlowDriver;
        use crate::device::adb_client::fake::{FakeAdb, ShDevice, sh_available};

        if !sh_available() {
            return;
        }
        let device = ShDevice::new(
            r#"
dumpsys() {
  rec dumpsys "$@"
  if [ "$1" = "window" ] && [ "$2" = "displays" ]; then
    printf '  mCurrentFocus=Window{4cb6b2c u0 %s}\n' "$(cat "$TAPWING_FAKE_DIR/focus")"
  fi
}
"#,
        );
        std::fs::write(device.dir().join("focus"), "com.example.app/.MainActivity").unwrap();
        let fake = FakeAdb::with_device(&device);
        let mut dev = Device::Android(crate::device::Android::with_adb_client("fake-serial".into(), fake.client()));

        // Connected fake Flutter driver answering `assertNotVisible` and `tapOn` immediately.
        let mut drv = Driver::listen(0).unwrap();
        let port = drv.port().unwrap();
        let peer = std::thread::spawn(move || {
            let mut app = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(app, r#"{{"event":"hello","params":{{"protocol":1,"pid":77}}}}"#).unwrap();
            let mut reader = BufReader::new(app.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].as_u64().unwrap();
                writeln!(app, r#"{{"id":{id},"result":null,"us":1}}"#).unwrap();
                line.clear();
            }
        });
        drv.accept(Duration::from_secs(2)).unwrap();

        // Native gRPC server showing a permission dialog with "Allow".
        let dialog_xml = r#"<hierarchy rotation="0">
  <node text="Allow" package="com.google.android.permissioncontroller" enabled="true" bounds="[0,0][200,100]"/>
</hierarchy>"#;
        let (mut native, stop_grpc, grpc_srv) = serve_static_native(dialog_xml);
        let opts = test_opts(InputMode::Auto);
        let mut session = Session {
            dev: &mut dev,
            drv: &mut drv,
            opts: &opts,
            units_scale: 1.0,
            native: Some(&mut native),
            ios_native: None,
        };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());

        // Four consecutive `assertNotVisible` steps share a single cached `dumpsys window displays`.
        let sel_missing = ElementSelector { text_regex: Some("Missing".into()), ..Default::default() };
        for _ in 0..4 {
            backend.assert_not_visible(&sel_missing, 5000, false).unwrap();
        }
        assert_eq!(fake.shells().len(), 1, "consecutive assertNotVisible calls must reuse cached focus probe");
        assert_eq!(device.lines("dumpsys"), vec!["dumpsys window displays"]);
        fake.assert_scripts_parse();

        // A mutating `tapOn` invalidates the cache, so a newly opened permission dialog is caught.
        let tap_req = TapElementRequest {
            selector: ElementSelector { text_regex: Some("Request Permission".into()), ..Default::default() },
            timeout_ms: 1000,
            long_press: false,
            repeat: None,
            relative_point: None,
            retry_if_no_change: false,
            wait_until_visible: true,
            wait_to_settle_timeout_ms: None,
            optional: false,
            app_id: None,
        };
        backend.tap_on_element(&tap_req).unwrap();
        std::fs::write(device.dir().join("focus"), "com.google.android.permissioncontroller/.GrantPermissionsActivity")
            .unwrap();

        // `assertNotVisible: "Allow"` must probe fresh focus, route natively, and FAIL.
        let sel_allow = ElementSelector { text_regex: Some("Allow".into()), ..Default::default() };
        let err = backend.assert_not_visible(&sel_allow, 150, false).unwrap_err();
        assert!(backend.native_hint, "external permission dialog must set native_hint");
        assert_eq!(fake.shells().len(), 2, "tapOn must invalidate focus cache so the dialog is probed");
        assert!(
            err.to_string().contains("Allow") || err.to_string().to_lowercase().contains("visible"),
            "expected assertion failure for visible dialog button, got: {err}"
        );

        drop(backend);
        drop(drv);
        peer.join().unwrap();
        drop(native);
        let _ = stop_grpc.send(());
        grpc_srv.join().unwrap();
    }
}
