//! Starting (and restarting) a native-lane session against a fake ADB server:
//! the install check, the pre-clean, the forward, the `am instrument` command,
//! the readiness wait, the teardown, and the failures a restart must or must
//! not retry.
//!
//! The ADB server is [`FakeAdb`], the driver behind the forward is the fake
//! gRPC driver of the `ops` tests (or a peer that hangs up, like a forward
//! whose driver is not there), and the instrumentation child is a host `sh`
//! standing in for `adb shell am instrument`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use super::client::ScreenSize;
use super::connect::testing::{Peer, serve as serve_peer};
use super::ops::tests::{FakeState, serve as serve_driver};
use super::restart::{StartFailure, classify_start_failure, restart_within_budget};
use super::*;
use crate::device::adb_client::fake::{FakeAdb, Request, ShellReply, sh};

const SERIAL: &str = "fake-serial";
const APK: &str = "/nowhere/tapwing-driver.apk";

const PROBE: &str = "echo TAPWING_MARKER=$(cat /data/local/tmp/tapwing_driver_apk.sha256 2>/dev/null); \
                     pm list packages --user 0 dev.tapwing.driver 2>/dev/null; \
                     echo TAPWING_SDK=$(getprop ro.build.version.sdk)";
const FORCE_STOP: &str = "am force-stop dev.tapwing.driver; \
                          r=$(settings get global tapwing_ime_restore 2>/dev/null); \
                          case \"$r\" in ''|null) ;; *) { \
                          o=${r%%,*}; [ \"$o\" = - ] || ime set \"$o\"; \
                          case \"$r\" in *,1) ;; *) ime disable 'dev.tapwing.driver/.input.TapwingInputMethodService';; esac; \
                          settings delete global tapwing_ime_restore; \
                          } >/dev/null 2>&1;; esac";
const FORWARD: &str = "forward:tcp:0;tcp:7001";

fn sha() -> String {
    "a".repeat(64)
}

fn plan() -> apk::InstallPlan {
    apk::InstallPlan {
        packages: vec![(tapwing_apk::PACKAGE, PathBuf::from(APK))],
        marker: tapwing_apk::DEVICE_MARKER,
        sha256: sha(),
    }
}

/// A device that has this driver APK installed and reports API level `sdk`.
fn device_with_driver(sdk: u32) -> impl Fn(&str) -> ShellReply + Send + Sync + 'static {
    move |script| match script {
        PROBE => ShellReply::ok(format!("TAPWING_MARKER={}\npackage:dev.tapwing.driver\nTAPWING_SDK={sdk}\n", sha())),
        _ => ShellReply::default(),
    }
}

