//! iOS Simulator control via `xcrun simctl` (physical iOS devices are out of
//! scope). Mirrors what Maestro 2.12.1 does through `LocalSimulatorUtils.kt`
//! and `IOSDriver.kt`, minus the XCTest runner: the in-app driver handles all
//! UI work, so the host only manages the app lifecycle and device settings.
//!
//! Connection: the simulator shares the host loopback, so no forwarding is
//! needed. Flutter on iOS exposes no process environment to Dart, so the
//! per-session host port is written to `<app data container>/tmp/tapwing_port`
//! before launching or attaching; the driver re-reads it before every dial.

use anyhow::{Context, Result, anyhow, bail};
use image::DynamicImage;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::LaunchMetrics;
use super::android::{MIN_RECORDING, RECORDER_EXIT_TIMEOUT, wait_with_timeout};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_tmp_seq() -> u64 {
    TMP_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// How long `simctl` may take to create the recording before the start fails.
const RECORDING_START_TIMEOUT: Duration = Duration::from_secs(10);

/// File name the driver reads its host port from (`TapwingServer.portFileName`).
pub const PORT_FILE: &str = "tapwing_port";

/// A booted simulator as listed by `simctl list devices booted -j`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimInfo {
    pub udid: String,
    pub name: String,
}

/// Booted simulators (empty when Xcode / `xcrun` is unavailable).
pub fn list_booted() -> Vec<SimInfo> {
    match xcrun(&["simctl", "list", "devices", "booted", "-j"]) {
        Ok(out) => parse_booted_json(&out),
        Err(_) => Vec::new(),
    }
}

/// Parses `simctl list devices -j` output, keeping booted devices only.
pub fn parse_booted_json(json: &str) -> Vec<SimInfo> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(runtimes) = v["devices"].as_object() {
        for devices in runtimes.values() {
            for d in devices.as_array().into_iter().flatten() {
                if d["state"] == "Booted"
                    && let (Some(udid), Some(name)) = (d["udid"].as_str(), d["name"].as_str())
                {
                    out.push(SimInfo { udid: udid.to_string(), name: name.to_string() });
                }
            }
        }
    }
    out
}

/// Extracts the PID from `simctl launch` output (`"<bundle>: <pid>"`).
/// [`crate::driver::LivenessProbe`] for a simulator app: simulator apps are
/// host processes, so `kill -0 <pid>` tells whether it still runs.
pub fn liveness_probe() -> crate::driver::LivenessProbe {
    Box::new(|pid| {
        let status = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        Some(status.success())
    })
}

pub fn parse_launch_pid(out: &str) -> Option<i64> {
    out.lines().rev().find_map(|l| l.trim().rsplit(':').next().and_then(|p| p.trim().parse::<i64>().ok()))
}

/// Parses a boolean preference printed by `defaults read` / `plutil -extract raw`.
pub fn parse_bool_pref(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// Bundle ids of running user apps from `launchctl list` inside the simulator
/// (`UIKitApplication:<bundle>[<hash>][<n>]` labels with a live PID), skipping
/// Apple's own apps.
pub fn parse_running_apps(launchctl_list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in launchctl_list.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(pid), Some(label)) = (cols.first(), cols.last()) else { continue };
        if pid.parse::<i64>().is_err() {
            continue;
        }
        if let Some(rest) = label.strip_prefix("UIKitApplication:") {
            let bundle = rest.split('[').next().unwrap_or(rest);
            if !bundle.is_empty() && !bundle.starts_with("com.apple.") && !out.iter().any(|b| b == bundle) {
                out.push(bundle.to_string());
            }
        }
    }
    out
}

/// One `simctl privacy` invocation: (`grant` | `revoke` | `reset`, service).
pub type PrivacyAction = (&'static str, &'static str);

