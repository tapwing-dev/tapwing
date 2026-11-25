//! Android native lane (F5, F11a): a UiAutomator driver over gRPC.
//!
//! The Flutter driver only sees the Flutter semantics tree. Everything else
//! (runtime-permission dialogs, WebViews, notifications, other apps, and apps
//! without `tapwing_driver`) goes through an on-device UiAutomator driver,
//! started as an instrumentation, `adb forward`ed to a host port, and spoken
//! to via `maestro_android.proto`. The driver is Tapwing's own
//! (`drivers/android`, package `dev.tapwing.driver`).
//!
//! * [`apk`]: `adb` access and the hash-cached installer.
//! * [`tapwing_apk`]: finds Tapwing's driver APK.
//! * [`command`]: the `am` commands that start and stop it.
//! * [`client`]: synchronous gRPC facade ([`NativeDriverClient`]).
//! * [`hierarchy`]: UiAutomator XML into the shared selector engine.
//! * [`ops`]: Maestro-style commands (tap, assert, swipe, ...) on top of both.
//!
//! [`NativeLaneHandle`] warms the lane up on a background thread so that
//! pure-Flutter flows never wait for it; the first native command joins it.

pub mod apk;
pub mod client;
pub mod command;
pub mod hierarchy;
pub mod ops;
pub mod tapwing_apk;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

pub use apk::Adb;
pub use client::NativeDriverClient;
pub use ops::{ColdStart, NativeOps};

/// Warning for the deprecated `--native-driver` / `tapwing.nativeDriver` (`origin` names which).
/// `tapwing` is accepted silently; anything else asks for a driver that no longer exists.
/// Tapwing's own driver is always used.
pub fn deprecated_native_driver_warning(origin: &str, value: &str) -> Option<String> {
    if value.trim().eq_ignore_ascii_case("tapwing") {
        return None;
    }
    Some(format!(
        "warning: {origin} `{}` is deprecated and ignored: Tapwing's own native driver is always used \
         (Maestro's native drivers were removed)",
        value.trim()
    ))
}

/// Port the instrumentation's gRPC server listens on, on the device.
pub const DEVICE_PORT: u16 = 7001;
/// Overrides how long to wait for the on-device gRPC server (ms).
pub const STARTUP_TIMEOUT_ENV: &str = "TAPWING_NATIVE_STARTUP_TIMEOUT_MS";
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
/// Initial per-attempt readiness RPC timeout; capped so cancellation during a
/// probe stays fast, and stepped up to [`READINESS_PROBE_MAX_TIMEOUT`] only if
/// a probe actually times out (e.g. slow/cold emulator JVM).
const READINESS_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const READINESS_PROBE_MAX_TIMEOUT: Duration = Duration::from_millis(1000);
/// Delay between readiness probes (cancel-aware via [`CancelSignal`]).
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Cooperative cancellation signal backed by a [`Condvar`] so warm-up backoff
/// sleeps wake immediately when [`NativeLaneHandle`] is dropped.
///
/// Keeps an [`AtomicBool`] alongside the mutex-guarded predicate for lock-free
/// polling and compatibility with [`apk::ensure_installed`] (note: an in-flight
/// first-time `adb install` subprocess is only checked between steps).
struct CancelSignal {
    flag: AtomicBool,
    lock: Mutex<bool>,
    cv: Condvar,
}

impl CancelSignal {
    fn new() -> Self {
        Self { flag: AtomicBool::new(false), lock: Mutex::new(false), cv: Condvar::new() }
    }

    fn flag(&self) -> &AtomicBool {
        &self.flag
    }

    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire) || crate::interrupt::is_interrupted()
    }

    fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        let mut cancelled = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *cancelled = true;
        self.cv.notify_all();
    }

    /// Waits up to `duration`, returning `true` immediately if cancelled (or
    /// interrupted by `SIGINT`/`SIGTERM`) before or during the wait.
    fn wait_timeout(&self, duration: Duration) -> bool {
        if self.is_cancelled() {
            return true;
        }
        let deadline = Instant::now() + duration;
        loop {
            if self.is_cancelled() {
                return true;
            }
            let rem = deadline.saturating_duration_since(Instant::now());
            if rem.is_zero() {
                return self.is_cancelled();
            }
            let guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            if *guard {
                return true;
            }
            let chunk = rem.min(Duration::from_millis(50));
            let (guard, _) =
                self.cv.wait_timeout_while(guard, chunk, |cancelled| !*cancelled).unwrap_or_else(|e| e.into_inner());
            if *guard || self.is_cancelled() {
                return true;
            }
        }
    }
}

/// `--input`: which lane executes UI commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum InputMode {
    /// Flutter driver first; the native lane takes over for system dialogs,
    /// other apps, WebViews and apps without the driver.
    #[default]
    Auto,
    /// In-app Flutter driver only; never falls back to the native lane.
    Flutter,
    /// Everything through the Android UiAutomator driver (works for any app).
    Native,
}

/// The lane that executed a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Flutter,
    Native,
}

