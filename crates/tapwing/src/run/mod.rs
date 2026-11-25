//! Real device (Android + iOS Simulator) + `tapwing_driver` backend
//! implementing [`FlowDriver`] and the CLI entrypoint (`run_flow`) delegating
//! execution to [`Orchestra`].

mod flutter_ops;
mod hybrid;
mod launch;
mod mapping;
mod shard_outcome;
pub mod suite;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, anyhow};
use image::DynamicImage;
use serde_json::{Value, json};

pub use crate::backend::{
    AssertionError, DEFAULT_SCROLL_TIMEOUT_MS, InvalidNumericError, LOOKUP_TIMEOUT_MS, OPTIONAL_LOOKUP_TIMEOUT_MS,
    is_optional_downgradable, scroll_timeout_ms, speed_to_duration,
};
use crate::backend::{
    DeviceInfo, FlowDriver, LaunchRequest, ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest,
};
use crate::device::{Device, Platform};
use crate::driver::Driver;
use crate::flow::ParsedFlow;
use crate::flow::ir::{ElementSelector, KeyCode, Platform as IrPlatform};
use crate::native_lane::{InputMode, NativeLaneHandle};
use crate::orchestra::{Orchestra, OrchestraOpts};
pub use crate::orchestra::{adjusted_timeout, is_optional, kind};

pub use hybrid::{IosNativeSlot, LaneStats, flows_need_ios_native};
pub use mapping::{
    IosHostKey, android_keycode, driver_selector, explain, ios_host_key, is_in_app_key, is_in_app_key_ios, is_mutating,
    scale_crop_bounds, screen_swipe_ratios, scroll_duration_ms,
};

use launch::{LaunchPlan, LaunchPlanInput, decide_launch_plan, format_blackbox_demotion_warning};

/// Default `--settle-load-timeout-ms`; the driver's own default
/// (`HttpTracker.defaultSettleBound`) when the host sends nothing.
pub const DEFAULT_SETTLE_LOAD_TIMEOUT_MS: u64 = 5000;

/// Maximum `--settle-load-timeout-ms` (24 hours), preventing Dart `Duration`
/// microsecond overflow on huge values; pass `0` to remove the bound.
pub const MAX_SETTLE_LOAD_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

/// Which loads may keep settle waiting (`configure` `settleLoadTimeoutMs` /
/// `settleHttpIgnore`): CLI flags merged over `config.yaml` `tapwing:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettleLoadOpts {
    /// How long one in-flight HTTP request or pending image blocks settle, in
    /// ms; `0` removes the bound.
    pub timeout_ms: u64,
    /// Regexes (Dart syntax, unanchored, matched in the app against the full
    /// URL) for requests that never block settle.
    pub http_ignore: Vec<String>,
}

impl Default for SettleLoadOpts {
    fn default() -> Self {
        Self { timeout_ms: DEFAULT_SETTLE_LOAD_TIMEOUT_MS, http_ignore: Vec::new() }
    }
}

impl SettleLoadOpts {
    /// The CLI timeout wins over `tapwing.settleLoadTimeoutMs`; CLI ignore
    /// patterns are added to `tapwing.settleHttpIgnore`.
    pub fn resolve(
        cli_timeout_ms: Option<u64>,
        cli_http_ignore: &[String],
        config: &crate::workspace::config::TapwingConfig,
    ) -> Result<Self> {
        let timeout_ms = cli_timeout_ms.or(config.settle_load_timeout_ms).unwrap_or(DEFAULT_SETTLE_LOAD_TIMEOUT_MS);
        if timeout_ms > MAX_SETTLE_LOAD_TIMEOUT_MS {
            anyhow::bail!(
                "--settle-load-timeout-ms ({timeout_ms}) exceeds the maximum of {MAX_SETTLE_LOAD_TIMEOUT_MS} ms \
                 (24 hours); use 0 to disable the bound"
            );
        }
        let mut http_ignore = config.settle_http_ignore.clone();
        http_ignore.extend(cli_http_ignore.iter().cloned());
        Ok(Self { timeout_ms, http_ignore })
    }

    /// A driver that predates these options never confirms them and keeps
    /// waiting for every request; worth a warning only when the user changed
    /// something.
    pub fn unsupported_warning(&self, configure_result: &Value) -> Option<String> {
        (*self != Self::default() && configure_result.get("settleLoadTimeoutMs").is_none_or(Value::is_null)).then(
            || {
                "the app's tapwing_driver does not support --settle-load-timeout-ms / --settle-http-ignore \
                 (settings ignored); update the tapwing_driver package in the app"
                    .to_string()
            },
        )
    }
}