/// Maps Maestro `permissions` (`LocalSimulatorUtils.setAppleSimutilsPermissions`
/// + `setSimctlPermissions`) onto `simctl privacy` actions.
///
/// Returns the actions in application order plus the Maestro permission
/// names `simctl` cannot set (camera, faceid, homekit, notifications, speech,
/// userTracking: Maestro uses the third-party `applesimutils` for those).
///
/// - `all: allow|deny|unset` -> `grant|revoke|reset all` (+ `location` as
///   `always` for `allow`, matching `allowValueForPermission`); any other
///   `all` value is an error like in Maestro.
/// - Explicit entries override `all` (they are applied afterwards).
/// - `location`: `always` -> `grant location-always`, `inuse` ->
///   `grant location`, `never` -> `revoke location-always`, `unset` ->
///   `reset location-always`. Like Maestro (`setLocationPermission`), an
///   explicit `allow`/`deny` is rejected: those map to `always`/`never` only
///   through `all`.
pub fn privacy_actions(permissions: &BTreeMap<String, String>) -> Result<(Vec<PrivacyAction>, Vec<String>)> {
    let mut actions: Vec<PrivacyAction> = Vec::new();
    let mut unsupported = Vec::new();
    if let Some(all) = permissions.get("all") {
        match all.as_str() {
            "allow" => actions.extend([("grant", "all"), ("grant", "location-always")]),
            "deny" => actions.push(("revoke", "all")),
            "unset" => actions.push(("reset", "all")),
            other => bail!("Permission 'all' can be set to 'allow', 'deny' or 'unset', not '{other}'"),
        }
    }
    for (name, value) in permissions {
        if name == "all" {
            continue;
        }
        let service: &'static str = match name.as_str() {
            "location" => {
                actions.push(match value.as_str() {
                    "always" => ("grant", "location-always"),
                    "inuse" => ("grant", "location"),
                    "never" => ("revoke", "location-always"),
                    "unset" => ("reset", "location-always"),
                    other => bail!("wrong argument value '{other}' was provided for 'location' permission"),
                });
                continue;
            }
            "calendar" => "calendar",
            "contacts" if value == "limited" => {
                actions.push(("grant", "contacts-limited"));
                continue;
            }
            "contacts" => "contacts",
            "medialibrary" => "media-library",
            "microphone" => "microphone",
            "motion" => "motion",
            "photos" if value == "limited" => {
                actions.push(("grant", "photos-add"));
                continue;
            }
            "photos" => "photos",
            "reminders" => "reminders",
            "siri" => "siri",
            other => {
                unsupported.push(other.to_string());
                continue;
            }
        };
        actions.push(match value.as_str() {
            "allow" => ("grant", service),
            "deny" => ("revoke", service),
            "unset" => ("reset", service),
            other => bail!("Permission '{name}' can be set to 'allow', 'deny' or 'unset', not '{other}'"),
        });
    }
    Ok((actions, unsupported))
}

pub struct IosSim {
    pub udid: String,
    pub name: String,
    data_containers: HashMap<String, PathBuf>,
    display_names: HashMap<String, Option<String>>,
    /// Permissions applied per bundle in this session (reset by reinstall),
    /// so repeated `launchApp`s skip redundant `simctl privacy` calls.
    applied_permissions: HashMap<String, BTreeMap<String, String>>,
    recording: Option<IosRecording>,
}

/// A running `simctl io recordVideo`.
struct IosRecording {
    child: Child,
    out_path: PathBuf,
    stderr_log: PathBuf,
    started: Instant,
}

impl IosRecording {
    /// Sends SIGINT (`simctl` finalizes the file only on SIGINT, never on
    /// SIGKILL) and waits for the exit; kills the process if it hangs.
    /// Returns whether it exited by itself.
    fn stop(&mut self) -> bool {
        send_sigint(self.child.id());
        let exited = wait_with_timeout(&mut self.child, RECORDER_EXIT_TIMEOUT).is_some();
        if !exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_file(&self.stderr_log);
        exited
    }
}

impl Drop for IosSim {
    /// Never leaves a `simctl recordVideo` running.
    fn drop(&mut self) {
        if let Some(mut rec) = self.recording.take() {
            rec.stop();
        }
    }
}

fn send_sigint(pid: u32) {
    let _ = Command::new("kill").args(["-INT", &pid.to_string()]).output();
}

/// Media types Maestro accepts for `addMedia` (`MediaExt`).
const MEDIA_EXTENSIONS: [&str; 5] = ["png", "jpeg", "jpg", "gif", "mp4"];

/// Validates `addMedia` inputs the way `IOSDriver.addMediaToDevice` does:
/// the file must exist and have a supported extension (matched exactly).
pub fn validate_media(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("Media file not found: {}", path.display());
    }
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !MEDIA_EXTENSIONS.contains(&extension) {
        bail!("Extension .{extension} is not yet supported for add media");
    }
    Ok(())
}

/// Arguments after `simctl` for the video recorder (`screenrecord.sh` in
/// Maestro: `recordVideo --force --codec h264 <file>`).
pub fn record_video_args<'a>(udid: &'a str, out: &'a str) -> Vec<&'a str> {
    vec!["io", udid, "recordVideo", "--codec=h264", "--force", out]
}

