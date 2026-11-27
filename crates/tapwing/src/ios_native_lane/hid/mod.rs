//! In-process iOS Simulator HID and accessibility backend (spike for issue #30).
//!
//! `TAPWING_IOS_HID=1` opts in. Without it nothing here runs: no framework is loaded and no call
//! is made. With it, the Objective-C shim in `native/ios_hid/shim.m` serves four things without
//! the XCUITest runner: an accessibility snapshot (`AXPTranslator`), Indigo touches and buttons
//! (`SimulatorKit`) and the framebuffer. The private frameworks are `dlopen`ed by the shim on first
//! use, so a Mac without Xcode still starts and the flag is only a request.
//!
//! * [`HidBackend`]: the small trait the rest of the crate talks to (tests inject a fake).
//! * `ffi`: the macOS implementation over the shim, and a stub for every other target.
//! * [`HidSession`]: opens a backend lazily, serializes calls, writes timing events and turns
//!   "backend unavailable" into one warning and a process-wide switch-off.
//! * `tree`: accessibility tree to the runner's hierarchy JSON, and the frontmost-process answer.
//! * `surface`: [`HidOps`], a runner-less implementation of `NativeSurface` for SpringBoard alerts.
//!
//! Debug env `TAPWING_IOS_HID_FAIL=open|snapshot|touch|frame` makes the session fail that route on
//! purpose, so the fallback to the runner can be exercised on a real simulator. `open` is a
//! "backend unavailable" error (one warning, HID off for the process), the others fail one call.
//! `TAPWING_IOS_NATIVE_TIMING=1` adds `hid_open` and `hid` event lines (route, `ok`, duration).

mod ffi;
mod session;
pub mod surface;
pub mod tree;

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub use session::{FailPoint, Gate, HidSession, Opener};
pub use surface::{HidOps, SWIPE_HOLD_MS, TAP_HOLD_MS, hold_ms, swipe_path};
pub use tree::FrontApp;

/// Opt-in switch: `1` enables the backend.
pub const ENABLE_ENV: &str = "TAPWING_IOS_HID";
/// Debug-only: the route the session fails on purpose (see the module docs).
pub const FAIL_ENV: &str = "TAPWING_IOS_HID_FAIL";

/// How an HID call failed. The split decides what the caller may do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidError {
    /// The backend cannot serve at all: a framework, class, selector or symbol is missing, or the
    /// device is not found or not booted. The session switches HID off for the process.
    Unavailable(String),
    /// One call failed and nothing reached the device, so the runner may do the same work.
    Failed(String),
    /// An input call failed after SimulatorKit had been handed an event. The event may have
    /// reached the device, so the runner must never repeat it.
    InputMaybeSent(String),
    /// This surface does not implement the operation. Nothing was sent.
    Unsupported(&'static str),
}

impl std::fmt::Display for HidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "iOS HID backend unavailable: {m}"),
            Self::Failed(m) => write!(f, "iOS HID call failed: {m}"),
            Self::InputMaybeSent(m) => write!(f, "iOS HID input may have been delivered: {m}"),
            Self::Unsupported(what) => write!(f, "the HID surface does not support {what}"),
        }
    }
}

impl std::error::Error for HidError {}

impl HidError {
    /// Whether the failed call may already have moved the device.
    pub fn input_maybe_sent(&self) -> bool {
        matches!(self, Self::InputMaybeSent(_))
    }
}

/// Whether an error carries an input event that may have been delivered.
pub fn is_input_maybe_sent(e: &anyhow::Error) -> bool {
    e.downcast_ref::<HidError>().is_some_and(HidError::input_maybe_sent)
}

/// Portrait screen of the device type in points. HID touches and the framebuffer use this space
/// whatever the interface orientation is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// Hardware buttons. Only the Indigo targets verified in phase 1 are ever sent (see the shim).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Home,
    Lock,
}

/// What an accessibility read covers: the frontmost application, to `max_depth` levels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    /// `0` means the shim default (64).
    pub max_depth: u32,
    /// Bound for each request to the simulator, in seconds.
    pub timeout_s: f64,
}

/// What the rest of the crate needs from the simulator. Implemented over the shim, and by fakes in tests.
pub trait HidBackend: Send {
    fn screen(&mut self) -> Result<Screen, HidError>;
    /// The shim's JSON text (`{"roots":[...]}`).
    fn snapshot(&mut self, req: &Snapshot) -> Result<String, HidError>;
    /// A single finger through `path` (points): down at the first point, `hold_ms` there, then
    /// `step_ms` between the others, up at the last.
    fn touch(&mut self, path: &[(f64, f64)], hold_ms: u32, step_ms: u32) -> Result<(), HidError>;
    fn button(&mut self, button: Button) -> Result<(), HidError>;
    /// The unrotated framebuffer as PNG bytes.
    fn frame_png(&mut self) -> Result<Vec<u8>, HidError>;
}

/// Whether `TAPWING_IOS_HID=1` is set.
pub fn enabled() -> bool {
    std::env::var(ENABLE_ENV).is_ok_and(|v| v == "1")
}

/// The session for `udid`, or `None` when the flag is off. One session per simulator for the whole
/// process, so the client of the runner and the hybrid router share one backend and one switch-off.
pub fn session_for(udid: &str) -> Option<Arc<HidSession>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<HidSession>>>> = OnceLock::new();
    session_if(enabled(), || {
        let mut map = SESSIONS.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(map.entry(udid.to_string()).or_insert_with(|| Arc::new(HidSession::for_device(udid))))
    })
}

/// `make()` when `on`, nothing otherwise. Kept apart from the environment so a test can show that
/// a run without the flag builds no session.
pub(crate) fn session_if(on: bool, make: impl FnOnce() -> Arc<HidSession>) -> Option<Arc<HidSession>> {
    on.then(make)
}
