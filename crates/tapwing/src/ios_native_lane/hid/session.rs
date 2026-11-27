//! [`HidSession`]: one lazily opened backend per simulator, with the failure policy of the spike.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use super::tree::{self, FrontApp};
use super::{Button, FAIL_ENV, HidBackend, HidError, Screen, Snapshot, ffi};
use crate::ios_native_lane::timing;

/// Builds the backend for a simulator id. Production uses the shim, tests a fake.
pub type Opener = Box<dyn Fn(&str) -> Result<Box<dyn HidBackend>, HidError> + Send + Sync>;

/// Bound for the accessibility requests of the focus question. Short on purpose: it runs in the
/// middle of a lookup and its answer only routes a command.
const FOCUS_TIMEOUT_S: f64 = 1.0;
/// The focus question needs the root only, plus its first level for a cheap sanity check.
const FOCUS_DEPTH: u32 = 1;

/// The route [`FAIL_ENV`] makes fail on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    Open,
    Snapshot,
    Touch,
    Frame,
}

impl FailPoint {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "open" => Some(Self::Open),
            "snapshot" => Some(Self::Snapshot),
            "touch" => Some(Self::Touch),
            "frame" => Some(Self::Frame),
            _ => None,
        }
    }

    fn from_env() -> Option<Self> {
        std::env::var(FAIL_ENV).ok().and_then(|v| Self::parse(&v))
    }
}

/// The process-wide switch: once any session finds the backend unavailable, HID is off for every
/// session and the user sees one `warning:` line.
pub struct Gate {
    disabled: AtomicBool,
    warn: Box<dyn Fn(&str) + Send + Sync>,
}

impl Gate {
    pub fn new(warn: impl Fn(&str) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { disabled: AtomicBool::new(false), warn: Box::new(warn) })
    }

    /// The gate of this process, warning on stderr.
    fn process() -> Arc<Self> {
        static GATE: OnceLock<Arc<Gate>> = OnceLock::new();
        Arc::clone(GATE.get_or_init(|| Gate::new(|line| eprintln!("{line}"))))
    }

    fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Relaxed)
    }

    fn disable(&self, why: &str) {
        if !self.disabled.swap(true, Ordering::Relaxed) {
            (self.warn)(&format!(
                "warning: the iOS HID backend is unavailable ({why}); native steps use the XCUITest runner"
            ));
        }
    }
}

enum State {
    Closed,
    Open(Box<dyn HidBackend>),
}

/// Serializes calls to one simulator's backend. The backend opens on the first call, so a run that
/// never needs HID pays nothing and a dropped idle session does nothing.
pub struct HidSession {
    udid: String,
    opener: Opener,
    gate: Arc<Gate>,
    fail: Option<FailPoint>,
    state: Mutex<State>,
}

impl HidSession {
    /// The session of the shim, with the process gate and the debug fail point of the environment.
    pub fn for_device(udid: &str) -> Self {
        Self::new(udid, Box::new(ffi::open), Gate::process(), FailPoint::from_env())
    }

    pub fn new(udid: &str, opener: Opener, gate: Arc<Gate>, fail: Option<FailPoint>) -> Self {
        Self { udid: udid.to_string(), opener, gate, fail, state: Mutex::new(State::Closed) }
    }

    /// `false` once the backend was found unavailable. Opening is lazy, so `true` promises nothing yet.
    pub fn is_active(&self) -> bool {
        !self.gate.is_disabled()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn open(&self, state: &mut State) -> Result<(), HidError> {
        if matches!(state, State::Open(_)) {
            return Ok(());
        }
        let started = Instant::now();
        let res = if self.fail == Some(FailPoint::Open) {
            Err(HidError::Unavailable(format!("forced by {FAIL_ENV}=open")))
        } else {
            (self.opener)(&self.udid)
        };
        timing::emit("hid_open", started.elapsed(), &format!("ok={}", u8::from(res.is_ok())));
        *state = State::Open(res?);
        Ok(())
    }

    /// Runs `f` on the open backend. `Unavailable` from the open or from the call switches HID off.
    fn with<T>(
        &self,
        route: &str,
        forced: Option<FailPoint>,
        f: impl FnOnce(&mut dyn HidBackend) -> Result<T, HidError>,
    ) -> Result<T, HidError> {
        if self.gate.is_disabled() {
            return Err(HidError::Unavailable("switched off earlier in this run".into()));
        }
        let mut state = self.lock();
        let res = self.open(&mut state).and_then(|()| {
            let State::Open(backend) = &mut *state else {
                return Err(HidError::Unavailable("the backend is not open".into()));
            };
            let started = Instant::now();
            let res = if forced.is_some() && self.fail == forced {
                Err(HidError::Failed(format!("forced by {FAIL_ENV}={route}")))
            } else {
                f(backend.as_mut())
            };
            let detail = match &res {
                Ok(_) => String::new(),
                Err(e) => format!(" err={:?}", e.to_string()),
            };
            timing::emit("hid", started.elapsed(), &format!("route={route} ok={}{detail}", u8::from(res.is_ok())));
            res
        });
        drop(state);
        if let Err(HidError::Unavailable(why)) = &res {
            self.gate.disable(why);
        }
        res
    }

    pub fn screen(&self) -> Result<Screen, HidError> {
        self.with("screen", None, |b| b.screen())
    }

    /// The raw JSON of an accessibility read of the frontmost application.
    pub fn snapshot(&self, req: &Snapshot) -> Result<String, HidError> {
        self.with("snapshot", Some(FailPoint::Snapshot), |b| b.snapshot(req))
    }

    /// Which application is in front, from a shallow accessibility read.
    pub fn front_app(&self) -> Result<FrontApp, HidError> {
        let json = self.snapshot(&Snapshot { max_depth: FOCUS_DEPTH, timeout_s: FOCUS_TIMEOUT_S })?;
        Ok(tree::front_app(&json))
    }

    pub fn touch(&self, path: &[(f64, f64)], hold_ms: u32, step_ms: u32) -> Result<(), HidError> {
        self.with("touch", Some(FailPoint::Touch), |b| b.touch(path, hold_ms, step_ms))
    }

    pub fn button(&self, button: Button) -> Result<(), HidError> {
        let route = match button {
            Button::Home => "button_home",
            Button::Lock => "button_lock",
        };
        self.with(route, Some(FailPoint::Touch), |b| b.button(button))
    }

    pub fn frame_png(&self) -> Result<Vec<u8>, HidError> {
        self.with("frame", Some(FailPoint::Frame), |b| b.frame_png())
    }

    /// The runner's `isScreenStatic`: two frames back to back, equal bytes. The runner hashes two
    /// screenshots taken one after the other with no pause, so this keeps its meaning.
    pub fn screen_static(&self) -> Result<bool, HidError> {
        let first = self.frame_png()?;
        let second = self.frame_png()?;
        Ok(first == second)
    }
}
