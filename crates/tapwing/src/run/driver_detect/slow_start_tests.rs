//! `launchApp` against a real socket: what a missed handshake does, by what
//! the probe knows about the installed build. The host launch is faked, the
//! deadlines are shortened, and the driver is a thread that dials the real listener.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::tests::{APP, LAUNCHER, android, finished, metrics, temp_cache};
use super::*;
use crate::backend::{FlowDriver, LaunchRequest, is_transport_error};
use crate::device::{Device, IosSim, SimInfo};
use crate::driver::{Driver, LivenessProbe};
use crate::driver_probe::{Verdict, VerdictSource};
use crate::run::hybrid::tests::{serve_static_native, test_opts};
use crate::run::launch::HandshakeTiming;
use crate::run::{IosNativeSlot, Session};

/// The pid `metrics()` reports for the launched process.
const PID: i64 = 123;
const EVIDENCE: &str = "found tapwing_driver_hello_v1 in base.apk: lib/arm64-v8a/libapp.so";

/// Short for fast tests, with room for a loaded machine between `base` and `slow_start`.
const TIMING: HandshakeTiming = HandshakeTiming {
    base: Duration::from_millis(150),
    slow_start: Duration::from_millis(1500),
    liveness_every: Duration::from_millis(30),
};

fn verdict(presence: Presence, dials: bool, fingerprint: Option<&str>) -> Report {
    Report {
        verdict: Verdict { presence, dials, evidence: EVIDENCE.into() },
        source: VerdictSource::Scan,
        fingerprint: fingerprint.map(str::to_string),
        learned_misses: None,
    }
}

fn proven() -> Report {
    verdict(Presence::Present, true, Some("fp"))
}

