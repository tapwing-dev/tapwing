//! Shared concurrency, lifecycle, recovery, and command primitives for native device lanes.

pub mod ops;
pub mod recovery;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

/// Two missed Flutter driver handshakes in a row promote an app to sticky
/// shard-wide black-box mode.
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

    /// Records a successful Flutter handshake for `app_id`, resetting its consecutive-miss counter.
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

/// Cooperative cancellation signal backed by a [`Condvar`] so warm-up and readiness
/// waits wake immediately when cancelled (or interrupted via `SIGINT`/`SIGTERM`).
#[derive(Debug)]
pub struct CancelSignal {
    flag: AtomicBool,
    lock: Mutex<bool>,
    cv: Condvar,
}

impl Default for CancelSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelSignal {
    /// Creates an uncancelled signal.
    pub fn new() -> Self {
        Self { flag: AtomicBool::new(false), lock: Mutex::new(false), cv: Condvar::new() }
    }

    /// Exposes the underlying atomic flag for lock-free checks and FFI/subprocess hooks.
    pub fn flag(&self) -> &AtomicBool {
        &self.flag
    }

    /// Returns whether cancellation or process interruption has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire) || crate::interrupt::is_interrupted()
    }

    /// Marks the signal as cancelled and wakes all waiters immediately.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        let mut cancelled = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *cancelled = true;
        self.cv.notify_all();
    }

    /// Waits up to `duration`, returning `true` immediately if cancelled or interrupted.
    pub fn wait_timeout(&self, duration: Duration) -> bool {
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

/// Client operations required by [`LaneHandle`] to drain user-facing notices.
pub trait LaneClient: Sized + Send + 'static {
    /// One user-facing line per recovery restart since the last call.
    fn take_restart_notices(&mut self) -> Vec<String>;
}

enum LaneState<C> {
    Pending(Receiver<Result<C>>),
    Ready(Box<C>),
    Unavailable(String),
}

/// Shared background warm-up handle, readiness waiter, and blackbox tracker for
/// both the Android and iOS native lanes.
pub struct LaneHandle<C: LaneClient, Meta> {
    label: &'static str,
    thread_dead_msg: &'static str,
    state: LaneState<C>,
    meta: Option<Meta>,
    cancel: Arc<CancelSignal>,
    thread: Option<JoinHandle<()>>,
    blackbox: BlackboxTracker,
    failure_reported: bool,
}

impl<C: LaneClient, Meta> LaneHandle<C, Meta> {
    /// Spawns a background warm-up thread running `starter(&cancel)`.
    pub fn spawn_with<F>(
        label: &'static str,
        thread_dead_msg: &'static str,
        thread_name: &str,
        meta: Meta,
        starter: F,
    ) -> Self
    where
        F: FnOnce(&Arc<CancelSignal>) -> Result<C> + Send + 'static,
    {
        let cancel = Arc::new(CancelSignal::new());
        let (tx, rx) = mpsc::channel();
        let thread = {
            let cancel = cancel.clone();
            std::thread::Builder::new().name(thread_name.to_string()).spawn(move || {
                // A failed send means the handle is gone: the session drops here and cleans up.
                let _ = tx.send(starter(&cancel));
            })
        };
        match thread {
            Ok(thread) => Self {
                label,
                thread_dead_msg,
                state: LaneState::Pending(rx),
                meta: Some(meta),
                cancel,
                thread: Some(thread),
                blackbox: BlackboxTracker::default(),
                failure_reported: false,
            },
            Err(e) => Self::unavailable(label, thread_dead_msg, format!("cannot start the warm-up thread: {e}")),
        }
    }

    /// Creates a handle permanently marked unavailable with `reason`.
    pub fn unavailable(label: &'static str, thread_dead_msg: &'static str, reason: impl Into<String>) -> Self {
        Self {
            label,
            thread_dead_msg,
            state: LaneState::Unavailable(reason.into()),
            meta: None,
            cancel: Arc::new(CancelSignal::new()),
            thread: None,
            blackbox: BlackboxTracker::default(),
            failure_reported: false,
        }
    }