/// What the stand-in for `adb shell am instrument` does.
#[derive(Clone, Copy)]
enum Instrumentation {
    /// Stays up until it is killed, like a driver that serves.
    Runs,
    /// Prints this and exits with status 1.
    Exits(&'static str),
}

struct TestHost {
    apk: Result<apk::InstallPlan, &'static str>,
    /// `adb install` would fail with this; `None` runs the real install check.
    install_error: Option<&'static str>,
    instrumentation: Instrumentation,
    /// The `am instrument` commands asked for.
    commands: Mutex<Vec<String>>,
    attempts: AtomicUsize,
}

impl TestHost {
    fn new(instrumentation: Instrumentation) -> Self {
        Self {
            apk: Ok(plan()),
            install_error: None,
            instrumentation,
            commands: Mutex::new(Vec::new()),
            attempts: AtomicUsize::new(0),
        }
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

impl StartHost for TestHost {
    fn install_plan(&self) -> Result<apk::InstallPlan> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.apk.clone().map_err(|why| anyhow!(why))
    }

    fn ensure_installed(&self, adb: &Adb, plan: &apk::InstallPlan, cancel: &AtomicBool) -> Result<apk::InstallReport> {
        match self.install_error {
            Some(why) => Err(anyhow!(why).context(format!("installing {APK}"))),
            None => apk::ensure_installed(adb, plan, cancel),
        }
    }

    fn spawn_instrumentation(&self, _: &Adb, command: &str, log: fs::File) -> Result<Child> {
        self.commands.lock().unwrap().push(command.to_string());
        let mut sh = sh();
        match self.instrumentation {
            // `cat` on a pipe nobody writes to: up until the guard kills it.
            Instrumentation::Runs => sh.args(["-c", "exec cat"]).stdin(Stdio::piped()),
            Instrumentation::Exits(output) => {
                sh.args(["-c", "printf '%s\\n' \"$1\"; exit 1", "sh", output]).stdin(Stdio::null())
            }
        };
        Ok(sh.stdout(Stdio::from(log.try_clone()?)).stderr(Stdio::from(log)).spawn()?)
    }
}

/// A restart as [`restart_session`] runs it, with `host` in place of the `adb` CLI.
fn restart(
    host: &TestHost,
    adb: &Adb,
    cancel: &CancelSignal,
    budget: &mut RestartBudget,
) -> Result<NativeDriverClient> {
    restart_within_budget(budget, cancel, Duration::from_millis(1), |left| {
        start_session_with(host, adb, cancel, Duration::from_secs(10).min(left))
    })
}

fn roomy_budget() -> RestartBudget {
    RestartBudget::new(Duration::from_secs(30), Duration::from_secs(60))
}

fn count(requests: &[Request], wanted: &Request) -> usize {
    requests.iter().filter(|r| *r == wanted).count()
}

fn shell(script: &str) -> Request {
    Request::Shell(script.to_string())
}

fn host_service(service: impl Into<String>) -> Request {
    Request::Host(service.into())
}

#[test]
fn a_start_checks_the_install_cleans_up_forwards_instruments_and_waits_for_the_driver() {
    let driver = serve_driver(Arc::new(Mutex::new(FakeState::default())));
    let fake = FakeAdb::start(device_with_driver(35)).forward_to(driver.port);
    let adb = Adb::with_client(SERIAL, fake.client());
    let host = TestHost::new(Instrumentation::Runs);
    let cancel = CancelSignal::new();

    let mut client = start_session_with(&host, &adb, &cancel, Duration::from_secs(10)).unwrap();
    assert_eq!(client.device_info().unwrap(), ScreenSize { width: 1080, height: 2400 });

    // One round trip decides the install; the pre-clean and the forward then go out together.
    let started = fake.requests();
    assert_eq!(started.len(), 3, "{started:?}");
    assert_eq!(started[0], shell(PROBE));
    assert!(started[1..].contains(&shell(FORCE_STOP)) && started[1..].contains(&host_service(FORWARD)), "{started:?}");
    assert_eq!(
        host.commands(),
        vec!["am instrument -w -m -e port 7001 dev.tapwing.driver/.TapwingDriverInstrumentation"]
    );
    let log = std::env::temp_dir().join(format!("tapwing-instrument-{}-{}.log", std::process::id(), driver.port));
    assert!(log.exists(), "the instrumentation's output is kept while it runs");

    // Dropping the client stops the driver on the device and removes the forward.
    drop(client);
    let all = fake.requests();
    assert_eq!(all.len(), 5, "{all:?}");
    assert_eq!(count(&all, &shell(FORCE_STOP)), 2);
    assert!(all[3..].contains(&host_service(format!("killforward:tcp:{}", driver.port))), "{all:?}");
    assert!(!log.exists(), "the log is removed with the session");
    fake.assert_scripts_parse();
}

#[test]
fn a_device_before_api_26_is_instrumented_without_the_protobuf_flag() {
    let driver = serve_driver(Arc::new(Mutex::new(FakeState::default())));
    let fake = FakeAdb::start(device_with_driver(24)).forward_to(driver.port);
    let adb = Adb::with_client(SERIAL, fake.client());
    let host = TestHost::new(Instrumentation::Runs);
    start_session_with(&host, &adb, &CancelSignal::new(), Duration::from_secs(10)).unwrap();
    assert_eq!(host.commands(), vec!["am instrument -w -e port 7001 dev.tapwing.driver/.TapwingDriverInstrumentation"]);
    fake.assert_scripts_parse();
}

#[test]
fn an_instrumentation_that_crashes_is_not_restarted_again() {
    // The forward is there but nothing answers behind it, and the instrumentation is gone.
    let (dead_forward, _seen) = serve_peer(Peer::Closes);
    let fake = FakeAdb::start(device_with_driver(35)).forward_to(dead_forward);
    let adb = Adb::with_client(SERIAL, fake.client());
    let host = TestHost::new(Instrumentation::Exits(
        "INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0",
    ));
    let mut budget = roomy_budget();

    let err = restart(&host, &adb, &CancelSignal::new(), &mut budget).err().expect("the start fails");
    assert_eq!(classify_start_failure(&err), StartFailure::RetryOnce);
    let text = format!("{err:#}");
    assert!(text.starts_with("not retried, starting the driver again cannot fix this: "), "{text}");
    assert!(
        text.contains("the instrumentation exited during startup (exit status: 1: INSTRUMENTATION_RESULT")
            && text.contains("Process crashed."),
        "{text}"
    );
    assert_eq!(host.attempts(), 2, "{text}");

    // Both failed attempts still cleaned up after themselves.
    let requests = fake.requests();
    assert_eq!(count(&requests, &shell(FORCE_STOP)), 4, "{requests:?}");
    assert_eq!(count(&requests, &host_service(format!("killforward:tcp:{dead_forward}"))), 2, "{requests:?}");
    fake.assert_scripts_parse();
}

#[test]
fn a_device_that_is_offline_for_a_moment_is_waited_for() {
    let driver = serve_driver(Arc::new(Mutex::new(FakeState::default())));
    let fake = FakeAdb::start(device_with_driver(35)).forward_to(driver.port);
    fake.go_offline_for(1);
    let adb = Adb::with_client(SERIAL, fake.client());
    let host = TestHost::new(Instrumentation::Runs);
    let mut budget = roomy_budget();

    let mut client = restart(&host, &adb, &CancelSignal::new(), &mut budget).unwrap();
    assert_eq!(host.attempts(), 2, "the first attempt found the device offline");
    assert_eq!(host.commands().len(), 1, "only the attempt that reached the device instrumented it");
    assert_eq!(client.device_info().unwrap().width, 1080);
    fake.assert_scripts_parse();
}

#[test]
fn an_apk_the_device_refuses_is_not_installed_again() {
    let fake = FakeAdb::start(device_with_driver(35));
    let adb = Adb::with_client(SERIAL, fake.client());
    let mut host = TestHost::new(Instrumentation::Runs);
    host.install_error = Some(
        "adb install -r -t -d /nowhere/tapwing-driver.apk failed: adb: failed to install \
         /nowhere/tapwing-driver.apk: Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]",
    );
    let mut budget = roomy_budget();

    let text = format!("{:#}", restart(&host, &adb, &CancelSignal::new(), &mut budget).err().expect("fails"));
    assert!(text.starts_with("not retried, starting the driver again cannot fix this: installing "), "{text}");
    assert!(text.contains("INSTALL_FAILED_INSUFFICIENT_STORAGE"), "{text}");
    assert_eq!(host.attempts(), 1);
    assert!(host.commands().is_empty() && fake.requests().is_empty(), "nothing is forwarded or instrumented");

    // The same step failing because the device went away is worth another try.
    host.install_error = Some("adb install -r -t -d /nowhere/tapwing-driver.apk failed: adb: device offline");
    let err = start_session_with(&host, &adb, &CancelSignal::new(), Duration::from_secs(10)).err().expect("fails");
    assert_eq!(classify_start_failure(&err), StartFailure::Retryable, "{err:#}");
}

#[test]
fn a_missing_driver_apk_fails_before_anything_is_sent() {
    let fake = FakeAdb::start(device_with_driver(35));
    let adb = Adb::with_client(SERIAL, fake.client());
    let mut host = TestHost::new(Instrumentation::Runs);
    host.apk = Err("Tapwing's Android driver APK (tapwing-driver.apk) not found");
    let mut budget = roomy_budget();

    let text = format!("{:#}", restart(&host, &adb, &CancelSignal::new(), &mut budget).err().expect("fails"));
    assert_eq!(
        text,
        "not retried, starting the driver again cannot fix this: the driver APK is unusable: \
         Tapwing's Android driver APK (tapwing-driver.apk) not found"
    );
    assert_eq!(host.attempts(), 1);
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
}

#[test]
fn a_driver_that_never_answers_gets_the_time_the_budget_has_and_no_more() {
    let (dead_forward, _seen) = serve_peer(Peer::Closes);
    let fake = FakeAdb::start(device_with_driver(35)).forward_to(dead_forward);
    let adb = Adb::with_client(SERIAL, fake.client());
    let host = TestHost::new(Instrumentation::Runs);
    let cancel = CancelSignal::new();
    let limit = Duration::from_millis(200);
    let mut budget = RestartBudget::new(limit, limit);

    let err = restart(&host, &adb, &cancel, &mut budget).err().expect("the driver never answers");
    assert_eq!(classify_start_failure(&err), StartFailure::Retryable, "a slow driver may still come up");
    let text = format!("{err:#}");
    assert!(
        text.starts_with("gave up restarting the driver (restart budget: 0.2 s per restart, 0.2 s per run, "),
        "{text}"
    );
    assert!(text.contains("the driver did not start in time"), "{text}");
    assert!(budget.spent() >= limit, "the readiness wait ran to the end of the allowance");

    // Each attempt stopped the instrumentation it had started and removed its forward.
    let requests = fake.requests();
    let attempts = host.commands().len();
    assert!(attempts >= 1);
    assert_eq!(count(&requests, &shell(FORCE_STOP)), 2 * attempts, "{requests:?}");
    assert_eq!(count(&requests, &host_service(format!("killforward:tcp:{dead_forward}"))), attempts, "{requests:?}");

    // The run's budget is used up: the next restart is refused without touching the device.
    let before = fake.requests().len();
    let text = format!("{:#}", restart(&host, &adb, &cancel, &mut budget).err().expect("refused"));
    assert!(text.starts_with("no time is left to restart the driver in this run (restart budget: "), "{text}");
    assert_eq!((fake.requests().len(), host.commands().len()), (before, attempts));
    fake.assert_scripts_parse();
}

#[test]
fn a_cancelled_start_does_nothing() {
    let fake = FakeAdb::start(device_with_driver(35));
    let adb = Adb::with_client(SERIAL, fake.client());
    let host = TestHost::new(Instrumentation::Runs);
    let cancel = CancelSignal::new();
    cancel.cancel();
    let err = start_session_with(&host, &adb, &cancel, Duration::from_secs(10)).err().expect("cancelled");
    assert_eq!(err.to_string(), "cancelled");
    assert!(fake.requests().is_empty() && host.commands().is_empty());
}
