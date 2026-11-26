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
use crate::process::run_with_timeout;

/// Deadline of a `simctl` query or setting (`list`, `get_app_container`,
/// `privacy`, `ui`, `openurl`, `io screenshot`, ...). These answer in well
/// under a second; the slack covers a CoreSimulator service that is still
/// starting.
const SIMCTL_TIMEOUT: Duration = Duration::from_secs(30);
/// `simctl terminate`, also run from `Drop` guards, where a wedged simulator
/// must not hold the exit for long.
pub(crate) const SIMCTL_TERMINATE_TIMEOUT: Duration = Duration::from_secs(15);
/// `simctl launch` returns once the app process exists, which is slow on a
/// loaded CI host right after an install.
const SIMCTL_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
/// `simctl install` / `uninstall` / `addmedia` copy whole app bundles or media files.
const SIMCTL_INSTALL_TIMEOUT: Duration = Duration::from_secs(180);
/// Copying an app bundle aside for `clearState` (`cp -R`, a clone on APFS).
const APP_COPY_TIMEOUT: Duration = Duration::from_secs(120);
/// Local preference readers (`plutil`, `defaults`).
const HOST_TOOL_TIMEOUT: Duration = Duration::from_secs(10);

/// The deadline of `xcrun simctl <args>`, by subcommand.
fn simctl_timeout(args: &[&str]) -> Duration {
    match args.first().copied() {
        Some("install" | "uninstall" | "addmedia") => SIMCTL_INSTALL_TIMEOUT,
        Some("launch") => SIMCTL_LAUNCH_TIMEOUT,
        Some("terminate") => SIMCTL_TERMINATE_TIMEOUT,
        _ => SIMCTL_TIMEOUT,
    }
}

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
    match simctl(&["list", "devices", "booted", "-j"]) {
        Ok(out) => parse_booted_json(&out),
        Err(_) => Vec::new(),
    }
}

/// Whether `id` has the shape of a simulator UDID (`8-4-4-4-12` hex digits).
pub fn is_simulator_udid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `SimDeviceState` of a booted simulator in CoreSimulator's `device.plist`.
const SIM_STATE_BOOTED: i64 = 3;

/// The simulator `udid`, when CoreSimulator's own `device.plist` says it is
/// booted: the answer of `simctl list devices booted` for one UDID without
/// spawning `simctl` (about 70-100 ms). `None` on anything unexpected (not a
/// UDID, file missing, other state, other UDID inside, not an XML plist), and
/// the caller asks `simctl` then.
pub fn booted_from_device_plist(udid: &str) -> Option<SimInfo> {
    if !is_simulator_udid(udid) {
        return None;
    }
    let path = Path::new(&std::env::var_os("HOME")?)
        .join("Library/Developer/CoreSimulator/Devices")
        .join(udid)
        .join("device.plist");
    booted_from_plist_xml(udid, &std::fs::read_to_string(path).ok()?)
}

fn booted_from_plist_xml(udid: &str, xml: &str) -> Option<SimInfo> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let (mut depth, mut key, mut in_value) = (0usize, None::<String>, false);
    let (mut plist_udid, mut name, mut state) = (None, None, None);
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => {
                // Only the top-level dictionary's `<key>` / `<string>` / `<integer>` pairs count.
                match (depth, e.name().as_ref()) {
                    (2, "key") => key = None,
                    (2, "string" | "integer") => in_value = key.is_some(),
                    _ => {}
                }
                depth += 1;
            }
            Event::Text(t) if depth == 3 => {
                let text = t.xml10_content().to_string();
                match (key.as_deref(), in_value) {
                    (_, false) if key.is_none() => key = Some(text),
                    (Some("UDID"), true) => plist_udid = Some(text),
                    (Some("name"), true) => name = Some(text),
                    (Some("state"), true) => state = text.trim().parse::<i64>().ok(),
                    _ => {}
                }
            }
            // Entities and CDATA split text events: let `simctl` handle those names.
            Event::GeneralRef(_) | Event::CData(_) if depth == 3 => return None,
            Event::End(_) => {
                depth = depth.checked_sub(1)?;
                if depth == 2 {
                    in_value = false;
                }
            }
            Event::Empty(_) if depth == 2 => key = None,
            Event::Eof => break,
            _ => {}
        }
    }
    (plist_udid.as_deref() == Some(udid) && state == Some(SIM_STATE_BOOTED))
        .then(|| name.filter(|n| !n.is_empty()).map(|name| SimInfo { udid: udid.to_string(), name }))
        .flatten()
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
    let (actions, _, unsupported) = privacy_plan(permissions)?;
    Ok((actions, unsupported))
}

/// [`privacy_actions`] plus how many of the leading actions come from `all`.
/// Those never depend on each other, but every explicit entry after them
/// overrides `all` and must be applied afterwards.
fn privacy_plan(permissions: &BTreeMap<String, String>) -> Result<(Vec<PrivacyAction>, usize, Vec<String>)> {
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
    let from_all = actions.len();
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
    Ok((actions, from_all, unsupported))
}

/// Runs `actions` through `apply`, concurrently when there is more than one.
///
/// A concurrent run that reports any error is repeated once, sequentially and
/// in order, and that second result is the one returned: a transient lock on
/// the simulator's privacy database must not fail a launch.
fn apply_privacy_batch(actions: &[PrivacyAction], apply: &(impl Fn(&str, &str) -> Result<()> + Sync)) -> Result<()> {
    if let [(action, service)] = actions {
        return apply(action, service);
    }
    let concurrent = std::thread::scope(|scope| {
        let handles: Vec<_> = actions.iter().map(|(action, service)| scope.spawn(|| apply(action, service))).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow!("simctl privacy worker panicked"))))
            .collect::<Result<Vec<()>>>()
    });
    if concurrent.is_ok() {
        return Ok(());
    }
    actions.iter().try_for_each(|(action, service)| apply(action, service))
}

/// The file `containermanagerd` keeps in every container; the simulator's
/// installation records need it, so a wipe never touches it.
const CONTAINER_METADATA_FILE: &str = ".com.apple.mobile_container_manager.metadata.plist";
/// The directories an app expects in its data container, recreated by a wipe
/// (the layout `installd` creates for a fresh install).
const DATA_CONTAINER_DIRS: [&str; 5] = ["Documents", "Library/Caches", "Library/Preferences", "SystemData", "tmp"];

/// How `clearState` puts the app back to a first-launch state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClearMethod {
    /// Empty the app's data container on the host file system (about 0.5 s with the terminate).
    Wipe,
    /// Copy the bundle aside, uninstall and install it again (about 0.7 s, what Maestro does).
    Reinstall,
}

