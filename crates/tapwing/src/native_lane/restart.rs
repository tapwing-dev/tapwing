//! Bounds a restart of the Android driver session in time and stops it early
//! when the failure is one no retry can fix.
//!
//! A restart retries the whole session start (install check, forward,
//! `am instrument`, readiness probe) because after an adb drop the device is
//! often `offline` for a few seconds. Two things keep that from turning into
//! minutes of waiting:
//!
//! * [`classify_start_failure`] tells failures a retry may fix (the device is
//!   not back yet, the driver is slow to answer) from those it cannot (no
//!   driver APK, `adb install` rejected it, the instrumentation crashed).
//! * [`RestartBudget`] caps the time of one restart and of all restarts of a
//!   run. The attempt in flight gets what is left, so the cap holds for the
//!   readiness wait too and not only between attempts.

use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

/// Overrides how long one restart keeps trying to bring the driver back (ms).
pub const RESTART_TIMEOUT_ENV: &str = "TAPWING_NATIVE_RESTART_TIMEOUT_MS";
/// Overrides how long all restarts of one run may take together (ms).
pub const RESTART_BUDGET_ENV: &str = "TAPWING_NATIVE_RESTART_BUDGET_MS";
/// After an adb drop the device is often briefly `offline` or missing while the
/// adb server reconnects, so a restart keeps trying for a while.
const DEFAULT_RESTART_TIMEOUT: Duration = Duration::from_secs(30);
/// The run budget when unset, as a multiple of the per-restart timeout: room
/// for one restart that needs all of its time and for quicker ones around it.
const RUN_BUDGET_FACTOR: u32 = 2;
pub(super) const RESTART_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Whether starting the session again can make a failed start succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StartFailure {
    /// The device or the driver may just not be there yet.
    Retryable,
    /// The same start would fail the same way.
    Permanent,
}

/// Context put on a start error where it is known that no retry can fix it.
#[derive(Debug)]
pub(super) struct Unrecoverable(pub(super) &'static str);

impl fmt::Display for Unrecoverable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The host-side `adb shell am instrument` process ended while the driver was
/// still expected to come up.
#[derive(Debug)]
pub(super) struct InstrumentationExited {
    /// How the child ended, as `ExitStatus` prints it (`exit status: 1`, `signal: 9 (SIGKILL)`).
    pub(super) status: String,
    /// What the child wrote to stdout and stderr.
    pub(super) output: String,
}

impl fmt::Display for InstrumentationExited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the instrumentation exited during startup ({}: {})", self.status, self.output.trim())
    }
}

impl std::error::Error for InstrumentationExited {}

/// Messages the `adb` CLI and server print when the device, not the command,
/// is the problem. They pass on their own once the device is back.
fn is_adb_transport_error(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    ["device offline", "no devices/emulators found", "error: closed", "device unauthorized", "device still authorizing"]
        .iter()
        .any(|m| text.contains(m))
        || text.contains("cannot connect to daemon")
        || (text.contains("device '") && text.contains("' not found"))
}

/// `adb install` output for an APK the package manager (or `adb` itself)
/// refused: `Failure [INSTALL_FAILED_...]`, `INSTALL_PARSE_FAILED_...`, or an
/// APK file that cannot be read.
fn is_install_rejection(text: &str) -> bool {
    text.contains("INSTALL_FAILED_")
        || text.contains("INSTALL_PARSE_FAILED_")
        || text.contains("Failure [")
        || text.contains("failed to stat")
}

