//! Routing of a SpringBoard alert through the in-process HID backend while the runner is not started.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::tests::test_opts;
use super::*;
use crate::backend::TapElementRequest;
use crate::device::Device;
use crate::driver::Driver;
use crate::ios_native_lane::hid::HidError;
use crate::ios_native_lane::hid::fake::{ALERT_TREE, Fake, PLAYGROUND_TREE, with_process};
use crate::run::{Opts, Session};

const ALERT_LABEL: &str = "Povolit jen při používání aplikace";

fn miss() -> anyhow::Error {
    anyhow::Error::new(DriverError { code: "ELEMENT_NOT_FOUND".into(), message: "x".into(), details: Value::Null })
}

/// A driver with an app attached, as in a flow of an instrumented Flutter app.
fn connected_driver() -> Driver {
    let mut drv = Driver::listen(0).unwrap();
    let port = drv.port().unwrap();
    std::thread::spawn(move || {
        let mut app = TcpStream::connect(("127.0.0.1", port)).unwrap();
        writeln!(app, r#"{{"event":"hello","params":{{"protocol":1,"pid":77}}}}"#).unwrap();
        let mut reader = BufReader::new(app);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            line.clear();
        }
    });
    drv.accept(Duration::from_secs(2)).unwrap();
    drv
}

fn parts(connected: bool) -> (Device, Driver, Opts) {
    let dev = Device::Ios(crate::device::IosSim::new(crate::device::SimInfo {
        udid: "SIM-UDID".into(),
        name: "iPhone".into(),
    }));
    let drv = if connected { connected_driver() } else { Driver::listen(0).unwrap() };
    (dev, drv, test_opts(InputMode::Auto))
}

fn label_selector() -> ElementSelector {
    ElementSelector { text_regex: Some(ALERT_LABEL.into()), ..Default::default() }
}

fn tap_alert(ops: &mut PlatformNativeOps<'_>) -> Result<Value> {
    ops.tap_on_element(&TapElementRequest {
        selector: label_selector(),
        timeout_ms: 500,
        optional: false,
        long_press: false,
        repeat: None,
        relative_point: None,
        retry_if_no_change: false,
        wait_until_visible: false,
        wait_to_settle_timeout_ms: None,
        app_id: None,
    })
}