pub struct Opts {
    pub settle_policy: String,
    pub check_settle: bool,
    /// `--settle-load-timeout-ms` / `--settle-http-ignore`.
    pub settle_load: SettleLoadOpts,
    /// `-e KEY=VALUE` from the command line; wins over flow `env`.
    pub cli_env: HashMap<String, String>,
    /// Output directory for screenshots and diff artifacts (`--test-output-dir`).
    pub test_output_dir: PathBuf,
    /// Opt-in `--strict-input`: fail `inputText`/`eraseText` when no text field is focused.
    pub strict_input: bool,
    /// Optional host hint whether a soft keyboard can appear (`false` when a
    /// hardware keyboard suppresses the soft IME / simulator keyboard).
    pub soft_keyboard_hint: Option<bool>,
    /// `--maestro-compat=strict`: emulate Maestro quirks that the default mode
    /// deliberately fixes (iOS `hideKeyboard` failing on a focused field).
    pub maestro_compat_strict: bool,
    /// Shared HTTP client reused across flows for connection pooling & keep-alive.
    pub http_agent: Option<Arc<ureq::Agent>>,
    /// `--input`: which lane drives the UI (Android & iOS Simulator).
    pub input_mode: InputMode,
    /// Optional 0-based shard index (`MAESTRO_SHARD_INDEX` / `MAESTRO_SHARD_ID`).
    pub shard_index: Option<usize>,
    /// Line prefix for thread-safe console output (`""` single-shard, `"[shard N] "` sharded).
    pub log_prefix: String,
    /// F7 `--app-errors` policy, sent to the driver via `configure` `appIssues`.
    pub app_errors: crate::app_health::AppErrorsPolicy,
    /// F8b `--trace`: enables the driver's HTTP request log (`configure` `httpLog`).
    pub trace: bool,
}

pub struct Session<'a> {
    pub dev: &'a mut Device,
    pub drv: &'a mut Driver,
    pub opts: &'a Opts,
    /// Physical screenshot pixels per driver unit, reported by the driver's
    /// `configure` (`1` on Android, the device pixel ratio on iOS, where
    /// bounds are in points).
    pub units_scale: f64,
    /// Background-warmed Android native lane (`--input auto|native`).
    pub native: Option<&'a mut NativeLaneHandle>,
    /// Lazy or background-warmed iOS Simulator native lane (`--input auto|native`).
    pub ios_native: Option<&'a mut IosNativeSlot>,
}

impl Session<'_> {
    pub fn platform(&self) -> Platform {
        self.dev.platform()
    }
}

/// Real device (Android or iOS Simulator) + in-app `tapwing_driver` backend
/// implementing [`FlowDriver`].
pub struct DeviceFlutterBackend<'a> {
    pub dev: &'a mut Device,
    pub drv: &'a mut Driver,
    pub opts: &'a Opts,
    pub units_scale: &'a mut f64,
    pub flow_app: String,
    /// Android native lane for system dialogs, WebViews, other apps and uninstrumented apps.
    pub native: Option<&'a mut NativeLaneHandle>,
    /// iOS Simulator XCTest native lane for system alerts, rotation, Home/Lock and uninstrumented apps.
    pub ios_native: Option<&'a mut IosNativeSlot>,
    pub(super) lanes: LaneStats,
    /// A system / foreign window was last in front: look at the focus again
    /// before trusting the Flutter lane.
    pub(super) native_hint: bool,
    /// iOS: an element was last found only by the native lane (a system web-auth
    /// sheet, `WKWebView`, a remote view hosted inside the app's own window),
    /// where XCTest still reports the app as foreground. Plain commands
    /// (`inputText`, `pressKey`, `hideKeyboard`, ...) then follow the native
    /// lane until a Flutter lookup succeeds again.
    pub(super) overlay_hint: bool,
    /// Android: the app of the last native `launchApp` showed nothing on
    /// screen when the launch settled, so the next required native lookup
    /// gets the bounded cold-start retry (see `native_lookup`).
    pub(super) cold_app: Option<String>,
    /// Apps marked black-box for this flow only (cleared on the next
    /// `launchApp` for that app unless promoted to sticky shard-wide black-box).
    pub(super) flow_blackbox_apps: HashSet<String>,
    /// Native-lane command count at the last [`FlowDriver::take_app_issues`]
    /// (native steps send no driver RPC, so issues are re-read after them).
    native_at_issue_take: u32,
    /// Flow start, to pick crash reports written during this flow.
    started: SystemTime,
    /// `Driver::epoch()` at the most recent intentional `stopApp`/`killApp`/`clearState`
    /// for `flow_app`, so intentional stops are never misdiagnosed as crashes.
    intentionally_stopped_epoch: Option<u64>,
    /// F8b: lane counters at the last [`FlowDriver::trace_step`].
    trace_lanes: LaneStats,
    /// F8b: whether the driver confirmed its HTTP log during this flow.
    trace_http_seen: bool,
}

/// Compatibility alias for callers referring to the F3 backend by its initial name.
pub type AndroidFlutterBackend<'a> = DeviceFlutterBackend<'a>;

impl<'a> DeviceFlutterBackend<'a> {
    pub fn new(session: &'a mut Session<'_>, flow_app: String) -> Self {
        Self {
            dev: session.dev,
            drv: session.drv,
            opts: session.opts,
            units_scale: &mut session.units_scale,
            flow_app,
            native: session.native.as_deref_mut(),
            ios_native: session.ios_native.as_deref_mut(),
            lanes: LaneStats::default(),
            native_hint: false,
            overlay_hint: false,
            cold_app: None,
            flow_blackbox_apps: HashSet::new(),
            native_at_issue_take: 0,
            started: SystemTime::now(),
            intentionally_stopped_epoch: None,
            trace_lanes: LaneStats::default(),
            trace_http_seen: false,
        }
    }

    fn mark_intentionally_stopped(&mut self, app_id: &str) {
        if app_id == self.flow_app || self.flow_app.is_empty() {
            self.intentionally_stopped_epoch = Some(self.drv.epoch());
        }
    }

    fn is_intentionally_stopped(&self) -> bool {
        self.intentionally_stopped_epoch == Some(self.drv.epoch())
    }

