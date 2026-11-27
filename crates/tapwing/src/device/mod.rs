//! Device backends: Android (adb) and iOS Simulator (`xcrun simctl`).
//!
//! [`Device`] is the platform abstraction the runner talks to; the platform
//! is picked from `--device` (adb serial, simulator UDID or simulator name)
//! or forced with `--platform`.

pub mod adb_client;
pub mod android;
pub mod ios;

use anyhow::{Result, bail};
use image::DynamicImage;
use std::collections::BTreeMap;

pub use android::{Android, AnimationScaleGuard, SessionGuards};
pub use ios::{IosSim, SimInfo};

/// Platform whose Maestro semantics a session reproduces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Platform {
    Android,
    Ios,
}

impl Platform {
    /// Wire name used by the driver's `configure` (`DriverPlatform`).
    pub fn wire_name(self) -> &'static str {
        match self {
            Platform::Android => "android",
            Platform::Ios => "ios",
        }
    }
}

/// Phase breakdown and metadata of a `launchApp`.
#[derive(Clone, Debug)]
pub struct LaunchMetrics {
    pub stop_clear_ms: u64,
    pub permissions_ms: u64,
    /// `am start` (Android) or `simctl launch` (iOS).
    pub start_ms: u64,
    pub granted_permissions: usize,
    /// PID of the process started by this launch (or the running one when the
    /// app was not stopped).
    pub pid: Option<i64>,
    /// PIDs of the app that were running before it was stopped. A dying
    /// instance can still dial in and say hello, so these are never accepted.
    pub old_pids: Vec<i64>,
    /// Requested permissions the platform tooling cannot set (iOS: services
    /// `simctl privacy` does not cover).
    pub unsupported_permissions: Vec<String>,
    /// Why `clearState` reinstalled the app after the faster way of clearing
    /// failed (iOS: the data container could not be emptied). `None` when the
    /// chosen method worked. Reported with the launch, not only on stderr.
    pub clear_state_fallback: Option<String>,
}

/// The device a session runs on.
pub enum Device {
    Android(Android),
    Ios(IosSim),
}

/// Which backend [`choose`] picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Android(String),
    Ios(SimInfo),
}

impl Choice {
    /// Canonical device identifier (adb serial on Android, simulator UDID on iOS).
    pub fn id(&self) -> &str {
        match self {
            Self::Android(serial) => serial,
            Self::Ios(sim) => &sim.udid,
        }
    }
}

/// Picks the target device from `--device` / `--platform` and the connected
/// adb devices and booted simulators.
///
/// - `--device` matching a booted simulator's UDID or name selects iOS;
///   otherwise it is an adb serial (unless `--platform ios` says otherwise).
/// - Without `--device`, the single available device of the requested (or
///   any) platform is used; ambiguity is an error listing the candidates.
pub fn choose(
    device: Option<&str>,
    platform: Option<Platform>,
    adb_devices: &[String],
    simulators: &[SimInfo],
) -> Result<Choice> {
    let sim_match = |id: &str| simulators.iter().find(|s| s.udid == id || s.name == id).cloned();
    if let Some(id) = device {
        return match platform {
            Some(Platform::Android) => Ok(Choice::Android(id.to_string())),
            Some(Platform::Ios) => match sim_match(id) {
                Some(s) => Ok(Choice::Ios(s)),
                None => bail!("no booted iOS simulator matches `{id}` (UDID or name); boot it first"),
            },
            None => Ok(sim_match(id).map(Choice::Ios).unwrap_or_else(|| Choice::Android(id.to_string()))),
        };
    }
    let mut candidates: Vec<Choice> = Vec::new();
    if platform != Some(Platform::Ios) {
        candidates.extend(adb_devices.iter().cloned().map(Choice::Android));
    }
    if platform != Some(Platform::Android) {
        candidates.extend(simulators.iter().cloned().map(Choice::Ios));
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => match platform {
            Some(Platform::Android) => bail!("no Android device connected"),
            Some(Platform::Ios) => bail!("no booted iOS simulator"),
            None => bail!("no Android device connected and no iOS simulator booted"),
        },
        _ => {
            let names: Vec<String> = candidates
                .iter()
                .map(|c| match c {
                    Choice::Android(s) => format!("{s} (Android)"),
                    Choice::Ios(s) => format!("{} \"{}\" (iOS simulator)", s.udid, s.name),
                })
                .collect();
            bail!("multiple devices available, pass --device ({})", names.join(", "))
        }
    }
}

