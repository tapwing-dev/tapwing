//! iOS Simulator native lane (F5b, F11b): Tapwing's XCUITest runner over HTTP.
//!
//! The Flutter driver only sees the Flutter semantics tree. SpringBoard
//! permission alerts, other apps, `Home` / `Lock` and device rotation go
//! through Tapwing's own XCUITest runner (`drivers/ios`,
//! `dev.tapwing.ios-runnerUITests.xctrunner`): it is started with
//! `xcodebuild test-without-building` from its prebuilt products, listens on a
//! loopback port (the simulator shares the host network) and speaks a small
//! JSON-over-HTTP/1.1 API.
//!
//! * [`client`]: synchronous HTTP facade ([`IosNativeClient`]), one persistent connection.
//! * [`hierarchy`]: `AXElement` JSON into the shared selector engine.
//! * [`tapwing_driver`]: finds the runner (env override, release bundle, checkout build).
//! * [`ops`]: Maestro-style commands (tap, assert, swipe, ...) on top of the client.
//!
//! The release bundle is unpacked once into `~/.tapwing/ios-native-driver/<key>/`
//! (an atomic marker makes warm runs free). [`IosNativeLaneHandle`] starts the
//! runner on a background thread so pure-Flutter flows never wait for it; the
//! first native command joins it, and dropping the handle cancels startup
//! within one poll. A runner that dies mid-run is restarted by the client (see
//! [`client`]); the restart starts the same way and honours the same cancel flag.

pub mod client;
pub mod hierarchy;
pub mod ops;
pub mod tapwing_driver;

use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

pub use client::{IosDeviceInfo, IosNativeClient, MAX_RESTARTS, RestartFn, RunnerProcess, is_runner_answer};
pub use hierarchy::IosHierarchy;
pub use ops::IosNativeOps;

/// Fixes the host port of the runner (default: a free ephemeral port).
pub const PORT_ENV: &str = "TAPWING_IOS_NATIVE_PORT";
/// Overrides how long to wait for the runner to answer `/status` (ms).
pub const STARTUP_TIMEOUT_ENV: &str = "TAPWING_IOS_NATIVE_STARTUP_TIMEOUT_MS";
/// Forwarded as `TEST_RUNNER_snapshotKeyHonorModalViews` when set (`true` / `false`).
pub const HONOR_MODAL_VIEWS_ENV: &str = "TAPWING_IOS_NATIVE_HONOR_MODAL_VIEWS";
/// Bundle id of SpringBoard (permission alerts live there).
pub const SPRINGBOARD_BUNDLE_ID: &str = "com.apple.springboard";

pub(crate) const PRODUCTS_DIR: &str = "Debug-iphonesimulator";
/// Written last into a fresh extraction, so a partial one is never reused.
const COMPLETE_MARKER: &str = ".tapwing-complete";