    /// Android: `appCrash` when the app process is gone, with the crash
    /// buffer excerpt for its last PID (Java exception or native tombstone).
    fn diagnose_android_crash(&mut self) -> Option<Value> {
        if self.is_intentionally_stopped() {
            return None;
        }
        let package = self.flow_app.clone();
        if package.is_empty() {
            return None;
        }
        let pid = self.drv.last_pid();
        let started = self.started;
        let android = self.dev.as_android()?;
        let running = |a: &crate::device::android::Android| a.pid_of(&package).ok().flatten().is_some();
        // A tombstone takes a moment to be written; poll briefly.
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            if crate::interrupt::is_interrupted() {
                return None;
            }
            // A different (relaunched) process, or the same one, is no crash
            // of the process we talked to unless its crash is in the log.
            let alive = running(android);
            let elapsed = SystemTime::now().duration_since(started).unwrap_or_default();
            let excerpt = android
                .shell("echo \"TAPWING_NOW:$(date '+%Y %m %d %H %M %S')\" && logcat -b crash -d -v threadtime")
                .ok()
                .and_then(|out| parse_android_crash_probe(&out, &package, pid, elapsed));
            if let Some(e) = excerpt {
                return Some(crate::app_health::crash_issue("App crashed", Some(&e)));
            }
            if alive {
                return None;
            }
            if Instant::now() >= deadline {
                return Some(crate::app_health::crash_issue("App process exited (no crash report in logcat)", None));
            }
            if crate::interrupt::sleep(Duration::from_millis(250)).is_err() {
                return None;
            }
        }
    }

    /// iOS Simulator: `appCrash` from the newest `.ips` report of the flow's
    /// bundle written since the flow started.
    fn diagnose_ios_crash(&self) -> Option<Value> {
        let dir = std::env::var_os("HOME").map(PathBuf::from)?.join("Library/Logs/DiagnosticReports");
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            if crate::interrupt::is_interrupted() {
                return None;
            }
            let newest = std::fs::read_dir(&dir)
                .ok()?
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "ips"))
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .filter(|(m, _)| *m >= self.started)
                .filter_map(|(m, p)| {
                    let text = std::fs::read_to_string(&p).ok()?;
                    let header: Value = serde_json::from_str(text.lines().next()?).ok()?;
                    (header["bundleID"] == self.flow_app.as_str()).then_some((m, text))
                })
                .max_by_key(|(m, _)| *m);
            if let Some((_, text)) = newest {
                let excerpt = crate::app_health::ios_crash_excerpt(&text);
                return Some(crate::app_health::crash_issue("App crashed", excerpt.as_deref()));
            }
            if Instant::now() >= deadline {
                return None;
            }
            if crate::interrupt::sleep(Duration::from_millis(250)).is_err() {
                return None;
            }
        }
    }

    pub fn platform(&self) -> Platform {
        self.dev.platform()
    }

    /// Commands executed per lane so far in this flow.
    pub fn lane_stats(&self) -> LaneStats {
        self.lanes
    }

    /// Whether the app under test has no `tapwing_driver` (no handshake arrived).
    pub fn ran_blackbox(&self) -> bool {
        self.is_blackbox()
    }
}