/// Resolves one or more devices for single-shard or multi-shard execution.
///
/// - When `requested_shards == 1 && requested_devices.len() <= 1`, delegates
///   directly to [`choose`] to preserve single-device selection semantics.
/// - When `requested_shards > 1`:
///   - With explicit `requested_devices`, matches each ID against booted
///     simulators (by UDID or name, unless `platform == Some(Android)`) or
///     connected `adb_devices` (unless `platform == Some(Ios)`), failing on
///     unconnected or duplicate devices.
///   - Without `requested_devices`, collects all connected candidates matching
///     `platform` (Android first, then iOS simulators).
///   - Verifies at least `requested_shards` devices are available before
///     clamping to `effective_shards`.
pub fn choose_many(
    requested_devices: &[String],
    requested_shards: usize,
    effective_shards: usize,
    platform: Option<Platform>,
    adb_devices: &[String],
    simulators: &[SimInfo],
) -> Result<Vec<Choice>> {
    if requested_shards <= 1 && requested_devices.len() <= 1 {
        let single = choose(requested_devices.first().map(String::as_str), platform, adb_devices, simulators)?;
        return Ok(vec![single]);
    }

    let sim_match = |id: &str| simulators.iter().find(|s| s.udid == id || s.name == id).cloned();
    let adb_match = |id: &str| adb_devices.iter().find(|s| s.as_str() == id).cloned();

    let mut candidates = Vec::new();
    if !requested_devices.is_empty() {
        let mut seen_ids = Vec::with_capacity(requested_devices.len());
        for id in requested_devices {
            let matched = match platform {
                Some(Platform::Android) => adb_match(id).map(Choice::Android),
                Some(Platform::Ios) => sim_match(id).map(Choice::Ios),
                None => sim_match(id).map(Choice::Ios).or_else(|| adb_match(id).map(Choice::Android)),
            };
            let Some(choice) = matched else {
                bail!("Device {id} was requested, but it is not connected.");
            };
            let canonical = choice.id().to_string();
            if seen_ids.contains(&canonical) {
                bail!("Duplicate device requested: {id}");
            }
            seen_ids.push(canonical);
            candidates.push(choice);
        }
    } else {
        if platform != Some(Platform::Ios) {
            candidates.extend(adb_devices.iter().cloned().map(Choice::Android));
        }
        if platform != Some(Platform::Android) {
            candidates.extend(simulators.iter().cloned().map(Choice::Ios));
        }
    }

    if candidates.len() < requested_shards {
        bail!(
            "Not enough devices connected ({}) to run the requested number of shards ({requested_shards}).",
            candidates.len()
        );
    }

    candidates.truncate(effective_shards.max(1));
    Ok(candidates)
}

/// Whether `simctl list` must run to resolve `--device`.
///
/// It is skipped when `--platform android` rules simulators out, and when the
/// id is not shaped like a simulator UDID and `adb_ready` (asked only then)
/// lists it as a connected adb serial. Everything else, simulator names and
/// ids adb does not know included, still asks `simctl`.
fn needs_simulator_probe(
    device: Option<&str>,
    platform: Option<Platform>,
    adb_ready: impl FnOnce() -> Vec<String>,
) -> bool {
    match (device, platform) {
        (_, Some(Platform::Android)) => false,
        (Some(id), None) if !ios::is_simulator_udid(id) => !adb_ready().iter().any(|serial| serial == id),
        _ => true,
    }
}

