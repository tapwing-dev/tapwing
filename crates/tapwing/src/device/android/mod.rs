//! Android device control over the ADB smartsocket protocol (F4), eliminating
//! per-call `adb` subprocess spawns (`~0.1 ms` loopback TCP vs `~50–75 ms`).
//! Falls back to spawning `adb start-server` once if the daemon is not running
//! yet, and keeps a streaming `adb shell` child only for long-running
//! `screenrecord`.

pub mod animations;
mod helpers;
mod permissions;
mod recording;

use anyhow::{Context, Result, bail};
use image::DynamicImage;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use super::LaunchMetrics;
use super::adb_client::{AdbClient, ShellOutput};

pub use animations::{
    AnimationScaleGuard, AnimationScales, disable_animations_script, parse_animation_scales, restore_animations_script,
};
pub use helpers::{
    AirplaneRead, browser_package, format_launch_arguments, geo_fix_args, is_emulator, open_link_command,
    parse_airplane_cmd, parse_airplane_setting, sanitize_remote_name, user_rotation,
};
pub(super) use helpers::{run, wait_with_timeout};
pub use permissions::{PermissionOutcome, all_permission_targets_from_dump, translate_permission_name};
pub(super) use recording::{MIN_RECORDING, RECORDER_EXIT_TIMEOUT};
pub use recording::{remote_recording_path, screenrecord_command};

use helpers::{media_scan_script, plan_media, validate_package_name};
use permissions::{translate_appops_value, translate_permission_value};
use recording::AndroidRecording;

/// Cached per-package metadata discovered on the first `launchApp` in a session.
#[derive(Clone, Debug)]
struct AppLaunchInfo {
    launcher_component: String,
    runtime_permissions: Vec<String>,
    has_manage_external_storage: bool,
}

/// Result of [`Android::keep_awake`]: the restoration guard plus an optional
/// soft-keyboard hint discovered in the same shell round-trip.
pub struct AwakeSetup {
    pub guard: AwakeGuard,
    pub soft_keyboard_hint: Option<bool>,
}

pub struct Android {
    pub serial: String,
    adb: AdbClient,
    app_cache: HashMap<String, AppLaunchInfo>,
    /// `ro.build.version.sdk`, read on first use (`0` = not read yet).
    api_level: AtomicU32,
    recording: Option<AndroidRecording>,
}

/// Serials of the adb devices in the `device` state (empty when `adb` is not
/// installed or its server cannot start).
pub fn list_devices() -> Vec<String> {
    let client = AdbClient::new();
    if let Ok(devs) = client.ready_devices() {
        return devs;
    }
    // If the ADB server daemon was not running yet, `adb devices` auto-starts it.
    match run("adb", &["devices"]) {
        Ok(out) => parse_adb_devices(&out),
        Err(_) => Vec::new(),
    }
}

/// Parses `adb devices` output into the serials that are ready (`device`).
pub fn parse_adb_devices(out: &str) -> Vec<String> {
    out.lines().skip(1).filter_map(|l| l.trim_end().strip_suffix("\tdevice").map(str::to_string)).collect()
}

impl Android {
    pub fn new(serial: String) -> Self {
        Self::with_adb_client(serial, AdbClient::new())
    }

    pub fn with_adb_client(serial: String, adb: AdbClient) -> Self {
        Self { serial, adb, app_cache: HashMap::new(), api_level: AtomicU32::new(0), recording: None }
    }

    /// [`crate::driver::LivenessProbe`] for an app process on this device.
    /// The process is dead when `/proc/<pid>` is gone (the `shell` user can
    /// read other apps' `/proc`) or, while Android is still tearing it down,
    /// once its crash (`FATAL EXCEPTION` / `Fatal signal`) is in the crash
    /// buffer. Short adb timeouts, so a wedged adb never stalls a waiting call.
    pub fn liveness_probe(&self) -> crate::driver::LivenessProbe {
        let serial = self.serial.clone();
        let adb = self
            .adb
            .clone()
            .with_timeouts(std::time::Duration::from_millis(300), std::time::Duration::from_millis(800));
        Box::new(move |pid| {
            let out = adb.shell(&serial, &liveness_command(pid)).ok()?;
            parse_liveness(&out.stdout)
        })
    }