impl Lane {
    pub fn name(self) -> &'static str {
        match self {
            Lane::Flutter => "flutter",
            Lane::Native => "native",
        }
    }
}

/// Inputs of the routing decision for one UI command.
#[derive(Clone, Copy, Debug)]
pub struct RouteCtx<'a> {
    pub mode: InputMode,
    /// The app never completed the `tapwing_driver` handshake (uninstrumented).
    pub blackbox: bool,
    /// The flow's `appId`.
    pub flow_app: &'a str,
    /// Package owning the window that has input focus, when known.
    pub focused_package: Option<&'a str>,
}

/// Which lane runs a command. `flutter` and `native` are absolute; `auto`
/// goes native for uninstrumented apps and whenever the focused window is not
/// the app under test (permission controller, package installer, system UI,
/// a browser or any other app).
pub fn decide_lane(ctx: &RouteCtx<'_>) -> Lane {
    match ctx.mode {
        InputMode::Flutter => Lane::Flutter,
        InputMode::Native => Lane::Native,
        InputMode::Auto => {
            if ctx.blackbox || ctx.focused_package.is_some_and(|p| is_external_window(p, ctx.flow_app)) {
                Lane::Native
            } else {
                Lane::Flutter
            }
        }
    }
}

/// Whether a window owned by `package` is not the app under test.
pub fn is_external_window(package: &str, flow_app: &str) -> bool {
    !package.is_empty() && package != flow_app
}

/// Number of consecutive `launchApp` handshake misses before an app is
/// promoted to sticky shard-wide black-box mode.
pub const STICKY_BLACKBOX_THRESHOLD: u32 = 2;

/// Result of recording a missed `launchApp` `tapwing_driver` handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlackboxDemotion {
    /// Below [`STICKY_BLACKBOX_THRESHOLD`]: black-box for the current flow only;
    /// the next `launchApp` will retry the Flutter driver.
    FlowOnly { consecutive_misses: u32 },
    /// Reached [`STICKY_BLACKBOX_THRESHOLD`] consecutive misses: stays black-box
    /// for the rest of the shard.
    StickyShard { consecutive_misses: u32 },
}

/// Shard-scoped consecutive handshake-miss tracker per `app_id`.
///
/// One missed 15 s handshake falls back to black-box for that flow only; two
/// misses in a row promote the app to sticky shard-wide black-box, and any
/// successful Flutter handshake resets the counter.
#[derive(Clone, Debug, Default)]
pub struct BlackboxTracker {
    consecutive_misses: HashMap<String, u32>,
}

impl BlackboxTracker {
    /// Records a missed `launchApp` handshake for `app_id` and returns whether
    /// the fallback is flow-only or sticky for the rest of the shard.
    pub fn record_handshake_miss(&mut self, app_id: &str) -> BlackboxDemotion {
        let count = self.consecutive_misses.entry(app_id.to_string()).or_insert(0);
        *count = count.saturating_add(1);
        if *count >= STICKY_BLACKBOX_THRESHOLD {
            BlackboxDemotion::StickyShard { consecutive_misses: *count }
        } else {
            BlackboxDemotion::FlowOnly { consecutive_misses: *count }
        }
    }

    /// Records a successful Flutter handshake for `app_id`, resetting its
    /// consecutive-miss counter.
    pub fn record_handshake_success(&mut self, app_id: &str) {
        self.consecutive_misses.remove(app_id);
    }

    /// Whether `app_id` has been demoted to sticky shard-wide black-box.
    pub fn is_sticky(&self, app_id: &str) -> bool {
        self.consecutive_misses.get(app_id).copied().unwrap_or(0) >= STICKY_BLACKBOX_THRESHOLD
    }

    /// Number of consecutive missed handshakes recorded for `app_id`.
    pub fn consecutive_misses(&self, app_id: &str) -> u32 {
        self.consecutive_misses.get(app_id).copied().unwrap_or(0)
    }
}

/// Packages of the usual Android system dialogs (runtime permissions,
/// package installer, system UI); used for diagnostics only, since any
/// package other than the flow's app routes natively.
pub const SYSTEM_DIALOG_PACKAGES: &[&str] = &[
    "com.google.android.permissioncontroller",
    "com.android.permissioncontroller",
    "com.android.packageinstaller",
    "com.google.android.packageinstaller",
    "com.android.systemui",
];

/// `dumpsys window` probe for the focused window (one `adb shell` round-trip).
pub const FOCUS_PROBE: &str = "dumpsys window 2>/dev/null | grep -m1 mCurrentFocus";

/// Parses `mCurrentFocus=Window{<hash> u0 <package>/<activity>}` into the
/// owning package. Non-activity windows (`NotificationShade`, `Splash Screen
/// <package>`) map to their name / package; `null` and noise give `None`.
pub fn parse_focused_package(dump: &str) -> Option<String> {
    let line = dump.lines().find(|l| l.contains("mCurrentFocus"))?;
    let inner = line.split_once("Window{")?.1;
    let inner = inner.rsplit_once('}').map_or(inner, |(a, _)| a);
    // `<hash> u0 <title>`; the title is the rest after the user id.
    let title = inner.splitn(3, ' ').nth(2)?.trim();
    let title = title.strip_prefix("Splash Screen ").unwrap_or(title);
    let name = title.split('/').next()?.trim();
    // A title with spaces is not a package (e.g. `Application Error: ...`).
    Some(name.split_whitespace().next()?.to_string()).filter(|s| !s.is_empty())
}

