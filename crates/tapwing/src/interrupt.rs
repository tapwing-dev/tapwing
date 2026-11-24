//! Process-wide graceful interruption handling for `SIGINT` (Ctrl-C) and `SIGTERM`.
//!
//! Tapwing configures device-side state through RAII guards (`adb reverse`,
//! `stay_on_while_plugged_in`, `disableAnimations`, Android UiAutomator
//! instrumentation + `adb forward` + OS mock-location providers, iOS
//! `xcodebuild` + simulator runner, and screen recordings). Without a signal
//! handler, `SIGINT` or `SIGTERM` kills the process immediately and skips
//! every `Drop` guard, leaving the device modified and helper processes running.
//!
//! Installing this handler turns the **first** `SIGINT`/`SIGTERM` into a
//! cooperative cancellation: blocking loops return [`InterruptedError`],
//! the stack unwinds so every RAII guard cleans up the device, and the CLI
//! exits with the standard Unix status `128 + signal` (`130` for `SIGINT`,
//! `143` for `SIGTERM`). A **second** signal exits immediately via `_exit` so
//! an unresponsive ADB daemon or device never traps the user.

use std::fmt;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::Result;

/// Maximum sleep chunk used by [`sleep`] so cancellation is noticed promptly
/// without spinning.
const SLEEP_CHUNK: Duration = Duration::from_millis(50);

/// Error returned when execution is aborted by `SIGINT` or `SIGTERM`.
///
/// Not a [`crate::backend::MaestroError`], so `optional: true` never downgrades
/// it and `retry` never retries it; the orchestrator skips failure diagnostics
/// and `onFlowComplete` hooks so teardown starts immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptedError {
    pub signal: i32,
}

impl InterruptedError {
    pub fn current() -> Self {
        Self { signal: signal().unwrap_or(2) }
    }
}

impl fmt::Display for InterruptedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.signal {
            2 => f.write_str("interrupted by SIGINT"),
            15 => f.write_str("terminated by SIGTERM"),
            n => write!(f, "interrupted by signal {n}"),
        }
    }
}

impl std::error::Error for InterruptedError {}

/// Lock-free signal state (separated into a struct so unit tests can exercise
/// every transition without mutating the process-global signal atomics).
struct InterruptState {
    signal: AtomicI32,
    count: AtomicUsize,
}

impl InterruptState {
    const fn new() -> Self {
        Self { signal: AtomicI32::new(0), count: AtomicUsize::new(0) }
    }

    /// Records a received signal; returns `true` on the first signal (graceful
    /// shutdown) and `false` on any subsequent signal (immediate exit).
    fn record(&self, sig: i32) -> bool {
        let prev = self.count.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            self.signal.store(sig, Ordering::Release);
            true
        } else {
            false
        }
    }

    fn signal(&self) -> Option<i32> {
        let sig = self.signal.load(Ordering::Acquire);
        (sig != 0).then_some(sig)
    }

    fn is_interrupted(&self) -> bool {
        self.signal().is_some()
    }

    fn exit_code(&self) -> Option<i32> {
        self.signal().map(|sig| 128 + sig)
    }

    fn check(&self) -> Result<()> {
        match self.signal() {
            Some(signal) => Err(InterruptedError { signal }.into()),
            None => Ok(()),
        }
    }

    fn sleep(&self, duration: Duration) -> Result<()> {
        self.check()?;
        if duration.is_zero() {
            return Ok(());
        }
        let deadline = Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return self.check();
            }
            std::thread::sleep(remaining.min(SLEEP_CHUNK));
            self.check()?;
        }
    }
}

static STATE: InterruptState = InterruptState::new();

/// Installs the `SIGINT` and `SIGTERM` handlers once per process.
///
/// Safe to call multiple times; subsequent calls are no-ops.
pub fn install() {
    #[cfg(unix)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(install_unix_handlers);
    }
}

#[cfg(unix)]
extern "C" fn handle_signal(sig: libc::c_int) {
    if STATE.record(sig) {
        // Async-signal-safe notice via POSIX write(2).
        const MSG: &[u8] = b"\ninterrupted; cleaning up (press Ctrl-C again to exit immediately)\n";
        unsafe {
            let _ = libc::write(libc::STDERR_FILENO, MSG.as_ptr().cast(), MSG.len());
        }
    } else {
        // Second signal: exit immediately with standard 128 + signal status
        // so a stuck device or ADB daemon never traps the user.
        unsafe { libc::_exit(128 + sig) }
    }
}

#[cfg(unix)]
fn install_unix_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_signal as *const () as usize;
        // SA_RESTART avoids spurious EINTR errors inside third-party I/O while
        // all Tapwing wait/poll loops wake and check `is_interrupted()` promptly.
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&raw mut sa.sa_mask);
        let _ = libc::sigaction(libc::SIGINT, &raw const sa, std::ptr::null_mut());
        let _ = libc::sigaction(libc::SIGTERM, &raw const sa, std::ptr::null_mut());
    }
}

/// Whether `SIGINT` or `SIGTERM` has been received.
pub fn is_interrupted() -> bool {
    STATE.is_interrupted()
}

/// The first interrupting signal received (`SIGINT` = 2, `SIGTERM` = 15), if any.
pub fn signal() -> Option<i32> {
    STATE.signal()
}

