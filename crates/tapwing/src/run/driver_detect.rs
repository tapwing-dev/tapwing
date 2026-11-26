//! Runs the driver probe next to a launch and applies what it finds.
//!
//! The probe (see [`crate::driver_probe`]) reads the installed build on its own
//! thread, so it costs an instrumented app nothing: it starts before
//! `prepare_and_launch` and the handshake wait polls it. When it proves the
//! build has no driver the wait ends at once and the launch takes the native
//! path; when it finishes any other way the wait is the one it always was.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;

use super::DeviceFlutterBackend;
use super::launch::{
    DriverKnowledge, driver_probe_json, driver_skip_note, driver_skip_report_reason, format_driver_skip_warning,
};
use crate::device::Device;
use crate::device::LaunchMetrics;
use crate::device::adb_client::AdbClient;
use crate::driver_probe::cache::{DriverCache, InstallKey};
use crate::driver_probe::{self, Presence, Report, SkipReason};
use crate::ios_native_lane::IosNativeLaneHandle;
use crate::native_lane::InputMode;

/// The probe's own deadline: a slow device ends in an unknown verdict long
/// before the handshake wait it tries to spare would have.
pub(super) const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// What the probe reads: the device and, for iOS, the simulator.
pub(super) enum Target {
    Android { serial: String, adb: AdbClient },
    Ios { udid: String },
}

struct Outcome {
    report: Report,
    /// An iOS native runner the probe started the moment it proved the app has
    /// no driver, so the runner warms up while the app launches.
    runner: Mutex<Option<IosNativeLaneHandle>>,
}

/// A probe running for one `launchApp`. Dropping it cancels what is left of it.
pub(super) struct LaunchProbe {
    outcome: Arc<OnceLock<Outcome>>,
    cancel: Arc<AtomicBool>,
}

type RunnerStarter = Box<dyn FnOnce() -> IosNativeLaneHandle + Send>;

impl LaunchProbe {
    /// Starts probing `app_id` on `target`. `runner` starts the iOS native
    /// runner when the probe finds no driver.
    pub(super) fn start(
        target: Target,
        app_id: &str,
        cache: Option<DriverCache>,
        runner: Option<RunnerStarter>,
    ) -> Option<Self> {
        let app = app_id.to_string();
        Self::start_with(runner, move |cancel| {
            let deadline = Instant::now() + PROBE_DEADLINE;
            match &target {
                Target::Android { serial, adb } => {
                    driver_probe::android::probe(adb, serial, &app, deadline, cancel, cache.as_ref())
                }
                Target::Ios { udid } => driver_probe::ios::probe(udid, &app, deadline, cancel, cache.as_ref()),
            }
        })
    }

    /// [`Self::start`] with the work given, so tests need no device.
    pub(super) fn start_with(
        runner: Option<RunnerStarter>,
        job: impl FnOnce(&AtomicBool) -> Report + Send + 'static,
    ) -> Option<Self> {
        let outcome = Arc::new(OnceLock::new());
        let cancel = Arc::new(AtomicBool::new(false));
        let (slot, flag) = (outcome.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("tapwing-driver-probe".into())
            .spawn(move || {
                let report = job(&flag);
                let skip = report.skip_reason().is_some() && !flag.load(Ordering::Relaxed);
                let runner = if skip { runner.map(|start| start()) } else { None };
                let _ = slot.set(Outcome { report, runner: Mutex::new(runner) });
            })
            .ok()?;
        Some(Self { outcome, cancel })
    }

    pub(super) fn report(&self) -> Option<&Report> {
        self.outcome.get().map(|o| &o.report)
    }

    pub(super) fn skip_reason(&self) -> Option<SkipReason> {
        self.report().and_then(Report::skip_reason)
    }

    pub(super) fn knowledge(&self) -> DriverKnowledge {
        match self.report() {
            None => DriverKnowledge::Pending,
            Some(report) => report.skip_reason().map_or(DriverKnowledge::Wait, DriverKnowledge::Skip),
        }
    }

    pub(super) fn take_runner(&self) -> Option<IosNativeLaneHandle> {
        self.outcome.get()?.runner.lock().ok()?.take()
    }

    /// The install this probe is about, once its fingerprint is known.
    fn install_key(&self, device: &str, app: &str) -> Option<InstallKey> {
        let fingerprint = self.report()?.fingerprint.clone()?;
        Some(InstallKey { device: device.into(), app: app.into(), fingerprint })
    }
}

impl Drop for LaunchProbe {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl DeviceFlutterBackend<'_> {
    /// The persistent cache, in runs that keep one.
    pub(super) fn probe_cache(&self) -> Option<DriverCache> {
        #[cfg(test)]
        if let Some(cache) = &self.cache_override {
            return Some(cache.clone());
        }
        self.opts.driver_probe.then(DriverCache::default_location).flatten()
    }