/// File names of an iOS Simulator XCUITest runner bundle.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunnerLayout {
    pub xctestrun: &'static str,
    pub runner_zip: &'static str,
    pub app_zip: &'static str,
    pub runner_app: &'static str,
}

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
/// One `/status` attempt; short, so cancellation never waits long on a probe.
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
const POLL_SLEEP: Duration = Duration::from_millis(40);

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/.tapwing/ios-native-driver`.
pub(crate) fn cache_root() -> Result<PathBuf> {
    Ok(home_dir().context("$HOME is not set")?.join(".tapwing").join("ios-native-driver"))
}

pub(crate) fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks that `dir` is a complete runner layout and returns its xctestrun.
pub(crate) fn validate_driver_dir_layout(dir: &Path, layout: RunnerLayout) -> Result<PathBuf> {
    let xctestrun = dir.join(layout.xctestrun);
    if !xctestrun.is_file() {
        bail!("{} has no {}", dir.display(), layout.xctestrun);
    }
    if !dir.join(PRODUCTS_DIR).join(layout.runner_app).is_dir() {
        bail!("{} has no {PRODUCTS_DIR}/{}", dir.display(), layout.runner_app);
    }
    Ok(xctestrun)
}

/// Verifies that a symlink at relative archive path `rel_entry` pointing to
/// `target` cannot escape the extraction root directory.
#[cfg(unix)]
fn is_safe_symlink_target(rel_entry: &Path, target: &Path) -> bool {
    use std::path::Component;
    if target.is_absolute() || target.as_os_str().is_empty() {
        return false;
    }
    let mut depth: isize = rel_entry.parent().map_or(0, |p| p.components().count() as isize);
    for comp in target.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Unpacks an inner `.zip` of build products into `dir`, keeping Unix modes
/// (the Mach-O binaries inside `.app` bundles must stay executable).
fn unzip_products(bytes: &[u8], dir: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("the driver archive is not a valid zip")?;
    for i in 0..zip.len() {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let mut entry = zip.by_index(i)?;
        let rel = entry.enclosed_name().ok_or_else(|| {
            anyhow!("unsafe path in the driver archive: {}", String::from_utf8_lossy(entry.name_raw()))
        })?;
        let out = dir.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        if entry.is_symlink() {
            let mut target = String::new();
            entry.read_to_string(&mut target)?;
            let target = target.trim();
            if !is_safe_symlink_target(&rel, Path::new(target)) {
                bail!("unsafe symlink target `{target}` for `{}` in driver archive", rel.display());
            }
            std::os::unix::fs::symlink(target, &out)?;
            continue;
        }
        let mut file = fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
        std::io::copy(&mut entry, &mut file)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

pub(crate) fn is_complete_layout(dir: &Path, layout: RunnerLayout) -> bool {
    dir.join(COMPLETE_MARKER).is_file() && validate_driver_dir_layout(dir, layout).is_ok()
}

static EXTRACT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Unpacks a runner whose three files (`xctestrun`, `runner_zip`, `app_zip`)
/// `read` returns by name. The work happens in a sibling temp directory that
/// is renamed into place after the completion marker is written, so `dest` is
/// either absent or complete.
pub(crate) fn extract_driver_with(
    dest: &Path,
    layout: RunnerLayout,
    cancel: &AtomicBool,
    read: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    let parent = dest.parent().context("extraction directory has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let seq = EXTRACT_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        "{}.tmp{}-{nanos}-{seq}",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("driver"),
        std::process::id()
    ));
    let result = extract_into(read, layout, &tmp, cancel).and_then(|()| install_extraction(&tmp, dest, layout));
    if tmp.exists() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

fn extract_into(
    mut read: impl FnMut(&str) -> Result<Vec<u8>>,
    layout: RunnerLayout,
    tmp: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let products = tmp.join(PRODUCTS_DIR);
    fs::create_dir_all(&products)?;
    fs::write(tmp.join(layout.xctestrun), read(layout.xctestrun)?)?;
    for name in [layout.runner_zip, layout.app_zip] {
        unzip_products(&read(name)?, &products, cancel).with_context(|| format!("unpacking {name}"))?;
    }
    validate_driver_dir_layout(tmp, layout).context("the unpacked driver is incomplete")?;
    fs::write(tmp.join(COMPLETE_MARKER), env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

/// Moves a finished extraction into place; a concurrent winner is kept.
fn install_extraction(tmp: &Path, dest: &Path, layout: RunnerLayout) -> Result<()> {
    if dest.exists() {
        if is_complete_layout(dest, layout) {
            return Ok(());
        }
        // A leftover of an older, interrupted layout.
        fs::remove_dir_all(dest).with_context(|| format!("removing stale {}", dest.display()))?;
    }
    match fs::rename(tmp, dest) {
        Ok(()) => Ok(()),
        Err(_) if is_complete_layout(dest, layout) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("installing the driver into {}", dest.display())),
    }
}

/// Arguments of `xcodebuild test-without-building` (as Maestro's
/// `XCRunnerCLIUtils.runXcTestWithoutBuild` runs it), plus `-destination-timeout 1`
/// so a not-booted simulator fails fast.
pub fn xcodebuild_args(xctestrun: &Path, udid: &str, derived_data: &Path) -> Vec<String> {
    [
        "test-without-building",
        "-xctestrun",
        &xctestrun.to_string_lossy(),
        "-destination",
        &format!("id={udid}"),
        "-destination-timeout",
        "1",
        "-derivedDataPath",
        &derived_data.to_string_lossy(),
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

fn startup_timeout() -> Duration {
    std::env::var(STARTUP_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
}

/// Process-wide set of loopback ports reserved by live iOS native sessions so
/// concurrent `--shard-split N` runs in one process never collide before
/// `xcodebuild` binds its listener.
static RESERVED_PORTS: OnceLock<Arc<Mutex<HashSet<u16>>>> = OnceLock::new();
/// Monotonic counter keeping per-session scratch directories distinct even if a
/// port is reused after an earlier session ends.
static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

const MAX_PORT_PICK_ATTEMPTS: usize = 32;

fn reserved_ports() -> &'static Arc<Mutex<HashSet<u16>>> {
    RESERVED_PORTS.get_or_init(|| Arc::new(Mutex::new(HashSet::new())))
}

/// RAII reservation of a host loopback port in a shared port pool; releases
/// the port back to the pool when dropped.
struct PortReservation {
    port: u16,
    pool: Arc<Mutex<HashSet<u16>>>,
}

impl PortReservation {
    fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for PortReservation {
    fn drop(&mut self) {
        let mut guard = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&self.port);
    }
}

/// Reserves a single explicit port in `pool`, failing if another active shard
/// in this process already holds it.
fn reserve_explicit_port(pool: &Arc<Mutex<HashSet<u16>>>, port: u16) -> Result<PortReservation> {
    let mut guard = pool.lock().unwrap_or_else(|e| e.into_inner());
    if !guard.insert(port) {
        bail!("port {port} (from ${PORT_ENV}) is already reserved by another active iOS native session");
    }
    Ok(PortReservation { port, pool: Arc::clone(pool) })
}

/// Picks an ephemeral loopback port not already held in `pool`, keeping any
/// colliding probe listeners alive across retries so the OS does not hand back
/// the same port twice.
fn reserve_ephemeral_port<L>(
    pool: &Arc<Mutex<HashSet<u16>>>,
    mut bind_ephemeral: impl FnMut() -> Result<(u16, L)>,
) -> Result<PortReservation> {
    let mut held = Vec::new();
    for _ in 0..MAX_PORT_PICK_ATTEMPTS {
        let (port, listener) = bind_ephemeral()?;
        let inserted = {
            let mut guard = pool.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(port)
        };
        if inserted {
            return Ok(PortReservation { port, pool: Arc::clone(pool) });
        }
        held.push(listener);
    }
    bail!("could not reserve a free ephemeral port for the iOS driver after {MAX_PORT_PICK_ATTEMPTS} attempts")
}

/// Reserves `$TAPWING_IOS_NATIVE_PORT`, else a free ephemeral loopback port.
fn reserve_port() -> Result<PortReservation> {
    let pool = reserved_ports();
    if let Ok(raw) = std::env::var(PORT_ENV) {
        let port: u16 = raw.trim().parse().with_context(|| format!("${PORT_ENV} is not a port: {raw:?}"))?;
        return reserve_explicit_port(pool, port);
    }
    reserve_ephemeral_port(pool, || {
        let listener = TcpListener::bind("127.0.0.1:0").context("finding a free port for the iOS driver")?;
        let port = listener.local_addr()?.port();
        Ok((port, listener))
    })
}

fn session_dir_name(pid: u32, seq: u64, port: u16) -> String {
    format!("tapwing-ios-native-{pid}-{seq}-{port}")
}

/// The last lines of `xcodebuild`'s output, for failure messages.
fn log_tail(log: &Path) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(12)..].join("\n");
    match tail.char_indices().rev().nth(1500) {
        Some((i, _)) => tail[i..].to_string(),
        None => tail,
    }
}

/// The running `xcodebuild test-without-building` plus its scratch directory.
/// Dropping it kills `xcodebuild`, stops the runner app on the simulator, and
/// releases the reserved host port.
struct RunnerGuard {
    udid: String,
    runner_bundle_id: &'static str,
    child: Child,
    dir: PathBuf,
    log: PathBuf,
    // Dropped after `Drop::drop` finishes killing `xcodebuild` and terminating
    // the simulator runner, so the port is only released once the runner stops.
    _port: PortReservation,
}

impl RunnerGuard {
    /// `Some(description)` once `xcodebuild` has exited.
    fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(format!("xcodebuild exited with {status}:\n{}", log_tail(&self.log))),
            _ => None,
        }
    }
}

impl RunnerProcess for RunnerGuard {
    fn exited(&mut self) -> Option<String> {
        RunnerGuard::exited(self)
    }
}

impl Drop for RunnerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Killing the host-side `xcodebuild` does not stop the runner app
        // inside the simulator; terminate it so the port is freed.
        let _ = Command::new("xcrun")
            .args(["simctl", "terminate", &self.udid, self.runner_bundle_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Polls `GET /status` until the runner answers. Checks `cancel` and `exited`
/// on every iteration (never sleeping longer than [`POLL_SLEEP`]), so
/// cancellation is observed within one probe.
fn wait_for_runner(
    client: &IosNativeClient,
    driver_label: &str,
    mut exited: impl FnMut() -> Option<String>,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<()> {
    loop {
        if cancel.load(Ordering::Relaxed) || crate::interrupt::is_interrupted() {
            bail!("cancelled");
        }
        if let Some(why) = exited() {
            bail!("{driver_label} exited during startup ({why})");
        }
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            bail!("{driver_label} did not start in time");
        }
        match client.status(rem.min(PROBE_TIMEOUT)) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                return Err(e.context(format!("{driver_label} did not start in time")));
            }
            Err(_) => std::thread::sleep(POLL_SLEEP.min(deadline.saturating_duration_since(Instant::now()))),
        }
    }
}

/// Unpacks (if needed), launches the runner and waits for HTTP readiness. The
/// returned client owns the runner process (dropping it stops the runner).
fn start_session(udid: &str, cancel: &AtomicBool) -> Result<IosNativeClient> {
    let xctestrun = tapwing_driver::locate(cancel)?.xctestrun;
    let driver_label = "the Tapwing iOS driver";
    if cancel.load(Ordering::Relaxed) || crate::interrupt::is_interrupted() {
        bail!("cancelled");
    }
    let port_guard = reserve_port()?;
    let port = port_guard.port();
    let seq = SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(session_dir_name(std::process::id(), seq, port));
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let log = dir.join("xcodebuild.log");
    // Output goes to a file: a pipe nobody drains would block `xcodebuild`.
    let log_file = fs::File::create(&log).context("creating the xcodebuild log")?;
    let mut cmd = Command::new("xcodebuild");
    cmd.args(xcodebuild_args(&xctestrun, udid, &dir.join("derived")))
        .env("TEST_RUNNER_PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));
    if let Ok(honor) = std::env::var(HONOR_MODAL_VIEWS_ENV) {
        cmd.env("TEST_RUNNER_snapshotKeyHonorModalViews", honor.trim());
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            return Err(anyhow!(e).context("starting `xcodebuild` (is Xcode installed and selected?)"));
        }
    };
    let mut guard = RunnerGuard {
        udid: udid.to_string(),
        runner_bundle_id: tapwing_driver::RUNNER_BUNDLE_ID,
        child,
        dir,
        log,
        _port: port_guard,
    };

    let mut client = IosNativeClient::connect(port);
    wait_for_runner(&client, driver_label, || guard.exited(), cancel, Instant::now() + startup_timeout())?;
    client.set_runner(Box::new(guard));
    Ok(client)
}

/// Starts one runner session; the warm-up and every restart call it.
type Starter = dyn Fn(&AtomicBool) -> Result<IosNativeClient> + Send + Sync;

enum State {
    Pending(Receiver<Result<IosNativeClient>>),
    Ready(Box<IosNativeClient>),
    Unavailable(String),
}

/// Background-warmed iOS native lane. Created at session start; Flutter-only
/// flows never touch it, so they pay nothing for it.
pub struct IosNativeLaneHandle {
    state: State,
    udid: Option<String>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Apps that never completed the `tapwing_driver` handshake in this run.
    blackbox_apps: HashSet<String>,
    failure_reported: bool,
}

impl IosNativeLaneHandle {
    /// Starts warming up the lane for the simulator `udid` on a new thread.
    pub fn spawn(udid: &str) -> Self {
        let owned = udid.to_string();
        Self::spawn_with(udid, move |cancel| start_session(&owned, cancel))
    }

    /// Warms up with `start` on a new thread. The client it returns restarts a
    /// dead runner with the same `start` and the handle's cancel flag. A restart runs on
    /// the caller's thread while the handle is borrowed, so only the warm-up thread is
    /// cancelled by dropping the handle, never a restart in flight.
    fn spawn_with(udid: &str, start: impl Fn(&AtomicBool) -> Result<IosNativeClient> + Send + Sync + 'static) -> Self {
        let start: Arc<Starter> = Arc::new(start);
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let thread = {
            let cancel = cancel.clone();
            std::thread::Builder::new().name("tapwing-ios-native-warmup".into()).spawn(move || {
                let res = start(&cancel).map(|mut client| {
                    let (start, cancel) = (start.clone(), cancel.clone());
                    client.set_restart(Box::new(move || {
                        if cancel.load(Ordering::Relaxed) {
                            bail!("cancelled");
                        }
                        start(&cancel).context("restarting the Tapwing iOS driver")
                    }));
                    client
                });
                // A failed send means the handle is gone: the session drops here and cleans up.
                let _ = tx.send(res);
            })
        };
        match thread {
            Ok(thread) => Self {
                state: State::Pending(rx),
                udid: Some(udid.to_string()),
                cancel,
                thread: Some(thread),
                blackbox_apps: HashSet::new(),
                failure_reported: false,
            },
            Err(e) => Self::unavailable(format!("cannot start the warm-up thread: {e}")),
        }
    }

    /// A warm lane around `client`, without a runner process.
    #[cfg(test)]
    pub(crate) fn ready_for_test(client: IosNativeClient) -> Self {
        Self {
            state: State::Ready(Box::new(client)),
            udid: Some("SIM-UDID".into()),
            cancel: Arc::new(AtomicBool::new(false)),
            thread: None,
            blackbox_apps: HashSet::new(),
            failure_reported: false,
        }
    }

    /// A lane that is known not to work (no driver, not iOS, ...).
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: State::Unavailable(reason.into()),
            udid: None,
            cancel: Arc::new(AtomicBool::new(false)),
            thread: None,
            blackbox_apps: HashSet::new(),
            failure_reported: false,
        }
    }

    /// Remembers that `app_id` has no `tapwing_driver` (drive it black-box).
    pub fn mark_blackbox(&mut self, app_id: &str) {
        self.blackbox_apps.insert(app_id.to_string());
    }

    pub fn is_blackbox(&self, app_id: &str) -> bool {
        self.blackbox_apps.contains(app_id)
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

    /// The simulator this lane drives.
    pub fn udid(&self) -> Option<&str> {
        self.udid.as_deref()
    }

    /// One line per time the runner connection was lost and restarted since the last call.
    pub fn take_restart_notices(&mut self) -> Vec<String> {
        match &mut self.state {
            State::Ready(c) => c.take_restart_notices(),
            _ => Vec::new(),
        }
    }

    fn finish(&mut self, res: Result<IosNativeClient>) {
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
    fn poll(&mut self, timeout: Option<Duration>) -> Option<&mut IosNativeClient> {
        if let State::Pending(rx) = &self.state {
            let res = match timeout {
                Some(t) => {
                    let deadline = Instant::now() + t;
                    loop {
                        if crate::interrupt::is_interrupted() {
                            self.cancel.store(true, Ordering::Relaxed);
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
                    self.finish(Err(anyhow!("the iOS native-lane warm-up thread died")));
                }
            }
        }
        match &mut self.state {
            State::Ready(c) => Some(c),
            _ => None,
        }
    }

    /// Non-blocking: the client only if warm-up already finished.
    pub fn try_get(&mut self) -> Option<&mut IosNativeClient> {
        self.poll(None)
    }

    /// Waits up to `timeout` for the warm-up and returns the client.
    pub fn get_or_wait(&mut self, timeout: Duration) -> Result<&mut IosNativeClient> {
        crate::interrupt::check()?;
        self.poll(Some(timeout));
        crate::interrupt::check()?;
        match &mut self.state {
            State::Ready(c) => Ok(c),
            State::Unavailable(why) => Err(anyhow!("the iOS native lane is unavailable: {why}")),
            State::Pending(_) => Err(anyhow!("the iOS native lane is still starting after {} ms", timeout.as_millis())),
        }
    }
}

impl Drop for IosNativeLaneHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;

    use super::client::mock::{MockRunner, Resp};
    use super::*;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// Fresh scratch directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tapwing-ios-lane-test-{}-{tag}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn zip_bytes(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body, mode) in files {
            let opts = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(body).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    /// A miniature runner with the layout of Tapwing's driver.
    const LAYOUT: RunnerLayout = tapwing_driver::LAYOUT;

    fn fake_files(name: &str) -> Result<Vec<u8>> {
        let runner = zip_bytes(&[
            ("TapwingRunnerUITests-Runner.app/", b"", 0o755),
            ("TapwingRunnerUITests-Runner.app/PlugIns/TapwingRunnerUITests.xctest/", b"", 0o755),
            (
                "TapwingRunnerUITests-Runner.app/PlugIns/TapwingRunnerUITests.xctest/TapwingRunnerUITests",
                b"macho",
                0o755,
            ),
            ("TapwingRunnerUITests-Runner.app/Info.plist", b"<plist/>", 0o644),
        ]);
        let app = zip_bytes(&[("TapwingRunner.app/", b"", 0o755), ("TapwingRunner.app/TapwingRunner", b"app", 0o755)]);
        match name {
            n if n == LAYOUT.xctestrun => Ok(b"<plist/>".to_vec()),
            n if n == LAYOUT.runner_zip => Ok(runner),
            n if n == LAYOUT.app_zip => Ok(app),
            other => bail!("{other} is not a runner file"),
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn xcodebuild_arguments_start_the_runner_without_building() {
        let args = xcodebuild_args(Path::new("/c/x.xctestrun"), "47862A4D-0000", Path::new("/tmp/d/derived"));
        assert_eq!(
            args,
            vec![
                "test-without-building",
                "-xctestrun",
                "/c/x.xctestrun",
                "-destination",
                "id=47862A4D-0000",
                "-destination-timeout",
                "1",
                "-derivedDataPath",
                "/tmp/d/derived",
            ]
        );
    }

    #[test]
    fn extraction_unpacks_products_keeps_modes_and_marks_completion() {
        let s = Scratch::new("extract");
        let dest = s.0.join("cache").join("abc");
        extract_driver_with(&dest, LAYOUT, &no_cancel(), fake_files).unwrap();

        assert!(is_complete_layout(&dest, LAYOUT));
        assert_eq!(fs::read(dest.join(LAYOUT.xctestrun)).unwrap(), b"<plist/>");
        let products = dest.join(PRODUCTS_DIR);
        let bin =
            products.join("TapwingRunnerUITests-Runner.app/PlugIns/TapwingRunnerUITests.xctest/TapwingRunnerUITests");
        assert_eq!(fs::read(&bin).unwrap(), b"macho");
        assert!(products.join("TapwingRunner.app/TapwingRunner").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&bin), 0o755, "Mach-O binaries stay executable");
            assert_eq!(mode(&products.join("TapwingRunner.app/TapwingRunner")), 0o755);
            assert_eq!(mode(&products.join("TapwingRunnerUITests-Runner.app/Info.plist")), 0o644);
        }
        // No temp directories are left behind.
        let leftovers: Vec<_> = fs::read_dir(s.0.join("cache")).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn partial_extractions_are_redone_and_a_complete_one_is_kept() {
        let s = Scratch::new("cache");
        let dest = s.0.join("cache").join("k");
        extract_driver_with(&dest, LAYOUT, &no_cancel(), fake_files).unwrap();

        // A complete directory is kept (a sentinel inside survives).
        fs::write(dest.join("sentinel"), "x").unwrap();
        extract_driver_with(&dest, LAYOUT, &no_cancel(), fake_files).unwrap();
        assert!(dest.join("sentinel").exists(), "a complete extraction is not redone");

        // A directory without the completion marker is never reused.
        fs::remove_file(dest.join(COMPLETE_MARKER)).unwrap();
        assert!(!is_complete_layout(&dest, LAYOUT));
        extract_driver_with(&dest, LAYOUT, &no_cancel(), fake_files).unwrap();
        assert!(!dest.join("sentinel").exists(), "incomplete extraction was replaced");
        assert!(dest.join(COMPLETE_MARKER).is_file());
    }

    #[test]
    fn unsafe_archive_paths_and_missing_files_are_rejected() {
        let s = Scratch::new("unsafe");
        let evil = zip_bytes(&[("../escape.txt", b"x", 0o644)]);
        let err = unzip_products(&evil, &s.0.join("out"), &no_cancel()).unwrap_err().to_string();
        assert!(err.contains("unsafe path"), "{err}");
        assert!(!s.0.join("escape.txt").exists());

        // A source that lacks a runner file fails with a clear message and leaves no cache entry.
        let dest = s.0.join("cache").join("k");
        let err = format!(
            "{:#}",
            extract_driver_with(&dest, LAYOUT, &no_cancel(), |name| bail!("{name} is missing")).unwrap_err()
        );
        assert!(err.contains(LAYOUT.xctestrun), "{err}");
        assert!(!dest.exists());
        assert_eq!(fs::read_dir(s.0.join("cache")).unwrap().count(), 0, "temp directory cleaned up");
    }

    #[test]
    fn extraction_observes_cancellation() {
        let s = Scratch::new("cancel");
        let dest = s.0.join("cache").join("k");
        let cancel = AtomicBool::new(true);
        let err = extract_driver_with(&dest, LAYOUT, &cancel, fake_files).unwrap_err();
        assert!(format!("{err:#}").contains("cancelled"));
        assert!(!dest.exists());
    }

    #[test]
    fn log_tail_keeps_the_last_lines() {
        let s = Scratch::new("tail");
        let log = s.0.join("x.log");
        let body: String = (1..=30).map(|i| format!("line {i}\n\n")).collect();
        fs::write(&log, body).unwrap();
        let tail = log_tail(&log);
        assert_eq!(tail.lines().count(), 12);
        assert!(tail.starts_with("line 19") && tail.ends_with("line 30"));
        assert_eq!(log_tail(&s.0.join("missing.log")), "");
    }

    fn status_runner() -> MockRunner {
        MockRunner::start(Box::new(|req| match req.path() {
            "/deviceInfo" => Resp::json(r#"{"widthPixels":3,"heightPixels":3,"widthPoints":1,"heightPoints":1}"#),
            _ => Resp::json("{}"),
        }))
    }

    #[test]
    fn wait_for_runner_returns_as_soon_as_status_answers() {
        let r = status_runner();
        let client = IosNativeClient::connect(r.port);
        let t = Instant::now();
        wait_for_runner(
            &client,
            "the Tapwing iOS driver",
            || None,
            &no_cancel(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert!(t.elapsed() < Duration::from_secs(1));
        assert_eq!(r.calls("status").len(), 1);
    }

    #[test]
    fn wait_for_runner_polls_until_the_runner_appears() {
        // Keep a single persistently bound MockRunner (no ephemeral port rebind TOCTOU)
        // that returns HTTP 503 for the first 2 probes, then 200 `{}`.
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_h = attempts.clone();
        let r = MockRunner::start(Box::new(move |_| {
            let n = attempts_h.fetch_add(1, Ordering::Relaxed);
            if n < 2 {
                Resp { status: 503, content_type: "text/plain", body: b"starting".to_vec() }
            } else {
                Resp::json("{}")
            }
        }));
        let client = IosNativeClient::connect(r.port);
        wait_for_runner(
            &client,
            "the Tapwing iOS driver",
            || None,
            &no_cancel(),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn wait_for_runner_observes_cancel_exit_and_deadline_quickly() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let client = IosNativeClient::connect(port);
        let far = Instant::now() + Duration::from_secs(60);

        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        let t = Instant::now();
        let err = wait_for_runner(&client, "the Tapwing iOS driver", || None, &cancel, far).unwrap_err().to_string();
        assert_eq!(err, "cancelled");
        assert!(t.elapsed() < Duration::from_millis(900), "cancel observed within a poll: {:?}", t.elapsed());
        canceller.join().unwrap();

        let err =
            wait_for_runner(&client, "the Tapwing iOS driver", || Some("exit status: 65".into()), &no_cancel(), far)
                .unwrap_err();
        assert!(
            err.to_string().contains("Tapwing iOS driver")
                && err.to_string().contains("exited during startup")
                && err.to_string().contains("65"),
            "{err}"
        );

        let t = Instant::now();
        let err = wait_for_runner(
            &client,
            "the Tapwing iOS driver",
            || None,
            &no_cancel(),
            Instant::now() + Duration::from_millis(250),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("did not start in time"), "{err:#}");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    fn ready_session(r: &MockRunner) -> IosNativeClient {
        IosNativeClient::connect(r.port)
    }

    /// A starter that hands out `client` once and fails any later (re)start.
    fn once(client: IosNativeClient) -> impl Fn(&AtomicBool) -> Result<IosNativeClient> + Send + Sync + 'static {
        let slot = std::sync::Mutex::new(Some(client));
        move |_| slot.lock().unwrap().take().ok_or_else(|| anyhow!("no second runner in this test"))
    }

    #[test]
    fn handle_warms_up_in_the_background_and_hands_out_the_client() {
        let r = status_runner();
        let session = ready_session(&r);
        let mut h = IosNativeLaneHandle::spawn_with("UDID-1", once(session));
        assert_eq!(h.udid(), Some("UDID-1"));
        let client = h.get_or_wait(Duration::from_secs(5)).unwrap();
        client.status(Duration::from_millis(500)).unwrap();
        assert!(h.is_usable() && h.failure().is_none());
        assert!(h.try_get().is_some());
        assert!(h.take_failure_notice().is_none());
    }

    #[test]
    fn failed_warm_up_is_reported_once_and_never_blocks() {
        let mut h = IosNativeLaneHandle::spawn_with("UDID-2", |_| Err(anyhow!("xcodebuild exploded")));
        let err = h.get_or_wait(Duration::from_secs(5)).err().unwrap().to_string();
        assert!(err.contains("unavailable") && err.contains("xcodebuild exploded"), "{err}");
        assert!(!h.is_usable());
        assert!(h.try_get().is_none());
        assert!(h.take_failure_notice().unwrap().contains("xcodebuild exploded"));
        assert!(h.take_failure_notice().is_none(), "reported once");
    }

    #[test]
    fn pending_handle_times_out_without_failing() {
        let release = Arc::new(AtomicBool::new(false));
        let gate = release.clone();
        let r = status_runner();
        let session = once(ready_session(&r));
        let mut h = IosNativeLaneHandle::spawn_with("UDID-3", move |cancel| {
            while !gate.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            session(cancel)
        });
        assert!(h.try_get().is_none(), "non-blocking while starting");
        assert!(h.is_usable());
        let err = h.get_or_wait(Duration::from_millis(30)).err().unwrap().to_string();
        assert!(err.contains("still starting"), "{err}");
        release.store(true, Ordering::Relaxed);
        assert!(h.get_or_wait(Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn dropping_a_starting_handle_cancels_promptly() {
        let saw_cancel = Arc::new(AtomicBool::new(false));
        let seen = saw_cancel.clone();
        let h = IosNativeLaneHandle::spawn_with("UDID-4", move |cancel| {
            // A start-up that only ends when asked to.
            let deadline = Instant::now() + Duration::from_secs(30);
            while !cancel.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            seen.store(cancel.load(Ordering::Relaxed), Ordering::Relaxed);
            Err(anyhow!("cancelled"))
        });
        let t = Instant::now();
        drop(h);
        assert!(t.elapsed() < Duration::from_secs(1), "Drop must not wait for the start-up: {:?}", t.elapsed());
        assert!(saw_cancel.load(Ordering::Relaxed));
    }

    /// Starts a runner on a dead port first, then on `port`; counts the starts.
    fn dead_then(
        port: u16,
        starts: &Arc<AtomicUsize>,
    ) -> impl Fn(&AtomicBool) -> Result<IosNativeClient> + Send + Sync + use<> {
        let starts = starts.clone();
        move |_| {
            let n = starts.fetch_add(1, Ordering::Relaxed);
            let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            Ok(IosNativeClient::connect(if n == 0 { dead } else { port }))
        }
    }

    #[test]
    fn a_dead_runner_is_restarted_with_the_same_starter_and_reported_once() {
        let r = status_runner();
        let starts = Arc::new(AtomicUsize::new(0));
        let mut h = IosNativeLaneHandle::spawn_with("UDID-5", dead_then(r.port, &starts));
        let client = h.get_or_wait(Duration::from_secs(5)).unwrap();
        assert_eq!(client.device_info().unwrap().width_points, 1);
        assert_eq!(starts.load(Ordering::Relaxed), 2, "warm-up, then one restart");
        assert_eq!(r.calls("deviceInfo").len(), 1);
        let notices = h.take_restart_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("iOS native driver connection lost") && notices[0].contains("restart 1/3"));
        assert!(h.take_restart_notices().is_empty());
        assert!(h.is_usable());
    }

    #[test]
    fn a_cancelled_handle_does_not_start_a_new_runner() {
        let r = status_runner();
        let starts = Arc::new(AtomicUsize::new(0));
        let mut h = IosNativeLaneHandle::spawn_with("UDID-6", dead_then(r.port, &starts));
        h.get_or_wait(Duration::from_secs(5)).unwrap();
        // What dropping the handle does; a restart started afterwards must not spawn `xcodebuild`.
        h.cancel.store(true, Ordering::Relaxed);
        let err = format!("{:#}", h.get_or_wait(Duration::from_secs(1)).unwrap().device_info().unwrap_err());
        assert!(err.contains("could not be restarted: cancelled"), "{err}");
        assert_eq!(starts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_restart_observes_the_handle_cancel_flag_while_starting() {
        // The starter sees the handle's own flag, so `wait_for_runner` inside it stops on cancel.
        let r = status_runner();
        let seen: Arc<std::sync::Mutex<Vec<usize>>> = Arc::default();
        let (port, log) = (r.port, seen.clone());
        let mut h = IosNativeLaneHandle::spawn_with("UDID-7", move |cancel| {
            let mut log = log.lock().unwrap();
            log.push(std::ptr::from_ref(cancel) as usize);
            let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            Ok(IosNativeClient::connect(if log.len() == 1 { dead } else { port }))
        });
        h.get_or_wait(Duration::from_secs(5)).unwrap().device_info().unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1], "warm-up and restart share one cancel flag");
        assert_eq!(seen[1], Arc::as_ptr(&h.cancel) as usize);
    }

    #[test]
    fn unavailable_handle_reports_why_and_tracks_blackbox_apps() {
        let mut h = IosNativeLaneHandle::unavailable("no driver");
        assert!(!h.is_usable());
        assert_eq!(h.failure(), Some("no driver"));
        assert_eq!(h.udid(), None);
        assert!(h.try_get().is_none());
        let err = h.get_or_wait(Duration::from_millis(1)).err().unwrap();
        assert!(err.to_string().contains("no driver"));
        assert_eq!(h.take_failure_notice().as_deref(), Some("no driver"));
        assert!(h.take_failure_notice().is_none());

        assert!(!h.is_blackbox("dev.tapwing.sampleApp"));
        h.mark_blackbox("dev.tapwing.sampleApp");
        assert!(h.is_blackbox("dev.tapwing.sampleApp"));
        assert!(!h.is_blackbox("other"));
    }

    #[test]
    fn port_reservations_are_distinct_retry_collisions_and_release_on_drop() {
        let pool = Arc::new(Mutex::new(HashSet::new()));

        // Concurrent ephemeral reservations in one process yield distinct ports.
        let mut guards = Vec::new();
        let mut ports = HashSet::new();
        for _ in 0..8 {
            let g = reserve_ephemeral_port(&pool, || {
                let l = TcpListener::bind("127.0.0.1:0")?;
                let p = l.local_addr()?.port();
                Ok((p, l))
            })
            .unwrap();
            assert!(ports.insert(g.port()), "duplicate port reserved: {}", g.port());
            guards.push(g);
        }
        assert_eq!(pool.lock().unwrap().len(), 8);

        // Dropping releases every reserved port back to the pool.
        let freed = guards.remove(0).port();
        assert!(!pool.lock().unwrap().contains(&freed));
        let re_reserved = reserve_explicit_port(&pool, freed).unwrap();
        assert_eq!(re_reserved.port(), freed);
        // Reserving the same explicit port while held fails cleanly.
        assert!(reserve_explicit_port(&pool, freed).is_err());
        drop(re_reserved);
        drop(guards);
        assert!(pool.lock().unwrap().is_empty());

        // Colliding ports returned by the ephemeral picker are skipped and retried.
        let held = reserve_explicit_port(&pool, 41_000).unwrap();
        let mut candidates = [41_000_u16, 41_000, 41_001].into_iter();
        let picked = reserve_ephemeral_port(&pool, || Ok((candidates.next().unwrap(), ()))).unwrap();
        assert_eq!(picked.port(), 41_001);
        drop(held);
        drop(picked);
        assert!(pool.lock().unwrap().is_empty());

        // Exhausting retries on persistent collisions returns an error.
        let stuck = reserve_explicit_port(&pool, 42_000).unwrap();
        let err = reserve_ephemeral_port(&pool, || Ok((42_000, ()))).err().unwrap().to_string();
        assert!(err.contains("could not reserve a free ephemeral port"), "{err}");
        drop(stuck);
        assert!(pool.lock().unwrap().is_empty());
    }

    #[test]
    fn session_dir_names_include_monotonic_sequence() {
        let a = session_dir_name(1234, 0, 50000);
        let b = session_dir_name(1234, 1, 50000);
        assert_ne!(a, b);
        assert_eq!(a, "tapwing-ios-native-1234-0-50000");
        assert_eq!(b, "tapwing-ios-native-1234-1-50000");
    }
}