/// The default empties the data container; `--maestro-compat strict` keeps
/// Maestro's reinstall (`LocalSimulatorUtils.clearAppState`).
fn clear_method(maestro_compat_strict: bool) -> ClearMethod {
    if maestro_compat_strict { ClearMethod::Reinstall } else { ClearMethod::Wipe }
}

/// What `clearState` does after the app is terminated.
#[derive(Debug, PartialEq, Eq)]
enum ClearStep {
    /// The data container is empty; nothing else to do.
    Cleared,
    /// Reinstall the app; `warning` tells why when that is not the chosen method.
    Reinstall { warning: Option<String> },
}

/// Runs `wipe` when `method` asks for it. A failed wipe falls back to the
/// reinstall, with a warning: the app must never be left half cleared.
fn clear_step(method: ClearMethod, bundle: &str, wipe: impl FnOnce() -> Result<()>) -> ClearStep {
    if method == ClearMethod::Reinstall {
        return ClearStep::Reinstall { warning: None };
    }
    match wipe() {
        Ok(()) => ClearStep::Cleared,
        Err(err) => ClearStep::Reinstall {
            warning: Some(format!(
                "warning: clearState {bundle}: could not empty the data container ({err:#}); \
                 reinstalling the app instead"
            )),
        },
    }
}

/// Whether a wipe must also run `simctl privacy reset all`. A reinstall resets
/// every privacy (TCC) service; a wipe does not. With an `all` entry in the
/// flow's permissions the launch sets every service explicitly right after
/// the clear, so the reset would only cost a `simctl` call.
fn needs_privacy_reset(permissions: &BTreeMap<String, String>) -> bool {
    !permissions.contains_key("all")
}

/// Checks that `path` is the data container of `bundle` on simulator `udid`
/// before anything is deleted in it:
/// `.../<udid>/data/Containers/Data/Application/<UUID>`, a real directory
/// holding the container manager's metadata file, which names `bundle`.
fn validate_data_container(path: &Path, udid: &str, bundle: &str) -> Result<()> {
    use std::path::Component;
    let parts: Vec<&std::ffi::OsStr> = path
        .components()
        .map(|c| match c {
            Component::Normal(name) => Ok(name),
            _ => Err(anyhow!("`{}` is not a plain absolute path", path.display())),
        })
        .skip(1) // the root
        .collect::<Result<_>>()?;
    let n = parts.len();
    let is_container_path = path.is_absolute()
        && n >= 6
        && parts[n - 6] == std::ffi::OsStr::new(udid)
        && parts[n - 5..n - 1] == ["data", "Containers", "Data", "Application"].map(std::ffi::OsStr::new)
        && parts[n - 1].to_str().is_some_and(is_simulator_udid);
    if !is_container_path {
        bail!("`{}` is not `<simulator {udid}>/data/Containers/Data/Application/<UUID>`", path.display());
    }
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        bail!("`{}` is not a directory", path.display());
    }
    let metadata = path.join(CONTAINER_METADATA_FILE);
    if !std::fs::symlink_metadata(&metadata).is_ok_and(|m| m.is_file()) {
        bail!("`{}` has no {CONTAINER_METADATA_FILE}", path.display());
    }
    // Binary and XML plists both hold the identifier as plain ASCII.
    let names_bundle = std::fs::read(&metadata)
        .map(|bytes| bytes.windows(bundle.len()).any(|w| w == bundle.as_bytes()))
        .with_context(|| format!("reading {}", metadata.display()))?;
    if !names_bundle {
        bail!("`{}` belongs to another app than {bundle}", path.display());
    }
    Ok(())
}