/// Runs `body` on a backend of an iOS flow whose slot is lazy and whose HID session is `hid`.
fn with_backend<R>(hid: Option<&Fake>, body: impl FnOnce(&mut DeviceFlutterBackend<'_>) -> R) -> R {
    with_backend_attached(true, hid, body)
}

fn with_backend_attached<R>(
    connected: bool,
    hid: Option<&Fake>,
    body: impl FnOnce(&mut DeviceFlutterBackend<'_>) -> R,
) -> R {
    let (mut dev, mut drv, opts) = parts(connected);
    let mut slot = IosNativeSlot::lazy("SIM-UDID");
    let mut session = Session {
        dev: &mut dev,
        drv: &mut drv,
        opts: &opts,
        units_scale: 1.0,
        native: None,
        ios_native: Some(&mut slot),
    };
    let mut backend = DeviceFlutterBackend::new(&mut session, "dev.tapwing.sampleApp".into());
    backend.ios_hid = hid.map(|f| Arc::clone(&f.session));
    body(&mut backend)
}

fn runner_started(b: &DeviceFlutterBackend<'_>) -> bool {
    b.ios_native.as_deref().is_some_and(IosNativeSlot::is_started)
}

#[test]
fn hid_focus_answers_springboard_and_nothing_else() {
    let cases: [(&str, Result<String, HidError>, bool); 5] = [
        ("SpringBoard", Ok(with_process(ALERT_TREE, "SpringBoard")), true),
        ("an app", Ok(with_process(PLAYGROUND_TREE, "Runner")), false),
        ("a nil read", Err(HidError::Failed("no frontmost application".into())), false),
        ("an empty tree", Ok(r#"{"roots":[]}"#.into()), false),
        ("a dead backend", Err(HidError::Unavailable("gone".into())), false),
    ];
    for (name, snapshot, springboard) in cases {
        let fake = Fake::new(snapshot);
        let got = with_backend(Some(&fake), |b| b.hid_focus());
        let want = springboard.then(|| FocusProbe::External(SPRINGBOARD_BUNDLE_ID.to_string()));
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn flag_off_asks_nothing_and_opens_nothing() {
    let fake = Fake::alert();
    // The session exists but the backend was given none, as with `TAPWING_IOS_HID` unset.
    let got = with_backend(None, |b| (b.hid_focus(), b.ios_focus_probe(Duration::ZERO, false)));
    assert_eq!(got, (None, FocusProbe::Unknown));
    assert_eq!(fake.state.lock().unwrap().opens, 0);
}

#[test]
fn focus_probe_sees_the_alert_without_starting_the_runner() {
    let fake = Fake::alert();
    with_backend(Some(&fake), |b| {
        assert_eq!(b.ios_focus_probe(Duration::ZERO, false), FocusProbe::External(SPRINGBOARD_BUNDLE_ID.to_string()));
        assert!(!runner_started(b));
    });
}

#[test]
fn lazy_lookup_with_an_alert_runs_on_the_hid_surface_after_the_short_probe() {
    let fake = Fake::alert();
    let seen = RefCell::new(Vec::new());
    let res = with_backend(Some(&fake), |b| {
        let res = b.route_selector(
            10_000,
            false,
            |_, ms| {
                seen.borrow_mut().push(ms);
                Err::<&str, _>(miss())
            },
            |ops, _| {
                assert!(matches!(ops, PlatformNativeOps::IosHid(_)), "the runner must not serve this");
                tap_alert(ops).map(|_| "tapped")
            },
        );
        assert!(!runner_started(b), "no xcodebuild for a SpringBoard alert");
        assert_eq!(b.lanes.native, 1);
        res
    });
    assert_eq!(res.unwrap(), "tapped");
    assert_eq!(seen.into_inner(), vec![FLUTTER_PROBE_MS], "no 2 s probe before the HID focus answer");
    assert_eq!(fake.state.lock().unwrap().touches().len(), 1);
}

#[test]
fn lazy_lookup_with_an_app_in_front_keeps_waiting_on_flutter_up_to_the_lazy_probe() {
    let fake = Fake::new(Ok(with_process(PLAYGROUND_TREE, "Runner")));
    let seen = RefCell::new(Vec::new());
    let started = Instant::now();
    let res = with_backend(Some(&fake), |b| {
        let res = b.route_selector(
            1500,
            false,
            |_, ms| {
                seen.borrow_mut().push(ms);
                // The real first wait takes its 400 ms, which the lazy probe then no longer owes.
                std::thread::sleep(Duration::from_millis(60));
                Err::<(), _>(miss())
            },
            |_, _| panic!("no native look for an in-app miss"),
        );
        assert!(!runner_started(b));
        res
    });
    assert!(is_flutter_not_found(&res.unwrap_err()));
    let seen = seen.into_inner();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0], FLUTTER_PROBE_MS);
    assert!((1300..1500).contains(&seen[1]), "the rest of the 1500 ms budget: {seen:?}");
    assert!(started.elapsed() < Duration::from_secs(1), "the fake Flutter lane waits only for its sleeps");
}

#[test]
fn lazy_lookup_with_a_nil_focus_read_behaves_like_without_the_flag() {
    let fake = Fake::new(Err(HidError::Failed("no frontmost application".into())));
    let seen = RefCell::new(Vec::new());
    let found = with_backend(Some(&fake), |b| {
        let found = b.route_selector(
            10_000,
            false,
            |_, ms| {
                seen.borrow_mut().push(ms);
                if seen.borrow().len() == 2 { Ok("found by Flutter") } else { Err(miss()) }
            },
            |_, _| panic!("unknown focus must not reach the native lane"),
        );
        assert!(!runner_started(b));
        found
    });
    assert_eq!(found.unwrap(), "found by Flutter");
    assert_eq!(seen.into_inner()[0], FLUTTER_PROBE_MS);
}

#[test]
fn lazy_lookup_without_the_flag_keeps_the_2000_ms_probe() {
    let seen = RefCell::new(Vec::new());
    let res = with_backend(None, |b| {
        b.route_selector(
            1500,
            false,
            |_, ms| {
                seen.borrow_mut().push(ms);
                Err::<(), _>(miss())
            },
            |_, _| panic!("unreachable"),
        )
    });
    assert!(res.is_err());
    assert_eq!(seen.into_inner(), vec![1500], "one Flutter wait, exactly as before");
}

#[test]
fn assert_not_visible_sees_the_alert_on_the_hid_surface() {
    let fake = Fake::alert();
    let res = with_backend(Some(&fake), |b| {
        let res = b.route_selector(
            5000,
            true,
            |_, _| Ok::<&str, anyhow::Error>("absent in Flutter"),
            |ops, ms| {
                assert!(matches!(ops, PlatformNativeOps::IosHid(_)));
                ops.assert_not_visible(&label_selector(), ms.min(200)).map(|_| "native")
            },
        );
        assert!(!runner_started(b));
        res
    });
    // The alert is still on screen, so the native assertion fails like it would on the runner.
    assert!(res.is_err(), "{res:?}");
}

#[test]
fn hid_surface_failure_before_input_falls_back_but_final_outcomes_do_not() {
    let fake = Fake::alert();
    with_backend(Some(&fake), |b| {
        // A read that failed and sent nothing: `None` means the runner path runs.
        fake.state.lock().unwrap().snapshot = Err(HidError::Failed("timed out".into()));
        assert!(b.try_hid_surface(500, &|ops, _| tap_alert(ops)).is_none());
        // A Maestro outcome ends the command: no second look on the runner.
        fake.state.lock().unwrap().snapshot = Ok(with_process(ALERT_TREE, "SpringBoard"));
        let res = b.try_hid_surface(100, &|ops, ms| {
            ops.assert_visible(&ElementSelector { text_regex: Some("Nope".into()), ..Default::default() }, ms)
        });
        assert!(crate::ui_tree::is_element_not_found(&res.unwrap().unwrap_err()));
        // Typing is not served here and sends nothing: fall back.
        assert!(b.try_hid_surface(100, &|ops, _| ops.input_text("x")).is_none());
    });
}

#[test]
fn hid_input_that_may_have_been_sent_is_not_repeated_on_the_runner() {
    let fake = Fake::alert();
    fake.state.lock().unwrap().input_err = Some(HidError::InputMaybeSent("no acknowledgement".into()));
    with_backend(Some(&fake), |b| {
        let res = b.run_native_selector(500, &|ops, _| tap_alert(ops));
        let err = res.unwrap_err();
        assert!(crate::ios_native_lane::hid::is_input_maybe_sent(&err), "{err:#}");
        assert!(format!("{err:#}").contains("not repeated"), "{err:#}");
        assert!(!runner_started(b), "the runner was never asked to repeat the tap");
    });
    assert_eq!(fake.state.lock().unwrap().touches().len(), 1);
}

#[test]
fn a_started_runner_is_never_bypassed() {
    let fake = Fake::alert();
    let (mut dev, mut drv, opts) = parts(true);
    let client = crate::ios_native_lane::IosNativeClient::connect(1);
    let mut slot = IosNativeSlot::Started(IosNativeLaneHandle::ready_for_test(client));
    let mut session = Session {
        dev: &mut dev,
        drv: &mut drv,
        opts: &opts,
        units_scale: 1.0,
        native: None,
        ios_native: Some(&mut slot),
    };
    let mut backend = DeviceFlutterBackend::new(&mut session, "dev.tapwing.sampleApp".into());
    backend.ios_hid = Some(Arc::clone(&fake.session));
    assert!(backend.try_hid_surface(500, &|ops, _| tap_alert(ops)).is_none());
    assert!(fake.calls().is_empty(), "HID was not touched");
}

#[test]
fn unattached_driver_with_an_alert_goes_straight_to_the_hid_surface() {
    let fake = Fake::alert();
    let res = with_backend_attached(false, Some(&fake), |b| {
        let res = b.route_selector(
            10_000,
            false,
            |_, _| panic!("no driver is attached, so Flutter is not asked"),
            |ops, _| {
                assert!(matches!(ops, PlatformNativeOps::IosHid(_)));
                tap_alert(ops).map(|_| "tapped")
            },
        );
        assert!(!runner_started(b));
        res
    });
    assert_eq!(res.unwrap(), "tapped");
}