/// Formats Maestro `launchArguments` as CLI arguments for `xcrun simctl launch`
/// (`-<key> <value>` pairs populated into iOS `UserDefaults` / `NSArgumentDomain`,
/// matching Maestro 2.12.1's `LocalSimulatorUtils.launch`).
///
/// Keys are sorted for deterministic ordering. Keys that already start with `-`
/// keep their leading dash instead of being double-prefixed.
pub fn format_launch_arguments<'a, I>(arguments: I) -> Vec<String>
where
    I: IntoIterator<Item = (&'a String, &'a Value)>,
{
    let mut pairs: Vec<(&str, &Value)> = arguments.into_iter().map(|(k, v)| (k.as_str(), v)).collect();
    pairs.sort_by_key(|(k, _)| *k);
    let mut out = Vec::with_capacity(pairs.len() * 2);
    for (key, val) in pairs {
        let flag = if key.starts_with('-') { key.to_string() } else { format!("-{key}") };
        let value = match val {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        };
        out.push(flag);
        out.push(value);
    }
    out
}

fn write_port_file_in_dir(tmp: &Path, port: u16) -> Result<()> {
    std::fs::create_dir_all(tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let path = tmp.join(PORT_FILE);
    // Write-then-rename so the driver never reads a half-written port.
    let partial = tmp.join(format!("{PORT_FILE}.{}-{}.tmp", std::process::id(), next_tmp_seq()));
    std::fs::write(&partial, port.to_string()).with_context(|| format!("writing {}", partial.display()))?;
    if let Err(err) = std::fs::rename(&partial, &path) {
        let _ = std::fs::remove_file(&partial);
        return Err(err).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

impl IosSim {
    pub fn new(info: SimInfo) -> Self {
        Self {
            udid: info.udid,
            name: info.name,
            data_containers: HashMap::new(),
            display_names: HashMap::new(),
            applied_permissions: HashMap::new(),
            recording: None,
        }
    }

    fn simctl(&self, args: &[&str]) -> Result<String> {
        let mut full = vec!["simctl"];
        full.extend_from_slice(args);
        xcrun(&full)
    }

    /// The app's data container (cached until the app is reinstalled).
    pub fn data_container(&mut self, bundle: &str) -> Result<PathBuf> {
        if let Some(p) = self.data_containers.get(bundle) {
            return Ok(p.clone());
        }
        let out = self
            .simctl(&["get_app_container", &self.udid, bundle, "data"])
            .with_context(|| format!("{bundle} is not installed on simulator {}", self.name))?;
        let p = PathBuf::from(out.trim());
        self.data_containers.insert(bundle.to_string(), p.clone());
        Ok(p)
    }

    /// Hands the session's host port to the app (`tmp/tapwing_port`).
    pub fn write_port_file(&mut self, bundle: &str, port: u16) -> Result<()> {
        let tmp = self.data_container(bundle)?.join("tmp");
        write_port_file_in_dir(&tmp, port)
    }

    /// The app's `CFBundleDisplayName` (falling back to `CFBundleName`), which
    /// XCUITest reports as the application element's label.
    pub fn display_name(&mut self, bundle: &str) -> Option<String> {
        if let Some(n) = self.display_names.get(bundle) {
            return n.clone();
        }
        let name = self.simctl(&["get_app_container", &self.udid, bundle, "app"]).ok().and_then(|app| {
            let plist = Path::new(app.trim()).join("Info.plist");
            ["CFBundleDisplayName", "CFBundleName"].iter().find_map(|key| {
                let out = Command::new("plutil").args(["-extract", key, "raw", "-o", "-"]).arg(&plist).output().ok()?;
                let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
                (out.status.success() && !v.is_empty()).then_some(v)
            })
        });
        self.display_names.insert(bundle.to_string(), name.clone());
        name
    }

    /// `simctl terminate` (`stopApp`; also `killApp`, since iOS has no
    /// process death: `IOSDriver.killApp`). Not running is not an error.
    pub fn terminate(&self, bundle: &str) -> Result<()> {
        let out = Command::new("xcrun")
            .args(["simctl", "terminate", &self.udid, bundle])
            .output()
            .context("running xcrun simctl terminate")?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        // "found nothing to terminate" (not running) is fine, like Maestro.
        if err.contains("found nothing to terminate") || err.contains("not running") || err.contains("No such process")
        {
            return Ok(());
        }
        bail!("simctl terminate {bundle} failed: {}", err.trim())
    }

    /// `clearState`: terminate and reinstall the app bundle, the "most stable
    /// way to clear state" in Maestro (`LocalSimulatorUtils.clearAppState`).
    pub fn clear_state(&mut self, bundle: &str) -> Result<()> {
        self.terminate(bundle)?;
        self.reinstall(bundle)
    }

    fn reinstall(&mut self, bundle: &str) -> Result<()> {
        let app = self.simctl(&["get_app_container", &self.udid, bundle, "app"])?.trim().to_string();
        if app.is_empty() {
            bail!("Could not find app binary for bundle {bundle}");
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let tmp_dir =
            std::env::temp_dir().join(format!("tapwing-reinstall-{}-{stamp}-{}", std::process::id(), next_tmp_seq()));
        std::fs::create_dir_all(&tmp_dir)?;
        let copy = tmp_dir.join(format!("{bundle}.app"));
        // Once the app is uninstalled, `copy` is its only binary: it must
        // outlive a failed install.
        let mut keep_copy = false;
        let result = (|| -> Result<()> {
            // `cp -c` clones on APFS (copy-on-write), making the copy ~free.
            run("cp", &["-Rc", &app, path_str(&copy)?]).or_else(|_| run("cp", &["-R", &app, path_str(&copy)?]))?;
            self.simctl(&["uninstall", &self.udid, bundle])?;
            let install = ["install", self.udid.as_str(), path_str(&copy)?];
            install_with_one_retry(|| self.simctl(&install).map(|_| ())).map_err(|e| {
                keep_copy = true;
                e.context(reinstall_failure_hint(bundle, &self.udid, &copy))
            })
        })();
        if !keep_copy {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        }
        // A reinstall creates a fresh data container and resets privacy.
        self.data_containers.remove(bundle);
        self.applied_permissions.remove(bundle);
        result
    }

    /// Applies Maestro permissions via `simctl privacy`; skips the calls when
    /// the same set was already applied to this install during the session.
    /// Returns (number of `simctl privacy` actions run, unsupported names).
    pub fn set_permissions(
        &mut self,
        bundle: &str,
        permissions: &BTreeMap<String, String>,
    ) -> Result<(usize, Vec<String>)> {
        let (actions, unsupported) = privacy_actions(permissions)?;
        if self.applied_permissions.get(bundle) == Some(permissions) {
            return Ok((0, unsupported));
        }
        for (action, service) in &actions {
            self.simctl(&["privacy", &self.udid, action, service, bundle])?;
        }
        self.applied_permissions.insert(bundle.to_string(), permissions.clone());
        Ok((actions.len(), unsupported))
    }

    /// Writes the port file and launches the app with optional `launchArguments`;
    /// returns the new PID.
    pub fn launch<'a, I>(&mut self, bundle: &str, launch_arguments: I, port: u16) -> Result<i64>
    where
        I: IntoIterator<Item = (&'a String, &'a Value)>,
    {
        self.write_port_file(bundle, port)?;
        let cli_args = format_launch_arguments(launch_arguments);
        let out = Command::new("xcrun")
            .args(["simctl", "launch", &self.udid, bundle])
            .args(&cli_args)
            // Unused by Flutter today (no environment on iOS), kept for
            // non-Flutter hosts and future engines.
            .env("SIMCTL_CHILD_TAPWING_PORT", port.to_string())
            .output()
            .context("running xcrun simctl launch")?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            bail!("simctl launch {bundle} failed: {}{}", text, String::from_utf8_lossy(&out.stderr));
        }
        parse_launch_pid(&text).ok_or_else(|| anyhow!("simctl launch printed no pid: `{}`", text.trim()))
    }

    /// `launchApp`: optional terminate / reinstall, permissions (default
    /// `all: allow`, `Orchestra.kt:1345-1347`), port handoff and launch.
    pub fn prepare_and_launch<'a, I>(
        &mut self,
        bundle: &str,
        clear_state: bool,
        stop_app: bool,
        permissions: &BTreeMap<String, String>,
        launch_arguments: I,
        port: u16,
    ) -> Result<LaunchMetrics>
    where
        I: IntoIterator<Item = (&'a String, &'a Value)>,
    {
        let t0 = Instant::now();
        if clear_state {
            self.clear_state(bundle)?;
        } else if stop_app {
            self.terminate(bundle)?;
        }
        let t1 = Instant::now();
        let (granted, unsupported) = self.set_permissions(bundle, permissions)?;
        let t2 = Instant::now();
        let pid = self.launch(bundle, launch_arguments, port)?;
        let t3 = Instant::now();
        Ok(LaunchMetrics {
            stop_clear_ms: ms(t1 - t0),
            permissions_ms: ms(t2 - t1),
            start_ms: ms(t3 - t2),
            granted_permissions: granted,
            pid: Some(pid),
            old_pids: Vec::new(),
            unsupported_permissions: unsupported,
        })
    }

    /// Captures a PNG screenshot of the simulator display (physical pixels).
    pub fn screenshot_bytes(&self) -> Result<Vec<u8>> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path =
            std::env::temp_dir().join(format!("tapwing-shot-{}-{stamp}-{}.png", std::process::id(), next_tmp_seq()));
        let res = self.simctl(&["io", &self.udid, "screenshot", "--type=png", path_str(&path)?]);
        let bytes = res.and_then(|_| std::fs::read(&path).context("reading simulator screenshot"));
        let _ = std::fs::remove_file(&path);
        bytes
    }

    pub fn screenshot_image(&self) -> Result<DynamicImage> {
        let png = self.screenshot_bytes()?;
        image::load_from_memory(&png).context("decoding screenshot PNG")
    }

    /// `simctl ui <udid> appearance` (`IOSDriver.isDarkModeEnabled`).
    pub fn is_dark_mode(&self) -> Result<bool> {
        let out = self.simctl(&["ui", &self.udid, "appearance"])?;
        match out.trim() {
            "dark" => Ok(true),
            "light" => Ok(false),
            other => bail!("Received invalid response while trying to read dark mode state: {other}"),
        }
    }

    /// `setDarkMode` / `toggleDarkMode` (`IOSDriver.setDarkMode`).
    pub fn set_dark_mode(&self, dark: bool) -> Result<()> {
        self.simctl(&["ui", &self.udid, "appearance", if dark { "dark" } else { "light" }])?;
        Ok(())
    }

    /// `openLink` (`LocalSimulatorUtils.openURL`).
    pub fn open_url(&self, url: &str) -> Result<()> {
        self.simctl(&["openurl", &self.udid, url])?;
        Ok(())
    }

    /// `setLocation` (`LocalSimulatorUtils.setLocation`).
    pub fn set_location(&self, latitude: f64, longitude: f64) -> Result<()> {
        self.simctl(&["location", &self.udid, "set", &format!("{latitude},{longitude}")])?;
        Ok(())
    }

    /// `addMedia` (`LocalSimulatorUtils.addMedia`): `simctl addmedia` adds the
    /// files to the simulator's Photos library. Everything is validated first.
    pub fn add_media(&self, media_paths: &[String]) -> Result<()> {
        for p in media_paths {
            validate_media(Path::new(p))?;
        }
        if media_paths.is_empty() {
            return Ok(());
        }
        let mut args = vec!["addmedia", self.udid.as_str()];
        args.extend(media_paths.iter().map(String::as_str));
        self.simctl(&args)?;
        Ok(())
    }

    /// `setOrientation`: Maestro rotates the simulator through the XCTest
    /// runner (`XCUIDevice.orientation`); `simctl` has no equivalent.
    pub fn set_orientation(&self, orientation: &str) -> Result<()> {
        bail!(
            "setOrientation ({orientation}) on the iOS simulator requires the XCTest native lane \
             (run with --input auto or --input native and Maestro installed); simctl cannot rotate a simulator"
        )
    }

    /// `startRecording`: `simctl io recordVideo` straight into `out_path`.
    /// Returns `false` (doing nothing) when a recording is already running,
    /// like `Maestro.startScreenRecording`.
    pub fn start_recording(&mut self, out_path: &Path) -> Result<bool> {
        if self.recording.is_some() {
            return Ok(false);
        }
        if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        // The file's appearance is the start signal, so clear any leftover first.
        let _ = std::fs::remove_file(out_path);
        let out = path_str(out_path)?;
        // Redirect stderr to a temp log file so long recordings can never fill an
        // undrained OS pipe buffer and stall `simctl`.
        let stderr_log = std::env::temp_dir().join(format!(
            "tapwing-simctl-rec-{}-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0),
            next_tmp_seq()
        ));
        let log_file =
            std::fs::File::create(&stderr_log).with_context(|| format!("creating {}", stderr_log.display()))?;
        let mut child = Command::new("xcrun")
            .arg("simctl")
            .args(record_video_args(&self.udid, out))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file))
            .spawn()
            .context("running xcrun simctl io recordVideo")?;
        let deadline = Instant::now() + RECORDING_START_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                let err = std::fs::read_to_string(&stderr_log).unwrap_or_default();
                let _ = std::fs::remove_file(&stderr_log);
                bail!("Screen recording failed to start (simctl exited with {status}): {}", err.trim());
            }
            if out_path.exists() {
                break;
            }
            if Instant::now() >= deadline {
                send_sigint(child.id());
                if wait_with_timeout(&mut child, Duration::from_secs(5)).is_none() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let _ = std::fs::remove_file(&stderr_log);
                bail!("Screen recording failed to start: simctl did not create {out} within 10 s");
            }
            if let Err(err) = crate::interrupt::sleep(Duration::from_millis(20)) {
                send_sigint(child.id());
                if wait_with_timeout(&mut child, Duration::from_secs(5)).is_none() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let _ = std::fs::remove_file(&stderr_log);
                return Err(err);
            }
        }
        self.recording =
            Some(IosRecording { child, out_path: out_path.to_path_buf(), stderr_log, started: Instant::now() });
        Ok(true)
    }

    /// `stopRecording`: pads to [`MIN_RECORDING`], interrupts `simctl` with
    /// SIGINT so the file is finalized, and returns the recording's path.
    pub fn stop_recording(&mut self) -> Result<PathBuf> {
        let Some(mut rec) = self.recording.take() else {
            bail!("no screen recording is in progress");
        };
        if let Some(rest) = MIN_RECORDING.checked_sub(rec.started.elapsed())
            && let Err(err) = crate::interrupt::sleep(rest)
        {
            rec.stop();
            return Err(err);
        }
        let exited = rec.stop();
        let size = std::fs::metadata(&rec.out_path).map(|m| m.len()).unwrap_or(0);
        if !exited || size == 0 {
            bail!(
                "simctl did not finalize the screen recording at {} (exited cleanly: {exited}, {size} bytes)",
                rec.out_path.display()
            );
        }
        Ok(rec.out_path)
    }

    /// Whether a screen recording is currently running.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// `clearKeychain` (`LocalSimulatorUtils.clearKeychain`).
    pub fn clear_keychain(&self) -> Result<()> {
        self.simctl(&["keychain", &self.udid, "reset"])?;
        Ok(())
    }

    /// Whether Simulator.app connects the host keyboard to this device (then
    /// iOS shows no soft keyboard). Reads the per-device preference first and
    /// falls back to the global one; Simulator's default is connected.
    pub fn hardware_keyboard_connected(&self) -> bool {
        let per_device = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "defaults export com.apple.iphonesimulator - 2>/dev/null | \
                 plutil -extract 'DevicePreferences.{}.ConnectHardwareKeyboard' raw -o - - 2>/dev/null",
                self.udid
            ))
            .output()
            .ok()
            .and_then(|o| parse_bool_pref(&String::from_utf8_lossy(&o.stdout)));
        per_device
            .or_else(|| {
                run("defaults", &["read", "com.apple.iphonesimulator", "ConnectHardwareKeyboard"])
                    .ok()
                    .and_then(|o| parse_bool_pref(&o))
            })
            .unwrap_or(true)
    }

    /// User apps currently running on the simulator.
    pub fn running_apps(&self) -> Result<Vec<String>> {
        Ok(parse_running_apps(&self.simctl(&["spawn", &self.udid, "launchctl", "list"])?))
    }
}