impl FlowDriver for DeviceFlutterBackend<'_> {
    fn device_info(&self) -> DeviceInfo {
        match self.platform() {
            Platform::Android => DeviceInfo::android_default(),
            Platform::Ios => DeviceInfo {
                platform: IrPlatform::Ios,
                width_pixels: 1206,
                height_pixels: 2622,
                width_grid: 402,
                height_grid: 874,
            },
        }
    }

    fn set_flow_app_id(&mut self, app_id: &str) {
        self.flow_app = app_id.to_string();
    }

    fn launch_app(&mut self, req: &LaunchRequest) -> Result<Value> {
        self.warn_native_unavailable();
        let platform = self.platform();
        let app_changed = !self.flow_app.is_empty() && self.flow_app != req.app_id;
        self.flow_app = req.app_id.clone();
        self.native_hint = false;
        self.overlay_hint = false;
        self.cold_app = None;
        // Clear any flow-scoped black-box mark so the next `launchApp` tries
        // the fast Flutter path again unless the app missed twice in a row.
        self.clear_flow_blackbox(&req.app_id);

        let was_connected = self.drv.is_connected();
        let restarting = req.stop_app || req.clear_state || app_changed || !was_connected;
        let previous_pid = self.drv.last_pid();
        if restarting {
            self.drv.disconnect();
        }

        // Orchestra.kt:1338-1340 (iOS keychain; Android has none).
        if req.clear_keychain
            && let Some(sim) = self.dev.as_ios()
        {
            sim.clear_keychain()?;
        }
        let host_port = self.drv.port()?;
        let launch = self.dev.prepare_and_launch(
            &req.app_id,
            req.clear_state,
            req.stop_app,
            &req.permissions,
            &req.launch_arguments,
            host_port,
        )?;

        let plan = decide_launch_plan(&LaunchPlanInput {
            lane_mode: self.lane_mode(),
            blackbox: self.is_blackbox_app(&req.app_id),
            native_usable: self.native_usable(),
            stop_app: req.stop_app,
            clear_state: req.clear_state,
            app_changed,
            was_connected,
            previous_pid,
            launch_pid: launch.pid,
            old_pids: &launch.old_pids,
        });

        let start_key = match platform {
            Platform::Android => "amStartMs",
            Platform::Ios => "simctlLaunchMs",
        };

        match plan {
            LaunchPlan::Native { reason } => {
                self.drv.disconnect();
                self.finish_native_launch(&launch, reason)
            }
            LaunchPlan::AcceptHandshake { relaunched, expected_pid, reject_pids, can_fall_back, accept_timeout } => {
                // Resolve (and cache) the iOS display name for `configure` while
                // the app is still booting, off the accept -> configure path.
                if platform == Platform::Ios {
                    let _ = self.dev.app_display_name(&req.app_id);
                }
                if relaunched {
                    self.drv.disconnect();
                }

                let t_accept = Instant::now();
                let outcome = match self.drv.accept_with_pid(accept_timeout, expected_pid, &reject_pids) {
                    Ok(o) => o,
                    Err(e) if can_fall_back && !crate::interrupt::is_interrupted_error(&e) => {
                        let demotion = self.record_handshake_miss(&req.app_id);
                        self.drv.disconnect();
                        let reason = format!("{e:#}");
                        let warning =
                            format_blackbox_demotion_warning(&self.opts.log_prefix, &req.app_id, &reason, demotion);
                        eprintln!("{warning}");
                        let why = format!(
                            "no tapwing_driver handshake ({reason}); driving the app black-box via the native lane"
                        );
                        return self.finish_native_launch(&launch, &why);
                    }
                    Err(e) => return Err(e),
                };
                let accept_ms = t_accept.elapsed().as_secs_f64() * 1000.0;

                let hs = self.launch_handshake(&req.app_id, outcome.hello["pid"].as_i64())?;
                self.record_handshake_success(&req.app_id);

                let mut res = json!({
                    "hello": outcome.hello,
                    "idle": hs.idle,
                    "pid": outcome.hello["pid"],
                    "oldPids": launch.old_pids,
                    "grantedPermissions": launch.granted_permissions,
                    "phases": {
                        "stopClearMs": launch.stop_clear_ms,
                        "permissionsMs": launch.permissions_ms,
                        start_key: launch.start_ms,
                        "acceptMs": accept_ms,
                        "rejectedConnections": outcome.rejected_connections,
                        "configureMs": hs.configure_ms,
                        "waitIdleMs": hs.wait_idle_ms,
                    }
                });
                if hs.retries > 0 {
                    res["phases"]["handshakeRetries"] = json!(hs.retries);
                }
                add_unsupported_permissions_warning(&mut res, &launch.unsupported_permissions);
                Ok(res)
            }
            LaunchPlan::ReuseConnection => {
                if platform == Platform::Ios {
                    let _ = self.dev.app_display_name(&req.app_id);
                }
                let t_idle = Instant::now();
                let idle = self.drv.call("waitIdle", json!({ "timeoutMs": 5000 }))?;
                let wait_idle_ms = t_idle.elapsed().as_secs_f64() * 1000.0;
                self.record_handshake_success(&req.app_id);
                let mut res = json!({
                    "idle": idle,
                    "pid": launch.pid,
                    "grantedPermissions": launch.granted_permissions,
                    "phases": {
                        "stopClearMs": launch.stop_clear_ms,
                        "permissionsMs": launch.permissions_ms,
                        start_key: launch.start_ms,
                        "acceptMs": 0.0,
                        "rejectedConnections": 0,
                        "configureMs": 0.0,
                        "waitIdleMs": wait_idle_ms,
                    }
                });
                add_unsupported_permissions_warning(&mut res, &launch.unsupported_permissions);
                Ok(res)
            }
        }
    }

    fn stop_app(&mut self, app_id: &str) -> Result<Value> {
        self.cold_app = None;
        self.dev.stop_app(app_id)?;
        self.mark_intentionally_stopped(app_id);
        self.drv.disconnect();
        Ok(Value::Null)
    }

    fn kill_app(&mut self, app_id: &str) -> Result<Value> {
        self.cold_app = None;
        self.dev.kill_app(app_id)?;
        self.mark_intentionally_stopped(app_id);
        self.drv.disconnect();
        Ok(Value::Null)
    }

    fn clear_state(&mut self, app_id: &str) -> Result<Value> {
        self.cold_app = None;
        self.dev.clear_state(app_id)?;
        self.mark_intentionally_stopped(app_id);
        self.drv.disconnect();
        Ok(Value::Null)
    }

    fn clear_keychain(&mut self) -> Result<Value> {
        if let Some(sim) = self.dev.as_ios() {
            sim.clear_keychain()?;
        }
        // iOS-only; no-op on Android matching Maestro (`AndroidDriver.kt`).
        Ok(Value::Null)
    }

    fn set_permissions(&mut self, app_id: &str, permissions: &BTreeMap<String, String>) -> Result<Value> {
        if let Some(sim) = self.dev.as_ios() {
            let (applied, unsupported) = sim.set_permissions(app_id, permissions)?;
            if applied > 0 {
                // Permission changes may terminate the running app; reattach
                // (or fail clearly) on the next driver command.
                self.drv.disconnect();
            }
            let mut res = json!({ "privacyActions": applied });
            add_unsupported_permissions_warning(&mut res, &unsupported);
            return Ok(res);
        }
        let android = self.dev.as_android().ok_or_else(|| anyhow!("setPermissions: no device"))?;
        let outcome = android.set_permissions(app_id, permissions)?;
        if outcome.revoked > 0 {
            // Android kills an app whose runtime permission is revoked; its
            // driver connection is dead, so the next command reattaches (or
            // fails clearly) instead of talking to a stale socket.
            self.drv.disconnect();
        }
        Ok(json!({ "permissionOps": outcome.applied, "revoked": outcome.revoked }))
    }

    fn tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value> {
        self.route_selector_opt(
            req.timeout_ms,
            false,
            req.optional,
            |s, t| s.flutter_tap_on_element(&TapElementRequest { timeout_ms: t, ..req.clone() }),
            |ops, t| ops.tap_on_element(&TapElementRequest { timeout_ms: t, ..req.clone() }),
        )
    }

    fn tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value> {
        self.route_plain(false, |s| s.flutter_tap_on_point(req), |ops| ops.tap_on_point(req))
    }

    fn assert_visible(&mut self, selector: &ElementSelector, timeout_ms: u64, optional: bool) -> Result<Value> {
        self.route_selector_opt(
            timeout_ms,
            false,
            optional,
            |s, t| s.flutter_assert_visible(selector, t, optional),
            |ops, t| ops.assert_visible(selector, t),
        )
    }

    fn assert_not_visible(&mut self, selector: &ElementSelector, timeout_ms: u64, optional: bool) -> Result<Value> {
        // A system dialog on top leaves the (paused) Flutter tree answering
        // "not visible", so the window owner is checked before trusting it.
        self.route_selector(
            timeout_ms,
            true,
            |s, t| s.flutter_assert_not_visible(selector, t, optional),
            |ops, t| ops.assert_not_visible(selector, t),
        )
    }

    fn find_element_bounds(&mut self, selector: &ElementSelector, timeout_ms: u64, optional: bool) -> Result<[i64; 4]> {
        self.route_selector_opt(
            timeout_ms,
            false,
            optional,
            |s, t| s.flutter_find_element_bounds(selector, t, optional),
            |ops, t| ops.find_element_bounds(selector, t),
        )
    }

    fn input_text(&mut self, text: &str) -> Result<Value> {
        self.route_plain(false, |s| s.flutter_input_text(text), |ops| ops.input_text(text))
    }

    fn erase_text(&mut self, characters: i32) -> Result<Value> {
        self.route_plain(false, |s| s.flutter_erase_text(characters), |ops| ops.erase_text(characters))
    }

    fn copy_text_from(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        optional: bool,
    ) -> Result<(String, Value)> {
        self.route_selector_opt(
            timeout_ms,
            false,
            optional,
            |s, t| s.flutter_copy_text_from(selector, t, optional),
            |ops, t| ops.copy_text_from(selector, t),
        )
    }

    fn back_press(&mut self) -> Result<Value> {
        // Back must reach the window that owns the screen (a permission
        // dialog is not part of the Flutter navigator), so always look.
        self.route_plain(true, |s| s.flutter_back_press(), |ops| ops.back_press())
    }

    fn hide_keyboard(&mut self) -> Result<Value> {
        let strict = self.opts.maestro_compat_strict;
        self.route_plain(false, |s| s.flutter_hide_keyboard(), |ops| ops.hide_keyboard(strict))
    }

    fn press_key(&mut self, code: KeyCode) -> Result<Value> {
        // Dispatch BEFORE setting `native_hint = true` for `KeyCode::Home`:
        // otherwise `keys_use_host()` -> `decide(false)` probes focus while the
        // Flutter app is still in front and resets `native_hint` back to `false`.
        let res = if self.keys_use_host() { self.host_press_key(code) } else { self.flutter_press_key(code) }?;
        if code == KeyCode::Home {
            self.native_hint = true;
            self.cold_app = None;
        }
        Ok(res)
    }

    fn scroll(&mut self) -> Result<Value> {
        self.route_plain(false, |s| s.flutter_scroll(), |ops| ops.scroll())
    }

    fn swipe(&mut self, req: &SwipeRequest) -> Result<Value> {
        if req.element_selector.is_some() && req.direction.is_some() {
            return self.route_selector(
                req.timeout_ms,
                false,
                |s, t| s.flutter_swipe(&SwipeRequest { timeout_ms: t, ..req.clone() }),
                |ops, t| ops.swipe(&SwipeRequest { timeout_ms: t, ..req.clone() }),
            );
        }
        self.route_plain(false, |s| s.flutter_swipe(req), |ops| ops.swipe(req))
    }

    fn scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value> {
        // Check focus before starting a scroll loop so scrolling works in
        // browsers, Settings, and other external apps in `auto` mode.
        self.route_plain(true, |s| s.flutter_scroll_until_visible(req), |ops| ops.scroll_until_visible(req))
    }

    fn wait_for_animation_to_end(&mut self, timeout_ms: Option<u64>) -> Result<Value> {
        self.route_plain(
            false,
            |s| s.flutter_wait_for_animation_to_end(timeout_ms),
            |ops| ops.wait_for_animation_to_end(timeout_ms),
        )
    }

    fn screenshot_bytes(&mut self) -> Result<Vec<u8>> {
        self.dev.screenshot_bytes()
    }

    fn screenshot_image(&mut self) -> Result<DynamicImage> {
        self.dev.screenshot_image()
    }

    fn open_link(&mut self, link: &str, app_id: Option<&str>, auto_verify: bool, browser: bool) -> Result<Value> {
        // Another app is in front now: a launch's cold-start retry must not carry over to it.
        self.cold_app = None;
        if let Some(sim) = self.dev.as_ios() {
            sim.open_url(link)?;
            if self.ios_native.as_deref().is_some_and(IosNativeSlot::is_started) {
                self.native_hint = true;
            }
            return Ok(self.settle_if_connected());
        }
        let android = self.dev.as_android().ok_or_else(|| anyhow!("openLink: no device"))?;
        let warning = android.open_link(link, app_id, auto_verify, browser)?;
        // Opening a link can switch focus to a browser, chooser, or another app.
        self.native_hint = true;
        let mut res = match self.settle_if_connected() {
            Value::Null => json!({}),
            idle => json!({ "idle": idle }),
        };
        if let Some(w) = warning {
            res["warning"] = Value::from(w);
        }
        Ok(res)
    }

    fn set_location(&mut self, latitude: f64, longitude: f64) -> Result<Value> {
        if let Some(sim) = self.dev.as_ios() {
            sim.set_location(latitude, longitude)?;
            return Ok(json!({ "latitude": latitude, "longitude": longitude }));
        }
        let is_emu = self.dev.as_android().is_some_and(|a| crate::device::android::is_emulator(&a.serial));
        if is_emu {
            self.dev
                .as_android()
                .ok_or_else(|| anyhow!("setLocation: no device"))?
                .set_location(latitude, longitude)?;
            return Ok(json!({ "latitude": latitude, "longitude": longitude }));
        }
        if self.native_usable() {
            self.run_android_native(|ops| ops.set_location(latitude, longitude))?;
            return Ok(json!({ "latitude": latitude, "longitude": longitude, "lane": "native" }));
        }
        self.dev.as_android().ok_or_else(|| anyhow!("setLocation: no device"))?.set_location(latitude, longitude)?;
        Ok(json!({ "latitude": latitude, "longitude": longitude }))
    }

    fn set_orientation(&mut self, orientation: &str) -> Result<Value> {
        if self.platform() == Platform::Ios {
            if self.lane_mode() != InputMode::Flutter && self.native_usable() {
                let res = self.run_ios_native(|ops| ops.set_orientation(orientation))?;
                let _ = self.settle_if_connected();
                return Ok(res);
            }
            self.dev.as_ios().ok_or_else(|| anyhow!("setOrientation: no device"))?.set_orientation(orientation)?;
        } else {
            self.dev.as_android().ok_or_else(|| anyhow!("setOrientation: no device"))?.set_orientation(orientation)?;
            if let Some(client) = self.native.as_deref_mut().and_then(NativeLaneHandle::try_get) {
                client.invalidate_screen();
            }
        }
        // `Maestro.setOrientation` waits for the app to settle after rotating.
        Ok(self.settle_if_connected())
    }

    fn add_media(&mut self, media_paths: &[String]) -> Result<Value> {
        if let Some(sim) = self.dev.as_ios() {
            sim.add_media(media_paths)?;
            return Ok(json!({ "files": media_paths.len() }));
        }
        let android = self.dev.as_android().ok_or_else(|| anyhow!("addMedia: no device"))?;
        let remote = android.add_media(media_paths)?;
        Ok(json!({ "files": remote.len(), "devicePaths": remote }))
    }

    fn start_recording(&mut self, out_path: &Path) -> Result<Value> {
        let started = if let Some(sim) = self.dev.as_ios() {
            sim.start_recording(out_path)?
        } else {
            self.dev.as_android().ok_or_else(|| anyhow!("startRecording: no device"))?.start_recording(out_path)?
        };
        Ok(json!({ "path": out_path, "alreadyRecording": !started }))
    }

    fn stop_recording(&mut self) -> Result<Value> {
        let path = if let Some(sim) = self.dev.as_ios() {
            sim.stop_recording()?
        } else {
            self.dev.as_android().ok_or_else(|| anyhow!("stopRecording: no device"))?.stop_recording()?
        };
        Ok(json!({ "path": path }))
    }

    fn is_airplane_mode_enabled(&mut self) -> Result<bool> {
        match self.dev.as_android() {
            Some(android) => android.is_airplane_mode_enabled(),
            // Airplane mode does not exist on iOS simulators (Maestro parity).
            None => Ok(false),
        }
    }

    fn set_airplane_mode(&mut self, enabled: bool) -> Result<Value> {
        if self.platform() == Platform::Ios {
            return Ok(json!({
                "noop": true,
                "warning": "Airplane mode is not available on iOS simulators (Maestro parity: ignored)",
            }));
        }
        let android = self.dev.as_android().ok_or_else(|| anyhow!("setAirplaneMode: no device"))?;
        android.set_airplane_mode(enabled)?;
        Ok(json!({ "enabled": enabled }))
    }

    fn is_dark_mode_enabled(&mut self) -> Result<bool> {
        self.dev.is_dark_mode()
    }

    fn set_dark_mode(&mut self, enabled: bool) -> Result<Value> {
        if let Some(sim) = self.dev.as_ios() {
            sim.set_dark_mode(enabled)?;
            return Ok(self.settle_if_connected());
        }
        let android = self.dev.as_android().ok_or_else(|| anyhow!("setDarkMode: no device"))?;
        android.set_dark_mode(enabled)?;
        // Dark mode recreates the activity; let the app settle (`AndroidDriver` callers wait too).
        Ok(self.settle_if_connected())
    }

    fn check_busy_after(&mut self) -> Option<Value> {
        if self.drv.is_connected() { self.drv.call("busy", json!({})).ok() } else { None }
    }

    fn take_app_issues(&mut self) -> crate::app_health::IssueBatch {
        if !self.opts.app_errors.enabled() {
            return crate::app_health::IssueBatch::default();
        }
        if self.lanes.native != self.native_at_issue_take {
            self.native_at_issue_take = self.lanes.native;
            self.drv.poll_issues();
        }
        self.drv.take_issue_batch()
    }

    fn drain_app_issues(&mut self) -> crate::app_health::IssueBatch {
        if !self.opts.app_errors.enabled() {
            return crate::app_health::IssueBatch::default();
        }
        self.drv.poll_issues();
        self.drv.take_issue_batch()
    }

    fn trace_step(&mut self) -> crate::trace::TraceBatch {
        if !self.opts.trace {
            return crate::trace::TraceBatch::default();
        }
        let (now, before) = (self.lanes, self.trace_lanes);
        self.trace_lanes = now;
        let (flutter, native) = (now.flutter > before.flutter, now.native > before.native);
        self.trace_http_seen |= self.drv.http_log_enabled();
        // Native-lane steps send no driver RPC: re-read what finished meanwhile.
        if native {
            self.drv.poll_http();
        }
        let mut batch = self.drv.take_http_batch();
        batch.lane = Some(match (flutter, native) {
            (true, true) => "hybrid",
            (true, false) => "flutter",
            (false, true) => "native",
            (false, false) => "host",
        });
        batch
    }

    fn trace_drain(&mut self) -> crate::trace::TraceBatch {
        if !self.opts.trace {
            return crate::trace::TraceBatch::default();
        }
        self.trace_http_seen |= self.drv.http_log_enabled();
        let in_flight = self.drv.poll_http();
        let mut batch = self.drv.take_http_batch();
        batch.in_flight = in_flight;
        batch
    }

    fn trace_http_status(&self) -> &'static str {
        match (self.opts.trace, self.trace_http_seen || self.drv.http_log_enabled()) {
            (false, _) => "off",
            (true, true) => "enabled",
            (true, false) => "unsupported",
        }
    }

    fn diagnose_crash(&mut self) -> Option<Value> {
        // Only after the connection to the app dropped (and never after an
        // intentional stopApp / killApp / clearState): a failed assertion on
        // a live app is not a crash.
        if !self.opts.app_errors.enabled()
            || self.is_blackbox()
            || self.drv.is_connected()
            || self.is_intentionally_stopped()
        {
            return None;
        }
        match self.platform() {
            Platform::Android => self.diagnose_android_crash(),
            Platform::Ios => self.diagnose_ios_crash(),
        }
    }

    fn capture_failure_hierarchy(&mut self) -> Option<(String, crate::fake_driver::TreeNode)> {
        self.capture_failure_hierarchy_impl()
    }
}

