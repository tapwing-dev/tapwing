use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::fake::{ALERT_TREE, Fake, LANDSCAPE_TREE, PLAYGROUND_TREE, with_process};
use super::tree::{self, FrontApp};
use super::*;
use crate::backend::TapElementRequest;
use crate::flow::ir::ElementSelector;
use crate::ios_native_lane::IosNativeClient;
use crate::ios_native_lane::client::mock::{MockRunner, Req, Resp};
use crate::ios_native_lane::hierarchy::IosHierarchy;
use crate::native_core::ops::NativeHierarchy;

const ALERT_RUNNER_TREE: &str = include_str!("fixtures/alert.runner.json");
const ALLOW_WHILE_USING: &str = "Povolit jen při používání aplikace";

fn label(t: &str) -> ElementSelector {
    ElementSelector { text_regex: Some(t.into()), ..Default::default() }
}

fn hierarchy_of(tree_json: &str) -> IosHierarchy {
    let mapped = tree::map_snapshot(tree_json).unwrap();
    IosHierarchy::parse(&mapped.document, 440, 956).unwrap()
}

fn center(h: &IosHierarchy, text: &str) -> (i32, i32) {
    h.find(&label(text)).unwrap().unwrap_or_else(|| panic!("{text} not found")).bounds.unwrap().center()
}

fn tap_req(sel: ElementSelector) -> TapElementRequest {
    TapElementRequest {
        selector: sel,
        timeout_ms: 1000,
        optional: false,
        long_press: false,
        repeat: None,
        relative_point: None,
        retry_if_no_change: false,
        wait_until_visible: false,
        wait_to_settle_timeout_ms: None,
        app_id: None,
    }
}

// Tree mapping

#[test]
fn alert_label_resolves_to_the_same_tap_point_as_the_runner_tree() {
    let probe = hierarchy_of(ALERT_TREE);
    let runner = IosHierarchy::parse_str(ALERT_RUNNER_TREE, 440, 956).unwrap();
    assert_eq!(center(&probe, ALLOW_WHILE_USING), (220, 570));
    assert_eq!(center(&runner, ALLOW_WHILE_USING), (220, 570));
    for other in ["Povolit jednou", "Nepovolovat"] {
        assert_eq!(center(&probe, other), center(&runner, other), "{other}");
    }
    assert!(probe.find(&label("Chcete aplikaci Sample App povolit přístup k.polohovým údajům\\?")).unwrap().is_some());
}

#[test]
fn non_alert_tree_keeps_labels_ids_and_frames() {
    let h = hierarchy_of(PLAYGROUND_TREE);
    assert_eq!(center(&h, "Request Location Permission"), (220, 594));
    let id = ElementSelector { id_regex: Some("request_location_permission".into()), ..Default::default() };
    assert_eq!(h.find(&id).unwrap().unwrap().bounds.unwrap().center(), (220, 594));
    assert!(h.find(&label(ALLOW_WHILE_USING)).unwrap().is_none(), "no alert on this screen");
    // The status bar of SpringBoard is a second root under the same document root.
    assert!(h.find(&label("14:15")).unwrap().is_some());
}