fn ms(d: std::time::Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

/// Runs `install` up to twice. `clearState` has already uninstalled the app
/// when this runs, so one transient CoreSimulator failure must not cost it.
fn install_with_one_retry(mut install: impl FnMut() -> Result<()>) -> Result<()> {
    install().or_else(|_| install())
}

/// What the user needs when `clearState` could not put the app back.
fn reinstall_failure_hint(bundle: &str, udid: &str, copy: &Path) -> String {
    let copy = copy.display();
    format!(
        "{bundle} was uninstalled for clearState and could not be installed again; \
         its bundle is kept at {copy}: restore it with `xcrun simctl install {udid} '{copy}'`"
    )
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str().ok_or_else(|| anyhow!("non-UTF-8 path {}", p.display()))
}

fn xcrun(args: &[&str]) -> Result<String> {
    run("xcrun", args)
}

fn run(bin: &str, args: &[&str]) -> Result<String> {
    super::android::run(bin, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_failed_reinstall_is_retried_once_and_names_the_kept_bundle() {
        let mut calls = 0;
        let transient = install_with_one_retry(|| {
            calls += 1;
            if calls == 1 { Err(anyhow!("CoreSimulator hiccup")) } else { Ok(()) }
        });
        assert!(transient.is_ok());
        assert_eq!(calls, 2);

        let mut calls = 0;
        let broken = install_with_one_retry(|| {
            calls += 1;
            Err(anyhow!("attempt {calls} failed"))
        });
        assert_eq!(broken.unwrap_err().to_string(), "attempt 2 failed");
        assert_eq!(calls, 2);

        let hint = reinstall_failure_hint("dev.example.app", "UDID-1", Path::new("/tmp/k/dev.example.app.app"));
        assert!(hint.contains("kept at /tmp/k/dev.example.app.app"), "{hint}");
        assert!(hint.contains("xcrun simctl install UDID-1 '/tmp/k/dev.example.app.app'"), "{hint}");
    }

    #[test]
    fn parses_booted_simulators_from_simctl_json() {
        let json = r#"{"devices":{
            "com.apple.CoreSimulator.SimRuntime.iOS-27-2":[
              {"udid":"47862A4D-744F-4125-8D9E-20D8B51AD7DF","name":"iPhone 18 Pro","state":"Booted"},
              {"udid":"AAAA","name":"iPhone SE","state":"Shutdown"}],
            "com.apple.CoreSimulator.SimRuntime.watchOS-12-0":[]}}"#;
        assert_eq!(
            parse_booted_json(json),
            vec![SimInfo { udid: "47862A4D-744F-4125-8D9E-20D8B51AD7DF".into(), name: "iPhone 18 Pro".into() }]
        );
        assert!(parse_booted_json("not json").is_empty());
    }

    #[test]
    fn parses_launch_pid_and_bool_prefs() {
        assert_eq!(parse_launch_pid("dev.tapwing.sampleApp: 41234\n"), Some(41234));
        assert_eq!(parse_launch_pid("garbage"), None);
        assert_eq!(parse_bool_pref("1\n"), Some(true));
        assert_eq!(parse_bool_pref("false"), Some(false));
        assert_eq!(parse_bool_pref("The domain/default pair does not exist"), None);
    }

    #[test]
    fn parses_running_user_apps_from_launchctl() {
        let list = "PID\tStatus\tLabel\n\
            123\t0\tUIKitApplication:dev.tapwing.sampleApp[a1b2][rb-legacy]\n\
            -\t0\tUIKitApplication:com.example.notRunning[ffff][rb-legacy]\n\
            77\t0\tUIKitApplication:com.apple.mobilesafari[1234][rb-legacy]\n\
            88\t0\tcom.apple.backboardd\n";
        assert_eq!(parse_running_apps(list), vec!["dev.tapwing.sampleApp".to_string()]);
    }

    #[test]
    fn maps_maestro_permissions_to_simctl_privacy() {
        // Default launchApp permissions: all = allow (+ location always).
        let (a, u) = privacy_actions(&perms(&[("all", "allow")])).unwrap();
        assert_eq!(a, vec![("grant", "all"), ("grant", "location-always")]);
        assert!(u.is_empty());

        // Explicit entries are applied after (override) `all`.
        let (a, u) = privacy_actions(&perms(&[
            ("all", "deny"),
            ("camera", "allow"),
            ("location", "inuse"),
            ("photos", "allow"),
        ]))
        .unwrap();
        assert_eq!(a, vec![("revoke", "all"), ("grant", "location"), ("grant", "photos")]);
        assert_eq!(u, vec!["camera".to_string()]);

        let (a, _) = privacy_actions(&perms(&[("location", "never"), ("medialibrary", "unset")])).unwrap();
        assert_eq!(a, vec![("revoke", "location-always"), ("reset", "media-library")]);

        assert!(privacy_actions(&perms(&[("all", "maybe")])).is_err());
        assert!(privacy_actions(&perms(&[("location", "sometimes")])).is_err());
        // Maestro rejects explicit allow/deny for location (only via `all`).
        let err = privacy_actions(&perms(&[("location", "allow")])).unwrap_err().to_string();
        assert_eq!(err, "wrong argument value 'allow' was provided for 'location' permission");
    }

    #[test]
    fn add_media_validates_files_and_extensions() {
        let dir = std::env::temp_dir().join(format!("tapwing-ios-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jpg = dir.join("a.jpg");
        let bmp = dir.join("b.bmp");
        std::fs::write(&jpg, b"x").unwrap();
        std::fs::write(&bmp, b"x").unwrap();
        assert!(validate_media(&jpg).is_ok());
        assert_eq!(validate_media(&bmp).unwrap_err().to_string(), "Extension .bmp is not yet supported for add media");
        assert!(validate_media(&dir.join("nope.png")).unwrap_err().to_string().starts_with("Media file not found"));

        // Validation runs before `simctl` is invoked, so a bad path fails without a simulator.
        let sim = IosSim::new(SimInfo { udid: "NO-SUCH-UDID".into(), name: "none".into() });
        let paths = vec![jpg.display().to_string(), bmp.display().to_string()];
        assert!(sim.add_media(&paths).unwrap_err().to_string().contains(".bmp"));
        assert!(sim.add_media(&[]).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orientation_is_unsupported_on_the_simulator() {
        let sim = IosSim::new(SimInfo { udid: "U".into(), name: "n".into() });
        let err = sim.set_orientation("LANDSCAPE_LEFT").unwrap_err().to_string();
        assert!(err.contains("LANDSCAPE_LEFT") && err.contains("native lane"), "{err}");
    }

    #[test]
    fn record_video_uses_h264_and_overwrites() {
        assert_eq!(
            record_video_args("UDID", "/tmp/o.mp4"),
            vec!["io", "UDID", "recordVideo", "--codec=h264", "--force", "/tmp/o.mp4"]
        );
    }

    fn sleeper() -> Child {
        Command::new("sleep").arg("60").stderr(Stdio::piped()).spawn().unwrap()
    }

    fn process_alive(pid: u32) -> bool {
        Command::new("kill").args(["-0", &pid.to_string()]).output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn recording_state_machine_and_drop_safety() {
        let mut sim = IosSim::new(SimInfo { udid: "U".into(), name: "n".into() });
        assert!(!sim.is_recording());
        assert_eq!(sim.stop_recording().unwrap_err().to_string(), "no screen recording is in progress");

        let child = sleeper();
        let pid = child.id();
        sim.recording = Some(IosRecording {
            child,
            out_path: PathBuf::from("out.mp4"),
            stderr_log: std::env::temp_dir().join("tapwing-test-nonexistent.log"),
            started: Instant::now(),
        });
        // Already recording: start is a no-op that touches nothing.
        assert!(!sim.start_recording(Path::new("other.mp4")).unwrap());
        assert!(process_alive(pid));
        // Drop interrupts (SIGINT) and reaps the recorder.
        drop(sim);
        assert!(!process_alive(pid), "recorder process survived Drop");
    }

    #[test]
    fn format_launch_arguments_builds_simctl_user_defaults_cli_args() {
        assert!(format_launch_arguments(&HashMap::<String, Value>::new()).is_empty());

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("boolTrue".into(), serde_json::json!(true));
        args.insert("boolFalse".into(), serde_json::json!(false));
        args.insert("intVal".into(), serde_json::json!(42));
        args.insert("negInt".into(), serde_json::json!(-7));
        args.insert("floatVal".into(), serde_json::json!(3.125));
        args.insert("plainStr".into(), serde_json::json!("hello world"));
        args.insert("-AppleLanguages".into(), serde_json::json!("(en)"));
        args.insert("jsonArr".into(), serde_json::json!([1, "two"]));
        args.insert("nullVal".into(), Value::Null);

        assert_eq!(
            format_launch_arguments(&args),
            vec![
                "-AppleLanguages",
                "(en)",
                "-boolFalse",
                "false",
                "-boolTrue",
                "true",
                "-floatVal",
                "3.125",
                "-intVal",
                "42",
                "-jsonArr",
                "[1,\"two\"]",
                "-negInt",
                "-7",
                "-nullVal",
                "null",
                "-plainStr",
                "hello world",
            ]
        );
    }

    #[test]
    fn write_port_file_writes_atomically_and_cleans_temp_file_on_rename_failure() {
        let dir = std::env::temp_dir().join(format!("tapwing-ios-port-test-{}-{}", std::process::id(), next_tmp_seq()));
        let _ = std::fs::remove_dir_all(&dir);

        // Normal atomic write leaves only `tapwing_port` with the port number.
        write_port_file_in_dir(&dir, 45678).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(PORT_FILE)).unwrap(), "45678");
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1);

        // Overwriting an existing port file succeeds cleanly.
        write_port_file_in_dir(&dir, 54321).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(PORT_FILE)).unwrap(), "54321");

        // Force `fs::rename` failure by placing a directory at the target path:
        // the temporary `.tmp` file must be removed on error.
        let bad_dir = dir.join("rename-fail");
        std::fs::create_dir_all(bad_dir.join(PORT_FILE)).unwrap();
        assert!(write_port_file_in_dir(&bad_dir, 12345).is_err());
        let leftovers: Vec<String> = std::fs::read_dir(&bad_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec![PORT_FILE], "temp .tmp file must be cleaned up on rename failure");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