/// Standard Unix exit status (`128 + signal`) if interrupted, else `None`.
pub fn exit_code() -> Option<i32> {
    STATE.exit_code()
}

/// Returns `Err(InterruptedError)` if an interrupting signal has been received.
pub fn check() -> Result<()> {
    STATE.check()
}

/// Sleeps up to `duration`, waking within [`SLEEP_CHUNK`] (50 ms) to abort
/// early with [`InterruptedError`] if a signal arrives.
pub fn sleep(duration: Duration) -> Result<()> {
    STATE.sleep(duration)
}

/// Handle on the interrupt state for code that blocks outside Tapwing's own
/// poll loops (the JavaScript VM, a blocking HTTP request).
///
/// [`Watch::process`] observes the process-wide `SIGINT`/`SIGTERM` state. Unit
/// tests build a private one with `Watch::manual` so they can simulate a signal
/// without touching the process-global atomics shared by every parallel test.
#[derive(Clone, Copy)]
pub struct Watch {
    state: &'static InterruptState,
}

impl Watch {
    /// Watches the real process-wide signal state.
    pub fn process() -> Self {
        Self { state: &STATE }
    }

    /// A private state plus the trigger that simulates the first signal on it.
    #[cfg(test)]
    pub(crate) fn manual() -> (Self, impl Fn(i32) + Send + Sync + Copy + 'static) {
        let state: &'static InterruptState = Box::leak(Box::new(InterruptState::new()));
        (Self { state }, move |sig| {
            state.record(sig);
        })
    }

    pub fn is_interrupted(&self) -> bool {
        self.state.is_interrupted()
    }

    /// Returns `Err(InterruptedError)` once a signal has been recorded.
    pub fn check(&self) -> Result<()> {
        self.state.check()
    }

    /// Blocks until `rx` yields a value, waking every [`SLEEP_CHUNK`] to abort
    /// with [`InterruptedError`] once a signal arrives. `Ok(None)` means the
    /// sender went away without sending.
    pub fn recv<T>(&self, rx: &Receiver<T>) -> Result<Option<T>> {
        loop {
            self.check()?;
            match rx.recv_timeout(SLEEP_CHUNK) {
                Ok(value) => return Ok(Some(value)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }
}

/// Whether `err` (or any source in its chain) is an [`InterruptedError`].
pub fn is_interrupted_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| cause.is::<InterruptedError>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn state_records_first_signal_and_maps_exit_codes() {
        let s = InterruptState::new();
        assert!(!s.is_interrupted());
        assert_eq!(s.signal(), None);
        assert_eq!(s.exit_code(), None);
        assert!(s.check().is_ok());

        assert!(s.record(2), "first signal triggers graceful shutdown");
        assert!(s.is_interrupted());
        assert_eq!(s.signal(), Some(2));
        assert_eq!(s.exit_code(), Some(130));
        let err = s.check().unwrap_err();
        assert!(is_interrupted_error(&err));
        assert_eq!(err.to_string(), "interrupted by SIGINT");

        assert!(!s.record(15), "second signal triggers immediate exit");
        assert_eq!(s.signal(), Some(2), "first signal number is preserved");

        let s_term = InterruptState::new();
        assert!(s_term.record(15));
        assert_eq!(s_term.exit_code(), Some(143));
        assert_eq!(s_term.check().unwrap_err().to_string(), "terminated by SIGTERM");
    }

    #[test]
    fn is_interrupted_error_matches_wrapped_context_and_rejects_other_errors() {
        let wrapped = anyhow::Error::new(InterruptedError { signal: 15 }).context("waiting for device");
        assert!(is_interrupted_error(&wrapped));
        assert!(!is_interrupted_error(&anyhow::anyhow!("boom")));
        assert!(!crate::backend::is_maestro_error(&wrapped));
        assert!(!crate::backend::is_optional_downgradable(&wrapped));
    }

    #[test]
    fn sleep_aborts_promptly_when_state_is_interrupted() {
        let state = Arc::new(InterruptState::new());
        let trigger = Arc::clone(&state);
        let t0 = Instant::now();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            trigger.record(2);
        });
        let err = state.sleep(Duration::from_secs(10)).unwrap_err();
        handle.join().unwrap();
        assert!(is_interrupted_error(&err));
        assert!(t0.elapsed() < Duration::from_secs(1), "sleep took {:?}", t0.elapsed());
    }

    #[test]
    fn watch_recv_returns_values_disconnects_and_interrupts() {
        let (watch, trigger) = Watch::manual();
        let (tx, rx) = std::sync::mpsc::channel::<u8>();
        tx.send(7).unwrap();
        assert_eq!(watch.recv(&rx).unwrap(), Some(7));

        // Nobody ever writes to the channel again, so only the trigger (fired
        // from another thread, before or during the wait) can end `recv`.
        let handle = std::thread::spawn(move || trigger(15));
        let err = watch.recv(&rx).unwrap_err();
        handle.join().unwrap();
        assert_eq!(err.downcast_ref::<InterruptedError>(), Some(&InterruptedError { signal: 15 }));
        assert!(!Watch::process().is_interrupted(), "a manual watch never touches the process state");

        let (idle, _) = Watch::manual();
        drop(tx);
        assert_eq!(idle.recv(&rx).unwrap(), None);
    }
}