impl Device {
    /// Resolves and connects to the target device (see [`choose`]).
    pub fn select(device: Option<&str>, platform: Option<Platform>) -> Result<Self> {
        // Only probe the tools the selection can need: an explicit `--device`
        // is either a booted simulator or taken as an adb serial as-is.
        let adb =
            if platform == Some(Platform::Ios) || device.is_some() { Vec::new() } else { android::list_devices() };
        let sims = if needs_simulator_probe(device, platform, || {
            // Only asks a server that already runs; never starts one.
            adb_client::AdbClient::new().ready_devices().unwrap_or_default()
        }) {
            // A UDID that CoreSimulator's own `device.plist` shows as booted needs no `simctl list`.
            device.and_then(ios::booted_from_device_plist).map(|sim| vec![sim]).unwrap_or_else(ios::list_booted)
        } else {
            Vec::new()
        };
        Ok(match choose(device, platform, &adb, &sims)? {
            Choice::Android(serial) => Device::Android(Android::new(serial)),
            Choice::Ios(info) => Device::Ios(IosSim::new(info)),
        })
    }

    /// Resolves and connects to one or more devices for single-shard or
    /// multi-shard runs (see [`choose_many`]).
    pub fn select_many(
        requested_devices: &[String],
        requested_shards: usize,
        effective_shards: usize,
        platform: Option<Platform>,
    ) -> Result<Vec<Self>> {
        if requested_shards <= 1 && requested_devices.len() <= 1 {
            return Self::select(requested_devices.first().map(String::as_str), platform).map(|d| vec![d]);
        }
        let mut adb = if platform == Some(Platform::Ios) { Vec::new() } else { android::list_devices() };
        adb.sort();
        let mut sims = if platform == Some(Platform::Android) { Vec::new() } else { ios::list_booted() };
        sims.sort_by(|a, b| a.udid.cmp(&b.udid));
        let choices = choose_many(requested_devices, requested_shards, effective_shards, platform, &adb, &sims)?;
        Ok(choices
            .into_iter()
            .map(|c| match c {
                Choice::Android(serial) => Device::Android(Android::new(serial)),
                Choice::Ios(info) => Device::Ios(IosSim::new(info)),
            })
            .collect())
    }

    pub fn platform(&self) -> Platform {
        match self {
            Device::Android(_) => Platform::Android,
            Device::Ios(_) => Platform::Ios,
        }
    }

    /// Human-readable device description for the console.
    pub fn describe(&self) -> String {
        match self {
            Device::Android(a) => a.serial.clone(),
            Device::Ios(s) => format!("{} ({}, iOS simulator)", s.name, s.udid),
        }
    }

    /// Unique device identifier (adb serial on Android, simulator UDID on iOS).
    pub fn id(&self) -> &str {
        match self {
            Device::Android(a) => &a.serial,
            Device::Ios(s) => &s.udid,
        }
    }

    pub fn stop_app(&mut self, app_id: &str) -> Result<()> {
        match self {
            Device::Android(a) => a.stop_app(app_id),
            Device::Ios(s) => s.terminate(app_id),
        }
    }

    /// `killApp`: `am kill` on Android; on iOS, where there is no process
    /// death, a synonym for `stopApp` (`IOSDriver.killApp`).
    pub fn kill_app(&mut self, app_id: &str) -> Result<()> {
        match self {
            Device::Android(a) => a.kill_app(app_id),
            Device::Ios(s) => s.terminate(app_id),
        }
    }

    pub fn clear_state(&mut self, app_id: &str) -> Result<()> {
        match self {
            Device::Android(a) => a.clear_state(app_id),
            Device::Ios(s) => s.clear_state(app_id),
        }
    }

    /// `launchApp`; `host_port` is this session's port (handed to the app on
    /// iOS; Android reaches it through `adb reverse`).
    pub fn prepare_and_launch(
        &mut self,
        app_id: &str,
        clear_state: bool,
        stop_app: bool,
        permissions: &BTreeMap<String, String>,
        launch_arguments: &crate::flow::ir::ValueMap,
        host_port: u16,
    ) -> Result<LaunchMetrics> {
        match self {
            Device::Android(a) => a.prepare_and_launch(app_id, clear_state, stop_app, permissions, launch_arguments),
            Device::Ios(s) => {
                s.prepare_and_launch(app_id, clear_state, stop_app, permissions, launch_arguments, host_port)
            }
        }
    }

    /// Lets an already running app find this session before attaching
    /// (iOS: rewrites the port file the driver re-reads before every dial).
    pub fn prepare_attach(&mut self, app_id: &str, host_port: u16) -> Result<()> {
        match self {
            Device::Android(_) => Ok(()),
            Device::Ios(s) => s.write_port_file(app_id, host_port),
        }
    }