/// Deletes everything in `container` except the container manager's metadata
/// file, then recreates the standard directories. Fails on the first entry
/// that cannot be removed, so a failed wipe is never reported as a clean one.
fn empty_data_container(container: &Path) -> Result<()> {
    let entries = std::fs::read_dir(container).with_context(|| format!("listing {}", container.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("listing {}", container.display()))?;
        if entry.file_name() == CONTAINER_METADATA_FILE {
            continue;
        }
        let path = entry.path();
        // `remove_dir_all` and `remove_file` never follow a symlink out of the container.
        let removed = if entry.file_type().is_ok_and(|t| t.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.with_context(|| format!("removing {}", path.display()))?;
    }
    for dir in DATA_CONTAINER_DIRS {
        let path = container.join(dir);
        std::fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    }
    Ok(())
}

/// Whether a failed `defaults delete` only reports that nothing was stored.
fn defaults_found_nothing(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("not found")
}

pub struct IosSim {
    pub udid: String,
    pub name: String,
    data_containers: HashMap<String, PathBuf>,
    display_names: HashMap<String, Option<String>>,
    /// Last permissions set applied per bundle in this session (reset by reinstall).
    /// Used only to skip redundant `simctl privacy` calls when `launchApp` is
    /// called with `stopApp: false` and unchanged permissions, because
    /// `simctl privacy` kills a running app on iOS.
    applied_permissions: HashMap<String, BTreeMap<String, String>>,
    /// `--maestro-compat strict`: `clearState` reinstalls the app like Maestro.
    strict_clear_state: bool,
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
            strict_clear_state: false,
            recording: None,
        }
    }

    /// `--maestro-compat strict` makes `clearState` reinstall the app, as
    /// Maestro does, instead of emptying its data container.
    pub fn set_strict_clear_state(&mut self, strict: bool) {
        self.strict_clear_state = strict;
    }

    fn simctl(&self, args: &[&str]) -> Result<String> {
        simctl(args)
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
                let mut plutil = Command::new("plutil");
                plutil.args(["-extract", key, "raw", "-o", "-"]).arg(&plist);
                let out = run_with_timeout(&mut plutil, HOST_TOOL_TIMEOUT).ok()?;
                let v = out.stdout_lossy().trim().to_string();
                (out.status.success() && !v.is_empty()).then_some(v)
            })
        });
        self.display_names.insert(bundle.to_string(), name.clone());
        name
    }

    /// `simctl terminate` (`stopApp`; also `killApp`, since iOS has no
    /// process death: `IOSDriver.killApp`). Not running is not an error.
    pub fn terminate(&self, bundle: &str) -> Result<()> {
        let mut terminate = Command::new("xcrun");
        terminate.args(["simctl", "terminate", &self.udid, bundle]);
        let out = run_with_timeout(&mut terminate, SIMCTL_TERMINATE_TIMEOUT)?;
        if out.status.success() {
            return Ok(());
        }
        let err = out.stderr_lossy();
        // "found nothing to terminate" (not running) is fine, like Maestro.
        if err.contains("found nothing to terminate") || err.contains("not running") || err.contains("No such process")
        {
            return Ok(());
        }
        bail!("simctl terminate {bundle} failed: {}", err.trim())
    }

    /// `clearState`: terminate the app, then empty its data container (the
    /// default) or, under `--maestro-compat strict`, reinstall it like Maestro
    /// (`LocalSimulatorUtils.clearAppState`). Without a flow's permissions at
    /// hand (the `clearState` command), privacy is reset as well.
    pub fn clear_state(&mut self, bundle: &str) -> Result<()> {
        self.clear_state_for(bundle, true).map(|_| ())
    }

    /// Returns the fallback warning when the wipe failed and the app was
    /// reinstalled instead (already printed on stderr).
    fn clear_state_for(&mut self, bundle: &str, reset_privacy: bool) -> Result<Option<String>> {
        self.terminate(bundle)?;
        self.clear_terminated_state_with(bundle, reset_privacy, simctl)
    }

    /// [`Self::clear_state_for`] after the terminate, with `simctl` injected.
    fn clear_terminated_state_with(
        &mut self,
        bundle: &str,
        reset_privacy: bool,
        simctl: impl Fn(&[&str]) -> Result<String> + Sync,
    ) -> Result<Option<String>> {
        let step = clear_step(clear_method(self.strict_clear_state), bundle, || {
            self.wipe_data_container_with(bundle, reset_privacy, &simctl)
        });
        let warning = match step {
            ClearStep::Cleared => return Ok(None),
            ClearStep::Reinstall { warning } => warning,
        };
        if let Some(warning) = &warning {
            eprintln!("{warning}");
        }
        self.reinstall(bundle)?;
        Ok(warning)
    }

    /// Empties the data container of the terminated app, keeping its cached
    /// path (the container keeps its UUID; only a reinstall replaces it).
    ///
    /// 1. The app's preferences are dropped from `cfprefsd`: it serves a value
    ///    the app wrote just before it stopped from memory even after the
    ///    plist file is gone, so a wipe alone leaves the preference behind.
    ///    `defaults` addresses the plist by path, which reaches the app's
    ///    container from outside it.
    /// 2. The container's contents are deleted (see [`empty_data_container`]).
    /// 3. When `reset_privacy`, `simctl privacy reset all` runs alongside, as a
    ///    reinstall would (the app is already terminated: `simctl privacy`
    ///    kills a running app a moment after it returns).
    ///
    /// Any failure is an error and the caller reinstalls, which clears
    /// everything the failed wipe may have left.
    fn wipe_data_container_with(
        &mut self,
        bundle: &str,
        reset_privacy: bool,
        simctl: &(impl Fn(&[&str]) -> Result<String> + Sync),
    ) -> Result<()> {
        let container = self.data_container(bundle)?;
        validate_data_container(&container, &self.udid, bundle)?;
        // Whatever happens next, the permissions applied earlier no longer hold.
        self.applied_permissions.remove(bundle);
        let udid = self.udid.as_str();
        std::thread::scope(|scope| {
            let privacy = reset_privacy.then(|| scope.spawn(|| simctl(&["privacy", udid, "reset", "all", bundle])));
            let prefs = container.join("Library/Preferences").join(bundle);
            let wiped = path_str(&prefs)
                .and_then(|prefs| match simctl(&["spawn", udid, "defaults", "delete", prefs]) {
                    Err(err) if !defaults_found_nothing(&err) => Err(err.context("dropping the app's preferences")),
                    _ => Ok(()),
                })
                .and_then(|()| empty_data_container(&container));
            let privacy = privacy.map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err(anyhow!("simctl privacy worker panicked")))
                    .map(|_| ())
                    .context("resetting privacy")
            });
            wiped.and(privacy.unwrap_or(Ok(())))
        })
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
            run("cp", &["-Rc", &app, path_str(&copy)?], APP_COPY_TIMEOUT)
                .or_else(|_| run("cp", &["-R", &app, path_str(&copy)?], APP_COPY_TIMEOUT))?;
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

    /// Applies Maestro permissions via `simctl privacy`.
    /// Returns (number of `simctl privacy` actions run, unsupported names).
    pub fn set_permissions(
        &mut self,
        bundle: &str,
        permissions: &BTreeMap<String, String>,
    ) -> Result<(usize, Vec<String>)> {
        self.set_permissions_with(bundle, permissions, |udid, action, service, bundle| {
            simctl(&["privacy", udid, action, service, bundle]).map(|_| ())
        })
    }

    fn set_permissions_with(
        &mut self,
        bundle: &str,
        permissions: &BTreeMap<String, String>,
        apply_privacy: impl Fn(&str, &str, &str, &str) -> Result<()> + Sync,
    ) -> Result<(usize, Vec<String>)> {
        let (actions, from_all, unsupported) = privacy_plan(permissions)?;
        if actions.is_empty() {
            return Ok((0, unsupported));
        }
        // Clear any stale cache before applying so a partial failure cannot leave
        // a later `stopApp: false` thinking the new set is already active.
        self.applied_permissions.remove(bundle);
        // Entries from `all` first, explicit entries (which override them) after;
        // inside each batch the services are distinct, so order cannot matter.
        let udid = self.udid.as_str();
        let apply = |action: &str, service: &str| apply_privacy(udid, action, service, bundle);
        let (from_all, explicit) = actions.split_at(from_all);
        for batch in [from_all, explicit] {
            apply_privacy_batch(batch, &apply)?;
        }
        self.applied_permissions.insert(bundle.to_string(), permissions.clone());
        Ok((actions.len(), unsupported))
    }

    fn apply_launch_permissions_with(
        &mut self,
        bundle: &str,
        clear_state: bool,
        stop_app: bool,
        permissions: &BTreeMap<String, String>,
        apply_privacy: impl Fn(&str, &str, &str, &str) -> Result<()> + Sync,
    ) -> Result<(usize, Vec<String>)> {
        if should_apply_permissions_on_launch(self.applied_permissions.get(bundle), clear_state, stop_app, permissions)
        {
            return self.set_permissions_with(bundle, permissions, apply_privacy);
        }
        let (_, unsupported) = privacy_actions(permissions)?;
        Ok((0, unsupported))
    }

    /// Writes the port file and launches the app with optional `launchArguments`;
    /// returns the new PID. `terminate_running` makes `simctl` stop a running
    /// instance first (`--terminate-running-process`).
    pub fn launch<'a, I>(
        &mut self,
        bundle: &str,
        launch_arguments: I,
        port: u16,
        terminate_running: bool,
    ) -> Result<i64>
    where
        I: IntoIterator<Item = (&'a String, &'a Value)>,
    {
        self.write_port_file(bundle, port)?;
        let cli_args = format_launch_arguments(launch_arguments);
        let mut launch = Command::new("xcrun");
        launch
            .args(simctl_launch_args(&self.udid, bundle, terminate_running))
            .args(&cli_args)
            // Unused by Flutter today (no environment on iOS), kept for
            // non-Flutter hosts and future engines.
            .env("SIMCTL_CHILD_TAPWING_PORT", port.to_string());
        let out = run_with_timeout(&mut launch, SIMCTL_LAUNCH_TIMEOUT)?;
        let text = out.stdout_lossy();
        if !out.status.success() {
            bail!("simctl launch {bundle} failed: {}{}", text, out.stderr_lossy());
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
        let terminate_in_launch = terminate_with_launch(clear_state, stop_app, permissions);
        let mut clear_state_fallback = None;
        if clear_state {
            clear_state_fallback = self.clear_state_for(bundle, needs_privacy_reset(permissions))?;
        } else if stop_app && !terminate_in_launch {
            self.terminate(bundle)?;
        }
        let t1 = Instant::now();
        let (granted, unsupported) = self.apply_launch_permissions_with(
            bundle,
            clear_state,
            stop_app,
            permissions,
            |udid, action, service, bundle| simctl(&["privacy", udid, action, service, bundle]).map(|_| ()),
        )?;
        let t2 = Instant::now();
        let pid = self.launch(bundle, launch_arguments, port, terminate_in_launch)?;
        let t3 = Instant::now();
        Ok(LaunchMetrics {
            stop_clear_ms: ms(t1 - t0),
            permissions_ms: ms(t2 - t1),
            start_ms: ms(t3 - t2),
            granted_permissions: granted,
            pid: Some(pid),
            old_pids: Vec::new(),
            unsupported_permissions: unsupported,
            clear_state_fallback,
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
        let mut export = Command::new("sh");
        export.arg("-c").arg(format!(
            "defaults export com.apple.iphonesimulator - 2>/dev/null | \
             plutil -extract 'DevicePreferences.{}.ConnectHardwareKeyboard' raw -o - - 2>/dev/null",
            self.udid
        ));
        let per_device =
            run_with_timeout(&mut export, HOST_TOOL_TIMEOUT).ok().and_then(|o| parse_bool_pref(&o.stdout_lossy()));
        per_device
            .or_else(|| {
                run("defaults", &["read", "com.apple.iphonesimulator", "ConnectHardwareKeyboard"], HOST_TOOL_TIMEOUT)
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

/// Decides whether `launchApp` should invoke `simctl privacy`.
///
/// Re-applies whenever `clear_state` or `stop_app` is true (so two flows in
/// one shard with `permissions: {location: unset}` both reset TCC and see the
/// prompt). When `stopApp: false` and `!clear_state`, skips `simctl privacy`
/// if the exact same permission set was already applied to `bundle` in this
/// session, because `simctl privacy` kills a running app on iOS.
fn should_apply_permissions_on_launch(
    applied: Option<&BTreeMap<String, String>>,
    clear_state: bool,
    stop_app: bool,
    permissions: &BTreeMap<String, String>,
) -> bool {
    clear_state || stop_app || applied != Some(permissions)
}

/// Whether `launchApp` can leave the `stopApp` terminate to `simctl launch
/// --terminate-running-process` (one `simctl` call fewer).
///
/// Only when no `simctl privacy` call follows. `simctl privacy` kills a running
/// app on its own, a moment after it returns: with the old process still alive
/// that kill can land after the new launch and take the new process with it, so
/// whenever privacy runs the terminate stays first. Invalid permissions also
/// keep the terminate first, so the error is reported after the app was
/// stopped, as before.
fn terminate_with_launch(clear_state: bool, stop_app: bool, permissions: &BTreeMap<String, String>) -> bool {
    stop_app && !clear_state && matches!(privacy_actions(permissions), Ok((actions, _)) if actions.is_empty())
}

/// Arguments after `xcrun` for `simctl launch`.
fn simctl_launch_args<'a>(udid: &'a str, bundle: &'a str, terminate_running: bool) -> Vec<&'a str> {
    let mut args = vec!["simctl", "launch"];
    if terminate_running {
        args.push("--terminate-running-process");
    }
    args.extend([udid, bundle]);
    args
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

/// `xcrun simctl <args>` bounded by [`simctl_timeout`]; returns stdout.
fn simctl(args: &[&str]) -> Result<String> {
    let mut full = vec!["simctl"];
    full.extend_from_slice(args);
    run("xcrun", &full, simctl_timeout(args))
}

fn run(bin: &str, args: &[&str], timeout: Duration) -> Result<String> {
    super::android::run(bin, args, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};

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
    fn simctl_deadlines_follow_the_subcommand() {
        assert_eq!(simctl_timeout(&["install", "UDID", "/a.app"]), SIMCTL_INSTALL_TIMEOUT);
        assert_eq!(simctl_timeout(&["uninstall", "UDID", "dev.x"]), SIMCTL_INSTALL_TIMEOUT);
        assert_eq!(simctl_timeout(&["addmedia", "UDID", "/a.mp4"]), SIMCTL_INSTALL_TIMEOUT);
        assert_eq!(simctl_timeout(&["launch", "UDID", "dev.x"]), SIMCTL_LAUNCH_TIMEOUT);
        assert_eq!(simctl_timeout(&["terminate", "UDID", "dev.x"]), SIMCTL_TERMINATE_TIMEOUT);
        assert_eq!(simctl_timeout(&["list", "devices", "booted", "-j"]), SIMCTL_TIMEOUT);
        assert_eq!(simctl_timeout(&["io", "UDID", "screenshot"]), SIMCTL_TIMEOUT);
        assert_eq!(simctl_timeout(&[]), SIMCTL_TIMEOUT);
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

    fn device_plist(udid: &str, name: &str, state: i64) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\t<key>UDID</key>\n\t<string>{udid}</string>\n\
             \t<key>deviceType</key>\n\t<string>com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro</string>\n\
             \t<key>isDeleted</key>\n\t<false/>\n\t<key>lastUsedAt</key>\n\t<date>2026-11-25T16:49:16Z</date>\n\
             \t<key>name</key>\n\t<string>{name}</string>\n\t<key>state</key>\n\t<integer>{state}</integer>\n\
             </dict>\n</plist>\n"
        )
    }

    #[test]
    fn a_booted_simulator_is_read_from_its_device_plist_and_anything_else_is_left_to_simctl() {
        let udid = "EF1A4367-A3EE-4002-9879-331B46C027B2";
        assert_eq!(
            booted_from_plist_xml(udid, &device_plist(udid, "iPhone 18 Pro", 3)),
            Some(SimInfo { udid: udid.into(), name: "iPhone 18 Pro".into() })
        );
        // Not booted: shutdown (1), booting (2), shutting down (4).
        for state in [1, 2, 4] {
            assert_eq!(booted_from_plist_xml(udid, &device_plist(udid, "iPhone 18 Pro", state)), None, "{state}");
        }
        // A different simulator's file, a missing name, entities, binary and broken files.
        assert_eq!(booted_from_plist_xml(udid, &device_plist("AAAAAAAA-A3EE-4002-9879-331B46C027B2", "x", 3)), None);
        assert_eq!(booted_from_plist_xml(udid, &device_plist(udid, "", 3)), None);
        assert_eq!(booted_from_plist_xml(udid, &device_plist(udid, "Steve&apos;s iPhone", 3)), None);
        for broken in ["", "bplist00\u{1}\u{2}", "<plist><dict><key>state</key><integer>3</integer>", "not xml"] {
            assert_eq!(booted_from_plist_xml(udid, broken), None, "{broken:?}");
        }
        // Keys of nested dictionaries are not the device's own.
        let nested = "<plist><dict><key>other</key><dict><key>state</key><integer>3</integer>\
            <key>name</key><string>n</string></dict><key>UDID</key><string>U</string></dict></plist>";
        assert_eq!(booted_from_plist_xml("U", nested), None);

        // The id names a directory: only UDID-shaped ids are looked up.
        assert_eq!(booted_from_device_plist("../../etc"), None);
        assert_eq!(booted_from_device_plist("iPhone 18 Pro"), None);
        assert_eq!(booted_from_device_plist("00000000-0000-0000-0000-000000000000"), None);
    }

    #[test]
    fn simulator_udids_are_recognised_by_shape() {
        assert!(is_simulator_udid("EF1A4367-A3EE-4002-9879-331B46C027B2"));
        assert!(is_simulator_udid("ef1a4367-a3ee-4002-9879-331b46c027b2"));
        for not_udid in ["emulator-5554", "4C101JEKB02816", "iPhone 18 Pro", "192.168.1.5:5555", "", "47862A4D"] {
            assert!(!is_simulator_udid(not_udid), "{not_udid}");
        }
        // Right group lengths, but not hex.
        assert!(!is_simulator_udid("GF1A4367-A3EE-4002-9879-331B46C027B2"));
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

    /// Lets `parties` threads wait for each other, so a test can tell whether
    /// calls really overlapped without sleeping or racing a timer.
    struct Rendezvous {
        parties: usize,
        arrived: Mutex<usize>,
        all_here: Condvar,
    }

    impl Rendezvous {
        fn new(parties: usize) -> Self {
            Self { parties, arrived: Mutex::new(0), all_here: Condvar::new() }
        }

        /// True when every party arrived within the (generous) deadline.
        fn meet(&self) -> bool {
            let mut arrived = self.arrived.lock().unwrap();
            *arrived += 1;
            self.all_here.notify_all();
            let (arrived, timeout) =
                self.all_here.wait_timeout_while(arrived, Duration::from_secs(10), |n| *n < self.parties).unwrap();
            *arrived >= self.parties && !timeout.timed_out()
        }
    }

    #[test]
    fn permissions_from_all_run_together_before_the_explicit_overrides() {
        let mut sim = IosSim::new(SimInfo { udid: "SIM".into(), name: "iPhone".into() });
        let both_all_calls_overlapped = Mutex::new(Vec::new());
        let rendezvous = Rendezvous::new(2);
        let log: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let permissions = perms(&[("all", "allow"), ("location", "inuse"), ("photos", "deny")]);

        let (n, _) = sim
            .set_permissions_with("dev.app", &permissions, |_, action, service, _| {
                log.lock().unwrap().push(format!("start {action} {service}"));
                if service == "all" || service == "location-always" {
                    let met = rendezvous.meet();
                    both_all_calls_overlapped.lock().unwrap().push(met);
                }
                log.lock().unwrap().push(format!("end {action} {service}"));
                Ok(())
            })
            .unwrap();
        assert_eq!(n, 4);
        // `grant all` and `grant location-always` were in flight at the same time.
        assert_eq!(*both_all_calls_overlapped.lock().unwrap(), vec![true, true]);
        // Both finished before the first override (`location: inuse`) started.
        let log = log.into_inner().unwrap();
        let pos = |entry: &str| log.iter().position(|e| e == entry).unwrap_or_else(|| panic!("{entry} not in {log:?}"));
        let last_all_end = pos("end grant all").max(pos("end grant location-always"));
        let first_override_start = pos("start grant location").min(pos("start revoke photos"));
        assert!(last_all_end < first_override_start, "{log:?}");
        assert_eq!(log.len(), 8);
    }

    #[test]
    fn a_failed_concurrent_batch_is_repeated_sequentially_and_that_result_wins() {
        let mut sim = IosSim::new(SimInfo { udid: "SIM".into(), name: "iPhone".into() });
        let allow_all = perms(&[("all", "allow")]);

        // A transient lock: every service fails once, the sequential repeat succeeds.
        let attempts: Mutex<HashMap<String, usize>> = Mutex::new(HashMap::new());
        let in_flight = Mutex::new((0usize, 0usize)); // (now, max seen after the first round)
        let calls: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let rendezvous = Rendezvous::new(2);
        let (n, _) = sim
            .set_permissions_with("dev.app", &allow_all, |_, action, service, _| {
                let attempt = {
                    let mut attempts = attempts.lock().unwrap();
                    let entry = attempts.entry(service.to_string()).or_default();
                    *entry += 1;
                    *entry
                };
                calls.lock().unwrap().push(format!("{action} {service} #{attempt}"));
                if attempt == 1 {
                    // First round: both wait for each other, then both fail.
                    assert!(rendezvous.meet(), "first round was not concurrent");
                    bail!("database is locked");
                }
                let mut f = in_flight.lock().unwrap();
                f.0 += 1;
                f.1 = f.1.max(f.0);
                f.0 -= 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(n, 2);
        let calls = calls.into_inner().unwrap();
        // Sequential repeat, in the original order, after both concurrent attempts.
        assert_eq!(&calls[2..], ["grant all #2", "grant location-always #2"], "{calls:?}");
        assert_eq!(in_flight.into_inner().unwrap().1, 1);
        // The permissions count as applied: a `stopApp: false` relaunch skips `simctl privacy`.
        let none = |_: &str, _: &str, _: &str, _: &str| -> Result<()> { bail!("must not be called") };
        assert_eq!(sim.apply_launch_permissions_with("dev.app", false, false, &allow_all, none).unwrap().0, 0);

        // A failure that persists is reported after exactly one sequential repeat,
        // and the explicit overrides behind the failed batch never run.
        let mut sim = IosSim::new(SimInfo { udid: "SIM".into(), name: "iPhone".into() });
        let calls: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let err = sim
            .set_permissions_with(
                "dev.app",
                &perms(&[("all", "allow"), ("photos", "allow")]),
                |_, action, service, _| {
                    calls.lock().unwrap().push(format!("{action} {service}"));
                    if service == "location-always" { Err(anyhow!("simctl privacy: no such service")) } else { Ok(()) }
                },
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "simctl privacy: no such service");
        let mut calls = calls.into_inner().unwrap();
        assert_eq!(calls.iter().filter(|c| *c == "grant location-always").count(), 2, "{calls:?}");
        assert_eq!(calls.iter().filter(|c| *c == "grant all").count(), 2, "{calls:?}");
        calls.sort();
        calls.dedup();
        assert_eq!(calls, ["grant all", "grant location-always"], "photos must wait for the failed batch");
        // ... and nothing was recorded as applied.
        let record = |_: &str, _: &str, _: &str, _: &str| -> Result<()> { Ok(()) };
        assert_eq!(
            sim.apply_launch_permissions_with("dev.app", false, false, &perms(&[("all", "allow")]), record).unwrap().0,
            2
        );
    }

    #[test]
    fn a_single_privacy_action_is_not_repeated_on_failure() {
        let mut sim = IosSim::new(SimInfo { udid: "SIM".into(), name: "iPhone".into() });
        let calls = Mutex::new(0);
        let err = sim
            .set_permissions_with("dev.app", &perms(&[("location", "never")]), |_, _, _, _| {
                *calls.lock().unwrap() += 1;
                bail!("simctl failed")
            })
            .unwrap_err();
        assert_eq!(err.to_string(), "simctl failed");
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn stop_app_folds_the_terminate_into_launch_only_when_no_privacy_call_follows() {
        let default = perms(&[("all", "allow")]);
        let unsupported_only = perms(&[("notifications", "allow")]);
        let none = perms(&[]);
        // No `simctl privacy` follows: `launch --terminate-running-process` does the stop.
        assert!(terminate_with_launch(false, true, &unsupported_only));
        assert!(terminate_with_launch(false, true, &none));
        // `simctl privacy` runs (and kills a running app a moment later): terminate first.
        assert!(!terminate_with_launch(false, true, &default));
        assert!(!terminate_with_launch(false, true, &perms(&[("location", "never")])));
        // An invalid set still fails after the app was stopped, as before.
        assert!(!terminate_with_launch(false, true, &perms(&[("all", "maybe")])));
        // `stopApp: false` never kills anything; `clearState` terminates and reinstalls itself.
        assert!(!terminate_with_launch(false, false, &unsupported_only));
        assert!(!terminate_with_launch(true, true, &unsupported_only));
        assert!(!terminate_with_launch(true, false, &none));

        assert_eq!(
            simctl_launch_args("UDID", "dev.x", true),
            ["simctl", "launch", "--terminate-running-process", "UDID", "dev.x"]
        );
        assert_eq!(simctl_launch_args("UDID", "dev.x", false), ["simctl", "launch", "UDID", "dev.x"]);
    }

    #[test]
    fn launch_reapplies_permissions_unless_stop_app_false_with_unchanged_set() {
        let mut sim = IosSim::new(SimInfo { udid: "SIM-UDID".into(), name: "iPhone".into() });
        let unset_loc = perms(&[("location", "unset")]);
        let allow_all = perms(&[("all", "allow")]);
        let deny_loc = perms(&[("location", "never"), ("camera", "allow")]);

        let calls: Mutex<Vec<(String, String, String, String)>> = Mutex::new(Vec::new());
        let record = |u: &str, a: &str, s: &str, b: &str| -> Result<()> {
            calls.lock().unwrap().push((u.into(), a.into(), s.into(), b.into()));
            Ok(())
        };

        // Two flows in the same shard with `permissions: {location: unset}` (stop_app=true):
        // both runs must invoke `simctl privacy reset location-always`.
        let (n1, _) = sim.apply_launch_permissions_with("dev.app", false, true, &unset_loc, record).unwrap();
        let (n2, _) = sim.apply_launch_permissions_with("dev.app", false, true, &unset_loc, record).unwrap();
        assert_eq!((n1, n2), (1, 1));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                ("SIM-UDID".into(), "reset".into(), "location-always".into(), "dev.app".into()),
                ("SIM-UDID".into(), "reset".into(), "location-always".into(), "dev.app".into()),
            ]
        );

        // First launch with default `{all: allow}` applies; follow-up `stopApp: false`
        // with the same default skips `simctl privacy` so the running app is not killed.
        calls.lock().unwrap().clear();
        let (n_first, _) = sim.apply_launch_permissions_with("dev.app", false, true, &allow_all, record).unwrap();
        assert_eq!(n_first, 2);
        let (n_resume, _) = sim.apply_launch_permissions_with("dev.app", false, false, &allow_all, record).unwrap();
        assert_eq!(n_resume, 0);
        assert_eq!(
            calls.lock().unwrap().len(),
            2,
            "stopApp: false with unchanged permissions must not call simctl privacy"
        );

        // `stopApp: false` with CHANGED permissions DOES apply `simctl privacy`
        // (and still reports unsupported permissions on both apply and skip).
        calls.lock().unwrap().clear();
        let (n_changed, unsup1) =
            sim.apply_launch_permissions_with("dev.app", false, false, &deny_loc, record).unwrap();
        assert_eq!(n_changed, 1);
        assert_eq!(unsup1, vec!["camera"]);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![("SIM-UDID".into(), "revoke".into(), "location-always".into(), "dev.app".into())]
        );

        // Subsequent `stopApp: false` with the same `deny_loc` skips `simctl privacy`
        // while still reporting unsupported permissions.
        calls.lock().unwrap().clear();
        let (n_same_again, unsup2) =
            sim.apply_launch_permissions_with("dev.app", false, false, &deny_loc, record).unwrap();
        assert_eq!(n_same_again, 0);
        assert_eq!(unsup2, vec!["camera"]);
        assert!(calls.lock().unwrap().is_empty());

        // `setPermissions` command always applies even if the same map was applied before.
        let (n_set1, _) = sim.set_permissions_with("dev.app", &unset_loc, record).unwrap();
        let (n_set2, _) = sim.set_permissions_with("dev.app", &unset_loc, record).unwrap();
        assert_eq!((n_set1, n_set2), (1, 1));
        assert_eq!(calls.lock().unwrap().len(), 2);

        // Failed `set_permissions_with` clears stale cache so a later `stopApp: false` retries.
        let err =
            sim.set_permissions_with("dev.app", &perms(&[("location", "never")]), |_, _, _, _| bail!("simctl failed"));
        assert!(err.is_err());
        calls.lock().unwrap().clear();
        let (n_retry, _) = sim.apply_launch_permissions_with("dev.app", false, false, &unset_loc, record).unwrap();
        assert_eq!(n_retry, 1, "stale cache was cleared on failure");
    }

    // ---- clearState: wipe the data container instead of reinstalling ----

    const SIM: &str = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
    const CONTAINER_UUID: &str = "11111111-2222-3333-4444-555555555555";
    const BUNDLE: &str = "dev.example.app";

    /// A unique directory under the system temp dir, removed on drop.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tapwing-wipe-{label}-{}-{}", std::process::id(), next_tmp_seq()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `<root>/Devices/<udid>/data/Containers/Data/Application/<UUID>` as `installd`
    /// leaves it, plus what a running app adds to it.
    fn fake_container(root: &Path, udid: &str, bundle: &str) -> PathBuf {
        let c = root.join("Devices").join(udid).join("data/Containers/Data/Application").join(CONTAINER_UUID);
        for dir in DATA_CONTAINER_DIRS {
            std::fs::create_dir_all(c.join(dir)).unwrap();
        }
        // The real file is a binary plist; the identifier is stored as plain ASCII.
        std::fs::write(c.join(CONTAINER_METADATA_FILE), format!("bplist00 MCMMetadataIdentifier {bundle} end"))
            .unwrap();
        std::fs::write(c.join("Documents/tapwing_state.txt"), "saved-file").unwrap();
        std::fs::write(c.join("Library/Preferences").join(format!("{bundle}.plist")), "saved-pref").unwrap();
        std::fs::create_dir_all(c.join("Library/Caches/deep/er")).unwrap();
        std::fs::write(c.join("Library/Caches/deep/er/blob"), "x").unwrap();
        std::fs::create_dir_all(c.join("Library/Saved Application State")).unwrap();
        std::fs::write(c.join("tmp/tapwing_port"), "4242").unwrap();
        std::fs::write(c.join("stray.txt"), "x").unwrap();
        c
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn sim_with_container(container: &Path) -> IosSim {
        let mut sim = IosSim::new(SimInfo { udid: SIM.into(), name: "iPhone".into() });
        sim.data_containers.insert(BUNDLE.into(), container.to_path_buf());
        sim
    }

    /// Records every `simctl` call and answers with `reply`.
    struct FakeSimctl {
        calls: Mutex<Vec<Vec<String>>>,
        reply: fn(&[&str]) -> Result<String>,
        /// The container's `Documents` listing when `defaults delete` ran.
        documents_at_defaults: Mutex<Option<Vec<String>>>,
        container: PathBuf,
    }

    impl FakeSimctl {
        fn new(container: &Path, reply: fn(&[&str]) -> Result<String>) -> Self {
            Self {
                calls: Mutex::default(),
                reply,
                documents_at_defaults: Mutex::default(),
                container: container.into(),
            }
        }

        fn run(&self, args: &[&str]) -> Result<String> {
            self.calls.lock().unwrap().push(args.iter().map(|a| a.to_string()).collect());
            if args.contains(&"defaults") {
                *self.documents_at_defaults.lock().unwrap() = Some(names(&self.container.join("Documents")));
            }
            (self.reply)(args)
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().iter().map(|c| c.join(" ")).collect()
        }
    }

    fn ok(_: &[&str]) -> Result<String> {
        Ok(String::new())
    }

    #[test]
    fn a_wipe_keeps_the_metadata_file_and_recreates_the_standard_directories() {
        let tree = TempTree::new("wipe");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        let metadata = std::fs::read(c.join(CONTAINER_METADATA_FILE)).unwrap();

        empty_data_container(&c).unwrap();

        assert_eq!(names(&c), [CONTAINER_METADATA_FILE, "Documents", "Library", "SystemData", "tmp"]);
        assert_eq!(names(&c.join("Library")), ["Caches", "Preferences"]);
        for dir in ["Documents", "Library/Caches", "Library/Preferences", "SystemData", "tmp"] {
            assert!(names(&c.join(dir)).is_empty(), "{dir} still has content");
        }
        assert!(names(&c.join("Library/Caches")).is_empty());
        assert_eq!(std::fs::read(c.join(CONTAINER_METADATA_FILE)).unwrap(), metadata);
    }

    #[test]
    #[cfg(unix)]
    fn a_wipe_never_follows_a_symlink_out_of_the_container() {
        let tree = TempTree::new("symlink");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        let outside = tree.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, c.join("Documents/link")).unwrap();
        std::os::unix::fs::symlink(&outside, c.join("linked")).unwrap();

        empty_data_container(&c).unwrap();

        assert_eq!(names(&outside), ["keep.txt"]);
        assert!(!c.join("linked").exists());
    }

    #[test]
    fn only_the_data_container_of_the_app_on_this_simulator_is_accepted() {
        let tree = TempTree::new("validate");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        assert!(validate_data_container(&c, SIM, BUNDLE).is_ok());

        // Another simulator, another app.
        assert!(validate_data_container(&c, "FFFFFFFF-BBBB-CCCC-DDDD-EEEEEEEEEEEE", BUNDLE).is_err());
        let other = validate_data_container(&c, SIM, "dev.example.other").unwrap_err();
        assert!(other.to_string().contains("another app"), "{other}");

        // The bundle container, the simulator's own data, a plain directory, a relative path.
        let bundle_container =
            tree.0.join("Devices").join(SIM).join("data/Containers/Bundle/Application").join(CONTAINER_UUID);
        std::fs::create_dir_all(&bundle_container).unwrap();
        std::fs::write(bundle_container.join(CONTAINER_METADATA_FILE), BUNDLE).unwrap();
        for bad in [
            bundle_container,
            tree.0.join("Devices").join(SIM).join("data"),
            tree.0.clone(),
            PathBuf::from("/"),
            PathBuf::from("Devices").join(SIM).join("data/Containers/Data/Application").join(CONTAINER_UUID),
            c.join("Documents"),
            c.join("..").join(CONTAINER_UUID),
        ] {
            assert!(validate_data_container(&bad, SIM, BUNDLE).is_err(), "{} was accepted", bad.display());
        }

        // Right shape, but no metadata file (or the UUID is not one).
        std::fs::remove_file(c.join(CONTAINER_METADATA_FILE)).unwrap();
        let err = validate_data_container(&c, SIM, BUNDLE).unwrap_err();
        assert!(err.to_string().contains(CONTAINER_METADATA_FILE), "{err}");
        let not_uuid = tree.0.join("Devices").join(SIM).join("data/Containers/Data/Application/not-a-uuid");
        std::fs::create_dir_all(&not_uuid).unwrap();
        std::fs::write(not_uuid.join(CONTAINER_METADATA_FILE), BUNDLE).unwrap();
        assert!(validate_data_container(&not_uuid, SIM, BUNDLE).is_err());
    }

    #[test]
    fn a_refused_path_is_left_untouched_and_the_wipe_fails() {
        let tree = TempTree::new("refused");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        std::fs::remove_file(c.join(CONTAINER_METADATA_FILE)).unwrap();
        let before = names(&c);
        let simctl = FakeSimctl::new(&c, ok);

        let err = sim_with_container(&c).wipe_data_container_with(BUNDLE, true, &|a| simctl.run(a)).unwrap_err();

        assert!(err.to_string().contains(CONTAINER_METADATA_FILE), "{err}");
        assert_eq!(names(&c), before);
        assert!(c.join("Documents/tapwing_state.txt").exists());
        assert!(simctl.calls().is_empty(), "{:?}", simctl.calls());
    }

    #[test]
    fn a_wipe_drops_the_preferences_first_keeps_the_container_path_and_skips_the_reset_when_asked() {
        let tree = TempTree::new("order");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        let simctl = FakeSimctl::new(&c, ok);
        let mut sim = sim_with_container(&c);
        sim.applied_permissions.insert(BUNDLE.into(), perms(&[("all", "allow")]));

        sim.wipe_data_container_with(BUNDLE, false, &|a| simctl.run(a)).unwrap();

        // One call: the preference domain addressed by the plist path in this container.
        let prefs = c.join("Library/Preferences").join(BUNDLE);
        assert_eq!(simctl.calls(), [format!("spawn {SIM} defaults delete {}", prefs.display())]);
        // It ran while the container still held its files, and the wipe came after.
        assert_eq!(simctl.documents_at_defaults.lock().unwrap().clone().unwrap(), ["tapwing_state.txt"]);
        assert!(names(&c.join("Documents")).is_empty());
        // Same container UUID: the cached path stays, the permissions cache does not.
        assert_eq!(sim.data_containers.get(BUNDLE), Some(&c));
        assert!(!sim.applied_permissions.contains_key(BUNDLE));
    }

    #[test]
    fn privacy_is_reset_with_the_wipe_only_when_the_flow_has_no_all_entry() {
        assert!(needs_privacy_reset(&BTreeMap::new()));
        assert!(needs_privacy_reset(&perms(&[("location", "unset")])));
        assert!(needs_privacy_reset(&perms(&[("photos", "allow"), ("camera", "deny")])));
        for all in ["allow", "deny", "unset"] {
            assert!(!needs_privacy_reset(&perms(&[("all", all)])));
            assert!(!needs_privacy_reset(&perms(&[("all", all), ("location", "never")])));
        }

        let tree = TempTree::new("privacy");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        let simctl = FakeSimctl::new(&c, ok);
        sim_with_container(&c).wipe_data_container_with(BUNDLE, true, &|a| simctl.run(a)).unwrap();
        let mut calls = simctl.calls();
        calls.sort();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls.contains(&format!("privacy {SIM} reset all {BUNDLE}")), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with(&format!("spawn {SIM} defaults delete "))), "{calls:?}");
    }

    #[test]
    fn nothing_stored_is_fine_but_any_other_failure_stops_before_the_container_is_touched() {
        let tree = TempTree::new("defaults");
        let c = fake_container(&tree.0, SIM, BUNDLE);

        let nothing = FakeSimctl::new(&c, |_| {
            bail!("xcrun simctl spawn failed: Domain /x/y not found.\nDefaults have not been changed.")
        });
        sim_with_container(&c).wipe_data_container_with(BUNDLE, false, &|a| nothing.run(a)).unwrap();
        assert!(names(&c.join("Documents")).is_empty());

        let tree = TempTree::new("defaults-bad");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        let broken = FakeSimctl::new(&c, |_| bail!("Unable to boot device in current state: Shutdown"));
        let err = sim_with_container(&c).wipe_data_container_with(BUNDLE, false, &|a| broken.run(a)).unwrap_err();
        assert!(format!("{err:#}").contains("Shutdown"), "{err:#}");
        assert!(c.join("Documents/tapwing_state.txt").exists(), "the container was touched");
    }

    #[test]
    fn a_failed_privacy_reset_fails_the_wipe() {
        let tree = TempTree::new("privacy-fail");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        let simctl = FakeSimctl::new(&c, |args| {
            if args.first() == Some(&"privacy") { bail!("database is locked") } else { Ok(String::new()) }
        });
        let err = sim_with_container(&c).wipe_data_container_with(BUNDLE, true, &|a| simctl.run(a)).unwrap_err();
        assert!(format!("{err:#}").contains("resetting privacy: database is locked"), "{err:#}");
    }

    #[test]
    #[cfg(unix)]
    fn a_wipe_that_cannot_remove_an_entry_is_an_error_never_a_clean_success() {
        use std::os::unix::fs::PermissionsExt;
        let tree = TempTree::new("stuck");
        let c = fake_container(&tree.0, SIM, BUNDLE);
        let stuck = c.join("Library/Caches/deep/er");
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores directory permissions: only assert the error where they hold.
        let enforced = std::fs::File::create(stuck.join("probe")).is_err();

        let result = empty_data_container(&c);

        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();
        if enforced {
            let err = result.unwrap_err();
            assert!(format!("{err:#}").contains("removing"), "{err:#}");
        } else {
            assert!(result.is_ok());
            assert!(names(&c.join("Library/Caches")).is_empty());
        }
    }

    #[test]
    fn the_default_wipes_strict_compat_reinstalls_and_a_failed_wipe_falls_back_with_a_warning() {
        assert_eq!(clear_method(false), ClearMethod::Wipe);
        assert_eq!(clear_method(true), ClearMethod::Reinstall);

        // Wipe worked: nothing else to do.
        assert_eq!(clear_step(ClearMethod::Wipe, BUNDLE, || Ok(())), ClearStep::Cleared);

        // Strict: the wipe is not even attempted, and no warning (it is what was asked for).
        let step = clear_step(ClearMethod::Reinstall, BUNDLE, || panic!("strict compat must not wipe"));
        assert_eq!(step, ClearStep::Reinstall { warning: None });

        // A failed wipe reinstalls and says why.
        let ClearStep::Reinstall { warning: Some(warning) } =
            clear_step(ClearMethod::Wipe, BUNDLE, || Err(anyhow!("removing /x: Permission denied")))
        else {
            panic!("a failed wipe must fall back to the reinstall");
        };
        assert!(warning.starts_with("warning: clearState dev.example.app: "), "{warning}");
        assert!(warning.contains("Permission denied") && warning.contains("reinstalling the app instead"), "{warning}");
    }

    #[test]
    fn strict_compat_is_off_by_default_and_follows_the_setter() {
        let mut sim = IosSim::new(SimInfo { udid: SIM.into(), name: "iPhone".into() });
        assert_eq!(clear_method(sim.strict_clear_state), ClearMethod::Wipe);
        sim.set_strict_clear_state(true);
        assert_eq!(clear_method(sim.strict_clear_state), ClearMethod::Reinstall);
    }
}