/// Sorts a failed session start. Unknown failures are retryable: the budget
/// bounds them anyway, and a restart exists to ride out an unstable device.
pub(super) fn classify_start_failure(err: &anyhow::Error) -> StartFailure {
    if err.downcast_ref::<Unrecoverable>().is_some() {
        return StartFailure::Permanent;
    }
    if let Some(exited) = err.downcast_ref::<InstrumentationExited>() {
        // `adb shell` also ends when the device goes away under it; it then says so, or
        // says nothing at all. A driver that crashed or could not be instrumented leaves
        // `am instrument`'s report behind.
        let lost_device = exited.output.trim().is_empty() || is_adb_transport_error(&exited.output);
        return if lost_device { StartFailure::Retryable } else { StartFailure::Permanent };
    }
    let text = format!("{err:#}");
    if is_install_rejection(&text) && !is_adb_transport_error(&text) {
        return StartFailure::Permanent;
    }
    StartFailure::Retryable
}

fn env_duration(name: &str) -> Option<Duration> {
    std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok()).map(Duration::from_millis)
}

fn secs(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f64())
}

/// Time the restarts of one run may take: `per_restart` for each, `per_run`
/// for all of them together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RestartBudget {
    per_restart: Duration,
    per_run: Duration,
    spent: Duration,
}

impl RestartBudget {
    pub(super) fn new(per_restart: Duration, per_run: Duration) -> Self {
        Self { per_restart, per_run, spent: Duration::ZERO }
    }

    /// [`RESTART_TIMEOUT_ENV`] and [`RESTART_BUDGET_ENV`], else the defaults.
    pub(super) fn from_env() -> Self {
        Self::from_overrides(env_duration(RESTART_TIMEOUT_ENV), env_duration(RESTART_BUDGET_ENV))
    }

    fn from_overrides(per_restart: Option<Duration>, per_run: Option<Duration>) -> Self {
        let per_restart = per_restart.unwrap_or(DEFAULT_RESTART_TIMEOUT);
        Self::new(per_restart, per_run.unwrap_or_else(|| per_restart.saturating_mul(RUN_BUDGET_FACTOR)))
    }

    /// How long the next restart may take: the per-restart cap, or what is
    /// left of the run budget when that is less.
    pub(super) fn allowance(&self) -> Duration {
        self.per_restart.min(self.per_run.saturating_sub(self.spent))
    }

    pub(super) fn charge(&mut self, elapsed: Duration) {
        self.spent = self.spent.saturating_add(elapsed);
    }

    #[cfg(test)]
    pub(super) fn spent(&self) -> Duration {
        self.spent
    }

    /// The limits and what is used of them, for error messages.
    pub(super) fn describe(&self) -> String {
        format!(
            "restart budget: {} per restart, {} per run, {} used",
            secs(self.per_restart),
            secs(self.per_run),
            secs(self.spent)
        )
    }
}

/// Time and cancellation as the restart loop sees them; a seam so the budget
/// arithmetic is tested without waiting.
pub(super) trait RestartClock {
    fn now(&self) -> Instant;
    fn is_cancelled(&self) -> bool;
    /// Waits up to `duration`; `true` when cancelled before or during the wait.
    fn wait(&self, duration: Duration) -> bool;
}

/// Runs `attempt` until it succeeds, fails permanently, `clock` reports a
/// cancellation or the budget's allowance for this restart is used up, and
/// charges the time taken to `budget`. `attempt` is told how much time it has
/// left and must not take longer.
pub(super) fn restart_within_budget<T>(
    budget: &mut RestartBudget,
    clock: &dyn RestartClock,
    interval: Duration,
    attempt: impl FnMut(Duration) -> Result<T>,
) -> Result<T> {
    let allowance = budget.allowance();
    if allowance.is_zero() {
        bail!("no time is left to restart the driver in this run ({})", budget.describe());
    }
    let started = clock.now();
    let outcome = run_attempts(allowance, started, clock, interval, attempt);
    budget.charge(clock.now().saturating_duration_since(started));
    outcome.map_err(|stop| match stop {
        Stop::Cancelled(e) => e,
        Stop::Permanent(e) => e.context("not retried, starting the driver again cannot fix this"),
        Stop::OutOfTime(e) => e.context(format!("gave up restarting the driver ({})", budget.describe())),
    })
}