    /// Starts the driver probe for a launch that will wait for a handshake it
    /// could skip: `--input auto` with a native lane to hand over to, and a
    /// process (re)start. Anything else never probes.
    pub(super) fn start_driver_probe(&mut self, app_id: &str, restarting: bool) -> Option<LaunchProbe> {
        #[cfg(test)]
        if let Some(probe) = self.probe_override.take() {
            return Some(probe);
        }
        if !(self.opts.driver_probe
            && restarting
            && self.lane_mode() == InputMode::Auto
            && self.native_usable()
            && !self.is_blackbox_app(app_id))
        {
            return None;
        }
        let target = match &*self.dev {
            Device::Android(a) => Target::Android { serial: a.serial.clone(), adb: a.adb_client().clone() },
            Device::Ios(sim) => Target::Ios { udid: sim.udid.clone() },
        };
        // The runner warms up in parallel with the launch only when nothing has started it yet.
        let runner: Option<RunnerStarter> = match (&target, self.ios_native.as_deref()) {
            (Target::Ios { udid }, Some(slot)) if !slot.is_started() => {
                let udid = udid.clone();
                Some(Box::new(move || IosNativeLaneHandle::spawn(&udid)))
            }
            _ => None,
        };
        LaunchProbe::start(target, app_id, self.probe_cache(), runner)
    }

    /// Hands the runner the probe started to the iOS native slot.
    pub(super) fn adopt_probe_runner(&mut self, probe: &LaunchProbe) {
        if let (Some(slot), Some(runner)) = (self.ios_native.as_deref_mut(), probe.take_runner()) {
            slot.adopt(runner);
        }
    }

    /// The launch of an app with no driver: no handshake wait, the native lane
    /// at once, announced on stderr and marked in the reports.
    pub(super) fn launch_without_handshake(
        &mut self,
        launch: &LaunchMetrics,
        probe: Option<&LaunchProbe>,
        reason: SkipReason,
    ) -> Result<Value> {
        let app_id = self.flow_app.clone();
        let report = probe.and_then(LaunchProbe::report);
        let evidence = report.map_or("", |r| r.verdict.evidence.as_str());
        self.drv.disconnect();
        if let Some(probe) = probe {
            self.adopt_probe_runner(probe);
        }
        self.mark_flow_blackbox(&app_id);
        self.blackbox_reason = Some(driver_skip_report_reason(reason, evidence));
        eprintln!("{}", format_driver_skip_warning(&self.opts.log_prefix, &app_id, reason, evidence));
        let mut res = self.finish_native_launch(launch, &driver_skip_note(reason, evidence))?;
        if let Some(report) = report {
            res["driverProbe"] = driver_probe_json(reason, report);
        }
        Ok(res)
    }

    /// The handshake wait ran out: remember it against this install, but only
    /// when the app process was still alive (a crashing or never-started app
    /// says nothing about the driver) and the probe could not show that the
    /// build dials in.
    pub(super) fn learn_missed_handshake(&mut self, app_id: &str, pid: Option<i64>, probe: Option<&LaunchProbe>) {
        let Some(probe) = probe else { return };
        let Some(report) = probe.report() else { return };
        if report.verdict.presence == Presence::Absent
            || (report.verdict.presence == Presence::Present && report.verdict.dials)
        {
            return;
        }
        let alive = pid.and_then(|pid| self.drv.process_alive(pid)) == Some(true);
        if !alive {
            return;
        }
        let device = self.dev.id().to_string();
        if let (Some(key), Some(cache)) = (probe.install_key(&device, app_id), self.probe_cache()) {
            cache.record_miss(&key);
        }
    }

    /// A handshake arrived: what earlier runs learned against this app is stale.
    pub(super) fn forget_learned_misses(&self, app_id: &str) {
        if let Some(cache) = self.probe_cache() {
            cache.clear_after_handshake(self.dev.id(), app_id);
        }
    }
}

#[cfg(test)]
mod tests;