#[test]
fn roles_map_to_the_types_the_selector_engine_reads() {
    let tree = json!({ "roots": [{
        "role": "AXApplication", "label": "App", "frame": [0, 0, 440, 956], "enabled": 1, "focused": 0, "selected": 0,
        "children": [
            { "role": "AXSwitch", "label": "Wi-Fi", "value": 1, "frame": [0, 0, 50, 30], "enabled": 1, "focused": 0, "selected": 0, "children": [] },
            { "role": "AXSwitch", "label": "Bluetooth", "value": 0, "frame": [0, 40, 50, 30], "enabled": 1, "focused": 0, "selected": 0, "children": [] },
            { "role": "AXTextField", "subrole": "AXSearchField", "label": "Search", "value": "Hledat", "placeholder": "Hledat",
              "frame": [0, 80, 200, 30], "enabled": 1, "focused": 1, "selected": 0, "children": [] },
            { "role": "AXTextField", "label": "Email", "value": "a@b.cz", "placeholder": "Email", "identifier": "email_input",
              "frame": [0, 120, 200, 30], "enabled": 0, "focused": 0, "selected": 1, "children": [] },
        ]
    }] })
    .to_string();
    let h = hierarchy_of(&tree);
    let checked = ElementSelector { checked: Some(true), ..Default::default() };
    assert_eq!(h.find(&checked).unwrap().unwrap().accessibility_text.as_deref(), Some("Wi-Fi"));
    // An empty UIKit search field reports its placeholder as the value, the runner reports none.
    let search = h.find(&label("Search")).unwrap().unwrap();
    assert_eq!((search.text.clone(), search.hint_text.as_deref(), search.focused), (None, Some("Hledat"), Some(true)));
    // A typed value that differs from the placeholder stays.
    let email = h.find(&label("Email")).unwrap().unwrap();
    assert_eq!((email.text.as_deref(), email.id.as_deref()), (Some("a@b.cz"), Some("email_input")));
    assert_eq!((email.enabled, email.selected), (Some(false), Some(true)));
}

#[test]
fn unreadable_snapshots_are_call_failures() {
    for bad in ["not json", "{}", r#"{"roots":[]}"#] {
        assert!(matches!(tree::map_snapshot(bad), Err(HidError::Failed(_))), "{bad}");
    }
}

// Focus answer

#[test]
fn front_app_tells_springboard_from_an_app() {
    let sb = with_process(ALERT_TREE, "SpringBoard");
    assert_eq!(tree::front_app(&sb), FrontApp::SpringBoard);
    assert_eq!(tree::front_app(&with_process(PLAYGROUND_TREE, "Runner")), FrontApp::Other);
    // SpringBoard in a rotated interface is not served: the surface is portrait only.
    assert_eq!(tree::front_app(&with_process(LANDSCAPE_TREE, "SpringBoard")), FrontApp::Other);
    // No process name (an old shim) or no tree: do not guess.
    assert_eq!(tree::front_app(ALERT_TREE), FrontApp::Unknown);
    assert_eq!(tree::front_app("{}"), FrontApp::Unknown);
    assert_eq!(tree::front_app("garbage"), FrontApp::Unknown);
}