/// Why the attempts of one restart ended without a session.
enum Stop {
    Cancelled(anyhow::Error),
    Permanent(anyhow::Error),
    OutOfTime(anyhow::Error),
}

fn run_attempts<T>(
    allowance: Duration,
    started: Instant,
    clock: &dyn RestartClock,
    interval: Duration,
    mut attempt: impl FnMut(Duration) -> Result<T>,
) -> Result<T, Stop> {
    let remaining = || allowance.saturating_sub(clock.now().saturating_duration_since(started));
    let mut last = None;
    loop {
        let left = remaining();
        if left.is_zero() {
            break;
        }
        let err = match attempt(left) {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        if clock.is_cancelled() {
            return Err(Stop::Cancelled(err));
        }
        if classify_start_failure(&err) == StartFailure::Permanent {
            return Err(Stop::Permanent(err));
        }
        let left = remaining();
        if left.is_zero() {
            last = Some(err);
            break;
        }
        if clock.wait(left.min(interval)) {
            return Err(Stop::Cancelled(err.context("cancelled")));
        }
        last = Some(err);
    }
    Err(Stop::OutOfTime(last.unwrap_or_else(|| anyhow!("the restart ran out of time before its first attempt"))))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A clock that only moves when an attempt or a wait says so.
    struct FakeClock {
        origin: Instant,
        elapsed: Cell<Duration>,
        cancelled: Cell<bool>,
        waits: Cell<u32>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                origin: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
                cancelled: Cell::new(false),
                waits: Cell::new(0),
            }
        }

        fn advance(&self, by: Duration) {
            self.elapsed.set(self.elapsed.get() + by);
        }
    }

    impl RestartClock for FakeClock {
        fn now(&self) -> Instant {
            self.origin + self.elapsed.get()
        }

        fn is_cancelled(&self) -> bool {
            self.cancelled.get()
        }

        fn wait(&self, duration: Duration) -> bool {
            self.waits.set(self.waits.get() + 1);
            self.advance(duration);
            self.cancelled.get()
        }
    }

    const S: Duration = Duration::from_secs(1);

    fn exited(output: &str) -> anyhow::Error {
        anyhow::Error::new(InstrumentationExited { status: "exit status: 1".into(), output: output.into() })
    }

    #[test]
    fn failures_a_retry_cannot_fix_are_permanent() {
        let permanent = [
            anyhow!("APK not found").context(Unrecoverable("the driver APK is unusable")),
            // The marker is found under later context too.
            anyhow!("sha mismatch").context(Unrecoverable("the driver APK is unusable")).context("starting the driver"),
            exited("INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0"),
            exited("android.util.AndroidException: INSTRUMENTATION_FAILED: dev.tapwing.driver/.TapwingDriverInstrumentation"),
            exited("java.lang.SecurityException: Permission Denial").context("starting the Tapwing Android driver"),
            anyhow!("adb install -r -t -d /k.apk failed: adb: failed to install /k.apk: Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]")
                .context("installing /k.apk"),
            anyhow!("adb install failed: Failure [INSTALL_PARSE_FAILED_NO_CERTIFICATES]"),
            anyhow!("adb install failed: adb: failed to stat /k.apk: No such file or directory"),
        ];
        for err in permanent {
            assert_eq!(classify_start_failure(&err), StartFailure::Permanent, "{err:#}");
        }
    }

    #[test]
    fn a_device_that_is_away_or_slow_is_retryable() {
        let retryable = [
            anyhow!("the driver did not start in time"),
            anyhow!("connecting to ADB server at 127.0.0.1:5037: Connection refused"),
            anyhow!("FAIL: device offline").context("ephemeral adb forward"),
            // The device dropped out under `adb shell am instrument` or under `adb install`.
            exited("adb: device offline"),
            exited("error: device 'emulator-5554' not found"),
            exited("error: closed"),
            exited("  \n"),
            anyhow!("adb install -r -t -d /k.apk failed: adb: device offline").context("installing /k.apk"),
            anyhow!("adb install failed: adb: failed to install /k.apk: error: no devices/emulators found"),
            anyhow!("something nobody has seen before"),
        ];
        for err in retryable {
            assert_eq!(classify_start_failure(&err), StartFailure::Retryable, "{err:#}");
        }
    }

    #[test]
    fn the_allowance_is_the_restart_cap_clamped_to_what_the_run_has_left() {
        let mut budget = RestartBudget::new(30 * S, 70 * S);
        assert_eq!(budget.allowance(), 30 * S);
        budget.charge(30 * S);
        assert_eq!(budget.allowance(), 30 * S);
        budget.charge(25 * S);
        assert_eq!(budget.allowance(), 15 * S, "70 s per run, 55 s used");
        budget.charge(15 * S);
        assert_eq!(budget.allowance(), Duration::ZERO);
        budget.charge(Duration::MAX);
        assert_eq!(budget.allowance(), Duration::ZERO, "an overdrawn budget stays empty");
        assert_eq!(
            RestartBudget::new(30 * S, 60 * S).describe(),
            "restart budget: 30.0 s per restart, 60.0 s per run, 0.0 s used"
        );
    }

    #[test]
    fn the_run_budget_defaults_to_twice_the_restart_timeout() {
        assert_eq!(RestartBudget::from_overrides(None, None), RestartBudget::new(30 * S, 60 * S));
        assert_eq!(RestartBudget::from_overrides(Some(5 * S), None), RestartBudget::new(5 * S, 10 * S));
        assert_eq!(RestartBudget::from_overrides(Some(5 * S), Some(7 * S)), RestartBudget::new(5 * S, 7 * S));
        assert_eq!(RestartBudget::from_overrides(None, Some(S)), RestartBudget::new(30 * S, S));
    }

    #[test]
    fn retries_until_success_and_charges_the_time_taken() {
        let clock = FakeClock::new();
        let mut budget = RestartBudget::new(30 * S, 60 * S);
        let mut given = Vec::new();
        let v = restart_within_budget(&mut budget, &clock, S / 2, |left| {
            given.push(left);
            clock.advance(2 * S);
            if given.len() < 3 { Err(anyhow!("device offline")) } else { Ok(given.len()) }
        })
        .unwrap();
        assert_eq!(v, 3);
        // Each attempt is told what is left: 30 s, then minus a 2 s attempt and a 0.5 s wait.
        assert_eq!(given, vec![30 * S, 30 * S - 5 * S / 2, 25 * S]);
        assert_eq!(budget.spent(), 7 * S, "three 2 s attempts and two 0.5 s waits");
        assert_eq!(clock.waits.get(), 2);
    }

    #[test]
    fn a_healthy_restart_makes_one_attempt_and_never_waits() {
        let clock = FakeClock::new();
        let mut budget = RestartBudget::new(30 * S, 60 * S);
        let mut attempts = 0;
        restart_within_budget(&mut budget, &clock, S / 2, |_| {
            attempts += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!((attempts, clock.waits.get(), budget.spent()), (1, 0, Duration::ZERO));
    }

    #[test]
    fn a_permanent_failure_stops_at_once_with_its_cause() {
        let clock = FakeClock::new();
        let mut budget = RestartBudget::new(30 * S, 60 * S);
        let mut attempts = 0;
        let err = restart_within_budget::<()>(&mut budget, &clock, S / 2, |_| {
            attempts += 1;
            clock.advance(S);
            Err(exited("INSTRUMENTATION_RESULT: shortMsg=Process crashed."))
        })
        .unwrap_err();
        let text = format!("{err:#}");
        assert_eq!(attempts, 1, "{text}");
        assert_eq!(clock.waits.get(), 0);
        assert!(text.starts_with("not retried, starting the driver again cannot fix this: "), "{text}");
        assert!(
            text.contains("the instrumentation exited during startup") && text.contains("Process crashed"),
            "{text}"
        );
        assert_eq!(budget.spent(), S);
    }

    #[test]
    fn one_restart_never_outlasts_its_allowance_and_says_what_the_budget_was() {
        let clock = FakeClock::new();
        let mut budget = RestartBudget::new(30 * S, 60 * S);
        let mut given = Vec::new();
        let err = restart_within_budget::<()>(&mut budget, &clock, S / 2, |left| {
            given.push(left);
            // An attempt that uses all the time it was given, as a readiness wait does.
            clock.advance(left.min(20 * S));
            Err(anyhow!("the driver did not start in time #{}", given.len()))
        })
        .unwrap_err();
        assert_eq!(given, vec![30 * S, 19 * S / 2], "the second attempt only gets the 9.5 s that are left");
        assert_eq!(budget.spent(), 30 * S, "not 40.5 s: the attempt in flight is cut to the allowance");
        let text = format!("{err:#}");
        assert_eq!(
            text,
            "gave up restarting the driver (restart budget: 30.0 s per restart, 60.0 s per run, 30.0 s used): \
             the driver did not start in time #2"
        );
    }

    #[test]
    fn restarts_of_one_run_share_the_run_budget() {
        let clock = FakeClock::new();
        let mut budget = RestartBudget::new(30 * S, 50 * S);
        let never = |clock: &FakeClock, budget: &mut RestartBudget| {
            let mut given = Vec::new();
            let err = restart_within_budget::<()>(budget, clock, S, |left| {
                given.push(left);
                clock.advance(left);
                Err(anyhow!("device offline"))
            })
            .unwrap_err();
            (given, format!("{err:#}"))
        };

        assert_eq!(never(&clock, &mut budget).0, vec![30 * S]);
        // The second restart only gets the 20 s the run has left.
        let (given, text) = never(&clock, &mut budget);
        assert_eq!(given, vec![20 * S]);
        assert!(text.contains("30.0 s per restart, 50.0 s per run, 50.0 s used"), "{text}");
        // The third is refused before it tries anything.
        let (given, text) = never(&clock, &mut budget);
        assert!(given.is_empty(), "{given:?}");
        assert_eq!(
            text,
            "no time is left to restart the driver in this run \
             (restart budget: 30.0 s per restart, 50.0 s per run, 50.0 s used)"
        );
        assert_eq!(budget.spent(), 50 * S, "the worst case of a run is its budget");
    }

    #[test]
    fn a_cancellation_ends_the_restart_without_another_attempt() {
        // Cancelled while an attempt runs: its error comes back as it is.
        let clock = FakeClock::new();
        let mut budget = RestartBudget::new(30 * S, 60 * S);
        let mut attempts = 0;
        let err = restart_within_budget::<()>(&mut budget, &clock, S, |_| {
            attempts += 1;
            clock.cancelled.set(true);
            Err(anyhow!("device offline"))
        })
        .unwrap_err();
        assert_eq!((attempts, clock.waits.get()), (1, 0));
        assert_eq!(err.to_string(), "device offline");

        // Cancelled during the wait between two attempts.
        struct CancelOnWait(FakeClock);
        impl RestartClock for CancelOnWait {
            fn now(&self) -> Instant {
                self.0.now()
            }
            fn is_cancelled(&self) -> bool {
                self.0.is_cancelled()
            }
            fn wait(&self, _: Duration) -> bool {
                self.0.cancelled.set(true);
                true
            }
        }
        let clock = CancelOnWait(FakeClock::new());
        let mut attempts = 0;
        let err = restart_within_budget::<()>(&mut budget, &clock, S, |_| {
            attempts += 1;
            Err(anyhow!("device offline"))
        })
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(format!("{err:#}"), "cancelled: device offline");
    }
}