/// Package of the window with input focus on `adb`'s device, if it can be told.
pub fn focused_package(adb: &Adb) -> Option<String> {
    parse_focused_package(&adb.shell(FOCUS_PROBE).ok()?)
}

/// The device-side half of a started lane: stops the instrumentation and
/// removes the port forward, concurrently, to keep teardown to one adb
/// round-trip. Owned by the [`NativeDriverClient`] it serves.
struct SessionGuards {
    instrumentation: Option<InstrumentationGuard>,
    forward: Option<ForwardGuard>,
}

impl Drop for SessionGuards {
    fn drop(&mut self) {
        let (instr, fwd) = (self.instrumentation.take(), self.forward.take());
        std::thread::scope(|s| {
            // If thread creation fails, `spawn_scoped` drops the closure inline,
            // so the guard's `Drop` still runs without panicking.
            if let Some(fwd) = fwd {
                let _ = std::thread::Builder::new().spawn_scoped(s, move || drop(fwd));
            }
            drop(instr);
        });
    }
}

struct ForwardGuard {
    adb: Adb,
    host_port: u16,
}

impl Drop for ForwardGuard {
    fn drop(&mut self) {
        let local = format!("tcp:{}", self.host_port);
        if self.adb.client().forward_remove(&self.adb.serial, &local).is_err() {
            let _ = self.adb.run_cleanup(&["forward", "--remove", &local]);
        }
    }
}

struct InstrumentationGuard {
    adb: Adb,
    child: Child,
    log: PathBuf,
}

impl InstrumentationGuard {
    /// `Some(output)` when the instrumentation process already exited.
    fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let out = fs::read_to_string(&self.log).unwrap_or_default();
                Some(format!("exit status {status}: {}", out.trim()))
            }
            _ => None,
        }
    }
}

/// Allocates an ephemeral host-to-device forward to [`DEVICE_PORT`], preferring
/// [`crate::device::adb_client::AdbClient::forward_ephemeral`] (~0.1 ms
/// loopback TCP). Falls back to the `adb` CLI only when the local ADB daemon is
/// not reachable (mirroring [`Adb::shell`]), so smartsocket `FAIL` or protocol
/// errors never spawn a redundant subprocess or leak a second forward.
fn forward_ephemeral(adb: &Adb) -> Result<u16> {
    let remote = format!("tcp:{DEVICE_PORT}");
    match adb.client().forward_ephemeral(&adb.serial, &remote) {
        Ok(port) => Ok(port),
        Err(_) if !adb.client().is_available() => {
            let out = adb.run(&["forward", "tcp:0", &remote])?;
            let port: u16 = out.trim().parse().with_context(|| format!("unexpected `adb forward` output: {out:?}"))?;
            if port == 0 {
                bail!("unexpected `adb forward` port 0");
            }
            Ok(port)
        }
        Err(e) => Err(e.context("ephemeral adb forward")),
    }
}

impl Drop for InstrumentationGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Killing the host-side `adb shell` does not stop the device process.
        let _ = self.adb.shell(&command::force_stop_command());
        let _ = fs::remove_file(&self.log);
    }
}

fn startup_timeout() -> Duration {
    std::env::var(STARTUP_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
}

/// Polls `probe` until the gRPC driver responds, the instrumentation exits,
/// `cancel` fires, or `timeout` expires. Backoff sleeps use
/// [`CancelSignal::wait_timeout`] so dropping [`NativeLaneHandle`] wakes
/// immediately without waiting out a full poll interval.
///
/// The per-attempt probe budget starts at `initial_probe_cap` and doubles up to
/// `max_probe_cap` only when a probe actually hits its timeout (e.g. cold JVM
/// class loading on a slow emulator), keeping cancellation latency bounded on
/// the common fast-fail (`ECONNREFUSED`) path.
fn wait_for_driver_ready<E, F>(
    cancel: &CancelSignal,
    timeout: Duration,
    initial_probe_cap: Duration,
    max_probe_cap: Duration,
    poll_interval: Duration,
    mut has_exited: E,
    mut probe: F,
) -> Result<()>
where
    E: FnMut() -> Option<String>,
    F: FnMut(Duration) -> Result<()>,
{
    let deadline = Instant::now() + timeout;
    let mut probe_cap = initial_probe_cap;
    loop {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        if let Some(why) = has_exited() {
            bail!("the instrumentation exited during startup ({why})");
        }
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            bail!("the driver did not start in time");
        }
        let probe_timeout = rem.min(probe_cap);
        let probe_started = Instant::now();
        match probe(probe_timeout) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                return Err(e.context("the driver did not start in time"));
            }
            Err(_) => {
                if probe_started.elapsed() >= probe_timeout.saturating_sub(Duration::from_millis(25)) {
                    probe_cap = (probe_cap * 2).min(max_probe_cap);
                }
                let rem = deadline.saturating_duration_since(Instant::now());
                if cancel.wait_timeout(rem.min(poll_interval)) {
                    bail!("cancelled");
                }
            }
        }
    }
}