    /// Creates an immediately ready handle (primarily for unit tests).
    #[cfg(test)]
    pub(crate) fn ready(label: &'static str, thread_dead_msg: &'static str, client: C, meta: Meta) -> Self {
        Self {
            label,
            thread_dead_msg,
            state: LaneState::Ready(Box::new(client)),
            meta: Some(meta),
            cancel: Arc::new(CancelSignal::new()),
            thread: None,
            blackbox: BlackboxTracker::default(),
            failure_reported: false,
        }
    }

    /// Replaces the shard-scoped blackbox tracker.
    pub(crate) fn set_blackbox_tracker(&mut self, tracker: BlackboxTracker) {
        self.blackbox = tracker;
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

    /// Returns the warm-up failure reason once for user-facing warnings.
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
        !matches!(self.state, LaneState::Unavailable(_))
    }

    /// Failure message if warm-up failed.
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            LaneState::Unavailable(why) => Some(why),
            _ => None,
        }
    }

    /// Attached metadata when available.
    pub fn meta(&self) -> Option<&Meta> {
        self.meta.as_ref()
    }

    /// Exposes the handle's cancellation signal.
    #[cfg(test)]
    pub(crate) fn cancel_signal(&self) -> &Arc<CancelSignal> {
        &self.cancel
    }

    /// Drains restart notices from the client when ready.
    pub fn take_restart_notices(&mut self) -> Vec<String> {
        match &mut self.state {
            LaneState::Ready(client) => client.take_restart_notices(),
            _ => Vec::new(),
        }
    }

    fn finish(&mut self, res: Result<C>) {
        self.state = match res {
            Ok(client) => LaneState::Ready(Box::new(client)),
            Err(e) => LaneState::Unavailable(format!("{e:#}")),
        };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn poll(&mut self, timeout: Option<Duration>) -> Option<&mut C> {
        if let LaneState::Pending(rx) = &self.state {
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
                    self.finish(Err(anyhow!("{}", self.thread_dead_msg)));
                }
            }
        }
        match &mut self.state {
            LaneState::Ready(client) => Some(client),
            _ => None,
        }
    }

    /// Non-blocking: returns the client only if warm-up already finished.
    pub fn try_get(&mut self) -> Option<&mut C> {
        self.poll(None)
    }

    /// Waits up to `timeout` for warm-up and returns a mutable client handle.
    pub fn get_or_wait(&mut self, timeout: Duration) -> Result<&mut C> {
        crate::interrupt::check()?;
        self.poll(Some(timeout));
        crate::interrupt::check()?;
        match &mut self.state {
            LaneState::Ready(client) => Ok(client),
            LaneState::Unavailable(why) => Err(anyhow!("{} is unavailable: {why}", self.label)),
            LaneState::Pending(_) => Err(anyhow!("{} is still starting after {} ms", self.label, timeout.as_millis())),
        }
    }
}

impl<C: LaneClient, Meta> Drop for LaneHandle<C, Meta> {
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

    #[derive(Debug)]
    struct DummyClient(Vec<String>);

    impl LaneClient for DummyClient {
        fn take_restart_notices(&mut self) -> Vec<String> {
            std::mem::take(&mut self.0)
        }
    }

    #[test]
    fn cancel_signal_wakes_waiter_early() {
        let sig = Arc::new(CancelSignal::new());
        let sig_bg = Arc::clone(&sig);
        let t = std::thread::spawn(move || sig_bg.wait_timeout(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(15));
        sig.cancel();
        assert!(t.join().expect("waiter join"));
    }

    #[test]
    fn lane_handle_unavailable_and_ready_states() {
        let mut bad: LaneHandle<DummyClient, ()> =
            LaneHandle::unavailable("the test lane", "test thread died", "no device");
        assert!(!bad.is_usable());
        assert_eq!(bad.failure(), Some("no device"));
        assert!(bad.try_get().is_none());
        let err = bad.get_or_wait(Duration::from_millis(5)).unwrap_err();
        assert_eq!(err.to_string(), "the test lane is unavailable: no device");
        assert_eq!(bad.take_failure_notice().as_deref(), Some("no device"));
        assert!(bad.take_failure_notice().is_none());

        let mut ok = LaneHandle::ready("the test lane", "test thread died", DummyClient(vec!["restarted".into()]), ());
        assert!(ok.is_usable());
        assert!(ok.try_get().is_some());
        assert_eq!(ok.take_restart_notices(), vec!["restarted".to_string()]);
    }
}