    /// Runs a smartsocket operation; if the ADB daemon is not reachable yet,
    /// starts it once with `adb start-server` and retries.
    fn with_adb<T>(&self, op: impl Fn(&AdbClient) -> Result<T>) -> Result<T> {
        match op(&self.adb) {
            Ok(val) => Ok(val),
            Err(_) if !self.adb.is_available() => {
                let _ = Command::new("adb").arg("start-server").output();
                op(&self.adb)
            }
            Err(err) => Err(err),
        }
    }

    fn shell_raw(&self, cmd: &str) -> Result<ShellOutput> {
        self.with_adb(|adb| adb.shell(&self.serial, cmd))
    }

    // All adb traffic of the host commands goes through the five helpers below
    // (`shell`, `exec_out`, `push`, `pull`, `emu`) over the ADB smartsocket.

    pub fn shell(&self, cmd: &str) -> Result<String> {
        let out = self.shell_raw(cmd)?;
        if !out.success() {
            bail!("adb shell {cmd} failed (exit {}): {}", out.exit_code, out.combined());
        }
        Ok(out.stdout)
    }

    /// `adb exec-out`: binary-safe stdout of a device command (no CRLF mangling).
    pub fn exec_out(&self, args: &[&str]) -> Result<Vec<u8>> {
        let cmd = args.join(" ");
        self.with_adb(|adb| adb.exec_out(&self.serial, &cmd))
    }

    /// `adb push <local> <remote>` via ADB `sync:` (`SEND`/`DATA`/`DONE`).
    pub fn push(&self, local: &Path, remote: &str) -> Result<()> {
        self.with_adb(|adb| adb.push_file(&self.serial, local, remote, 0o644))
    }

    /// `adb pull <remote> <local>` via ADB `sync:` (`RECV`/`DATA`/`DONE`).
    pub fn pull(&self, remote: &str, local: &Path) -> Result<()> {
        self.with_adb(|adb| adb.pull_to_file(&self.serial, remote, local))
    }

    /// `adb emu <args...>`: emulator console command (emulators only), sent
    /// directly to the emulator console TCP socket.
    pub fn emu(&self, args: &[&str]) -> Result<String> {
        let cmd = args.join(" ");
        self.adb.emu_command(&self.serial, &cmd)
    }