    /// iOS display name for the hierarchy's application element.
    pub fn app_display_name(&mut self, app_id: &str) -> Option<String> {
        match self {
            Device::Android(_) => None,
            Device::Ios(s) => s.display_name(app_id),
        }
    }

    pub fn screenshot_bytes(&self) -> Result<Vec<u8>> {
        match self {
            Device::Android(a) => a.screenshot_bytes(),
            Device::Ios(s) => s.screenshot_bytes(),
        }
    }

    pub fn screenshot_image(&self) -> Result<DynamicImage> {
        match self {
            Device::Android(a) => a.screenshot_image(),
            Device::Ios(s) => s.screenshot_image(),
        }
    }

    pub fn is_dark_mode(&self) -> Result<bool> {
        match self {
            Device::Android(a) => a.is_dark_mode(),
            Device::Ios(s) => s.is_dark_mode(),
        }
    }

    pub fn as_ios(&mut self) -> Option<&mut IosSim> {
        match self {
            Device::Ios(s) => Some(s),
            Device::Android(_) => None,
        }
    }

    pub fn as_android(&mut self) -> Option<&mut Android> {
        match self {
            Device::Android(a) => Some(a),
            Device::Ios(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(udid: &str, name: &str) -> SimInfo {
        SimInfo { udid: udid.into(), name: name.into() }
    }

    #[test]
    fn device_flag_picks_simulator_by_udid_or_name_else_adb() {
        let sims = [sim("47862A4D", "iPhone 18 Pro")];
        let adb = ["emulator-5554".to_string()];
        assert_eq!(choose(Some("47862A4D"), None, &adb, &sims).unwrap(), Choice::Ios(sims[0].clone()));
        assert_eq!(choose(Some("iPhone 18 Pro"), None, &adb, &sims).unwrap(), Choice::Ios(sims[0].clone()));
        assert_eq!(choose(Some("emulator-5554"), None, &adb, &sims).unwrap(), Choice::Android("emulator-5554".into()));
        // An explicit serial is passed through even if adb does not list it yet.
        assert_eq!(choose(Some("R5CX"), None, &[], &[]).unwrap(), Choice::Android("R5CX".into()));
        assert!(choose(Some("nope"), Some(Platform::Ios), &adb, &sims).is_err());
        assert_eq!(
            choose(Some("47862A4D"), Some(Platform::Android), &adb, &sims).unwrap(),
            Choice::Android("47862A4D".into())
        );
    }

    #[test]
    fn a_connected_adb_serial_skips_simctl_and_everything_else_still_asks_it() {
        let adb = || vec!["emulator-5554".to_string(), "R5CX".to_string()];
        let udid = "EF1A4367-A3EE-4002-9879-331B46C027B2";
        // `--device emulator-5554` without `--platform`: adb knows it, no `simctl list`.
        assert!(!needs_simulator_probe(Some("emulator-5554"), None, adb));
        assert!(!needs_simulator_probe(Some("R5CX"), None, adb));
        // Simulator names and ids adb does not list still go to `simctl` (and then to Android).
        assert!(needs_simulator_probe(Some("iPhone 18 Pro"), None, adb));
        assert!(needs_simulator_probe(Some("emulator-9999"), None, adb));
        // No adb server or no device at all: ask `simctl`.
        assert!(needs_simulator_probe(Some("emulator-5554"), None, Vec::new));
        // Platform wins: Android never lists simulators, iOS always does.
        assert!(!needs_simulator_probe(Some("iPhone 18 Pro"), Some(Platform::Android), adb));
        assert!(needs_simulator_probe(Some("emulator-5554"), Some(Platform::Ios), adb));
        // No `--device`: both lists are needed unless Android is forced.
        assert!(needs_simulator_probe(None, None, adb));
        assert!(!needs_simulator_probe(None, Some(Platform::Android), adb));

        // adb is only asked when the answer can change the outcome.
        let asked = std::cell::Cell::new(0);
        let counting = || {
            asked.set(asked.get() + 1);
            Vec::new()
        };
        assert!(needs_simulator_probe(Some(udid), None, counting));
        assert!(needs_simulator_probe(Some("emulator-5554"), Some(Platform::Ios), counting));
        assert!(needs_simulator_probe(None, None, counting));
        assert!(!needs_simulator_probe(Some("emulator-5554"), Some(Platform::Android), counting));
        assert_eq!(asked.get(), 0);
        assert!(needs_simulator_probe(Some("emulator-5554"), None, counting));
        assert_eq!(asked.get(), 1);

        // With simulators not probed, `choose` and `choose_many` resolve the serial as before.
        assert_eq!(choose(Some("emulator-5554"), None, &[], &[]).unwrap(), Choice::Android("emulator-5554".into()));
        let multi = vec!["emulator-5554".to_string(), "R5CX".to_string()];
        let adb_list = adb();
        assert_eq!(
            choose_many(&multi, 2, 2, None, &adb_list, &[]).unwrap(),
            vec![Choice::Android("emulator-5554".into()), Choice::Android("R5CX".into())]
        );
    }

    #[test]
    fn without_device_flag_requires_a_single_candidate() {
        let sims = [sim("47862A4D", "iPhone 18 Pro")];
        let adb = ["emulator-5554".to_string()];
        let err = choose(None, None, &adb, &sims).unwrap_err().to_string();
        assert!(err.contains("emulator-5554") && err.contains("47862A4D"), "{err}");
        assert_eq!(choose(None, Some(Platform::Ios), &adb, &sims).unwrap(), Choice::Ios(sims[0].clone()));
        assert_eq!(
            choose(None, Some(Platform::Android), &adb, &sims).unwrap(),
            Choice::Android("emulator-5554".into())
        );
        assert_eq!(choose(None, None, &adb, &[]).unwrap(), Choice::Android("emulator-5554".into()));
        assert_eq!(choose(None, None, &[], &sims).unwrap(), Choice::Ios(sims[0].clone()));
        assert!(choose(None, None, &[], &[]).is_err());
    }

    #[test]
    fn choose_many_handles_multi_shard_device_matching_duplicates_and_truncation() {
        let sims = [sim("47862A4D", "iPhone 18 Pro"), sim("9999BBBB", "iPhone SE")];
        let adb = ["emulator-5554".to_string(), "emulator-5556".to_string(), "emulator-5558".to_string()];

        // Automatic candidate selection (Android first, then iOS) + clamping from 3 requested to 2 effective.
        let picked = choose_many(&[], 3, 2, None, &adb, &sims).unwrap();
        assert_eq!(picked, vec![Choice::Android("emulator-5554".into()), Choice::Android("emulator-5556".into())]);

        // Platform filter: iOS only.
        let ios_picked = choose_many(&[], 2, 2, Some(Platform::Ios), &adb, &sims).unwrap();
        assert_eq!(ios_picked, vec![Choice::Ios(sims[0].clone()), Choice::Ios(sims[1].clone())]);

        // Not enough devices before clamping -> error.
        let err = choose_many(&[], 3, 2, Some(Platform::Ios), &adb, &sims).unwrap_err().to_string();
        assert_eq!(err, "Not enough devices connected (2) to run the requested number of shards (3).");

        // Missing requested device.
        let req = vec!["emulator-5554".to_string(), "emulator-9999".to_string()];
        let err = choose_many(&req, 2, 2, None, &adb, &sims).unwrap_err().to_string();
        assert_eq!(err, "Device emulator-9999 was requested, but it is not connected.");

        // Duplicate adb device.
        let dup_adb = vec!["emulator-5554".to_string(), "emulator-5554".to_string()];
        let err = choose_many(&dup_adb, 2, 2, None, &adb, &sims).unwrap_err().to_string();
        assert_eq!(err, "Duplicate device requested: emulator-5554");

        // Duplicate simulator requested once by name and once by UDID.
        let dup_sim = vec!["iPhone 18 Pro".to_string(), "47862A4D".to_string()];
        let err = choose_many(&dup_sim, 2, 2, None, &adb, &sims).unwrap_err().to_string();
        assert_eq!(err, "Duplicate device requested: 47862A4D");
    }
}
