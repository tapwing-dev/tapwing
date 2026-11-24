//! In-app Flutter driver RPC operations (`tapwing_driver` TCP JSON-RPC) and
//! launch/attach helpers on [`DeviceFlutterBackend`].

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::backend::{AssertionError, ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest};
use crate::device::{LaunchMetrics, Platform};
use crate::flow::ir::{ElementSelector, KeyCode};
use crate::native_lane::InputMode;

use super::hybrid::DriverNotAttachedError;
use super::mapping::{
    IosHostKey, android_keycode, direction, driver_selector, in_app_key_name, ios_host_key, ios_native_key_name,
    is_in_app_key, is_in_app_key_ios, scale_crop_bounds, screen_swipe_ratios, scroll_duration_ms,
};
use super::{DeviceFlutterBackend, add_unsupported_permissions_warning, driver_config};

/// How long the driver waits for a host-injected Back to show an effect.
const HOST_BACK_TIMEOUT_MS: u64 = 1000;

/// Result of [`DeviceFlutterBackend::launch_handshake`].
pub(super) struct Handshake {
    /// `waitIdle` result of the answered attempt.
    pub(super) idle: Value,
    pub(super) configure_ms: f64,
    pub(super) wait_idle_ms: f64,
    /// `1` when the first attempt got no answer and the reconnect answered.
    pub(super) retries: u32,
}

fn is_unknown_method(e: &anyhow::Error) -> bool {
    e.downcast_ref::<crate::driver::DriverError>().is_some_and(|d| d.code == "UNKNOWN_METHOD")
}