    /// Device API level (`ro.build.version.sdk`), cached.
    pub fn api_level(&self) -> Result<u32> {
        let cached = self.api_level.load(Ordering::Relaxed);
        if cached != 0 {
            return Ok(cached);
        }
        let out = self.shell("getprop ro.build.version.sdk")?;
        let level = out
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("Invalid API level from 'getprop ro.build.version.sdk': {}", out.trim()))?;
        self.api_level.store(level, Ordering::Relaxed);
        Ok(level)
    }

    /// Keep the display on for the session and wake it if it is asleep, while
    /// probing hardware-keyboard IME suppression in the same `adb shell` call.
    pub fn keep_awake(&self) -> Result<AwakeSetup> {
        const KEYGUARD: &str = "dumpsys window 2>/dev/null | grep -m1 -o 'isKeyguardShowing=[a-z]*' || true";
        let out = self.shell(&format!(
            // 15 = stay on while plugged into AC, USB, wireless or dock (what
            // `svc power stayon true` writes, without its ~1 s JVM start).
            "settings get global stay_on_while_plugged_in; settings put global stay_on_while_plugged_in 15; \
             dumpsys power 2>/dev/null | grep -m1 mWakefulness=; {KEYGUARD}; \
             printf 'TAPWING_IME_HW=%s\\n' \"$(settings get secure show_ime_with_hard_keyboard 2>/dev/null)\"; \
             dumpsys input_method 2>/dev/null | grep -m1 -o 'mHardKeyboardAvailable=[a-z]*' || true"
        ))?;
        // Created first so the setting is restored even if we bail below.
        let guard = AwakeGuard {
            serial: self.serial.clone(),
            adb: self.adb.clone(),
            previous: out.lines().next().unwrap_or("").trim().to_string(),
            disarmed: false,
        };
        let mut locked = out.contains("isKeyguardShowing=true");
        if !out.contains("mWakefulness=Awake") {
            self.shell("cmd input keyevent KEYCODE_WAKEUP 2>/dev/null || input keyevent KEYCODE_WAKEUP")?;
            locked = self.shell(KEYGUARD)?.contains("=true");
        }
        if locked {
            // Unlocks a swipe-only lock screen; a secure one stays locked.
            self.shell("wm dismiss-keyguard")?;
            std::thread::sleep(std::time::Duration::from_millis(300));
            if self.shell(KEYGUARD)?.contains("=true") {
                bail!(
                    "device {} is locked; unlock it (test devices are best left without a secure lock screen)",
                    self.serial
                );
            }
        }
        let soft_keyboard_hint = if out.contains("TAPWING_IME_HW=0") && out.contains("mHardKeyboardAvailable=true") {
            Some(false)
        } else {
            None
        };
        Ok(AwakeSetup { guard, soft_keyboard_hint })
    }

    /// Forward the app's dial-out port to this session's host port.
    pub fn reverse(&self, device_port: u16, host_port: u16) -> Result<ReverseGuard> {
        let remote = format!("tcp:{device_port}");
        let local = format!("tcp:{host_port}");
        self.with_adb(|adb| adb.reverse(&self.serial, &remote, &local))?;
        Ok(ReverseGuard { serial: self.serial.clone(), adb: self.adb.clone(), device_port, disarmed: false })
    }

    /// Reads the current global animation scales and sets all three to `0`,
    /// returning an RAII [`AnimationScaleGuard`] that restores them on drop.
    pub fn disable_animations(&self) -> Result<AnimationScaleGuard> {
        let out = self.shell(disable_animations_script())?;
        let previous = parse_animation_scales(&out);
        Ok(AnimationScaleGuard { serial: self.serial.clone(), adb: self.adb.clone(), previous, disarmed: false })
    }

    /// Force-stops the app (`stopApp`, `AndroidDriver.kt:240-245`).
    pub fn stop_app(&self, app_id: &str) -> Result<()> {
        validate_package_name(app_id)?;
        self.shell(&format!("am force-stop {app_id}"))?;
        Ok(())
    }

    /// Triggers system-initiated process death on background processes while
    /// preserving the activity task (`killApp`, `AndroidDriver.kt:247-252`).
    pub fn kill_app(&self, app_id: &str) -> Result<()> {
        validate_package_name(app_id)?;
        self.shell(&format!("am kill {app_id}"))?;
        Ok(())
    }

    pub fn clear_state(&self, app_id: &str) -> Result<()> {
        validate_package_name(app_id)?;
        let out = self.shell(&format!("pm clear {app_id}"))?;
        if !out.contains("Success") {
            bail!("pm clear {app_id} failed: {out}");
        }
        Ok(())
    }

    /// Executes `launchApp` in a single merged `adb shell` round trip:
    /// optional `pm clear` / `am force-stop`, Maestro-parity permission grants,
    /// launcher activity resolution (cached), `am start` with typed intent
    /// extra `launchArguments`, and `pidof`.
    pub fn prepare_and_launch<'a, I>(
        &mut self,
        app_id: &str,
        clear_state: bool,
        stop_app: bool,
        permissions: &BTreeMap<String, String>,
        launch_arguments: I,
    ) -> Result<LaunchMetrics>
    where
        I: IntoIterator<Item = (&'a String, &'a serde_json::Value)>,
    {
        validate_package_name(app_id)?;
        let cached = self.app_cache.get(app_id).cloned();
        let mut script = String::with_capacity(1024);

        let restarts = clear_state || stop_app;
        script.push_str("T0=$(date +%s%3N); ");
        if restarts {
            script.push_str(&format!("OLD=$(pidof {app_id} 2>/dev/null); echo \"TAPWING_OLDPIDS:$OLD\"; "));
        } else {
            script.push_str("OLD=; ");
        }
        if clear_state {
            script.push_str(&format!(
                "CLR=$(pm clear {app_id} 2>&1); case \"$CLR\" in *Success*) ;; *) echo \"TAPWING_ERR:pm clear failed: $CLR\"; exit 11 ;; esac; "
            ));
        } else if stop_app {
            script.push_str(&format!("am force-stop {app_id}; "));
        }
        script.push_str("T1=$(date +%s%3N); ");

        script.push_str("GRANTED=0; ");
        let all_val = permissions.get("all").map(String::as_str);

        if let Some(info) = &cached {
            if let Some(val) = all_val {
                let verb = translate_permission_value(val);
                for perm in &info.runtime_permissions {
                    script.push_str(&format!(
                        "pm {verb} {app_id} {perm} 2>/dev/null && GRANTED=$((GRANTED+1)) || true; "
                    ));
                }
                if info.has_manage_external_storage {
                    let op_val = translate_appops_value(val);
                    script.push_str(&format!(
                        "appops set --uid {app_id} MANAGE_EXTERNAL_STORAGE {op_val} 2>/dev/null || true; "
                    ));
                }
            }
        } else {
            script.push_str(&format!("DUMP=$(dumpsys package {app_id} 2>/dev/null); "));
            script.push_str(
                "RPERMS=$(printf '%s\\n' \"$DUMP\" | sed -n '/runtime permissions:/,/User /p' | grep -oE '[A-Za-z0-9._]+: granted=(true|false)' | sed 's/: granted=.*//'); ",
            );
            script.push_str("for P in $RPERMS; do echo \"TAPWING_RPERM:$P\"; ");
            if let Some(val) = all_val {
                let verb = translate_permission_value(val);
                script.push_str(&format!("pm {verb} {app_id} \"$P\" 2>/dev/null && GRANTED=$((GRANTED+1)) || true; "));
            }
            script.push_str("done; ");
            script.push_str(
                "case \"$DUMP\" in *android.permission.MANAGE_EXTERNAL_STORAGE*) echo 'TAPWING_MANAGE_STORAGE:1'; ",
            );
            if let Some(val) = all_val {
                let op_val = translate_appops_value(val);
                script.push_str(&format!(
                    "appops set --uid {app_id} MANAGE_EXTERNAL_STORAGE {op_val} 2>/dev/null || true; "
                ));
            }
            script.push_str(";; esac; ");
        }

        for (raw_name, raw_val) in permissions {
            if raw_name == "all" {
                continue;
            }
            let verb = translate_permission_value(raw_val);
            let op_val = translate_appops_value(raw_val);
            for perm in translate_permission_name(raw_name) {
                if perm == "android.permission.MANAGE_EXTERNAL_STORAGE" {
                    script.push_str(&format!(
                        "appops set --uid {app_id} MANAGE_EXTERNAL_STORAGE {op_val} 2>/dev/null || true; "
                    ));
                } else {
                    script.push_str(&format!(
                        "pm {verb} {app_id} {perm} 2>/dev/null && GRANTED=$((GRANTED+1)) || true; "
                    ));
                }
            }
        }
        script.push_str("T2=$(date +%s%3N); ");

        if let Some(info) = &cached {
            script.push_str(&format!("COMP='{}'; ", info.launcher_component));
        } else {
            script.push_str(&format!(
                "COMP=$(cmd package resolve-activity --brief -c android.intent.category.LAUNCHER {app_id} 2>&1 | grep '/' | tail -n 1 | tr -d '\\r'); \
                 if [ -z \"$COMP\" ]; then echo 'TAPWING_ERR:no launcher activity'; exit 12; fi; \
                 echo \"TAPWING_COMP:$COMP\"; "
            ));
        }
        let extra_flags = format_launch_arguments(launch_arguments);
        let extra_part = if extra_flags.is_empty() { String::new() } else { format!(" {extra_flags}") };
        script.push_str(&format!(
            "START_OUT=$(am start -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -n \"$COMP\"{extra_part} 2>&1); \
             case \"$START_OUT\" in *Error*) echo \"TAPWING_ERR:am start failed: $START_OUT\"; exit 13 ;; esac; \
             T3=$(date +%s%3N); ",
        ));
        script.push_str(&format!(
            "PID=; N=0; while [ -z \"$PID\" ] && [ $N -lt 150 ]; do \
               for P in $(pidof {app_id} 2>/dev/null); do case \" $OLD \" in *\" $P \"*) ;; *) PID=$P ;; esac; done; \
               [ -z \"$PID\" ] && sleep 0.01; N=$((N+1)); \
             done; "
        ));
        script.push_str(
            "echo \"TAPWING_PID:$PID\"; \
             echo \"TAPWING_GRANTED:$GRANTED\"; \
             echo \"TAPWING_T:$T0:$T1:$T2:$T3\"",
        );

        let raw = self.shell_raw(&script)?;
        let out = raw.combined();
        for line in out.lines() {
            if let Some(err) = line.strip_prefix("TAPWING_ERR:") {
                bail!("{err}");
            }
        }
        if !raw.success() {
            bail!("adb shell failed (exit {}): {out}", raw.exit_code);
        }

        let mut comp = cached.as_ref().map(|c| c.launcher_component.clone());
        let mut rperms = Vec::new();
        let mut has_manage_storage = false;
        let mut pid: Option<i64> = None;
        let mut old_pids: Vec<i64> = Vec::new();
        let mut granted_permissions: usize = 0;
        let mut times: [u64; 4] = [0; 4];

        for line in out.lines().map(str::trim) {
            if let Some(v) = line.strip_prefix("TAPWING_COMP:") {
                if !v.is_empty() {
                    comp = Some(v.to_string());
                }
            } else if let Some(v) = line.strip_prefix("TAPWING_RPERM:") {
                if !v.is_empty() {
                    rperms.push(v.to_string());
                }
            } else if line == "TAPWING_MANAGE_STORAGE:1" {
                has_manage_storage = true;
            } else if let Some(v) = line.strip_prefix("TAPWING_PID:") {
                pid = v.trim().parse::<i64>().ok();
            } else if let Some(v) = line.strip_prefix("TAPWING_OLDPIDS:") {
                old_pids = v.split_whitespace().filter_map(|p| p.parse::<i64>().ok()).collect();
            } else if let Some(v) = line.strip_prefix("TAPWING_GRANTED:") {
                granted_permissions = v.trim().parse::<usize>().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("TAPWING_T:") {
                let parts: Vec<u64> = v.split(':').filter_map(|p| p.parse::<u64>().ok()).collect();
                if parts.len() == 4 {
                    times.copy_from_slice(&parts);
                }
            }
        }

        if cached.is_none()
            && let Some(launcher_component) = comp
        {
            self.app_cache.insert(
                app_id.to_string(),
                AppLaunchInfo {
                    launcher_component,
                    runtime_permissions: rperms,
                    has_manage_external_storage: has_manage_storage,
                },
            );
        }

        Ok(LaunchMetrics {
            stop_clear_ms: times[1].saturating_sub(times[0]),
            permissions_ms: times[2].saturating_sub(times[1]),
            start_ms: times[3].saturating_sub(times[2]),
            granted_permissions,
            pid,
            old_pids,
            unsupported_permissions: Vec::new(),
        })
    }

    /// Captures a PNG screenshot via `adb exec-out screencap -p`.
    pub fn screenshot_bytes(&self) -> Result<Vec<u8>> {
        self.exec_out(&["screencap", "-p"])
    }

    /// Captures a decoded screenshot image via `adb exec-out screencap -p`.
    pub fn screenshot_image(&self) -> Result<DynamicImage> {
        let png = self.screenshot_bytes()?;
        image::load_from_memory(&png).context("decoding screenshot PNG")
    }

    /// PID of `app_id`'s main process (`pidof`), or `None` when it is not running.
    pub fn pid_of(&self, app_id: &str) -> Result<Option<i64>> {
        validate_package_name(app_id)?;
        let out = self.shell(&format!("pidof {app_id} 2>/dev/null || true"))?;
        Ok(out.split_whitespace().next().and_then(|p| p.parse().ok()))
    }

    /// Injects an Android system key event without a fixed 300 ms sleep;
    /// prefers `cmd input` (no `app_process`/JVM startup on Android <= 11).
    pub fn keyevent_fast(&self, code: u16) -> Result<()> {
        self.shell(&format!("cmd input keyevent {code} 2>/dev/null || input keyevent {code}"))?;
        Ok(())
    }

    /// Injects an Android hardware key event with Maestro's 300 ms sleep
    /// (`AndroidDriver.kt:322-323`), used for `Power` and `Lock`.
    pub fn keyevent_with_sleep(&self, code: u16) -> Result<()> {
        self.keyevent_fast(code)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        Ok(())
    }

    /// Reads the system dark-mode setting directly via `cmd uimode night`.
    pub fn is_dark_mode(&self) -> Result<bool> {
        let out = self.shell("cmd uimode night")?;
        match out.trim() {
            "Night mode: no" => Ok(false),
            "Night mode: yes" => Ok(true),
            other => bail!("Received invalid response while trying to read dark mode state: {other}"),
        }
    }

    /// `openLink` (`AndroidDriver.openLink`, `AndroidDriver.kt:561-575`):
    /// `am start -a android.intent.action.VIEW -d <link>`, optionally pinned to
    /// Chrome / Firefox when `browser` is set.
    pub fn open_link(
        &self,
        link: &str,
        app_id: Option<&str>,
        auto_verify: bool,
        browser: bool,
    ) -> Result<Option<String>> {
        if link.trim().is_empty() {
            bail!("openLink: link is empty");
        }
        let package = if browser { browser_package(&self.shell("pm list packages")?) } else { None };
        let out = self.shell(&open_link_command(link, package))?;
        if let Some(err) = out.lines().map(str::trim).find(|l| l.starts_with("Error")) {
            bail!("openLink {link} failed: {err}");
        }
        Ok(auto_verify.then(|| {
            format!(
                "openLink autoVerify{} was not applied automatically: tap through any 'Open with' chooser / Chrome \
                 first-run dialogs with explicit flow steps",
                app_id.map(|a| format!(" for {a}")).unwrap_or_default()
            )
        }))
    }

    /// `setLocation` on an emulator via the console (`adb emu geo fix`, which
    /// takes **longitude first**). Physical devices route through the native
    /// lane's mock location provider in [`crate::run`].
    pub fn set_location(&self, latitude: f64, longitude: f64) -> Result<()> {
        if !is_emulator(&self.serial) {
            bail!(
                "setLocation on a physical device ({}) requires the Android native lane (run with --input auto or \
                 --input native and Tapwing's driver APK available); `adb emu geo fix` only works on emulators",
                self.serial
            );
        }
        let args = geo_fix_args(latitude, longitude);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.emu(&refs)?;
        Ok(())
    }

    /// `setOrientation` (`AndroidDriver.kt:673-685`): disables auto-rotation,
    /// then pins `user_rotation`.
    pub fn set_orientation(&self, orientation: &str) -> Result<()> {
        let rotation = user_rotation(orientation)?;
        self.shell(&format!(
            "settings put system accelerometer_rotation 0; settings put system user_rotation {rotation}"
        ))?;
        Ok(())
    }

    /// `setDarkMode` / `toggleDarkMode` (`AndroidDriver.kt:831-836`).
    pub fn set_dark_mode(&self, enabled: bool) -> Result<()> {
        let out = self.shell(&format!("cmd uimode night {}", if enabled { "yes" } else { "no" }))?;
        if out.contains("No shell command implementation") {
            bail!("this device does not support `cmd uimode night` (Android 10+ required)");
        }
        Ok(())
    }

    /// `isAirplaneModeEnabled` (`AndroidDriver.kt:787-804`).
    pub fn is_airplane_mode_enabled(&self) -> Result<bool> {
        let raw = self.shell_raw("cmd connectivity airplane-mode")?;
        if !raw.success() {
            return parse_airplane_setting(&self.shell("settings get global airplane_mode_on")?);
        }
        match parse_airplane_cmd(&raw.stdout) {
            AirplaneRead::State(on) => Ok(on),
            AirplaneRead::Fallback => parse_airplane_setting(&self.shell("settings get global airplane_mode_on")?),
            AirplaneRead::Invalid(other) => {
                bail!("Received invalid response while trying to read airplane mode state: {other}")
            }
        }
    }

    /// `setAirplaneMode` (`AndroidDriver.kt:806-819`).
    pub fn set_airplane_mode(&self, enabled: bool) -> Result<()> {
        if self.api_level()? < 28 {
            self.shell(&format!("settings put global airplane_mode_on {}", u8::from(enabled)))?;
            let broadcast = format!("am broadcast -a android.intent.action.AIRPLANE_MODE --ez state {enabled}");
            return match self.shell(&broadcast) {
                Ok(_) => Ok(()),
                Err(e) if e.to_string().contains("Security exception: Permission Denial:") => {
                    self.shell(&format!("su root {broadcast}")).map(drop).map_err(|_| {
                        anyhow::anyhow!(
                            "Failed to broadcast airplane mode change. Make sure to run an emulator with root \
                             access for API < 28"
                        )
                    })
                }
                Err(e) => Err(e),
            };
        }
        self.shell(&format!("cmd connectivity airplane-mode {}", if enabled { "enable" } else { "disable" }))?;
        Ok(())
    }

    /// `addMedia` (`AndroidDriver.addMedia`, `AndroidDriver.kt:779-785, 993-1020`).
    pub fn add_media(&self, media_paths: &[String]) -> Result<Vec<String>> {
        let plans = media_paths.iter().map(|p| plan_media(Path::new(p))).collect::<Result<Vec<_>>>()?;
        if plans.is_empty() {
            return Ok(Vec::new());
        }
        let mut dirs: Vec<&str> = plans.iter().map(|p| p.remote_dir).collect();
        dirs.sort_unstable();
        dirs.dedup();
        self.shell(&format!("mkdir -p {}", dirs.join(" ")))?;
        for plan in &plans {
            self.push(&plan.local, &plan.remote_path)
                .with_context(|| format!("copying {} to the device", plan.local.display()))?;
        }
        let remote: Vec<String> = plans.into_iter().map(|p| p.remote_path).collect();
        self.shell(&media_scan_script(&remote))?;
        Ok(remote)
    }
}