/// Installs (if needed), forwards, instruments and waits for gRPC readiness.
/// The returned client owns the session's teardown guards.
fn start_session(adb: &Adb, cancel: &CancelSignal) -> Result<NativeDriverClient> {
    let plan = tapwing_apk::locate()?.install_plan();
    let install = apk::ensure_installed(adb, &plan, cancel.flag())?;
    if cancel.is_cancelled() {
        bail!("cancelled");
    }

    // Pre-clean any leftover instrumentation concurrently with establishing the
    // host-side ephemeral port forward so we never connect to a stale server
    // and never pay two serial round-trips.
    let stop = command::force_stop_command();
    let forward = std::thread::scope(|s| {
        if std::thread::Builder::new()
            .spawn_scoped(s, || {
                let _ = adb.shell(&stop);
            })
            .is_err()
        {
            let _ = adb.shell(&stop);
        }
        let host_port = forward_ephemeral(adb)?;
        Ok::<_, anyhow::Error>(ForwardGuard { adb: adb.clone(), host_port })
    })?;
    if cancel.is_cancelled() {
        bail!("cancelled");
    }

    let host_port = forward.host_port;
    // Build the lazy gRPC client before spawning `am instrument` so runtime
    // creation failure never leaks an instrumentation process.
    let mut client = NativeDriverClient::connect(host_port)?;

    let log = std::env::temp_dir().join(format!("tapwing-instrument-{}-{host_port}.log", std::process::id()));
    let log_file = fs::File::create(&log).context("creating the instrumentation log")?;
    let child = adb
        .command(&["shell", &command::instrument_command(install.api_level, DEVICE_PORT)])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file))
        .spawn()
        .context("starting `am instrument`")?;
    let instrumentation = InstrumentationGuard { adb: adb.clone(), child, log };

    // Own the guards immediately so any early cancellation or error drops
    // `instrumentation` and `forward` concurrently via `SessionGuards::drop`.
    let mut guards = SessionGuards { instrumentation: Some(instrumentation), forward: Some(forward) };
    wait_for_driver_ready(
        cancel,
        startup_timeout(),
        READINESS_PROBE_TIMEOUT,
        READINESS_PROBE_MAX_TIMEOUT,
        READINESS_POLL_INTERVAL,
        || guards.instrumentation.as_mut().and_then(InstrumentationGuard::exited),
        |probe_timeout| client.device_info_with_timeout(probe_timeout).map(|_| ()),
    )?;
    client.set_keepalive(Box::new(guards));
    Ok(client)
}

/// Overrides how long a restart keeps trying to bring the driver back (ms).
pub const RESTART_TIMEOUT_ENV: &str = "TAPWING_NATIVE_RESTART_TIMEOUT_MS";
/// After an adb drop the device is often briefly `offline` or missing while the
/// adb server reconnects, so a restart keeps trying for a while.
const DEFAULT_RESTART_TIMEOUT: Duration = Duration::from_secs(30);
const RESTART_RETRY_INTERVAL: Duration = Duration::from_millis(500);

fn restart_timeout() -> Duration {
    std::env::var(RESTART_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_RESTART_TIMEOUT)
}

/// Runs `attempt` until it succeeds, `cancel` fires or `timeout` has passed
/// (an attempt in flight is never interrupted, so the total can overshoot by
/// one attempt). Returns the last error when it gives up.
fn retry_until<T>(
    cancel: &CancelSignal,
    timeout: Duration,
    interval: Duration,
    mut attempt: impl FnMut() -> Result<T>,
) -> Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        let err = match attempt() {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        let rem = deadline.saturating_duration_since(Instant::now());
        if cancel.is_cancelled() || rem.is_zero() {
            return Err(err);
        }
        if cancel.wait_timeout(rem.min(interval)) {
            return Err(err.context("cancelled"));
        }
    }
}

/// Starts a replacement session after the transport died, waiting for the
/// device to come back first if it has to.
fn restart_session(adb: &Adb, cancel: &CancelSignal) -> Result<NativeDriverClient> {
    retry_until(cancel, restart_timeout(), RESTART_RETRY_INTERVAL, || start_session(adb, cancel))
}

enum State {
    Pending(Receiver<Result<NativeDriverClient>>),
    Ready(Box<NativeDriverClient>),
    Unavailable(String),
}

/// Background-warmed native lane. Created at session start; Flutter-only
/// flows never touch it, so they pay nothing for it.
pub struct NativeLaneHandle {
    state: State,
    adb: Option<Adb>,
    cancel: Arc<CancelSignal>,
    thread: Option<JoinHandle<()>>,
    /// Consecutive `launchApp` handshake misses per app in this shard.
    blackbox: BlackboxTracker,
    failure_reported: bool,
}