fn request() -> LaunchRequest {
    LaunchRequest {
        app_id: APP.into(),
        clear_state: false,
        clear_keychain: false,
        stop_app: true,
        permissions: Default::default(),
        launch_arguments: Default::default(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum On {
    Android,
    Ios,
}

/// Runs `body` with a session on `platform` whose native lane could take
/// over: a static UiAutomator server on Android, a lazy XCUITest slot on iOS,
/// which must still be lazy afterwards (no test here may start the runner).
fn with_session(platform: On, liveness: LivenessProbe, body: impl FnOnce(&mut Session<'_>)) {
    let mut drv = Driver::listen(0).unwrap();
    drv.set_liveness_probe(liveness);
    let opts = test_opts(InputMode::Auto);
    match platform {
        On::Android => {
            let (mut native, stop, srv) = serve_static_native(LAUNCHER);
            let mut dev = android();
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: Some(&mut native),
                ios_native: None,
            };
            body(&mut session);
            drop(native);
            let _ = stop.send(());
            srv.join().unwrap();
        }
        On::Ios => {
            let mut sim = IosSim::new(SimInfo { udid: "UDID".into(), name: "iPhone".into() });
            sim.remember_display_name(APP, None);
            let mut dev = Device::Ios(sim);
            let mut slot = IosNativeSlot::lazy("UDID");
            let mut session = Session {
                dev: &mut dev,
                drv: &mut drv,
                opts: &opts,
                units_scale: 1.0,
                native: None,
                ios_native: Some(&mut slot),
            };
            body(&mut session);
            assert!(!slot.is_started(), "the XCUITest runner was started for a build that contains the driver");
        }
    }
}

/// One flow's `launchApp` on `session`, with the probe's answer given.
fn launch(
    session: &mut Session<'_>,
    report: Option<Report>,
    timing: HandshakeTiming,
    check: impl FnOnce(anyhow::Result<Value>, Duration, &mut DeviceFlutterBackend<'_>),
) {
    let mut backend = DeviceFlutterBackend::new(session, APP.into());
    backend.probe_override = report.map(finished);
    backend.launch_override = Some(metrics());
    backend.timing_override = Some(timing);
    let t0 = Instant::now();
    let res = backend.launch_app(&request());
    check(res, t0.elapsed(), &mut backend);
}

/// A fake `tapwing_driver` with pid [`PID`]. It dials once `gate` says so
/// (at once without one), dials again while the host drops it unanswered (the
/// launch drains what dialed in before it), then answers `configure` and
/// `waitIdle` until the host hangs up.
fn fake_app(port: u16, gate: Option<Receiver<()>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        if let Some(gate) = gate
            && gate.recv_timeout(Duration::from_secs(20)).is_err()
        {
            return;
        }
        for _ in 0..500 {
            let Ok(mut app) = TcpStream::connect(("127.0.0.1", port)) else { return };
            if writeln!(app, r#"{{"event":"hello","params":{{"protocol":1,"pid":{PID}}}}}"#).is_err() {
                return;
            }
            let mut reader = BufReader::new(app.try_clone().unwrap());
            let mut served = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                served = true;
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].as_u64().unwrap();
                let reply = if req["method"] == "configure" {
                    format!(r#"{{"id":{id},"result":{{"pixelsPerUnit":1}},"us":1}}"#)
                } else {
                    format!(r#"{{"id":{id},"result":{{"idle":true}},"us":1}}"#)
                };
                if writeln!(app, "{reply}").is_err() {
                    break;
                }
            }
            if served {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}

fn alive() -> LivenessProbe {
    Box::new(|_| Some(true))
}

/// A liveness probe that says alive and tells `started` each time it is
/// asked. The launch asks first when the usual wait has run out, so a fake app
/// gated on it dials only during the longer wait, however loaded the machine is.
fn alive_and_signal(started: Sender<()>) -> LivenessProbe {
    Box::new(move |_| {
        let _ = started.send(());
        Some(true)
    })
}

#[test]
fn a_late_handshake_of_a_proven_build_stays_on_the_flutter_lane() {
    for platform in [On::Android, On::Ios] {
        let (started, gate) = channel();
        let mut app = None;
        with_session(platform, alive_and_signal(started), |session| {
            app = Some(fake_app(session.drv.port().unwrap(), Some(gate)));
            let timing = HandshakeTiming { slow_start: Duration::from_secs(20), ..TIMING };
            launch(session, Some(proven()), timing, |res, waited, backend| {
                let res = res.unwrap_or_else(|e| panic!("{platform:?}: {e:#}"));
                assert_eq!(res["hello"]["pid"], PID, "{platform:?}: {res}");
                assert!(res.get("blackbox").is_none() && res.get("lane").is_none(), "{platform:?}: {res}");
                assert!(!backend.ran_blackbox(), "{platform:?}");
                assert!(backend.drv.is_connected(), "{platform:?}");
                assert!(waited >= TIMING.base && waited < Duration::from_secs(15), "{platform:?}: {waited:?}");
                let probe = &res["driverProbe"];
                assert_eq!(
                    (probe["verdict"].as_str(), probe["source"].as_str(), probe["slowStartWait"].as_str()),
                    (Some("present"), Some("scan"), Some("used")),
                    "{platform:?}: {res}"
                );
                assert_eq!(probe["evidence"], EVIDENCE);
                assert!(probe["handshakeWaitedMs"].as_u64().unwrap() >= 150, "{platform:?}: {res}");
                assert!(probe.get("connected").is_none(), "{platform:?}: {res}");
                let warning = res["warning"].as_str().unwrap_or_default();
                assert!(warning.contains("past the usual 150 ms") && warning.contains(EVIDENCE), "{warning}");
                assert_eq!(backend.lane_stats().cold_start_retries, 1, "the slow start is counted");
                assert!(res["phases"]["acceptMs"].as_f64().unwrap() >= 150.0, "{res}");
                assert!(backend.take_failed_step_details().is_none());
                assert!(!backend.drv.slow_start_spent(APP));
                assert!(!backend.is_sticky_blackbox_app(APP));
            });
        });
        app.unwrap().join().unwrap();
    }
}

#[test]
fn a_proven_build_that_never_connects_fails_the_launch_and_the_next_flow_tries_the_driver_again() {
    for platform in [On::Android, On::Ios] {
        let mut app = None;
        with_session(platform, alive(), |session| {
            // Flow 1: the whole slow-start wait, then a failure that names the cause.
            launch(session, Some(proven()), TIMING, |res, waited, backend| {
                let err = res.expect_err("a build with the driver must not be driven black-box");
                let text = format!("{err:#}");
                for part in [
                    "launchApp dev.example.app: the installed build contains tapwing_driver",
                    EVIDENCE,
                    "never connected",
                    "no handshake within 1500 ms",
                    "the app process 123 is running",
                    "--input native",
                    "docs/troubleshooting.md",
                ] {
                    assert!(text.contains(part), "{platform:?}: missing {part:?} in {text}");
                }
                assert!(!text.contains("not repeated"), "{text}");
                assert!(!is_transport_error(&err), "a live app is no crash: {text}");
                assert!(waited >= TIMING.slow_start && waited < Duration::from_secs(15), "{platform:?}: {waited:?}");
                assert!(!backend.ran_blackbox() && !backend.is_sticky_blackbox_app(APP), "{platform:?}");
                assert_eq!(backend.blackbox(), crate::run::Blackbox::default());
                assert_eq!(backend.lane_stats().cold_start_retries, 1);
                assert_eq!(backend.lane_stats().native, 0, "the native lane ran nothing");
                let details = backend.take_failed_step_details().expect("the failed step carries the probe");
                assert_eq!(details["pid"], PID);
                let probe = &details["driverProbe"];
                assert_eq!(
                    (probe["verdict"].as_str(), probe["slowStartWait"].as_str(), probe["evidence"].as_str()),
                    (Some("present"), Some("used"), Some(EVIDENCE)),
                    "{details}"
                );
                assert_eq!((&probe["connected"], &probe["processAlive"]), (&Value::Bool(false), &Value::Bool(true)));
                assert!(probe["handshakeWaitedMs"].as_u64().unwrap() >= 1500, "{details}");
                assert!(backend.take_failed_step_details().is_none(), "handed out once");
                assert!(backend.drv.slow_start_spent(APP));
            });

            // Flow 2: the driver is tried again, with the short wait only, and the error says why.
            launch(session, Some(proven()), TIMING, |res, waited, backend| {
                let text = format!("{:#}", res.expect_err("still no black-box"));
                for part in ["no handshake within 150 ms", "(1500 ms) was not repeated", "already waited that long"] {
                    assert!(text.contains(part), "{platform:?}: missing {part:?} in {text}");
                }
                assert!(waited >= TIMING.base && waited < TIMING.slow_start, "{platform:?}: {waited:?}");
                assert!(!backend.ran_blackbox() && !backend.is_sticky_blackbox_app(APP), "{platform:?}");
                assert_eq!(backend.lane_stats().cold_start_retries, 0, "no longer wait was taken");
                let details = backend.take_failed_step_details().unwrap();
                assert_eq!(details["driverProbe"]["slowStartWait"], "notRepeated", "{details}");
            });

            // Flow 3: the app dials in time, so the flow is on the Flutter lane and the long wait is back.
            app = Some(fake_app(session.drv.port().unwrap(), None));
            launch(session, Some(proven()), TIMING, |res, _, backend| {
                let res = res.unwrap_or_else(|e| panic!("{platform:?}: {e:#}"));
                assert_eq!(res["hello"]["pid"], PID);
                assert!(res.get("driverProbe").is_none() && res.get("warning").is_none(), "{res}");
                assert!(!backend.ran_blackbox());
                assert_eq!(backend.lane_stats().cold_start_retries, 0);
                assert!(!backend.drv.slow_start_spent(APP), "a handshake restores the longer wait");
            });
        });
        app.unwrap().join().unwrap();
    }
}

#[test]
fn a_proven_build_whose_process_dies_fails_at_once_as_a_crash() {
    // (liveness answers in order, then the last one forever; longer wait taken?)
    let cases: [(&[Option<bool>], u32); 2] = [(&[Some(true), Some(true), Some(false)], 1), (&[Some(false)], 0)];
    for platform in [On::Android, On::Ios] {
        for (answers, slow_starts) in cases {
            let mut asked = 0usize;
            let answers = answers.to_vec();
            let liveness: LivenessProbe = Box::new(move |pid| {
                assert_eq!(pid, PID);
                let answer = answers[asked.min(answers.len() - 1)];
                asked += 1;
                answer
            });
            with_session(platform, liveness, |session| {
                let timing = HandshakeTiming { slow_start: Duration::from_secs(60), ..TIMING };
                launch(session, Some(proven()), timing, |res, waited, backend| {
                    let err = res.expect_err("a dead app cannot connect");
                    let text = format!("{err:#}");
                    assert!(is_transport_error(&err), "the crash diagnosis runs on transport errors: {text}");
                    assert!(text.contains("the app process 123 died before tapwing_driver connected"), "{text}");
                    assert!(text.contains(EVIDENCE), "{text}");
                    assert!(waited < Duration::from_secs(15), "{platform:?}: the wait did not end at once: {waited:?}");
                    assert_eq!(backend.lane_stats().cold_start_retries, slow_starts);
                    assert!(!backend.ran_blackbox() && !backend.is_sticky_blackbox_app(APP));
                    assert!(!backend.drv.slow_start_spent(APP), "a crash says nothing about a slow start");
                    let details = backend.take_failed_step_details().unwrap();
                    assert_eq!(details["driverProbe"]["processAlive"], false, "{details}");
                });
            });
        }
    }
}

#[test]
fn a_build_not_proven_to_dial_still_falls_back_and_is_demoted_after_two_misses() {
    let (cache, dir) = temp_cache("slow-start-unknown");
    let key = InstallKey { device: "unreachable".into(), app: APP.into(), fingerprint: "fp".into() };
    with_session(On::Android, alive(), |session| {
        let unknown = || Some(verdict(Presence::Unknown, false, Some("fp")));
        let miss = |session: &mut Session<'_>, sticky: bool| {
            let mut backend = DeviceFlutterBackend::new(session, APP.into());
            backend.probe_override = unknown().map(finished);
            backend.launch_override = Some(metrics());
            backend.timing_override = Some(TIMING);
            backend.cache_override = Some(cache.clone());
            let t0 = Instant::now();
            let res = backend.launch_app(&request()).unwrap();
            let waited = t0.elapsed();
            assert_eq!((&res["blackbox"], &res["lane"]), (&Value::Bool(true), &Value::from("native")), "{res}");
            let note = res["note"].as_str().unwrap();
            assert!(note.starts_with("no tapwing_driver handshake (app did not connect within 150 ms"), "{note}");
            assert!(res.get("driverProbe").is_none(), "{res}");
            assert!(waited >= TIMING.base && waited < TIMING.slow_start, "no longer wait: {waited:?}");
            assert!(backend.ran_blackbox());
            assert_eq!(backend.blackbox().reason, None);
            assert_eq!(backend.is_sticky_blackbox_app(APP), sticky);
            assert_eq!(backend.lane_stats().cold_start_retries, 0);
            assert!(backend.take_failed_step_details().is_none());
            assert!(!backend.drv.slow_start_spent(APP));
        };
        miss(session, false);
        miss(session, true);

        // Flow 3: sticky, so no wait and no driver.
        launch(session, None, HandshakeTiming { base: Duration::from_secs(30), ..TIMING }, |res, waited, backend| {
            let res = res.unwrap();
            assert_eq!(res["note"], "app demoted to black-box after consecutive missed handshakes");
            assert!(waited < Duration::from_secs(10), "{waited:?}");
            assert!(backend.ran_blackbox());
        });
    });
    // Both misses were learned against the install, as before.
    assert_eq!(cache.lookup(&key).and_then(|e| e.learned_misses(cache.now())), Some(2));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_build_that_links_the_driver_without_a_dial_path_falls_back_as_before() {
    with_session(On::Android, alive(), |session| {
        launch(session, Some(verdict(Presence::Present, false, Some("fp"))), TIMING, |res, waited, backend| {
            let res = res.unwrap();
            assert_eq!(res["blackbox"], true, "{res}");
            assert!(waited < TIMING.slow_start, "{waited:?}");
            assert!(backend.ran_blackbox() && !backend.is_sticky_blackbox_app(APP));
        });
    });
}

#[test]
fn a_build_without_the_driver_still_skips_the_wait_at_once() {
    with_session(On::Android, alive(), |session| {
        let timing = HandshakeTiming { base: Duration::from_secs(30), ..TIMING };
        launch(session, Some(verdict(Presence::Absent, false, Some("fp"))), timing, |res, waited, backend| {
            let res = res.unwrap();
            assert!(waited < Duration::from_secs(10), "the wait was not skipped: {waited:?}");
            assert_eq!((&res["blackbox"], &res["lane"]), (&Value::Bool(true), &Value::from("native")), "{res}");
            assert_eq!(res["driverProbe"]["verdict"], "absent");
            assert_eq!(res["driverProbe"]["handshakeWaitSkippedMs"], 15_000);
            assert!(
                backend.blackbox().reason.is_some_and(|r| r.starts_with("no tapwing_driver in the installed build"))
            );
        });
    });
}

#[test]
fn a_launch_that_could_not_read_the_pid_looks_the_process_up_when_the_wait_runs_out() {
    use crate::device::adb_client::fake::{FakeAdb, ShellReply};
    use crate::device::{Android, LaunchMetrics};
    let no_pid = || LaunchMetrics { pid: None, ..metrics() };
    struct Case {
        pidof: &'static str,
        launch_pid: Option<i64>,
        /// The pid and the liveness the launch goes on with.
        expected: (Option<i64>, Option<bool>),
        asks_pidof: bool,
    }
    let cases = [
        Case { pidof: "4711\n", launch_pid: None, expected: (Some(4711), Some(true)), asks_pidof: true },
        Case { pidof: "", launch_pid: None, expected: (None, Some(false)), asks_pidof: true },
        Case { pidof: "4711\n", launch_pid: Some(PID), expected: (Some(PID), Some(true)), asks_pidof: false },
    ];
    for Case { pidof, launch_pid, expected, asks_pidof } in cases {
        let fake = FakeAdb::start(move |cmd| ShellReply::ok(if cmd.contains("pidof") { pidof } else { "" }));
        let mut dev = Device::Android(Android::with_adb_client("emulator-5554".into(), fake.client()));
        let mut drv = Driver::listen(0).unwrap();
        drv.set_liveness_probe(alive());
        let opts = test_opts(InputMode::Auto);
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
        let launch = LaunchMetrics { pid: launch_pid, ..no_pid() };
        assert_eq!(backend.launched_process(APP, &launch), expected, "pidof={pidof:?} launch={launch_pid:?}");
        assert_eq!(!fake.requests().is_empty(), asks_pidof, "pidof={pidof:?} launch={launch_pid:?}");
    }

    // The iOS launch always knows the pid; without one nothing can be said.
    let mut dev = Device::Ios(IosSim::new(SimInfo { udid: "UDID".into(), name: "iPhone".into() }));
    let mut drv = Driver::listen(0).unwrap();
    drv.set_liveness_probe(alive());
    let opts = test_opts(InputMode::Auto);
    let mut session =
        Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
    let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
    assert_eq!(backend.launched_process(APP, &no_pid()), (None, None));
}