/// Removes an `adb reverse` rule on drop (best effort).
pub struct ReverseGuard {
    serial: String,
    adb: AdbClient,
    device_port: u16,
    disarmed: bool,
}

impl ReverseGuard {
    fn cleanup(&mut self) {
        if self.disarmed {
            return;
        }
        self.disarmed = true;
        let _ = self.adb.reverse_remove(&self.serial, &format!("tcp:{}", self.device_port));
    }
}

impl Drop for ReverseGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Restores `stay_on_while_plugged_in` on drop (best effort).
pub struct AwakeGuard {
    serial: String,
    adb: AdbClient,
    previous: String,
    disarmed: bool,
}

impl AwakeGuard {
    fn cleanup(&mut self) {
        if self.disarmed {
            return;
        }
        self.disarmed = true;
        let restore = if self.previous.parse::<u32>().is_ok() {
            format!("settings put global stay_on_while_plugged_in {}", self.previous)
        } else {
            "svc power stayon false".to_string()
        };
        let _ = self.adb.shell(&self.serial, &restore);
    }
}

impl Drop for AwakeGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Drops `ReverseGuard`, `AwakeGuard`, and optional `AnimationScaleGuard`
/// concurrently so CLI teardown pays only one adb round-trip window instead
/// of sequential calls.
pub struct SessionGuards {
    reverse: ReverseGuard,
    awake: AwakeGuard,
    animations: Option<AnimationScaleGuard>,
}