/// Run one flow on a real Android or iOS Simulator device; returns `(passed, report_entry)`.
pub fn run_flow(flow: &ParsedFlow, s: &mut Session) -> (bool, Value) {
    let name = flow
        .config
        .name
        .clone()
        .unwrap_or_else(|| flow.path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    let prefix = s.opts.log_prefix.clone();
    if prefix.is_empty() {
        crate::orchestra::print_prefixed("", &format!("\n▶ {name}"));
    } else {
        crate::orchestra::print_prefixed(&prefix, &format!("▶ {name}"));
    }

    let opts = OrchestraOpts {
        lookup_timeout_ms: LOOKUP_TIMEOUT_MS,
        optional_lookup_timeout_ms: OPTIONAL_LOOKUP_TIMEOUT_MS,
        cli_env: s.opts.cli_env.clone(),
        test_output_dir: Some(s.opts.test_output_dir.clone()),
        flow_path: flow.path.clone(),
        device_id: Some(s.dev.id().to_string()),
        shard_index: s.opts.shard_index,
        log_prefix: prefix.clone(),
        check_settle: s.opts.check_settle,
        settle_policy: s.opts.settle_policy.clone(),
        print_progress: true,
        travel_sleep: true,
        http_agent: s.opts.http_agent.clone(),
        app_health: crate::app_health::AppHealthOpts { policy: s.opts.app_errors, ..Default::default() },
        trace: None,
    };
    let settle_policy = s.opts.settle_policy.clone();
    let check_settle = s.opts.check_settle;

    let mut backend = DeviceFlutterBackend::new(s, flow.app_id().to_string());
    let mut orchestra = Orchestra::new(&mut backend, opts);
    let result = orchestra.run_parsed_flow(flow);
    let lanes = backend.lane_stats();
    let blackbox = backend.ran_blackbox();

    crate::orchestra::print_prefixed(
        &prefix,
        &format!("{} {name} in {:.0} ms", if result.success { "PASSED" } else { "FAILED" }, result.total_ms),
    );
    if check_settle {
        crate::orchestra::print_prefixed(
            &prefix,
            &format!("settle check ({settle_policy}): {} violation(s)", result.settle_violations),
        );
    }
    let mut report = result.to_report_json(&name, &flow.path, &settle_policy);
    report["lane"] = json!(lanes.lane_name());
    report["laneStats"] = json!({ "flutter": lanes.flutter, "native": lanes.native });
    if lanes.cold_start_retries > 0 {
        report["coldStartRetries"] = json!(lanes.cold_start_retries);
    }
    if blackbox {
        report["blackbox"] = json!(true);
    }
    (result.success, report)
}

/// Session-level driver settings. Always sent on attach because an app
/// outliving an earlier session retains its old settings.
pub fn driver_config(opts: &Opts, platform: Platform, app_name: Option<&str>) -> Value {
    let mut cfg = json!({
        "settlePolicy": opts.settle_policy,
        "strictInput": opts.strict_input,
        "platform": platform.wire_name(),
        "strictHideKeyboard": opts.maestro_compat_strict && platform == Platform::Ios,
        // `""` clears a name left over from an earlier session.
        "appName": app_name.unwrap_or(""),
    });
    if let Some(sk) = opts.soft_keyboard_hint {
        cfg["softKeyboard"] = Value::from(sk);
    }
    // F7: explicit either way, so `off` also uninstalls hooks left over from
    // an earlier session with the same app process.
    cfg["appIssues"] = Value::Bool(opts.app_errors.enabled());
    // F8b: explicit either way, so a run without `--trace` also stops a log
    // left enabled by an earlier traced session in the same app process.
    cfg["httpLog"] = Value::Bool(opts.trace);
    // Explicit either way, for the same reason; older drivers ignore both.
    cfg["settleLoadTimeoutMs"] = Value::from(opts.settle_load.timeout_ms);
    cfg["settleHttpIgnore"] = json!(opts.settle_load.http_ignore);
    cfg
}

pub(super) fn add_unsupported_permissions_warning(res: &mut Value, unsupported: &[String]) {
    if !unsupported.is_empty() {
        res["unsupportedPermissions"] = json!(unsupported);
        res["warning"] = Value::from(format!(
            "permissions not settable on the iOS simulator via simctl (ignored): {}",
            unsupported.join(", ")
        ));
    }
}

/// Parses the `TAPWING_NOW:` device clock header + `logcat -b crash -d -v threadtime`
/// output and filters crashes to those occurring at or after `elapsed` before now.
fn parse_android_crash_probe(output: &str, package: &str, pid: Option<i64>, elapsed: Duration) -> Option<String> {
    let mut clock = None;
    let mut log = output;
    if let Some((first, rest)) = output.split_once('\n')
        && let Some(raw_dt) = first.trim().strip_prefix("TAPWING_NOW:")
    {
        clock = crate::app_health::parse_device_datetime(raw_dt);
        log = rest;
    } else if let Some(raw_dt) = output.trim().strip_prefix("TAPWING_NOW:") {
        clock = crate::app_health::parse_device_datetime(raw_dt);
        log = "";
    }
    let elapsed_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
    let since = clock.map(|c| (c.epoch_ms.saturating_sub(elapsed_ms), c));
    crate::app_health::android_crash_excerpt(log, package, pid, since)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_crash_probe_rejects_old_crashes_and_allows_recent_ones() {
        let log = "\
TAPWING_NOW:2026 11 25 13 15 00
--------- beginning of crash
11-25 13:10:00.000  1111  1111 E AndroidRuntime: FATAL EXCEPTION: main
11-25 13:10:00.000  1111  1111 E AndroidRuntime: Process: dev.tapwing.app, PID: 1111
11-25 13:10:00.000  1111  1111 E AndroidRuntime: java.lang.RuntimeException: old crash
";
        // Flow started 5 s ago (13:14:55): 13:10:00 crash is rejected whether PID is None or Some(1111).
        assert!(parse_android_crash_probe(log, "dev.tapwing.app", None, Duration::from_secs(5)).is_none());
        assert!(parse_android_crash_probe(log, "dev.tapwing.app", Some(1111), Duration::from_secs(5)).is_none());

        let with_new = format!(
            "{log}11-25 13:14:58.250  2222  2222 E AndroidRuntime: FATAL EXCEPTION: main\n\
             11-25 13:14:58.250  2222  2222 E AndroidRuntime: Process: dev.tapwing.app, PID: 2222\n\
             11-25 13:14:58.250  2222  2222 E AndroidRuntime: java.lang.IllegalStateException: new crash\n"
        );
        let excerpt = parse_android_crash_probe(&with_new, "dev.tapwing.app", None, Duration::from_secs(5)).unwrap();
        assert!(excerpt.contains("new crash"), "{excerpt}");
        assert!(!excerpt.contains("old crash"), "{excerpt}");

        // With known PID 2222, matches new crash; with stale PID 1111, rejects.
        assert!(parse_android_crash_probe(&with_new, "dev.tapwing.app", Some(2222), Duration::from_secs(5)).is_some());
        assert!(parse_android_crash_probe(&with_new, "dev.tapwing.app", Some(1111), Duration::from_secs(5)).is_none());
    }

    #[test]
    fn android_crash_probe_without_clock_requires_known_pid() {
        let log = "\
11-25 13:10:00.000  1111  1111 E AndroidRuntime: FATAL EXCEPTION: main
11-25 13:10:00.000  1111  1111 E AndroidRuntime: Process: dev.tapwing.app, PID: 1111
";
        assert!(parse_android_crash_probe(log, "dev.tapwing.app", None, Duration::from_secs(5)).is_none());
        assert!(parse_android_crash_probe(log, "dev.tapwing.app", Some(1111), Duration::from_secs(5)).is_some());
    }
}