impl NativeLaneHandle {
    /// Starts warming up the lane for the device `serial` on a new thread.
    pub fn spawn(serial: &str) -> Self {
        let adb = Adb::new(serial);
        let starter_adb = adb.clone();
        Self::spawn_with(adb, move |cancel| {
            let mut client = start_session(&starter_adb, cancel).context("starting the Tapwing Android driver")?;
            let (restart_adb, restart_cancel) = (starter_adb.clone(), cancel.clone());
            client.set_restart(Box::new(move || restart_session(&restart_adb, &restart_cancel)));
            Ok(client)
        })
    }

    fn spawn_with<F>(adb: Adb, starter: F) -> Self
    where
        F: FnOnce(&Arc<CancelSignal>) -> Result<NativeDriverClient> + Send + 'static,
    {
        let cancel = Arc::new(CancelSignal::new());
        let (tx, rx) = mpsc::channel();
        let thread = {
            let cancel = cancel.clone();
            std::thread::Builder::new().name("tapwing-native-warmup".into()).spawn(move || {
                // A failed send means the handle is gone: the session drops here and cleans up.
                let _ = tx.send(starter(&cancel));
            })
        };
        match thread {
            Ok(thread) => Self {
                state: State::Pending(rx),
                adb: Some(adb),
                cancel,
                thread: Some(thread),
                blackbox: BlackboxTracker::default(),
                failure_reported: false,
            },
            Err(e) => Self::unavailable(format!("cannot start the warm-up thread: {e}")),
        }
    }

    /// A lane that is known not to work (no driver APK, iOS, ...).
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: State::Unavailable(reason.into()),
            adb: None,
            cancel: Arc::new(CancelSignal::new()),
            thread: None,
            blackbox: BlackboxTracker::default(),
            failure_reported: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn ready_for_test(client: NativeDriverClient, adb: Adb) -> Self {
        Self {
            state: State::Ready(Box::new(client)),
            adb: Some(adb),
            cancel: Arc::new(CancelSignal::new()),
            thread: None,
            blackbox: BlackboxTracker::default(),
            failure_reported: false,
        }
    }

    /// Records a missed `launchApp` handshake for `app_id`.
    pub fn record_handshake_miss(&mut self, app_id: &str) -> BlackboxDemotion {
        self.blackbox.record_handshake_miss(app_id)
    }

    /// Records a successful Flutter handshake for `app_id`, resetting misses.
    pub fn record_handshake_success(&mut self, app_id: &str) {
        self.blackbox.record_handshake_success(app_id);
    }

    /// Whether `app_id` is permanently demoted to black-box for this shard.
    pub fn is_sticky_blackbox(&self, app_id: &str) -> bool {
        self.blackbox.is_sticky(app_id)
    }

    /// The warm-up failure reason, once (for a single user-visible warning).
    pub fn take_failure_notice(&mut self) -> Option<String> {
        self.poll(None);
        if self.failure_reported {
            return None;
        }
        let why = self.failure()?.to_string();
        self.failure_reported = true;
        Some(why)
    }

    /// `false` once warm-up is known to have failed.
    pub fn is_usable(&self) -> bool {
        !matches!(self.state, State::Unavailable(_))
    }

    /// Why the lane is unusable, if it is.
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            State::Unavailable(why) => Some(why),
            _ => None,
        }
    }

    pub fn adb(&self) -> Option<&Adb> {
        self.adb.as_ref()
    }

    /// One line per time the driver connection was lost and restarted since the last call.
    pub fn take_restart_notices(&mut self) -> Vec<String> {
        match &mut self.state {
            State::Ready(c) => c.take_restart_notices(),
            _ => Vec::new(),
        }
    }

    fn finish(&mut self, res: Result<NativeDriverClient>) {
        self.state = match res {
            Ok(s) => State::Ready(Box::new(s)),
            Err(e) => State::Unavailable(format!("{e:#}")),
        };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Joins the warm-up for at most `timeout` (`None` = poll once) and
    /// returns the client when ready.
    fn poll(&mut self, timeout: Option<Duration>) -> Option<&mut NativeDriverClient> {
        if let State::Pending(rx) = &self.state {
            let res = match timeout {
                Some(t) => {
                    let deadline = Instant::now() + t;
                    loop {
                        if crate::interrupt::is_interrupted() {
                            self.cancel.cancel();
                            break Err(RecvTimeoutError::Timeout);
                        }
                        let rem = deadline.saturating_duration_since(Instant::now());
                        match rx.recv_timeout(rem.min(Duration::from_millis(50))) {
                            Ok(r) => break Ok(r),
                            Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                            Err(e) => break Err(e),
                        }
                    }
                }
                None => rx.try_recv().map_err(|e| match e {
                    TryRecvError::Empty => RecvTimeoutError::Timeout,
                    TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                }),
            };
            match res {
                Ok(r) => self.finish(r),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.finish(Err(anyhow!("the native-lane warm-up thread died")));
                }
            }
        }
        match &mut self.state {
            State::Ready(c) => Some(c),
            _ => None,
        }
    }

    /// Non-blocking: the client only if warm-up already finished.
    pub fn try_get(&mut self) -> Option<&mut NativeDriverClient> {
        self.poll(None)
    }

    /// Waits up to `timeout` for the warm-up and returns the client.
    pub fn get_or_wait(&mut self, timeout: Duration) -> Result<&mut NativeDriverClient> {
        crate::interrupt::check()?;
        self.poll(Some(timeout));
        crate::interrupt::check()?;
        match &mut self.state {
            State::Ready(c) => Ok(c),
            State::Unavailable(why) => Err(anyhow!("the native lane is unavailable: {why}")),
            State::Pending(_) => Err(anyhow!("the native lane is still starting after {} ms", timeout.as_millis())),
        }
    }
}