impl SessionGuards {
    pub fn new(reverse: ReverseGuard, awake: AwakeGuard, animations: Option<AnimationScaleGuard>) -> Self {
        Self { reverse, awake, animations }
    }
}

impl Drop for SessionGuards {
    fn drop(&mut self) {
        let rev = &mut self.reverse;
        let awk = &mut self.awake;
        let anim = self.animations.as_mut();
        std::thread::scope(|s| {
            s.spawn(|| rev.cleanup());
            s.spawn(|| awk.cleanup());
            if let Some(a) = anim {
                s.spawn(|| a.cleanup());
            }
        });
    }
}

/// Shell command behind [`Android::liveness_probe`].
fn liveness_command(pid: i64) -> String {
    format!(
        "if [ -d /proc/{pid} ]; then if logcat -b crash -d --pid={pid} 2>/dev/null | grep -qE 'FATAL EXCEPTION|Fatal signal'; \
         then echo crashed; else echo alive; fi; else echo gone; fi"
    )
}

/// `Some(false)` when the process is gone or has crashed, `None` when the
/// output is unexpected (the probe never fails a call on its own doubt).
fn parse_liveness(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "alive" => Some(true),
        "gone" | "crashed" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod liveness_tests {
    #[test]
    fn liveness_probe_command_and_parsing() {
        let cmd = super::liveness_command(4321);
        assert!(cmd.contains("[ -d /proc/4321 ]") && cmd.contains("--pid=4321"), "{cmd}");
        assert_eq!(super::parse_liveness("alive\n"), Some(true));
        assert_eq!(super::parse_liveness("gone"), Some(false));
        assert_eq!(super::parse_liveness("crashed\r\n"), Some(false));
        assert_eq!(super::parse_liveness("error: device offline"), None);
    }
}