impl DeviceFlutterBackend<'_> {
    pub(super) fn configure_driver(&mut self, app_id: &str) -> Result<()> {
        let platform = self.platform();
        let app_name = if platform == Platform::Ios { self.dev.app_display_name(app_id) } else { None };
        let res = self.drv.call("configure", driver_config(self.opts, platform, app_name.as_deref()))?;
        *self.units_scale = res["pixelsPerUnit"].as_f64().filter(|v| *v > 0.0).unwrap_or(1.0);
        // F8b: older drivers ignore `httpLog` and never confirm it.
        self.drv.set_http_log(self.opts.trace && res["httpLog"] == true);
        Ok(())
    }

    /// `configure` + `waitIdle` right after a fresh app connection, with one
    /// bounded cold-start retry.
    ///
    /// A freshly (re)started app can say hello and then go silent: on a
    /// CPU-starved emulator its isolate (or the `adb reverse` tunnel) gave
    /// no `progress` heartbeat for the whole 10 s window. When the first
    /// attempt fails with exactly that silence (not a closed socket, not a
    /// dead process, not a driver error), the connection is dropped, the
    /// same process (`pid`) gets one silence window to dial in again, and
    /// the handshake is sent once more on the new connection. Both calls are
    /// idempotent (settings and a read-only wait); no app action is
    /// repeated. A healthy first attempt costs nothing extra; a persistent
    /// failure ends after at most two more silence windows (2 × 10 s) with
    /// the same error as before.
    pub(super) fn launch_handshake(&mut self, app_id: &str, pid: Option<i64>) -> Result<Handshake> {
        let first = self.handshake_once(app_id);
        let err = match first {
            Ok(hs) => return Ok(hs),
            Err(e) if crate::driver::is_rpc_silence(&e) => e,
            Err(e) => return Err(e),
        };
        let window = self.drv.silence_timeout();
        let who = pid.map_or_else(|| "the app".to_string(), |p| format!("the app (pid {p})"));
        eprintln!(
            "{}warning: launchApp {app_id}: {who} went silent right after its cold start ({err}); \
             reconnecting once (cold-start retry, at most {} more)",
            self.opts.log_prefix,
            crate::driver::human_duration(2 * window)
        );
        // Counted from here on, so a retry that fails is reported as well.
        self.lanes.cold_start_retries += 1;
        // The whole retry (reconnect plus handshake) gets two silence windows, measured now.
        let budget_end = Instant::now() + 2 * window;
        self.drv.disconnect();
        if let Err(e) = self.drv.accept_same_process(window, pid) {
            if crate::interrupt::is_interrupted_error(&e) {
                return Err(e);
            }
            // It never dialed in again: report the original failure.
            return Err(err);
        }
        let res = self.handshake_within(app_id, budget_end, window);
        self.drv.set_silence_timeout(window);
        match res {
            Ok(mut hs) => {
                hs.retries = 1;
                Ok(hs)
            }
            // Silent again: the same failure as before, not a second, differently named one.
            Err(e) if crate::driver::is_rpc_silence(&e) => Err(err),
            Err(e) => Err(e),
        }
    }

    /// [`Self::handshake_once`] with every call's silence window cut to what is left
    /// of `end`, so slow stages cannot add up past the budget. A silence window is only
    /// checked every 250 ms, so when less than that is left the budget is spent: the next
    /// call is not started and the failure is reported as the silence it ends in.
    fn handshake_within(&mut self, app_id: &str, end: Instant, window: Duration) -> Result<Handshake> {
        const SMALLEST: Duration = Duration::from_millis(250);
        let bound = |s: &mut Self, method: &str| -> Result<()> {
            let left = end.saturating_duration_since(Instant::now());
            if left < SMALLEST {
                return Err(crate::driver::RpcSilenceError { method: method.to_string(), silence: window }.into());
            }
            s.drv.set_silence_timeout(left.min(window));
            Ok(())
        };
        bound(self, "configure")?;
        let t_cfg = Instant::now();
        self.configure_driver(app_id)?;
        let configure_ms = t_cfg.elapsed().as_secs_f64() * 1000.0;
        bound(self, "waitIdle")?;
        let t_idle = Instant::now();
        let idle = self.drv.call("waitIdle", json!({ "timeoutMs": 5000 }))?;
        let wait_idle_ms = t_idle.elapsed().as_secs_f64() * 1000.0;
        Ok(Handshake { idle, configure_ms, wait_idle_ms, retries: 0 })
    }

    fn handshake_once(&mut self, app_id: &str) -> Result<Handshake> {
        let t_cfg = Instant::now();
        self.configure_driver(app_id)?;
        let configure_ms = t_cfg.elapsed().as_secs_f64() * 1000.0;
        let t_idle = Instant::now();
        let idle = self.drv.call("waitIdle", json!({ "timeoutMs": 5000 }))?;
        let wait_idle_ms = t_idle.elapsed().as_secs_f64() * 1000.0;
        Ok(Handshake { idle, configure_ms, wait_idle_ms, retries: 0 })
    }

    fn attach(&mut self, app_id: &str, timeout: Duration) -> Result<Value> {
        let port = self.drv.port()?;
        self.dev.prepare_attach(app_id, port)?;
        let hello = self.drv.accept(timeout)?.clone();
        self.configure_driver(app_id)?;
        Ok(hello)
    }

    fn ensure_connected(&mut self) -> Result<()> {
        if !self.drv.is_connected() {
            let app_id = self.flow_app.clone();
            self.attach(&app_id, Duration::from_secs(5)).map_err(|e| {
                if crate::interrupt::is_interrupted_error(&e) {
                    return e;
                }
                anyhow::Error::new(DriverNotAttachedError(format!(
                    "{app_id} is not running with tapwing_driver (no launchApp before this step?): {e}"
                )))
            })?;
        }
        Ok(())
    }

    pub(super) fn settle_if_connected(&mut self) -> Value {
        if self.drv.is_connected() {
            self.drv.call("waitIdle", json!({ "timeoutMs": 3000 })).unwrap_or(Value::Null)
        } else {
            Value::Null
        }
    }

    /// `launchApp` without a Flutter handshake (`--input native` or an
    /// uninstrumented app): the host launch is already done; wait for the
    /// native lane and for the UI to settle.
    pub(super) fn finish_native_launch(&mut self, launch: &LaunchMetrics, why: &str) -> Result<Value> {
        let t = Instant::now();
        let app_id = self.flow_app.clone();
        let settled = self.run_native(|ops| ops.settle_launch(&app_id));
        let shown = settled.map_err(|e| {
            if crate::interrupt::is_interrupted_error(&e) {
                return e;
            }
            anyhow!("{why}, and the native lane could not take over: {e:#}")
        })?;
        // Not under `--maestro-compat=strict`: the extra lookup window changes
        // Maestro-visible timing (Maestro's first lookup is the plain 17 s).
        if !shown && !self.opts.maestro_compat_strict {
            self.cold_app = Some(app_id);
        }
        let start_key = match self.platform() {
            Platform::Android => "amStartMs",
            Platform::Ios => "simctlLaunchMs",
        };
        let mut res = json!({
            "lane": "native",
            "blackbox": true,
            "pid": launch.pid,
            "grantedPermissions": launch.granted_permissions,
            "note": why,
            "phases": {
                "stopClearMs": launch.stop_clear_ms,
                "permissionsMs": launch.permissions_ms,
                start_key: launch.start_ms,
                "nativeSettleMs": t.elapsed().as_secs_f64() * 1000.0,
            }
        });
        add_unsupported_permissions_warning(&mut res, &launch.unsupported_permissions);
        Ok(res)
    }

    pub(super) fn flutter_tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value> {
        self.ensure_connected()?;
        let repeat = req.repeat.as_ref();
        self.drv.call(
            "tapOn",
            json!({
                "selector": driver_selector(&req.selector)?,
                "timeoutMs": req.timeout_ms,
                "longPress": req.long_press,
                "repeat": repeat.map(|r| r.repeat),
                "delayMs": repeat.map(|r| r.delay),
                "relativePoint": req.relative_point,
                "retryIfNoChange": req.retry_if_no_change,
                "waitUntilVisible": req.wait_until_visible,
                "settleTimeoutMs": req.wait_to_settle_timeout_ms,
            }),
        )
    }

    pub(super) fn flutter_tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value> {
        self.ensure_connected()?;
        let repeat = req.repeat.as_ref();
        self.drv.call(
            "tapPoint",
            json!({
                "point": req.point,
                "longPress": req.long_press,
                "repeat": repeat.map(|r| r.repeat),
                "delayMs": repeat.map(|r| r.delay),
                "retryIfNoChange": req.retry_if_no_change,
                "settleTimeoutMs": req.wait_to_settle_timeout_ms,
            }),
        )
    }

    pub(super) fn flutter_assert_visible(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        _optional: bool,
    ) -> Result<Value> {
        self.ensure_connected()?;
        self.drv.call("assertVisible", json!({ "selector": driver_selector(selector)?, "timeoutMs": timeout_ms }))
    }

    pub(super) fn flutter_assert_not_visible(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        _optional: bool,
    ) -> Result<Value> {
        self.ensure_connected()?;
        self.drv.call("assertNotVisible", json!({ "selector": driver_selector(selector)?, "timeoutMs": timeout_ms }))
    }

    pub(super) fn flutter_find_element_bounds(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        _optional: bool,
    ) -> Result<[i64; 4]> {
        self.ensure_connected()?;
        let res = self
            .drv
            .call("assertVisible", json!({ "selector": driver_selector(selector)?, "timeoutMs": timeout_ms }))?;
        let arr = res["element"]["bounds"].as_array().ok_or_else(|| anyhow!("cropOn element has no bounds"))?;
        if arr.len() != 4 {
            bail!("cropOn element bounds malformed");
        }
        let mut bounds = [0_i64; 4];
        for (idx, val) in arr.iter().enumerate() {
            let f = val.as_f64().ok_or_else(|| anyhow!("cropOn element bound is not a number"))?;
            bounds[idx] = f.round() as i64;
        }
        if bounds[2] - bounds[0] <= 0 || bounds[3] - bounds[1] <= 0 {
            return Err(AssertionError(format!(
                "Cannot crop screenshot: element has invalid dimensions (width: {}, height: {})",
                bounds[2] - bounds[0],
                bounds[3] - bounds[1]
            ))
            .into());
        }
        Ok(scale_crop_bounds(bounds, *self.units_scale))
    }

    pub(super) fn flutter_input_text(&mut self, text: &str) -> Result<Value> {
        self.ensure_connected()?;
        self.drv.call("inputText", json!({ "text": text }))
    }

    pub(super) fn flutter_erase_text(&mut self, characters: i32) -> Result<Value> {
        self.ensure_connected()?;
        self.drv.call("eraseText", json!({ "count": characters }))
    }

    pub(super) fn flutter_copy_text_from(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        _optional: bool,
    ) -> Result<(String, Value)> {
        self.ensure_connected()?;
        let res = self
            .drv
            .call("copyTextFrom", json!({ "selector": driver_selector(selector)?, "timeoutMs": timeout_ms }))?;
        let text = res["text"].as_str().ok_or_else(|| anyhow!("driver copyTextFrom returned no text"))?.to_string();
        Ok((text, res))
    }

    pub(super) fn flutter_back_press(&mut self) -> Result<Value> {
        if self.platform() == Platform::Ios {
            return Ok(json!({ "noop": true, "note": "IOSDriver.backPress() is a no-op" }));
        }
        self.ensure_connected()?;
        self.android_host_back()
    }

    /// Android Back as a real `KEYCODE_BACK`, like Maestro's
    /// `AndroidDriver.backPress`: the OS decides whether the IME, the Flutter
    /// framework or (with `enableOnBackInvokedCallback="true"` and a framework
    /// that did not claim Back) the system itself handles it. Delivering
    /// `popRoute` in-app instead would hide apps that Back sends to the
    /// background. The driver snapshots state before the key (`armBack`) and
    /// reports what happened (`awaitBack`: `navigation`, `keyboard`,
    /// `background` or `none`), then settles unless the app left the
    /// foreground.
    fn android_host_back(&mut self) -> Result<Value> {
        let dev = self.dev.as_android().ok_or_else(|| anyhow!("back: no Android device"))?;
        if let Err(e) = self.drv.call("armBack", json!({})) {
            if is_unknown_method(&e) {
                // Driver built before host Back: keep the in-app `popRoute`.
                return self.drv.call("back", json!({}));
            }
            return Err(e);
        }
        dev.keyevent_fast(android_keycode(KeyCode::Back))?;
        let mut res = self.drv.call("awaitBack", json!({ "timeoutMs": HOST_BACK_TIMEOUT_MS }))?;
        res["keycode"] = json!(android_keycode(KeyCode::Back));
        if res["outcome"] == "background" {
            // The Flutter semantics tree outlives the app leaving the screen,
            // so look at window focus before trusting it again (as after Home).
            self.native_hint = true;
            res["warning"] = json!(
                "Back sent the app to the background: Android handled it, not the Flutter navigator \
                 (enableOnBackInvokedCallback with a route the framework did not claim)"
            );
        }
        Ok(res)
    }

    pub(super) fn flutter_hide_keyboard(&mut self) -> Result<Value> {
        self.ensure_connected()?;
        if self.platform() != Platform::Android {
            return self.drv.call("hideKeyboard", json!({}));
        }
        // Without a soft keyboard Maestro's `hideKeyboard` is a plain Back
        // (`AndroidDriver.kt:490-496`); the driver hands that Back to the host.
        let res = self.drv.call("hideKeyboard", json!({ "hostBack": true }))?;
        if res["hostBack"] != true {
            return Ok(res);
        }
        let mut back = self.android_host_back()?;
        back["warning"] = match (res["warning"].as_str(), back["warning"].as_str()) {
            (Some(quirk), Some(background)) => json!(format!("{quirk}. {background}")),
            (Some(quirk), None) => json!(quirk),
            (None, _) => back["warning"].take(),
        };
        Ok(back)
    }

    pub(super) fn flutter_press_key(&mut self, code: KeyCode) -> Result<Value> {
        if self.platform() == Platform::Ios {
            if is_in_app_key_ios(code) {
                self.ensure_connected()?;
                return self.drv.call("pressKey", json!({ "key": in_app_key_name(code) }));
            }
            return match ios_host_key(code) {
                IosHostKey::Unsupported => {
                    if self.lane_mode() != InputMode::Flutter && self.native_usable() {
                        return self.run_ios_native(|ops| ops.press_key(ios_native_key_name(code)));
                    }
                    bail!(
                        "pressKey {code:?} on the iOS simulator requires the XCTest native lane \
                         (run with --input auto or --input native and Maestro installed)"
                    )
                }
                IosHostKey::Ignored => Ok(json!({ "noop": true, "note": "IOSDriver.pressKey ignores this key" })),
            };
        }
        if code == KeyCode::Back {
            return self.flutter_back_press();
        }
        if is_in_app_key(code) {
            self.ensure_connected()?;
            self.drv.call("pressKey", json!({ "key": in_app_key_name(code) }))
        } else {
            let dev = self.dev.as_android().ok_or_else(|| anyhow!("pressKey: no Android device"))?;
            let keycode = android_keycode(code);
            if matches!(code, KeyCode::Power | KeyCode::Lock) {
                dev.keyevent_with_sleep(keycode)?;
                Ok(json!({ "keycode": keycode }))
            } else {
                dev.keyevent_fast(keycode)?;
                let settle = if self.drv.is_connected() {
                    self.drv.call("waitIdle", json!({ "timeoutMs": 300 })).ok()
                } else {
                    None
                };
                Ok(json!({ "keycode": keycode, "idle": settle }))
            }
        }
    }

    pub(super) fn flutter_scroll(&mut self) -> Result<Value> {
        self.ensure_connected()?;
        let duration_ms = scroll_duration_ms(self.platform());
        self.drv.call("swipe", json!({ "start": "50%, 50%", "end": "50%, 10%", "durationMs": duration_ms }))
    }

    pub(super) fn flutter_swipe(&mut self, req: &SwipeRequest) -> Result<Value> {
        self.ensure_connected()?;
        let platform = self.platform();
        if let (Some(sel), Some(dir)) = (&req.element_selector, req.direction) {
            return self.drv.call(
                "swipe",
                json!({
                    "selector": driver_selector(sel)?,
                    "direction": direction(dir),
                    "relativePoint": req.relative_point,
                    "timeoutMs": req.timeout_ms,
                    "durationMs": req.duration_ms,
                    "settleTimeoutMs": req.wait_to_settle_timeout_ms,
                }),
            );
        }
        let (start, end) =
            match (&req.start_relative, &req.end_relative, req.direction, &req.start_point, &req.end_point) {
                (Some(a), Some(b), ..) => (a.clone(), b.clone()),
                (_, _, Some(d), ..) => {
                    let (a, b) = screen_swipe_ratios(d, platform);
                    (a.to_string(), b.to_string())
                }
                (_, _, None, Some(a), Some(b)) => (format!("{}, {}", a.x, a.y), format!("{}, {}", b.x, b.y)),
                _ => bail!("illegal arguments for swipe"),
            };
        self.drv.call(
            "swipe",
            json!({
                "start": start,
                "end": end,
                "durationMs": req.duration_ms,
                "settleTimeoutMs": req.wait_to_settle_timeout_ms,
            }),
        )
    }

    pub(super) fn flutter_scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value> {
        self.ensure_connected()?;
        self.drv.call(
            "scrollUntilVisible",
            json!({
                "selector": driver_selector(&req.selector)?,
                "direction": direction(req.direction),
                "timeoutMs": req.timeout_ms,
                "durationMs": req.duration_ms,
                "visibilityPercentage": req.visibility_percentage,
                "centerElement": req.center_element,
                "settleTimeoutMs": req.wait_to_settle_timeout_ms,
            }),
        )
    }

    pub(super) fn flutter_wait_for_animation_to_end(&mut self, timeout_ms: Option<u64>) -> Result<Value> {
        self.ensure_connected()?;
        self.drv.call("waitIdle", json!({ "timeoutMs": timeout_ms.unwrap_or(15_000) }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::Value;

    use crate::device::Device;
    use crate::driver::Driver;
    use crate::native_lane::InputMode;
    use crate::run::{DeviceFlutterBackend, Opts, Session};

    /// Short for fast tests, but long against the 250 ms silence polling and the scheduling
    /// jitter of a loaded machine (the retry budget is two of these).
    const SILENCE: Duration = Duration::from_millis(600);
    const PID: i64 = 42;

    /// What the fake app does on one connection.
    #[derive(Clone, Copy)]
    enum Conn {
        /// Answers the first `n` requests, then reads but never answers
        /// (a starved isolate or a wedged tunnel) until the host hangs up.
        SilentAfter(usize),
        /// Answers every request.
        Answer,
        /// Closes the socket on the first request.
        Close,
        /// Answers the first request with a driver error.
        DriverError,
    }

    /// A fake `tapwing_driver` that dials in once per entry of `script`
    /// (with hello `pid`), reconnecting after the host hangs up, and records
    /// the methods it received per connection.
    /// Methods received, one list per connection.
    type Seen = Arc<Mutex<Vec<Vec<String>>>>;

    fn fake_app(port: u16, script: Vec<(i64, Conn)>) -> (Seen, std::thread::JoinHandle<()>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let handle = std::thread::spawn(move || {
            for (pid, conn) in script {
                let Ok(mut app) = TcpStream::connect(("127.0.0.1", port)) else { return };
                // The host may already be gone (a handshake that ended without a retry): not an error.
                if writeln!(app, r#"{{"event":"hello","params":{{"protocol":1,"pid":{pid},"platform":"android"}}}}"#)
                    .is_err()
                {
                    return;
                }
                log.lock().unwrap().push(Vec::new());
                let mut reader = BufReader::new(app.try_clone().unwrap());
                let mut answered = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break; // The host hung up.
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let (id, method) = (req["id"].as_u64().unwrap(), req["method"].as_str().unwrap().to_string());
                    log.lock().unwrap().last_mut().unwrap().push(method.clone());
                    let reply = match conn {
                        Conn::SilentAfter(n) if answered >= n => continue,
                        Conn::Close => break,
                        Conn::DriverError => {
                            format!(r#"{{"id":{id},"error":{{"code":"INTERNAL","message":"boom"}}}}"#)
                        }
                        _ if method == "configure" => format!(r#"{{"id":{id},"result":{{"pixelsPerUnit":1}},"us":1}}"#),
                        _ => format!(r#"{{"id":{id},"result":{{"idle":true}},"us":1}}"#),
                    };
                    answered += 1;
                    if writeln!(app, "{reply}").is_err() {
                        break;
                    }
                }
            }
        });
        (seen, handle)
    }

    fn opts() -> Opts {
        Opts {
            settle_policy: "v2".into(),
            check_settle: false,
            cli_env: HashMap::new(),
            test_output_dir: PathBuf::from("out"),
            strict_input: false,
            soft_keyboard_hint: None,
            maestro_compat_strict: false,
            http_agent: None,
            input_mode: InputMode::Flutter,
            shard_index: None,
            log_prefix: String::new(),
            app_errors: crate::app_health::AppErrorsPolicy::Report,
            trace: false,
        }
    }

    /// Runs the launch handshake against a fake app following `script`,
    /// returning the outcome (retries or error text), the elapsed time, and
    /// the methods received per connection.
    fn handshake(script: Vec<(i64, Conn)>) -> (Result<u32, String>, Duration, Vec<Vec<String>>) {
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        drv.set_silence_timeout(SILENCE);
        let (seen, app) = fake_app(drv.port().unwrap(), script);
        drv.accept_with_pid(Duration::from_secs(2), Some(PID), &[]).unwrap();
        let opts = opts();
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        let t = Instant::now();
        let res = backend.launch_handshake("com.example.app", Some(PID));
        let elapsed = t.elapsed();
        let res = res.map(|hs| hs.retries).map_err(|e| e.to_string());
        drop(backend);
        drop(drv); // Hangs up on the fake app so its thread ends.
        app.join().unwrap();
        let seen = seen.lock().unwrap().clone();
        (res, elapsed, seen)
    }

    fn methods(list: &[&str]) -> Vec<String> {
        list.iter().map(|m| m.to_string()).collect()
    }

    const SILENT_CONFIGURE: &str =
        "connection to app lost during `configure`: no response for 600 ms (app hung or unreachable)";

    #[test]
    fn a_healthy_handshake_sends_configure_and_wait_idle_once_on_one_connection() {
        let (res, elapsed, seen) = handshake(vec![(PID, Conn::Answer)]);
        assert_eq!(res, Ok(0));
        assert_eq!(seen, vec![methods(&["configure", "waitIdle"])], "no extra RPC, no reconnect");
        assert!(elapsed < SILENCE, "{elapsed:?}");
    }

    #[test]
    fn a_silent_first_configure_reconnects_once_and_the_handshake_passes() {
        let (res, elapsed, seen) = handshake(vec![(PID, Conn::SilentAfter(0)), (PID, Conn::Answer)]);
        assert_eq!(res, Ok(1));
        assert_eq!(seen, vec![methods(&["configure"]), methods(&["configure", "waitIdle"])]);
        assert!(elapsed >= SILENCE && elapsed < 2 * SILENCE, "{elapsed:?}");
    }

    #[test]
    fn a_silent_launch_wait_idle_is_retried_with_the_whole_handshake() {
        let (res, _, seen) = handshake(vec![(PID, Conn::SilentAfter(1)), (PID, Conn::Answer)]);
        assert_eq!(res, Ok(1));
        assert_eq!(seen, vec![methods(&["configure", "waitIdle"]), methods(&["configure", "waitIdle"])]);
    }

    #[test]
    fn a_handshake_that_stays_silent_fails_after_exactly_one_retry_with_the_same_error() {
        let (res, elapsed, seen) =
            handshake(vec![(PID, Conn::SilentAfter(0)), (PID, Conn::SilentAfter(0)), (PID, Conn::Answer)]);
        assert_eq!(res, Err(SILENT_CONFIGURE.to_string()));
        // Two connections were used: the original and one retry, never a third.
        assert_eq!(seen[..2], [methods(&["configure"]), methods(&["configure"])]);
        assert!(seen.len() <= 3 && seen.get(2).is_none_or(Vec::is_empty), "{seen:?}");
        // Worst case: the first silence + reconnect window + one more silence.
        assert!(elapsed >= 2 * SILENCE && elapsed < 3 * SILENCE + Duration::from_millis(250), "{elapsed:?}");
    }

    #[test]
    fn an_app_that_never_dials_back_fails_with_the_original_error() {
        // The same process never reconnects; a different process (pid 43)
        // must not be taken for it.
        for script in [vec![(PID, Conn::SilentAfter(0))], vec![(PID, Conn::SilentAfter(0)), (43, Conn::Answer)]] {
            let (res, elapsed, seen) = handshake(script);
            assert_eq!(res, Err(SILENT_CONFIGURE.to_string()));
            assert_eq!(seen[0], methods(&["configure"]));
            assert!(seen.get(1).is_none_or(Vec::is_empty), "pid 43 got no request: {seen:?}");
            assert!(elapsed >= 2 * SILENCE && elapsed < 2 * SILENCE + Duration::from_millis(250), "{elapsed:?}");
        }
    }

    #[test]
    fn only_silence_is_retried() {
        let (res, elapsed, seen) = handshake(vec![(PID, Conn::Close), (PID, Conn::Answer)]);
        assert_eq!(res, Err("app closed the connection during `configure`".to_string()));
        assert_eq!(seen[0], methods(&["configure"]));
        assert!(seen.get(1).is_none_or(Vec::is_empty), "{seen:?}");
        assert!(elapsed < SILENCE, "{elapsed:?}");

        let (res, _, seen) = handshake(vec![(PID, Conn::DriverError), (PID, Conn::Answer)]);
        assert_eq!(res, Err("boom".to_string()));
        assert_eq!(seen[0], methods(&["configure"]));
        assert!(seen.get(1).is_none_or(Vec::is_empty), "{seen:?}");
    }

    #[test]
    fn progress_heartbeats_prevent_silence_retry() {
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        drv.set_silence_timeout(SILENCE);
        let port = drv.port().unwrap();
        let connections = Arc::new(Mutex::new(0usize));
        let conns = connections.clone();
        let app = std::thread::spawn(move || {
            let Ok(mut sock) = TcpStream::connect(("127.0.0.1", port)) else { return };
            *conns.lock().unwrap() += 1;
            writeln!(sock, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID},"platform":"android"}}}}"#)
                .unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let (id, method) = (req["id"].as_u64().unwrap(), req["method"].as_str().unwrap());
                if method == "configure" {
                    // Emit progress heartbeats for longer (800 ms) than the 600 ms silence window.
                    for _ in 0..8 {
                        std::thread::sleep(Duration::from_millis(100));
                        writeln!(sock, r#"{{"event":"progress","id":{id}}}"#).unwrap();
                    }
                    writeln!(sock, r#"{{"id":{id},"result":{{"pixelsPerUnit":1}},"us":1}}"#).unwrap();
                } else {
                    writeln!(sock, r#"{{"id":{id},"result":{{"idle":true}},"us":1}}"#).unwrap();
                }
            }
        });
        drv.accept_with_pid(Duration::from_secs(2), Some(PID), &[]).unwrap();
        let opts = opts();
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        let hs = backend.launch_handshake("com.example.app", Some(PID)).unwrap();
        assert_eq!(hs.retries, 0, "heartbeats must keep the connection alive without retrying");
        assert_eq!(*connections.lock().unwrap(), 1);
        drop(backend);
        drop(drv);
        app.join().unwrap();
    }

    #[test]
    fn late_response_after_silence_never_corrupts_retry_connection() {
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        drv.set_silence_timeout(SILENCE);
        let port = drv.port().unwrap();
        let app = std::thread::spawn(move || {
            // Connection 1: stays silent until the host times out and hangs up,
            // then attempts to write a late error reply on the closed socket.
            let mut first = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(first, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID},"platform":"android"}}}}"#)
                .unwrap();
            let mut r1 = BufReader::new(first.try_clone().unwrap());
            let mut line = String::new();
            let _ = r1.read_line(&mut line); // configure
            line.clear();
            let _ = r1.read_line(&mut line); // blocks until host drops socket on silence
            let _ = writeln!(first, r#"{{"id":1,"error":{{"code":"INTERNAL","message":"stale"}}}}"#);
            drop(first);

            // Connection 2: clean reconnect for the same PID; injects a mismatched-id
            // stale response ahead of the real reply to verify request-id correlation.
            let Ok(mut second) = TcpStream::connect(("127.0.0.1", port)) else { return };
            writeln!(second, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID},"platform":"android"}}}}"#)
                .unwrap();
            let mut r2 = BufReader::new(second.try_clone().unwrap());
            loop {
                let mut l = String::new();
                if r2.read_line(&mut l).unwrap_or(0) == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&l).unwrap();
                let (id, method) = (req["id"].as_u64().unwrap(), req["method"].as_str().unwrap());
                if method == "configure" {
                    let _ = writeln!(second, r#"{{"id":999,"error":{{"code":"INTERNAL","message":"stale-id"}}}}"#);
                }
                let reply = if method == "configure" {
                    format!(r#"{{"id":{id},"result":{{"pixelsPerUnit":1}},"us":1}}"#)
                } else {
                    format!(r#"{{"id":{id},"result":{{"idle":true}},"us":1}}"#)
                };
                if writeln!(second, "{reply}").is_err() {
                    break;
                }
            }
        });
        drv.accept_with_pid(Duration::from_secs(2), Some(PID), &[]).unwrap();
        let opts = opts();
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        let hs = backend.launch_handshake("com.example.app", Some(PID)).unwrap();
        assert_eq!(hs.retries, 1);
        drop(backend);
        drop(drv);
        app.join().unwrap();
    }

    #[test]
    fn same_pid_reconnect_must_not_duplicate_f7_issues_on_poll() {
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        drv.set_silence_timeout(SILENCE);
        let port = drv.port().unwrap();
        let app = std::thread::spawn(move || {
            let issue_json = serde_json::json!({
                "id": 1, "seq": 1, "kind": "buildError", "message": "startup boom",
                "summary": "startup boom", "request": serde_json::Value::Null, "count": 1,
                "firstSeenMs": 1, "lastSeenMs": 1
            });
            // Conn 1: configure delivers startup issue seq=1 (and dropped=2), then waitIdle goes silent.
            let mut c1 = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(c1, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID},"platform":"android"}}}}"#).unwrap();
            let mut r1 = BufReader::new(c1.try_clone().unwrap());
            let mut line = String::new();
            r1.read_line(&mut line).unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let id = req["id"].as_u64().unwrap();
            writeln!(
                c1,
                "{}",
                serde_json::json!({
                    "id": id, "result": {"pixelsPerUnit": 1}, "us": 1,
                    "issues": [issue_json], "droppedIssues": 2
                })
            )
            .unwrap();
            // Read waitIdle and go silent until host hangs up.
            line.clear();
            let _ = r1.read_line(&mut line);
            line.clear();
            let _ = r1.read_line(&mut line);
            drop(c1);

            // Conn 2 (same process): AppIssueLog still retains seq=1 and dropped=2 with _deliveredSeq=1.
            let Ok(mut c2) = TcpStream::connect(("127.0.0.1", port)) else { return };
            writeln!(c2, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID},"platform":"android"}}}}"#).unwrap();
            let mut r2 = BufReader::new(c2.try_clone().unwrap());
            loop {
                let mut l = String::new();
                if r2.read_line(&mut l).unwrap_or(0) == 0 {
                    break;
                }
                let req: Value = serde_json::from_str(&l).unwrap();
                let (id, method) = (req["id"].as_u64().unwrap(), req["method"].as_str().unwrap());
                let reply = match method {
                    "configure" => serde_json::json!({"id": id, "result": {"pixelsPerUnit": 1}, "us": 1}),
                    "appIssues" => {
                        let since = req["params"]["since"].as_u64().unwrap_or(0);
                        let items = if since < 1 { vec![issue_json.clone()] } else { vec![] };
                        serde_json::json!({
                            "id": id,
                            "result": { "issues": items, "latestSeq": 1, "dropped": 2 },
                            "us": 1
                        })
                    }
                    _ => serde_json::json!({"id": id, "result": {"idle": true}, "us": 1}),
                };
                if writeln!(c2, "{reply}").is_err() {
                    break;
                }
            }
        });

        drv.accept_with_pid(Duration::from_secs(2), Some(PID), &[]).unwrap();
        let opts = opts();
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        let hs = backend.launch_handshake("com.example.app", Some(PID)).unwrap();
        assert_eq!(hs.retries, 1);

        // Step 1 (`launchApp`) collects seq=1 and dropped=2.
        let batch1 = backend.drv.take_issue_batch();
        assert_eq!(batch1.current.len(), 1);
        assert_eq!(batch1.dropped, 2);

        // Flow-end drain (`poll_issues`) must NOT re-deliver seq=1 or double-count dropped=2.
        backend.drv.poll_issues();
        let batch2 = backend.drv.take_issue_batch();
        drop(backend);
        drop(drv);
        app.join().unwrap();

        assert!(batch2.current.is_empty() && batch2.late.is_empty(), "duplicate issue re-fetched: {batch2:?}");
        assert_eq!(batch2.dropped, 0, "dropped issues double-counted: {batch2:?}");
    }

    #[test]
    fn handshake_retry_added_time_stays_within_two_silence_windows() {
        // Use an exact multiple of RPC_POLL_INTERVAL (250 ms) so quantization does not skew the bound.
        let silence = Duration::from_millis(500);
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        drv.set_silence_timeout(silence);
        let port = drv.port().unwrap();
        let app = std::thread::spawn(move || {
            // Conn 1: silent on configure -> host drops after 1x silence (500 ms).
            let c1 = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(&c1, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID},"platform":"android"}}}}"#).unwrap();
            let mut r1 = BufReader::new(c1.try_clone().unwrap());
            let mut l = String::new();
            let _ = r1.read_line(&mut l);
            l.clear();
            let _ = r1.read_line(&mut l);
            drop(c1);

            // Reconnect late (400 ms < 500 ms), answer configure late (400 ms < 500 ms),
            // then stall on waitIdle (500 ms) -> ~1300 ms added (> 2x silence = 1000 ms).
            std::thread::sleep(Duration::from_millis(400));
            let Ok(mut c2) = TcpStream::connect(("127.0.0.1", port)) else { return };
            writeln!(c2, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID},"platform":"android"}}}}"#).unwrap();
            let mut r2 = BufReader::new(c2.try_clone().unwrap());
            l.clear();
            if r2.read_line(&mut l).unwrap_or(0) == 0 {
                return;
            }
            let req: Value = serde_json::from_str(&l).unwrap();
            let id = req["id"].as_u64().unwrap();
            std::thread::sleep(Duration::from_millis(400));
            let _ = writeln!(c2, r#"{{"id":{id},"result":{{"pixelsPerUnit":1}},"us":1}}"#);
            l.clear();
            let _ = r2.read_line(&mut l); // waitIdle
            l.clear();
            let _ = r2.read_line(&mut l); // wait for host hangup
        });

        drv.accept_with_pid(Duration::from_secs(2), Some(PID), &[]).unwrap();
        let opts = opts();
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        let t0 = Instant::now();
        let res = backend.launch_handshake("com.example.app", Some(PID));
        let elapsed = t0.elapsed();
        drop(backend);
        drop(drv);
        app.join().unwrap();

        assert!(res.is_err());
        // First failure takes 1x silence (500 ms); documented cap is at most 2x silence added (1500 ms total).
        assert!(
            elapsed <= 3 * silence + Duration::from_millis(80),
            "retry exceeded documented 2x silence added-time bound: total={elapsed:?} (silence={silence:?})"
        );
    }

    #[test]
    fn failed_handshake_retry_still_counts_in_cold_start_retries() {
        let mut dev = Device::Android(crate::device::Android::new("unreachable".into()));
        let mut drv = Driver::listen(0).unwrap();
        drv.set_silence_timeout(SILENCE);
        let (_seen, app) =
            fake_app(drv.port().unwrap(), vec![(PID, Conn::SilentAfter(0)), (PID, Conn::SilentAfter(0))]);
        drv.accept_with_pid(Duration::from_secs(2), Some(PID), &[]).unwrap();
        let opts = opts();
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, "com.example.app".into());
        let res = backend.launch_handshake("com.example.app", Some(PID));
        assert!(res.is_err());
        // Native lane records cold_start_retries=1 when a retry was attempted and failed; Flutter lane omits it.
        assert_eq!(backend.lane_stats().cold_start_retries, 1);
        drop(backend);
        drop(drv);
        app.join().unwrap();
    }
}