#[test]
fn session_front_app_reports_answers_nil_and_backend_errors() {
    let f = Fake::new(Ok(with_process(ALERT_TREE, "SpringBoard")));
    assert_eq!(f.session.front_app(), Ok(FrontApp::SpringBoard));
    let f = Fake::new(Ok(with_process(PLAYGROUND_TREE, "Runner")));
    assert_eq!(f.session.front_app(), Ok(FrontApp::Other));
    let f = Fake::new(Ok(r#"{"roots":[]}"#.into()));
    assert_eq!(f.session.front_app(), Ok(FrontApp::Unknown));
    // The shim fails with "no frontmost application" when the read is nil.
    let f = Fake::new(Err(HidError::Failed("no frontmost application".into())));
    assert_eq!(f.session.front_app(), Err(HidError::Failed("no frontmost application".into())));
    assert!(f.session.is_active(), "a failed read is not an unavailable backend");
    assert!(f.warnings.lock().unwrap().is_empty());
}

// Session policy

#[test]
fn flag_off_builds_and_opens_nothing() {
    let built = AtomicUsize::new(0);
    let none = session_if(false, || {
        built.fetch_add(1, Ordering::Relaxed);
        Fake::alert().session
    });
    assert!(none.is_none());
    assert_eq!(built.load(Ordering::Relaxed), 0);
    // A session that is never called never opens its backend either.
    let f = Fake::alert();
    assert_eq!(f.state.lock().unwrap().opens, 0);
    let on = session_if(true, || Arc::clone(&f.session));
    assert!(on.is_some());
    assert_eq!(f.state.lock().unwrap().opens, 0, "opening is lazy");
}

#[test]
fn unavailable_backend_warns_once_and_switches_hid_off() {
    let f = Fake::alert();
    f.state.lock().unwrap().open_err = Some(HidError::Unavailable("SimulatorKit is missing".into()));
    assert!(matches!(f.session.screen(), Err(HidError::Unavailable(_))));
    assert!(!f.session.is_active());
    for _ in 0..3 {
        assert!(matches!(f.session.touch(&[(1.0, 1.0)], 0, 0), Err(HidError::Unavailable(_))));
        assert!(f.session.frame_png().is_err());
    }
    let warnings = f.warnings.lock().unwrap().clone();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("warning: ") && warnings[0].contains("SimulatorKit is missing"), "{warnings:?}");
    assert_eq!(f.state.lock().unwrap().opens, 1, "an unavailable backend is not opened again");
    assert!(f.calls().is_empty());
}

#[test]
fn a_failed_call_does_not_switch_hid_off() {
    let f = Fake::alert();
    f.state.lock().unwrap().snapshot = Err(HidError::Failed("timed out".into()));
    assert!(f.session.snapshot(&Snapshot { max_depth: 0, timeout_s: 1.0 }).is_err());
    assert!(f.session.is_active());
    assert!(f.session.touch(&[(5.0, 6.0)], 10, 0).is_ok());
    assert!(f.warnings.lock().unwrap().is_empty());
    assert_eq!(f.state.lock().unwrap().opens, 1);
}

#[test]
fn forced_failures_hit_exactly_their_route() {
    assert_eq!(FailPoint::parse("open"), Some(FailPoint::Open));
    assert_eq!(FailPoint::parse(" touch "), Some(FailPoint::Touch));
    assert_eq!(FailPoint::parse("bogus"), None);

    let open = Fake::with_fail(Ok(ALERT_TREE.into()), Some(FailPoint::Open));
    assert!(matches!(open.session.screen(), Err(HidError::Unavailable(_))));
    assert_eq!(open.warnings.lock().unwrap().len(), 1);
    assert_eq!(open.state.lock().unwrap().opens, 0, "forced before the opener ran");

    let snap = Fake::with_fail(Ok(ALERT_TREE.into()), Some(FailPoint::Snapshot));
    assert!(matches!(snap.session.front_app(), Err(HidError::Failed(_))));
    assert!(snap.session.touch(&[(1.0, 2.0)], 0, 0).is_ok());
    assert!(snap.session.frame_png().is_ok());

    let touch = Fake::with_fail(Ok(ALERT_TREE.into()), Some(FailPoint::Touch));
    let err = touch.session.touch(&[(1.0, 2.0)], 0, 0).unwrap_err();
    assert!(matches!(err, HidError::Failed(_)) && !err.input_maybe_sent(), "nothing was sent: {err}");
    assert!(touch.state.lock().unwrap().touches().is_empty(), "the backend never saw the touch");
    assert!(touch.session.frame_png().is_ok());

    let frame = Fake::with_fail(Ok(ALERT_TREE.into()), Some(FailPoint::Frame));
    assert!(matches!(frame.session.screen_static(), Err(HidError::Failed(_))));
    assert!(frame.session.touch(&[(1.0, 2.0)], 0, 0).is_ok());
}

#[test]
fn shim_errors_are_classified_by_their_prefix() {
    use super::ffi::classify_shim_error;
    assert_eq!(
        classify_shim_error("[unavailable] no simulator with UDID X"),
        HidError::Unavailable("no simulator with UDID X".into())
    );
    assert_eq!(
        classify_shim_error("[sent] the simulator did not acknowledge the input within 5s"),
        HidError::InputMaybeSent("the simulator did not acknowledge the input within 5s".into())
    );
    assert_eq!(classify_shim_error("accessibility read failed"), HidError::Failed("accessibility read failed".into()));
}

#[test]
fn screen_static_compares_two_frames() {
    let f = Fake::alert();
    assert_eq!(f.session.screen_static(), Ok(true));
    f.state.lock().unwrap().set_frames(vec![Ok(b"a".to_vec()), Ok(b"b".to_vec())]);
    assert_eq!(f.session.screen_static(), Ok(false));
}

#[test]
fn swipe_path_runs_from_start_to_end_in_16_ms_steps() {
    let (path, step) = swipe_path((10.0, 800.0), (10.0, 100.0), 320);
    assert_eq!(path.len(), 21);
    assert_eq!((path[0], path[20]), ((10.0, 800.0), (10.0, 100.0)));
    assert_eq!(step, 16);
    let (short, _) = swipe_path((0.0, 0.0), (10.0, 0.0), 1);
    assert_eq!(short.len(), 3, "at least two segments");
    assert_eq!(hold_ms(3.0), 3000);
    assert_eq!(hold_ms(-1.0), 0);
}

// The runner-less surface

#[test]
fn surface_taps_the_alert_button_and_survives_the_screen_change() {
    let f = Fake::alert();
    let mut ops = HidOps::new(&f.session);
    let res = ops.tap_on_element(&tap_req(label(ALLOW_WHILE_USING))).unwrap();
    assert_eq!((res["x"].as_i64(), res["y"].as_i64()), (Some(220), Some(570)));
    assert!(ops.input_sent());
    let touches = f.state.lock().unwrap().touches().into_iter().cloned().collect::<Vec<_>>();
    assert_eq!(touches, vec!["touch n=1 first=220,570 hold=10 step=0".to_string()]);
    // The app is in front afterwards; the alert label is gone.
    ops.assert_not_visible(&label(ALLOW_WHILE_USING), 200).unwrap();
}

#[test]
fn surface_reports_a_missing_element_as_not_found() {
    let f = Fake::alert();
    let mut ops = HidOps::new(&f.session);
    let err = ops.assert_visible(&label("Nope"), 100).unwrap_err();
    assert!(crate::ui_tree::is_element_not_found(&err));
    assert!(surface::is_final(&err, ops.input_sent()), "a Maestro outcome is not retried on the runner");
    assert!(f.state.lock().unwrap().touches().is_empty());
}

#[test]
fn surface_refuses_a_rotated_interface_without_sending_anything() {
    let f = Fake::new(Ok(with_process(LANDSCAPE_TREE, "SpringBoard")));
    let mut ops = HidOps::new(&f.session);
    let err = ops.assert_visible(&label("Selectors & Taps.*"), 100).unwrap_err();
    assert!(matches!(err.downcast_ref::<HidError>(), Some(HidError::Unsupported(_))), "{err:#}");
    assert!(!surface::is_final(&err, ops.input_sent()), "the runner may serve it");
}

#[test]
fn surface_failing_before_input_may_fall_back() {
    let f = Fake::new(Err(HidError::Failed("no frontmost application".into())));
    let mut ops = HidOps::new(&f.session);
    let err = ops.tap_on_element(&tap_req(label(ALLOW_WHILE_USING))).unwrap_err();
    assert!(!ops.input_sent());
    assert!(!surface::is_final(&err, ops.input_sent()), "{err:#}");
    assert!(f.state.lock().unwrap().touches().is_empty());
}

#[test]
fn surface_never_repeats_input_that_may_have_been_sent() {
    let f = Fake::alert();
    f.state.lock().unwrap().input_err = Some(HidError::InputMaybeSent("no acknowledgement".into()));
    let mut ops = HidOps::new(&f.session);
    let err = ops.tap_on_element(&tap_req(label(ALLOW_WHILE_USING))).unwrap_err();
    assert!(ops.input_sent());
    assert!(is_input_maybe_sent(&err));
    assert!(surface::is_final(&err, ops.input_sent()));
}

#[test]
fn surface_answers_an_unreadable_screen_after_a_tap_with_an_empty_tree() {
    let f = Fake::alert();
    // After the tap the frontmost read is nil (the accessibility flags can be 0 once an app is in front).
    f.state.lock().unwrap().snapshot_after_touch = None;
    let state = Arc::clone(&f.state);
    let mut ops = HidOps::new(&f.session);
    let sel = label(ALLOW_WHILE_USING);
    let (node, hier) = crate::native_core::ops::poll_find(&mut ops, &sel, 500).unwrap();
    assert!(node.bounds.is_some());
    state.lock().unwrap().snapshot = Err(HidError::Failed("no frontmost application".into()));
    // The tap itself succeeded, so the failed read must not fail the command.
    crate::native_core::ops::perform_tap(&mut ops, (220, 570), false, None, false, Some(hier), None).unwrap();
    assert!(ops.input_sent());
}

#[test]
fn surface_does_not_serve_typing() {
    let f = Fake::alert();
    let ops = HidOps::new(&f.session);
    let err = ops.unsupported::<()>("typing").unwrap_err();
    assert!(matches!(err.downcast_ref::<HidError>(), Some(HidError::Unsupported("typing"))));
    assert!(!surface::is_final(&err, false));
}

// Client level, with the runner up

struct Rig {
    runner: MockRunner,
    client: IosNativeClient,
    fake: Fake,
}

fn rig(portrait: bool) -> Rig {
    let runner = MockRunner::start(Box::new(move |req: &Req| match req.path() {
        "/deviceInfo" => {
            let (w, h) = if portrait { (440, 956) } else { (956, 440) };
            Resp::json(&format!(
                r#"{{"widthPixels":{},"heightPixels":{},"widthPoints":{w},"heightPoints":{h}}}"#,
                w * 3,
                h * 3
            ))
        }
        "/isScreenStatic" => Resp::json(r#"{"isScreenStatic":false}"#),
        _ => Resp::json("{}"),
    }));
    let mut client = IosNativeClient::connect(runner.port);
    let fake = Fake::alert();
    client.set_hid(Some(Arc::clone(&fake.session)));
    Rig { runner, client, fake }
}

#[test]
fn client_touch_goes_through_hid_and_skips_the_runner() {
    let mut r = rig(true);
    r.client.touch(220.0, 570.0, None).unwrap();
    r.client.touch(10.0, 20.0, Some(3.0)).unwrap();
    let touches = r.fake.state.lock().unwrap().touches().into_iter().cloned().collect::<Vec<_>>();
    assert_eq!(touches, vec!["touch n=1 first=220,570 hold=10 step=0", "touch n=1 first=10,20 hold=3000 step=0"]);
    assert!(r.runner.calls("touch").is_empty());
}

#[test]
fn client_falls_back_to_the_runner_when_a_call_failed_before_sending() {
    let mut r = rig(true);
    r.fake.state.lock().unwrap().input_err = Some(HidError::Failed("no HID client".into()));
    r.client.touch(1.0, 2.0, None).unwrap();
    r.client.swipe_v2(None, (1.0, 2.0), (3.0, 4.0), 0.3, &[]).unwrap();
    r.client.press_button("home").unwrap();
    assert_eq!(r.runner.calls("touch").len(), 1);
    assert_eq!(r.runner.calls("swipeV2").len(), 1);
    assert_eq!(r.runner.calls("pressButton").len(), 1);
}

#[test]
fn client_never_repeats_input_that_may_have_been_sent() {
    let mut r = rig(true);
    r.fake.state.lock().unwrap().input_err = Some(HidError::InputMaybeSent("no acknowledgement".into()));
    assert!(r.client.touch(1.0, 2.0, None).is_err());
    assert!(r.client.swipe_v2(None, (1.0, 2.0), (3.0, 4.0), 0.3, &[]).is_err());
    assert!(r.client.press_button("lock").is_err());
    for route in ["touch", "swipeV2", "pressButton"] {
        assert!(r.runner.calls(route).is_empty(), "{route} must not reach the runner");
    }
}

#[test]
fn client_uses_the_runner_outside_portrait_and_after_a_rotation() {
    let mut r = rig(false);
    r.client.touch(1.0, 2.0, None).unwrap();
    assert_eq!(r.runner.calls("touch").len(), 1);
    assert!(r.fake.state.lock().unwrap().touches().is_empty());

    let mut r = rig(true);
    r.client.set_orientation("landscapeLeft").unwrap();
    r.client.touch(1.0, 2.0, None).unwrap();
    assert_eq!(r.runner.calls("touch").len(), 1, "after setOrientation to landscape the runner serves touches");
    r.client.set_orientation("portrait").unwrap();
    r.client.touch(1.0, 2.0, None).unwrap();
    assert_eq!(r.runner.calls("touch").len(), 1, "back in portrait HID serves them again");
    assert_eq!(r.fake.state.lock().unwrap().touches().len(), 1);
}

#[test]
fn client_buttons_use_hid_in_any_orientation() {
    let mut r = rig(false);
    r.client.press_button("home").unwrap();
    r.client.press_button("lock").unwrap();
    assert!(r.runner.calls("pressButton").is_empty());
    let calls = r.fake.calls();
    assert!(calls.contains(&"button Home".to_string()) && calls.contains(&"button Lock".to_string()), "{calls:?}");
}

#[test]
fn client_screen_static_compares_frames_and_falls_back_on_failure() {
    let mut r = rig(true);
    assert!(r.client.is_screen_static().unwrap());
    assert!(r.runner.calls("isScreenStatic").is_empty());
    r.fake.state.lock().unwrap().set_frames(vec![Ok(b"a".to_vec()), Ok(b"b".to_vec())]);
    assert!(!r.client.is_screen_static().unwrap());
    r.fake.state.lock().unwrap().set_frames(vec![Err(HidError::Failed("no surface".into()))]);
    // The runner mock answers "moving".
    assert!(!r.client.is_screen_static().unwrap());
    assert_eq!(r.runner.calls("isScreenStatic").len(), 1);
}

#[test]
fn client_without_a_session_never_touches_hid() {
    let runner = MockRunner::start(Box::new(|req: &Req| match req.path() {
        "/deviceInfo" => Resp::json(r#"{"widthPixels":1320,"heightPixels":2868,"widthPoints":440,"heightPoints":956}"#),
        _ => Resp::json("{}"),
    }));
    let mut client = IosNativeClient::connect(runner.port);
    client.set_hid(None);
    client.touch(1.0, 2.0, None).unwrap();
    client.press_button("home").unwrap();
    assert_eq!(runner.calls("touch").len(), 1);
    assert_eq!(runner.calls("pressButton").len(), 1);
}

#[test]
fn an_unavailable_backend_in_the_client_warns_once_and_uses_the_runner() {
    let mut r = rig(true);
    r.fake.state.lock().unwrap().open_err = Some(HidError::Unavailable("AXPTranslator is not available".into()));
    for _ in 0..3 {
        r.client.touch(1.0, 2.0, None).unwrap();
    }
    assert_eq!(r.runner.calls("touch").len(), 3);
    assert_eq!(r.fake.warnings.lock().unwrap().len(), 1);
    assert_eq!(r.fake.state.lock().unwrap().opens, 1);
}

#[test]
fn surface_frame_failure_falls_back_before_input_and_is_tolerated_after() {
    let f = Fake::alert();
    f.state.lock().unwrap().set_frames(vec![Err(HidError::Failed("no surface".into()))]);
    let mut ops = HidOps::new(&f.session);
    let err = ops.tap_on_element(&tap_req(label(ALLOW_WHILE_USING))).unwrap_err();
    assert!(!ops.input_sent() && !surface::is_final(&err, false), "{err:#}");
    assert!(f.state.lock().unwrap().touches().is_empty());

    // After the tap went out, the same failure only ends the wait.
    let mut ops = HidOps::new(&f.session);
    ops.tap_on_point(&crate::backend::TapPointRequest {
        point: "220,570".into(),
        long_press: false,
        repeat: None,
        retry_if_no_change: false,
        wait_to_settle_timeout_ms: None,
    })
    .unwrap_or_else(|e| panic!("{e:#}"));
}