impl Drop for NativeLaneHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(mode: InputMode, blackbox: bool, focused: Option<&'a str>) -> RouteCtx<'a> {
        RouteCtx { mode, blackbox, flow_app: "com.example.app", focused_package: focused }
    }

    #[test]
    fn auto_routes_by_focused_window_and_instrumentation() {
        // In-app window: Flutter fast path.
        assert_eq!(decide_lane(&ctx(InputMode::Auto, false, Some("com.example.app"))), Lane::Flutter);
        // Unknown focus must not slow down or divert Flutter flows.
        assert_eq!(decide_lane(&ctx(InputMode::Auto, false, None)), Lane::Flutter);
        // System dialogs, other apps, browsers, the shade: native.
        for pkg in SYSTEM_DIALOG_PACKAGES.iter().chain(&["com.android.chrome", "NotificationShade", "com.other"]) {
            assert_eq!(decide_lane(&ctx(InputMode::Auto, false, Some(pkg))), Lane::Native, "{pkg}");
        }
        // Uninstrumented APK: everything native, whatever has focus.
        assert_eq!(decide_lane(&ctx(InputMode::Auto, true, Some("com.example.app"))), Lane::Native);
        assert_eq!(decide_lane(&ctx(InputMode::Auto, true, None)), Lane::Native);
    }

    #[test]
    fn flutter_and_native_modes_are_absolute() {
        for focused in [None, Some("com.example.app"), Some("com.google.android.permissioncontroller")] {
            for blackbox in [false, true] {
                assert_eq!(decide_lane(&ctx(InputMode::Flutter, blackbox, focused)), Lane::Flutter);
                assert_eq!(decide_lane(&ctx(InputMode::Native, blackbox, focused)), Lane::Native);
            }
        }
    }

    #[test]
    fn external_window_is_anything_but_the_flow_app() {
        assert!(!is_external_window("com.example.app", "com.example.app"));
        assert!(is_external_window("com.example.app.debug", "com.example.app"));
        assert!(is_external_window("com.android.systemui", "com.example.app"));
        assert!(!is_external_window("", "com.example.app"));
    }

    #[test]
    fn parses_focused_window_dumps() {
        let p = |s: &str| parse_focused_package(s);
        assert_eq!(
            p("  mCurrentFocus=Window{4cb6b2c u0 com.google.android.permissioncontroller/com.android.permissioncontroller.permission.ui.GrantPermissionsActivity}").as_deref(),
            Some("com.google.android.permissioncontroller")
        );
        assert_eq!(
            p("mCurrentFocus=Window{1f2e3d u0 com.example.app/com.example.app.MainActivity}").as_deref(),
            Some("com.example.app")
        );
        assert_eq!(p("  mCurrentFocus=Window{9ab u0 NotificationShade}").as_deref(), Some("NotificationShade"));
        assert_eq!(p("mCurrentFocus=Window{5d0 u0 Splash Screen com.example.app}").as_deref(), Some("com.example.app"));
        assert_eq!(p("mCurrentFocus=Window{7a u10 com.example.app/.Main}").as_deref(), Some("com.example.app"));
        assert_eq!(p("  mCurrentFocus=null"), None);
        assert_eq!(p("something else"), None);
        assert_eq!(p(""), None);
    }

    #[test]
    fn unavailable_handle_reports_why() {
        let mut h = NativeLaneHandle::unavailable("no driver");
        assert!(!h.is_usable());
        assert_eq!(h.failure(), Some("no driver"));
        assert!(h.try_get().is_none());
        let err = h.get_or_wait(Duration::from_millis(1)).err().unwrap();
        assert!(err.to_string().contains("no driver"));
    }

    #[test]
    fn forward_ephemeral_uses_adb_client_and_does_not_fall_back_on_server_fail() {
        use crate::device::adb_client::AdbClient;
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut len_hex = [0u8; 4];
                stream.read_exact(&mut len_hex).unwrap();
                let len = usize::from_str_radix(std::str::from_utf8(&len_hex).unwrap(), 16).unwrap();
                let mut cmd = vec![0u8; len];
                stream.read_exact(&mut cmd).unwrap();
                assert_eq!(
                    std::str::from_utf8(&cmd).unwrap(),
                    format!("host-serial:tapwing-test-serial:forward:tcp:0;tcp:{DEVICE_PORT}")
                );
                if idx == 0 {
                    stream.write_all(b"OKAYOKAY000548291").unwrap();
                } else {
                    let msg = "device offline";
                    let frame = format!("FAIL{:04x}{msg}", msg.len());
                    stream.write_all(frame.as_bytes()).unwrap();
                }
                stream.flush().unwrap();
            }
            // Keep listener open briefly for the `is_available()` probe on the FAIL path.
            let _ = listener.accept();
        });

        let adb = Adb::with_client("tapwing-test-serial", AdbClient::with_addr(addr));
        assert_eq!(forward_ephemeral(&adb).unwrap(), 48291);

        let err = format!("{:#}", forward_ephemeral(&adb).unwrap_err());
        assert!(err.contains("device offline"), "expected smartsocket FAIL without CLI fallback, got: {err}");
        server.join().unwrap();
    }

    #[test]
    fn cancel_signal_wakes_wait_timeout_immediately() {
        let cancel = Arc::new(CancelSignal::new());
        let waiter = cancel.clone();
        let (entered_tx, entered_rx) = mpsc::channel();

        let thread = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            let start = Instant::now();
            let was_cancelled = waiter.wait_timeout(Duration::from_secs(10));
            (was_cancelled, start.elapsed())
        });

        entered_rx.recv().unwrap();
        // Give the waiter a moment to enter the condvar wait, then cancel.
        std::thread::sleep(Duration::from_millis(15));
        cancel.cancel();

        let (was_cancelled, elapsed) = thread.join().unwrap();
        assert!(was_cancelled);
        assert!(elapsed < Duration::from_secs(1), "cancel should wake wait_timeout immediately, took {elapsed:?}");
        assert!(cancel.wait_timeout(Duration::from_secs(10)), "subsequent waits return immediately once cancelled");
    }

    #[test]
    fn wait_for_driver_ready_cancels_immediately_during_backoff_sleep() {
        let cancel = Arc::new(CancelSignal::new());
        let waiter = cancel.clone();
        let (probed_tx, probed_rx) = mpsc::channel();

        let thread = std::thread::spawn(move || {
            wait_for_driver_ready(
                &waiter,
                Duration::from_secs(10),
                READINESS_PROBE_TIMEOUT,
                READINESS_PROBE_MAX_TIMEOUT,
                Duration::from_secs(5),
                || None,
                |_| {
                    let _ = probed_tx.send(());
                    Err(anyhow!("not listening yet"))
                },
            )
        });

        probed_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(15));
        let t = Instant::now();
        cancel.cancel();

        let err = thread.join().unwrap().unwrap_err().to_string();
        assert!(err.contains("cancelled"), "{err}");
        assert!(t.elapsed() < Duration::from_millis(500), "cancellation stalled for {:?}", t.elapsed());
    }

    #[test]
    fn wait_for_driver_ready_caps_probe_timeout_steps_up_on_timeout_and_detects_exit() {
        let cancel = CancelSignal::new();
        let mut max_timeout = Duration::ZERO;
        let mut attempts = 0usize;
        wait_for_driver_ready(
            &cancel,
            Duration::from_secs(5),
            READINESS_PROBE_TIMEOUT,
            READINESS_PROBE_MAX_TIMEOUT,
            Duration::from_millis(5),
            || None,
            |probe_timeout| {
                attempts += 1;
                max_timeout = max_timeout.max(probe_timeout);
                if attempts < 2 { Err(anyhow!("connection refused")) } else { Ok(()) }
            },
        )
        .unwrap();
        assert_eq!(attempts, 2);
        assert!(max_timeout <= READINESS_PROBE_TIMEOUT);

        // Simulated slow probe that exhausts its cap should step up from 30 ms to 60 ms.
        let mut timeouts = Vec::new();
        wait_for_driver_ready(
            &cancel,
            Duration::from_secs(5),
            Duration::from_millis(30),
            Duration::from_millis(120),
            Duration::from_millis(5),
            || None,
            |probe_timeout| {
                timeouts.push(probe_timeout);
                if timeouts.len() == 1 {
                    std::thread::sleep(probe_timeout);
                    Err(anyhow!("timed out"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!(timeouts, vec![Duration::from_millis(30), Duration::from_millis(60)]);

        let err = wait_for_driver_ready(
            &cancel,
            Duration::from_secs(5),
            READINESS_PROBE_TIMEOUT,
            READINESS_PROBE_MAX_TIMEOUT,
            Duration::from_millis(5),
            || Some("exit status 1: crash".into()),
            |_| Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("the instrumentation exited during startup") && err.contains("crash"), "{err}");
    }

    #[test]
    fn wait_for_driver_ready_expires_at_deadline() {
        let cancel = CancelSignal::new();
        let t = Instant::now();
        let err = format!(
            "{:#}",
            wait_for_driver_ready(
                &cancel,
                Duration::from_millis(30),
                READINESS_PROBE_TIMEOUT,
                READINESS_PROBE_MAX_TIMEOUT,
                Duration::from_millis(10),
                || None,
                |_| Err(anyhow!("still starting")),
            )
            .unwrap_err()
        );
        assert!(err.contains("the driver did not start in time"), "{err}");
        assert!(t.elapsed() < Duration::from_millis(500), "deadline expiry stalled for {:?}", t.elapsed());
    }

    #[test]
    fn dropping_pending_handle_wakes_and_joins_immediately() {
        let saw_cancel = Arc::new(AtomicBool::new(false));
        let seen = saw_cancel.clone();
        let (entered_tx, entered_rx) = mpsc::channel();

        let handle = NativeLaneHandle::spawn_with(Adb::new("tapwing-test-serial"), move |cancel| {
            entered_tx.send(()).unwrap();
            let cancelled = cancel.wait_timeout(Duration::from_secs(10));
            seen.store(cancelled, Ordering::SeqCst);
            bail!("cancelled");
        });

        entered_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(15));
        let t = Instant::now();
        drop(handle);
        assert!(t.elapsed() < Duration::from_millis(500), "Drop stalled for {:?}", t.elapsed());
        assert!(saw_cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn native_driver_option_is_deprecated_except_for_tapwing() {
        assert_eq!(deprecated_native_driver_warning("--native-driver", "tapwing"), None);
        assert_eq!(deprecated_native_driver_warning("--native-driver", "Tapwing"), None);
        let w = deprecated_native_driver_warning("--native-driver", "maestro").unwrap();
        assert!(w.starts_with("warning: --native-driver `maestro` is deprecated and ignored"), "{w}");
        let w = deprecated_native_driver_warning("config.yaml tapwing.nativeDriver", "espresso").unwrap();
        assert!(w.contains("tapwing.nativeDriver `espresso`"), "{w}");
    }

    #[test]
    fn retry_until_retries_until_success_and_gives_up_with_the_last_error() {
        let cancel = CancelSignal::new();
        let mut calls = 0;
        let v = retry_until(&cancel, Duration::from_secs(5), Duration::from_millis(1), || {
            calls += 1;
            if calls < 3 { Err(anyhow!("device offline")) } else { Ok(calls) }
        })
        .unwrap();
        assert_eq!(v, 3);

        let mut calls = 0;
        let t = Instant::now();
        let err = retry_until::<()>(&cancel, Duration::from_millis(40), Duration::from_millis(5), || {
            calls += 1;
            Err(anyhow!("device offline #{calls}"))
        })
        .unwrap_err();
        assert!(calls > 1, "retried before giving up");
        assert!(err.to_string().contains(&format!("device offline #{calls}")), "last error wins: {err}");
        assert!(t.elapsed() < Duration::from_secs(2), "stalled for {:?}", t.elapsed());
    }

    #[test]
    fn retry_until_stops_at_once_when_cancelled() {
        let cancel = Arc::new(CancelSignal::new());
        let canceller = cancel.clone();
        let t = Instant::now();
        let err = retry_until::<()>(&cancel, Duration::from_secs(30), Duration::from_secs(10), || {
            canceller.cancel();
            Err(anyhow!("device offline"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("device offline"), "{err}");
        assert!(t.elapsed() < Duration::from_millis(500), "cancellation stalled for {:?}", t.elapsed());
    }

    #[test]
    fn blackbox_tracker_promotes_only_after_two_consecutive_misses() {
        let mut tracker = BlackboxTracker::default();
        assert!(!tracker.is_sticky("com.example.app"));
        assert_eq!(tracker.consecutive_misses("com.example.app"), 0);

        // First miss: flow-only fallback, not sticky.
        assert_eq!(
            tracker.record_handshake_miss("com.example.app"),
            BlackboxDemotion::FlowOnly { consecutive_misses: 1 }
        );
        assert!(!tracker.is_sticky("com.example.app"));
        assert_eq!(tracker.consecutive_misses("com.example.app"), 1);

        // A successful handshake resets the consecutive counter.
        tracker.record_handshake_success("com.example.app");
        assert_eq!(tracker.consecutive_misses("com.example.app"), 0);
        assert!(!tracker.is_sticky("com.example.app"));

        // Another single miss is still flow-only.
        assert_eq!(
            tracker.record_handshake_miss("com.example.app"),
            BlackboxDemotion::FlowOnly { consecutive_misses: 1 }
        );
        assert!(!tracker.is_sticky("com.example.app"));

        // Different app is tracked independently.
        assert_eq!(
            tracker.record_handshake_miss("com.other.app"),
            BlackboxDemotion::FlowOnly { consecutive_misses: 1 }
        );
        assert!(!tracker.is_sticky("com.other.app"));

        // Second consecutive miss for `com.example.app` -> sticky shard-wide.
        assert_eq!(
            tracker.record_handshake_miss("com.example.app"),
            BlackboxDemotion::StickyShard { consecutive_misses: 2 }
        );
        assert!(tracker.is_sticky("com.example.app"));
        assert!(!tracker.is_sticky("com.other.app"));
    }
}
