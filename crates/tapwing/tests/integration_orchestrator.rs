//! Port of Maestro 2.12.1 `IntegrationTest.kt` (Cases 091–160, unnumbered
//! orchestrator/error/callback tests) plus F3-specific verification (CLI `-e`
//! precedence, `retry` cap at 3, `assertScreenshot` + `cropOn`, AI command
//! messaging, lazy JS / compiled bytecode cache / HTTP connection pooling).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};
use serde_json::json;
use tapwing::backend::{
    AssertionError, FlowDriver, InvalidNumericError, MaestroError, MaestroErrorKind, TransportError,
};
use tapwing::fake_driver::{Bounds, FakeDriver, FakeEvent, FakeLayoutElement};
use tapwing::flow::ir::{
    AssertCondition, Basic, Condition, Direction, ElementSelector, LaunchApp, MaestroCommand, Retry, RunFlow,
    TapOnElement, ValueMap,
};
use tapwing::flow::{self, ParsedFlow};
use tapwing::orchestra::{ErrorResolution, FlowResult, Orchestra, OrchestraCallbacks, OrchestraOpts};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempOut(PathBuf);

impl TempOut {
    fn new(tag: &str) -> Self {
        let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tapwing_it2_{tag}_{}_{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempOut {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_path(case_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../compat/maestro-fixtures/maestro-test")
        .join(format!("{case_name}.yaml"))
}

fn load_case(case_name: &str) -> ParsedFlow {
    let p = fixture_path(case_name);
    flow::parse_file(&p).unwrap_or_else(|e| panic!("failed to parse {case_name}: {}", e.render()))
}

fn default_opts(flow: &ParsedFlow) -> OrchestraOpts {
    OrchestraOpts {
        lookup_timeout_ms: 0,
        optional_lookup_timeout_ms: 0,
        flow_path: flow.path.clone(),
        ..Default::default()
    }
}

fn run_case(case_name: &str, driver: &mut FakeDriver) -> FlowResult {
    let flow = load_case(case_name);
    let mut orch = Orchestra::new(driver, default_opts(&flow));
    orch.run_parsed_flow(&flow)
}

fn run_case_with_logs(case_name: &str, driver: &mut FakeDriver) -> (FlowResult, Vec<String>) {
    let flow = load_case(case_name);
    let logs = Rc::new(RefCell::new(Vec::<String>::new()));
    let logs_ref = Rc::clone(&logs);
    let cbs = OrchestraCallbacks {
        on_command_metadata_update: Some(Box::new(move |_, meta| {
            logs_ref.borrow_mut().extend(meta.log_messages.iter().cloned());
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(driver, default_opts(&flow), cbs);
    let res = orch.run_parsed_flow(&flow);
    let out = logs.borrow().clone();
    (res, out)
}

fn assert_events_in_order(actual: &[FakeEvent], expected: &[FakeEvent]) {
    let mut pos = 0usize;
    for exp in expected {
        if let Some(found) = actual[pos..].iter().position(|a| a == exp) {
            pos += found + 1;
        } else {
            panic!("Expected event {exp:?} in order after index {pos}.\nActual events: {actual:#?}");
        }
    }
}

fn is_assertion_failure(err: &anyhow::Error) -> bool {
    err.is::<AssertionError>()
        || err.downcast_ref::<MaestroError>().is_some_and(|m| m.kind == MaestroErrorKind::AssertionFailure)
}

#[test]
fn case_091_assert_visible_by_index() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Item".into());
        e.bounds = Some(Bounds::of_size(100, 100));
    });
    driver.add_element(|e| {
        e.text = Some("Item".into());
        e.bounds = Some(Bounds::of_size(100, 100).translate(0, 100));
    });
    let res = run_case("091_assert_visible_by_index", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_092_log_messages() {
    let mut driver = FakeDriver::new();
    let (res, logs) = run_case_with_logs("092_log_messages", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(logs, vec!["Log from evalScript", "Log from runScript"]);
}

#[test]
fn case_093_js_default_values() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.default");
    let res = run_case("093_js_default_value", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(
        driver.has_event(&FakeEvent::LaunchApp {
            app_id: "com.example.default".into(),
            launch_arguments: ValueMap::new()
        })
    );
}

#[test]
fn case_094_subflow_with_inlined_commands() {
    let mut driver = FakeDriver::new();
    let res = run_case("094_runFlow_inline", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::InputText("Inner Parameter".into())));
}

#[test]
fn case_095_launch_arguments() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("095_launch_arguments", &mut driver);
    assert!(res.success, "{:?}", res.error);
    let mut expected_args = ValueMap::new();
    expected_args.insert("argumentA".into(), json!(true));
    expected_args.insert("argumentB".into(), json!(4));
    expected_args.insert("argumentC".into(), json!(4.0));
    expected_args.insert("argumentD".into(), json!("Hello String Value true"));
    assert!(
        driver.has_event(&FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: expected_args })
    );
}

#[test]
fn case_096_platform_condition() {
    let mut driver = FakeDriver::new();
    let res = run_case("096_platform_condition", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::InputText("Hello iOS".into())));
    assert!(driver.has_event(&FakeEvent::InputText("Hello ios".into())));
    assert!(!driver.has_event(&FakeEvent::InputText("Hello Android".into())));
}

#[test]
fn case_097_contains_descendants() {
    let mut driver = FakeDriver::new();
    let id1 = driver.add_element(|e| {
        e.id = Some("id1".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    let mid = FakeLayoutElement::add_child(&id1, |e| {
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    let id2 = FakeLayoutElement::add_child(&mid, |e| {
        e.id = Some("id2".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    FakeLayoutElement::add_child(&id2, |e| {
        e.text = Some("Child 1".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 50));
    });
    FakeLayoutElement::add_child(&mid, |e| {
        e.text = Some("Child 2".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.enabled = Some(false);
    });
    let res = run_case("097_contains_descendants", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_098a_execute_javascript_conditionally() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Click me".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(|el| {
            el.borrow_mut().text = Some("Clicked".into());
            Ok(())
        }));
    });
    let (res, logs) = run_case_with_logs("098_runscript_conditionals", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
    assert_eq!(logs, vec!["Log from runScript"]);
}

#[test]
fn case_098b_execute_conditions_eagerly() {
    let mut driver = FakeDriver::new();
    let (res, logs) = run_case_with_logs("098_runscript_conditionals_eager", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
    assert!(logs.is_empty());
}

#[test]
fn case_156_start_recording_failure_leaves_no_file() {
    let out = TempOut::new("156");
    let flow = load_case("156_screen_recording_start_fails");
    let mut driver = FakeDriver::new();
    driver.start_recording_error = Some("emulator cannot record".into());
    let mut opts = default_opts(&flow);
    opts.test_output_dir = Some(out.path().to_path_buf());
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(!res.success);
    assert!(!out.path().join("startRecording/156_clip.mp4").exists());
}

#[test]
fn case_160_templated_app_id_passed_to_tap_and_open_link() {
    let flow = load_case("160_templated_app_id_tap_and_open_link");
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Log in".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let mut opts = default_opts(&flow);
    opts.cli_env.insert("APP_ID".into(), "com.example.app".into());
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 50 }));
    assert_eq!(driver.last_tap_app_id.as_deref(), Some("com.example.app"));
    assert_eq!(driver.last_open_link_app_id.as_deref(), Some("com.example.app"));
}

#[test]
fn case_099_screen_recording() {
    let out = TempOut::new("099");
    let flow = load_case("099_screen_recording");
    let mut driver = FakeDriver::new();
    let mut opts = default_opts(&flow);
    opts.test_output_dir = Some(out.path().to_path_buf());
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::StartRecording, FakeEvent::StopRecording]);
    assert!(out.path().join("startRecording/099_screen_recording.mp4").exists());
}

#[test]
fn case_100_tap_on_multiple_times() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("100_tapOn_multiple_times", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 3);
}

#[test]
fn case_101_double_tap_on() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("101_doubleTapOn", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 2);
}

#[test]
fn case_102_graaljs_config() {
    let mut driver = FakeDriver::new();
    let res = run_case("102_graaljs", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::InputText("foo".into()), FakeEvent::InputText("bar".into())]);
}

#[test]
fn case_103_on_flow_start_and_complete_hooks() {
    let mut driver = FakeDriver::new();
    let (res, logs) = run_case_with_logs("103_on_flow_start_complete_hooks", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(logs, vec!["setup", "teardown"]);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::InputText("test1".into()),
            FakeEvent::Tap { x: 100, y: 200 },
            FakeEvent::InputText("test2".into()),
        ],
    );
}

#[test]
fn case_104_hooks_run_when_flow_failed() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("another_id".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("104_on_flow_start_complete_hooks_flow_failed", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::InputText("test1".into()), FakeEvent::InputText("test2".into())],
    );
}

#[test]
fn case_105_hooks_when_js_output_is_set() {
    let mut driver = FakeDriver::new();
    let (res, logs) = run_case_with_logs("105_on_flow_start_complete_when_js_output_set", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(logs, vec!["setup", "teardown"]);
}

#[test]
fn case_106_hooks_when_js_output_set_with_subflows() {
    let mut driver = FakeDriver::new();
    let (res, logs) = run_case_with_logs("106_on_flow_start_complete_when_js_output_set_subflows", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(logs, vec!["subflow", "setup subflow", "teardown subflow"]);
}

#[test]
fn case_107_define_variables_before_hooks() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let (res, logs) = run_case_with_logs("107_define_variables_command_before_hooks", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(logs, vec!["com.example.app"]);
    assert!(
        driver.has_event(&FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() })
    );
}

#[test]
fn case_108_fail_flow_and_skip_commands_on_start_hook_failure() {
    let mut driver = FakeDriver::new();
    let (res, logs) = run_case_with_logs("108_failed_start_hook", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
    assert_eq!(logs, vec!["on start", "on complete"]);
}

#[test]
fn case_109_fail_flow_and_execute_commands_on_complete_hook_failure() {
    let mut driver = FakeDriver::new();
    let (res, logs) = run_case_with_logs("109_failed_complete_hook", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
    assert_eq!(logs, vec!["on start", "main flow", "on complete"]);
}

#[test]
fn case_110_add_media() {
    let mut driver = FakeDriver::new();
    let res = run_case("110_add_media_device", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.events.iter().filter(|e| matches!(e, FakeEvent::AddMedia(_))).count(), 1);
}

#[test]
fn case_111_add_multiple_media() {
    let mut driver = FakeDriver::new();
    let res = run_case("111_add_multiple_media", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.events.iter().filter(|e| matches!(e, FakeEvent::AddMedia(_))).count(), 3);
}

#[test]
fn case_112_scroll_until_visible_with_center() {
    let mut driver = FakeDriver::new();
    let h = driver.device_info().height_grid;
    driver.add_element(|e| {
        e.text = Some("Test".into());
        e.bounds = Some(Bounds::new(0, h, 100, h + 100));
    });
    let res = run_case("112_scroll_until_visible_center", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::SwipeElementWithDirection { point: (270, 480), direction: Direction::Up, duration_ms: 1 }],
    );
}

#[test]
fn case_113_tap_on_element_with_settle_timeout() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.mutating_text = Some(Box::new(|| format!("The time is {:?}", Instant::now())));
    });
    let t0 = Instant::now();
    let res = run_case("113_tap_on_element_settle_timeout", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
}

#[test]
fn case_114_child_of_selector() {
    let mut driver = FakeDriver::new();
    let id1 = driver.add_element(|e| {
        e.id = Some("id1".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 600));
    });
    let p1 = FakeLayoutElement::add_child(&id1, |e| {
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
        e.text = Some("parent_id_1".into());
    });
    FakeLayoutElement::add_child(&p1, |e| {
        e.text = Some("child_id".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 200));
    });
    let p2 = FakeLayoutElement::add_child(&id1, |e| {
        e.bounds = Some(Bounds::new(0, 200, 200, 400));
        e.text = Some("parent_id_2".into());
    });
    FakeLayoutElement::add_child(&p2, |e| {
        e.text = Some("child_id".into());
        e.bounds = Some(Bounds::new(0, 200, 100, 400));
    });
    let p3 = FakeLayoutElement::add_child(&id1, |e| {
        e.bounds = Some(Bounds::new(0, 400, 200, 600));
        e.text = Some("parent_id_3".into());
    });
    FakeLayoutElement::add_child(&p3, |e| {
        e.text = Some("child_id_1".into());
        e.bounds = Some(Bounds::new(0, 400, 100, 600));
    });
    let res = run_case("114_child_of_selector", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_115_airplane_mode() {
    let mut driver = FakeDriver::new();
    let res = run_case("115_airplane_mode", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_116_kill_app() {
    let mut driver = FakeDriver::new();
    let res = run_case("116_kill_app", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::KillApp("com.example.app".into())));
    assert!(driver.has_event(&FakeEvent::KillApp("another.app".into())));
}

#[test]
fn case_117_scroll_until_visible_speed_and_timeout_eval() {
    let flow = load_case("117_scroll_until_visible_speed");
    let mut driver = FakeDriver::new();
    let h = driver.device_info().height_grid;
    driver.add_element(|e| {
        e.id = Some("maestro".into());
        e.bounds = Some(Bounds::new(0, h, 100, h + 100));
    });
    let captured = Rc::new(RefCell::new((String::new(), String::new())));
    let cap2 = Rc::clone(&captured);
    let cbs = OrchestraCallbacks {
        on_command_metadata_update: Some(Box::new(move |_, meta| {
            if let Some(MaestroCommand::ScrollUntilVisible(s)) = &meta.evaluated_command {
                *cap2.borrow_mut() = (s.scroll_duration.clone(), s.timeout.clone());
            }
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(&mut driver, default_opts(&flow), cbs);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(*captured.borrow(), ("601".to_string(), "20000".to_string()));
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::SwipeElementWithDirection { point: (270, 480), direction: Direction::Up, duration_ms: 601 }],
    );
}

#[test]
fn case_118_scroll_until_visible_speed_out_of_range_rejected() {
    let mut driver = FakeDriver::new();
    let h = driver.device_info().height_grid;
    driver.add_element(|e| {
        e.id = Some("maestro".into());
        e.bounds = Some(Bounds::new(0, h, 100, h + 100));
    });
    let res = run_case("118_scroll_until_visible_negative", &mut driver);
    assert!(!res.success);
    assert!(res.error.as_ref().unwrap().is::<InvalidNumericError>());
}

#[test]
fn case_119_retry_set_of_commands_with_n_attempts() {
    let mut driver = FakeDriver::new();
    let counter = Rc::new(Cell::new(0i32));
    let indicator = driver.add_element(|e| {
        e.text = Some("0".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let c2 = Rc::clone(&counter);
    let ind2 = Rc::clone(&indicator);
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            let n = c2.get() + 1;
            c2.set(n);
            if n == 1 {
                return Err(MaestroError::unable_to_launch("Flake on first attempt").into());
            }
            ind2.borrow_mut().text = Some(n.to_string());
            Ok(())
        }));
    });
    let res = run_case("119_retry_commands", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::Scroll, FakeEvent::Scroll, FakeEvent::Tap { x: 50, y: 50 }, FakeEvent::Scroll],
    );
}

#[test]
fn case_120_tap_on_element_retry_if_no_change_opt_in() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Primary button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("120_tap_on_element_retryTapIfNoChange", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 2);
}

#[test]
fn case_125_assert_visible_by_css_selector() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("http://example.com");
    driver.add_element(|e| {
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.text = Some("Test Element".into());
        e.matches_css_filter = Some(".test".into());
    });
    let res = run_case("125_assert_by_css", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_144_tap_by_css_selector_on_element_with_children() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("http://example.com");
    let btn = driver.add_element(|e| {
        e.text = Some("Open user menu".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.matches_css_filter = Some("button[aria-label='Open user menu']".into());
    });
    FakeLayoutElement::add_child(&btn, |e| {
        e.text = Some("FR".into());
        e.bounds = Some(Bounds::new(10, 10, 90, 90));
    });
    let res = run_case("144_tap_by_css_on_element_with_children", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
}

#[test]
fn case_126_set_orientation() {
    let mut driver = FakeDriver::new();
    let res = run_case("126_set_orientation", &mut driver);
    assert!(res.success, "{:?}", res.error);
    for o in ["PORTRAIT", "LANDSCAPE_LEFT", "LANDSCAPE_RIGHT", "UPSIDE_DOWN"] {
        assert!(driver.has_event(&FakeEvent::SetOrientation(o.into())));
    }
}

#[test]
fn case_126_set_orientation_with_env_variables() {
    let mut driver = FakeDriver::new();
    let res = run_case("126_set_orientation_with_env", &mut driver);
    assert!(res.success, "{:?}", res.error);
    for o in ["PORTRAIT", "LANDSCAPE_LEFT", "LANDSCAPE_RIGHT", "UPSIDE_DOWN"] {
        assert!(driver.has_event(&FakeEvent::SetOrientation(o.into())));
    }
}

#[test]
fn case_127_graaljs_env_vars_isolated_between_flows() {
    let mut driver = FakeDriver::new();
    let res = run_case("127_env_vars_isolation_graaljs", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_128_random_data_generation() {
    let mut driver = FakeDriver::new();
    let res = run_case("128_datafaker_graaljs", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_129_text_and_id_with_child_elements() {
    let mut driver = FakeDriver::new();
    let p = driver.add_element(|e| {
        e.id = Some("some_id".into());
        e.text = Some("some_text".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    FakeLayoutElement::add_child(&p, |e| {
        e.id = Some(String::new());
        e.text = Some("some_text".into());
        e.bounds = Some(Bounds::new(50, 50, 150, 150));
    });
    let res = run_case("129_text_and_id", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_130_duplicate_elements_deepest_hierarchy() {
    let mut driver = FakeDriver::new();
    driver.root.borrow_mut().id = Some("0".into());
    driver.add_element(|e| {
        e.id = Some("1".into());
        e.text = Some("some_text".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    let n2 = driver.add_element(|e| {
        e.id = Some("2".into());
        e.text = Some("some_text".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    let n3 = FakeLayoutElement::add_child(&n2, |e| {
        e.id = Some("3".into());
        e.text = Some("some_text".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    FakeLayoutElement::add_child(&n3, |e| {
        e.id = Some("4".into());
        e.text = Some("some_text".into());
        e.bounds = Some(Bounds::new(50, 50, 150, 150));
    });
    let res = run_case("130_text_and_index", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_131_set_permissions_on_installed_app() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("131_setPermissions", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::SetPermissions {
        app_id: "com.example.app".into(),
        permissions: BTreeMap::from([("all".into(), "deny".into()), ("notifications".into(), "unset".into())]),
    }));
}

#[test]
fn case_133_set_clipboard() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("inputField".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let res = run_case("133_setClipboard", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::InputText("Hello, Maestro!".into())]);
}

#[test]
fn case_134_take_screenshot_with_path() {
    let out = TempOut::new("134");
    let flow = load_case("134_take_screenshot_with_path");
    let mut driver = FakeDriver::new();
    let mut opts = default_opts(&flow);
    opts.test_output_dir = Some(out.path().to_path_buf());
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::TakeScreenshot]);
    assert!(out.path().join("screenshots/134_screenshots/filename.png").exists());
}

#[test]
fn case_135_screen_recording_with_path() {
    let out = TempOut::new("135");
    let flow = load_case("135_screen_recording_with_path");
    let mut driver = FakeDriver::new();
    let mut opts = default_opts(&flow);
    opts.test_output_dir = Some(out.path().to_path_buf());
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::StartRecording, FakeEvent::StopRecording]);
    assert!(out.path().join("startRecording/135_recordings/filename.mp4").exists());
}

#[test]
fn case_136_relative_path_in_http_multipart_script_local_server() {
    // Runs the exact Case 136 pattern (flow -> ./script/upload.js -> ../media/abc.png)
    // against a local ephemeral HTTP test server so tests never depend on external internet.
    let tmp = TempOut::new("136");
    let script_dir = tmp.path().join("script");
    let media_dir = tmp.path().join("media");
    std::fs::create_dir_all(&script_dir).unwrap();
    std::fs::create_dir_all(&media_dir).unwrap();
    std::fs::write(media_dir.join("abc.png"), b"PNG_FAKE_BYTES").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let srv = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut cl = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.trim().eq_ignore_ascii_case("content-length")
            {
                cl = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; cl];
        reader.read_exact(&mut body).unwrap();
        let raw = String::from_utf8_lossy(&body);
        let ok = raw.contains("PNG_FAKE_BYTES") && raw.contains("logo");
        let resp_body = format!(r#"{{"received":{ok}}}"#);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });

    std::fs::write(
        script_dir.join("multipart_request_file_script.js"),
        format!(
            r#"
            const response = http.post('http://127.0.0.1:{port}/anything', {{
                multipartForm: {{
                    imagefile: {{ filePath: "../media/abc.png" }},
                    name: "logo"
                }}
            }});
            output.received = json(response.body).received;
            "#
        ),
    )
    .unwrap();
    let flow_file = tmp.path().join("flow.yaml");
    std::fs::write(
        &flow_file,
        "appId: com.example.app\n---\n- runScript: \"./script/multipart_request_file_script.js\"\n- assertTrue: ${output.received === true}\n",
    )
    .unwrap();

    let flow = flow::parse_file(&flow_file).unwrap();
    let mut driver = FakeDriver::new();
    let mut orch = Orchestra::new(&mut driver, default_opts(&flow));
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(res.js_metrics.http_requests, 1);
    srv.join().unwrap();
}

#[test]
fn case_138_take_cropped_screenshot() {
    let out = TempOut::new("138");
    let flow = load_case("138_take_cropped_screenshot");
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("element_id".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let info = driver.device_info();
    let dpr = (info.height_pixels / info.height_grid) as u32;
    let mut opts = default_opts(&flow);
    opts.test_output_dir = Some(out.path().to_path_buf());
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::TakeScreenshot]);
    let img_path = out.path().join("screenshots/138_take_cropped_screenshot_with_filename.png");
    assert!(img_path.exists());
    let img = image::open(&img_path).unwrap();
    assert_eq!(img.dimensions(), (100 * dpr, 100 * dpr));
}

#[test]
fn case_137_shard_and_device_env_vars() {
    let out = TempOut::new("137");
    let flow = load_case("137_shard_device_env_vars");
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let mut opts = default_opts(&flow);
    opts.test_output_dir = Some(out.path().to_path_buf());
    opts.device_id = Some("test-device".into());
    opts.shard_index = Some(0);
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
            FakeEvent::TakeScreenshot,
        ],
    );
    assert!(out.path().join("screenshots/137_shard_device_env_vars_test-device_shard1_idx0.png").exists());
}

#[test]
fn case_149_child_of_selector_polls_fresh_hierarchy_for_deferred_child() {
    let flow = load_case("149_child_of_selector_deferred");
    let mut driver = FakeDriver::new();
    let calls = Rc::new(Cell::new(0usize));
    let c2 = Rc::clone(&calls);
    let parent = driver.add_element(|e| {
        e.text = Some("parent".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    FakeLayoutElement::add_child(&parent, |e| {
        e.bounds = Some(Bounds::new(10, 10, 190, 50));
        e.mutating_text = Some(Box::new(move || {
            let n = c2.get();
            c2.set(n + 1);
            if n == 0 { "not_yet".into() } else { "target_text".into() }
        }));
    });
    let mut opts = default_opts(&flow);
    opts.lookup_timeout_ms = 2000;
    opts.optional_lookup_timeout_ms = 500;
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_150_child_of_selector_polls_fresh_hierarchy_for_deferred_parent() {
    let flow = load_case("150_child_of_selector_deferred_parent");
    let mut driver = FakeDriver::new();
    let calls = Rc::new(Cell::new(0usize));
    let c2 = Rc::clone(&calls);
    let parent = driver.add_element(|e| {
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
        e.mutating_text = Some(Box::new(move || {
            let n = c2.get();
            c2.set(n + 1);
            if n == 0 { "not_yet".into() } else { "parent".into() }
        }));
    });
    FakeLayoutElement::add_child(&parent, |e| {
        e.text = Some("target_text".into());
        e.bounds = Some(Bounds::new(10, 10, 190, 50));
    });
    let mut opts = default_opts(&flow);
    opts.lookup_timeout_ms = 2000;
    opts.optional_lookup_timeout_ms = 500;
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_151_directional_swipe_on_element_with_relative_point() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("swiping element".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 200));
    });
    let res = run_case("151_swipe_from_element_point", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::SwipeElementWithDirection {
        point: (50, 170),
        direction: Direction::Right,
        duration_ms: 400,
    }));
}

#[test]
fn case_152_dark_mode() {
    let mut driver = FakeDriver::new();
    let res = run_case("152_dark_mode", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.dark_mode);
}

#[test]
fn case_153_assert_dark_and_light_mode_pass() {
    let mut driver = FakeDriver::new();
    let res = run_case("153_assert_dark_light_mode_pass", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_154_assert_dark_mode_fails_in_light_mode() {
    let mut driver = FakeDriver::new();
    let res = run_case("154_assert_dark_mode_fail", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
}

// --- Unnumbered Maestro `IntegrationTest.kt` orchestrator/error/callback tests ---

#[test]
fn retry_command_only_retries_on_maestro_error_propagates_others() {
    let tap_count = Rc::new(Cell::new(0usize));
    let tc2 = Rc::clone(&tap_count);
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            tc2.set(tc2.get() + 1);
            Err(anyhow::anyhow!("infra failure — not a MaestroException"))
        }));
    });
    let cmds = vec![MaestroCommand::Retry(Retry {
        max_retries: Some("3".into()),
        commands: vec![MaestroCommand::TapOnElement(TapOnElement {
            selector: ElementSelector { text_regex: Some("Button".into()), ..Default::default() },
            retry_if_no_change: None,
            wait_until_visible: None,
            long_press: None,
            repeat: None,
            wait_to_settle_timeout_ms: None,
            relative_point: None,
            label: None,
            optional: false,
        })],
        source_description: None,
        config: None,
        label: None,
        optional: false,
    })];
    let mut orch = Orchestra::new(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
    );
    let res = orch.run_commands(&cmds);
    assert!(!res.success);
    assert_eq!(tap_count.get(), 1, "must not retry non-Maestro errors");
}

#[test]
fn retry_command_retries_on_maestro_error_until_success() {
    let tap_count = Rc::new(Cell::new(0usize));
    let tc2 = Rc::clone(&tap_count);
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            let n = tc2.get() + 1;
            tc2.set(n);
            if n == 1 {
                return Err(MaestroError::unable_to_launch("flake on first attempt").into());
            }
            Ok(())
        }));
    });
    let cmds = vec![MaestroCommand::Retry(Retry {
        max_retries: Some("3".into()),
        commands: vec![MaestroCommand::TapOnElement(TapOnElement {
            selector: ElementSelector { text_regex: Some("Button".into()), ..Default::default() },
            retry_if_no_change: None,
            wait_until_visible: None,
            long_press: None,
            repeat: None,
            wait_to_settle_timeout_ms: None,
            relative_point: None,
            label: None,
            optional: false,
        })],
        source_description: None,
        config: None,
        label: None,
        optional: false,
    })];
    let mut orch = Orchestra::new(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
    );
    let res = orch.run_commands(&cmds);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(tap_count.get(), 2);
}

#[test]
fn hide_keyboard_succeeds_when_hidden() {
    let mut driver = FakeDriver::new();
    let cmds = vec![MaestroCommand::HideKeyboard(Basic::default())];
    let mut orch = Orchestra::new(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
    );
    let res = orch.run_commands(&cmds);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::HideKeyboard]);
}

#[test]
fn hide_keyboard_fails_when_keyboard_remains_visible() {
    let mut driver = FakeDriver::new();
    driver.keyboard_remains_visible = true;
    let cmds = vec![MaestroCommand::HideKeyboard(Basic::default())];
    let mut orch = Orchestra::new(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
    );
    let res = orch.run_commands(&cmds);
    assert!(!res.success);
    assert!(
        res.error
            .as_ref()
            .unwrap()
            .downcast_ref::<MaestroError>()
            .is_some_and(|m| m.kind == MaestroErrorKind::HideKeyboardFailure)
    );
    assert_events_in_order(&driver.events, &[FakeEvent::HideKeyboard]);
}

#[test]
fn callback_order_correct_for_main_flow_and_subflow_and_failure() {
    // 1. Main flow success
    let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));
    let mut driver = FakeDriver::new();
    let mut cbs = OrchestraCallbacks::default();
    let e1 = Rc::clone(&events);
    cbs.on_command_start = Some(Box::new(move |_, _| e1.borrow_mut().push("start")));
    let e2 = Rc::clone(&events);
    cbs.on_command_metadata_update = Some(Box::new(move |_, _| e2.borrow_mut().push("meta")));
    let e3 = Rc::clone(&events);
    cbs.on_command_complete = Some(Box::new(move |_, _| e3.borrow_mut().push("complete")));
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    assert!(orch.run_commands(&[MaestroCommand::BackPress(Basic::default())]).success);
    assert_eq!(*events.borrow(), vec!["start", "meta", "complete"]);

    // 2. Subflow success
    events.borrow_mut().clear();
    let mut cbs = OrchestraCallbacks::default();
    let e1 = Rc::clone(&events);
    cbs.on_command_start = Some(Box::new(move |_, _| e1.borrow_mut().push("start")));
    let e2 = Rc::clone(&events);
    cbs.on_command_metadata_update = Some(Box::new(move |_, _| e2.borrow_mut().push("meta")));
    let e3 = Rc::clone(&events);
    cbs.on_command_complete = Some(Box::new(move |_, _| e3.borrow_mut().push("complete")));
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let subflow = vec![MaestroCommand::RunFlow(RunFlow {
        commands: vec![MaestroCommand::BackPress(Basic::default())],
        condition: None,
        source_description: None,
        config: None,
        label: None,
        optional: false,
    })];
    assert!(orch.run_commands(&subflow).success);
    assert_eq!(*events.borrow(), vec!["start", "meta", "start", "meta", "complete", "complete"]);

    // 3. Main flow failure
    events.borrow_mut().clear();
    let mut cbs = OrchestraCallbacks::default();
    let e1 = Rc::clone(&events);
    cbs.on_command_start = Some(Box::new(move |_, _| e1.borrow_mut().push("start")));
    let e2 = Rc::clone(&events);
    cbs.on_command_metadata_update = Some(Box::new(move |_, _| e2.borrow_mut().push("meta")));
    let e3 = Rc::clone(&events);
    cbs.on_command_failed = Some(Box::new(move |_, _, _| {
        e3.borrow_mut().push("failed");
        ErrorResolution::Fail
    }));
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let fail_cmd = vec![MaestroCommand::AssertCondition(AssertCondition {
        condition: Condition {
            visible: Some(ElementSelector { id_regex: Some("missing".into()), ..Default::default() }),
            ..Default::default()
        },
        timeout: None,
        label: None,
        optional: false,
    })];
    assert!(!orch.run_commands(&fail_cmd).success);
    assert_eq!(*events.borrow(), vec!["start", "meta", "failed"]);
}

#[test]
fn transport_death_never_routed_through_on_command_failed() {
    let mut driver = FakeDriver::new();
    driver.set_command_transport_error("broken pipe");
    let failed_called = Rc::new(Cell::new(false));
    let fc = Rc::clone(&failed_called);
    let cbs = OrchestraCallbacks {
        on_command_failed: Some(Box::new(move |_, _, _| {
            fc.set(true);
            ErrorResolution::Fail
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let res = orch.run_commands(&[MaestroCommand::BackPress(Basic::default())]);
    assert!(!res.success);
    assert!(res.error.as_ref().unwrap().is::<TransportError>());
    assert!(!failed_called.get());
}

#[test]
fn non_device_command_error_is_routed_through_on_command_failed() {
    let mut driver = FakeDriver::new();
    driver.set_command_runtime_error("device operation failed");
    let failed_called = Rc::new(Cell::new(false));
    let fc = Rc::clone(&failed_called);
    let cbs = OrchestraCallbacks {
        on_command_failed: Some(Box::new(move |_, _, _| {
            fc.set(true);
            ErrorResolution::Fail
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let res = orch.run_commands(&[MaestroCommand::BackPress(Basic::default())]);
    assert!(!res.success);
    assert!(failed_called.get());
}

#[test]
fn device_death_during_launch_app_escapes_as_transport_error() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    driver.set_launch_transport_error("broken pipe during launchApp");
    let failed_called = Rc::new(Cell::new(false));
    let fc = Rc::clone(&failed_called);
    let cbs = OrchestraCallbacks {
        on_command_failed: Some(Box::new(move |_, _, _| {
            fc.set(true);
            ErrorResolution::Fail
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let res = orch.run_commands(&[MaestroCommand::LaunchApp(LaunchApp {
        app_id: "com.example.app".into(),
        clear_state: None,
        clear_keychain: None,
        stop_app: None,
        permissions: None,
        launch_arguments: None,
        label: None,
        optional: false,
    })]);
    assert!(!res.success);
    assert!(res.error.as_ref().unwrap().is::<TransportError>());
    assert!(!failed_called.get());
}

#[test]
fn optional_launch_app_not_installed_is_warned_not_failed() {
    let mut driver = FakeDriver::new();
    let warned = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let w2 = Rc::clone(&warned);
    let f2 = Rc::clone(&failed);
    let cbs = OrchestraCallbacks {
        on_command_warned: Some(Box::new(move |_, _| w2.set(true))),
        on_command_failed: Some(Box::new(move |_, _, _| {
            f2.set(true);
            ErrorResolution::Fail
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let res = orch.run_commands(&[MaestroCommand::LaunchApp(LaunchApp {
        app_id: "non.existent.app.id".into(),
        clear_state: None,
        clear_keychain: None,
        stop_app: None,
        permissions: None,
        launch_arguments: None,
        label: None,
        optional: true,
    })]);
    assert!(res.success);
    assert!(warned.get());
    assert!(!failed.get());
}

#[test]
fn js_engine_is_closed_after_run_flow_completes() {
    let mut driver = FakeDriver::new();
    let mut orch = Orchestra::new(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
    );
    let res = orch.run_commands(&[MaestroCommand::BackPress(Basic::default())]);
    assert!(res.success);
    assert!(res.js_closed);
}

#[test]
fn optional_assert_dark_mode_is_warned_not_failed() {
    let mut driver = FakeDriver::new();
    let warned = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let w2 = Rc::clone(&warned);
    let f2 = Rc::clone(&failed);
    let cbs = OrchestraCallbacks {
        on_command_warned: Some(Box::new(move |_, _| w2.set(true))),
        on_command_failed: Some(Box::new(move |_, _, _| {
            f2.set(true);
            ErrorResolution::Fail
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let res = orch.run_commands(&[MaestroCommand::AssertDarkMode(Basic { label: None, optional: true })]);
    assert!(res.success);
    assert!(warned.get());
    assert!(!failed.get());
}

#[test]
fn optional_command_with_unparseable_numeric_field_fails_hard() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Foo".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let warned = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let w2 = Rc::clone(&warned);
    let f2 = Rc::clone(&failed);
    let cbs = OrchestraCallbacks {
        on_command_warned: Some(Box::new(move |_, _| w2.set(true))),
        on_command_failed: Some(Box::new(move |_, _, _| {
            f2.set(true);
            ErrorResolution::Fail
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
        cbs,
    );
    let res = orch.run_commands(&[MaestroCommand::TapOnElement(TapOnElement {
        selector: ElementSelector {
            text_regex: Some("Foo".into()),
            index: Some("undefined".into()),
            ..Default::default()
        },
        retry_if_no_change: None,
        wait_until_visible: None,
        long_press: None,
        repeat: None,
        wait_to_settle_timeout_ms: None,
        relative_point: None,
        label: None,
        optional: true,
    })]);
    assert!(!res.success);
    assert!(failed.get());
    assert!(!warned.get());
}

// --- Additional F3 verification tests ---

#[test]
fn cli_env_wins_over_top_level_flow_env_while_subflow_can_override() {
    let tmp = TempOut::new("cli_env_prec");
    let flow_path = tmp.path().join("flow.yaml");
    std::fs::write(
        &flow_path,
        r#"appId: com.example.app
env:
  TARGET: flow_default
  DERIVED: "${TARGET}_derived"
---
- inputText: ${TARGET}
- inputText: ${DERIVED}
- runFlow:
    env:
      TARGET: subflow_override
    commands:
      - inputText: ${TARGET}
- inputText: ${TARGET}
"#,
    )
    .unwrap();
    let parsed = flow::parse_file(&flow_path).unwrap();
    let mut driver = FakeDriver::new();
    let mut opts = default_opts(&parsed);
    opts.cli_env = HashMap::from([("TARGET".to_string(), "cli_value".to_string())]);
    let mut orch = Orchestra::new(&mut driver, opts);
    let res = orch.run_parsed_flow(&parsed);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(
        driver.events,
        vec![
            FakeEvent::InputText("cli_value".into()),
            FakeEvent::InputText("cli_value_derived".into()),
            FakeEvent::InputText("subflow_override".into()),
            FakeEvent::InputText("cli_value".into()),
        ]
    );
    // Pure env interpolation never initialized QuickJS!
    assert!(!res.js_metrics.runtime_initialized, "pure env flow must stay on zero-JS fast path");
}

#[test]
fn retry_caps_max_retries_at_three() {
    let attempts = Rc::new(Cell::new(0usize));
    let a2 = Rc::clone(&attempts);
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Btn".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            a2.set(a2.get() + 1);
            Err(MaestroError::assertion("always fails").into())
        }));
    });
    let cmds = vec![MaestroCommand::Retry(Retry {
        max_retries: Some("10".into()),
        commands: vec![MaestroCommand::TapOnElement(TapOnElement {
            selector: ElementSelector { text_regex: Some("Btn".into()), ..Default::default() },
            retry_if_no_change: None,
            wait_until_visible: None,
            long_press: None,
            repeat: None,
            wait_to_settle_timeout_ms: None,
            relative_point: None,
            label: None,
            optional: false,
        })],
        source_description: None,
        config: None,
        label: None,
        optional: false,
    })];
    let mut orch = Orchestra::new(
        &mut driver,
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() },
    );
    let res = orch.run_commands(&cmds);
    assert!(!res.success);
    // 1 initial attempt + 3 capped retries = 4 total executions.
    assert_eq!(attempts.get(), 4);
}

#[test]
fn assert_screenshot_passes_and_fails_with_fake_driver_and_crop_on() {
    let out = TempOut::new("assert_shot");
    let ref_img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(100, 100, Rgba([20, 40, 60, 255])));
    let ref_path = out.path().join("ref.png");
    ref_img.save(&ref_path).unwrap();

    let flow_path = out.path().join("shot.yaml");
    std::fs::write(
        &flow_path,
        "appId: com.example.app\n---\n- assertScreenshot:\n    path: ref\n    thresholdPercentage: 99\n",
    )
    .unwrap();
    let parsed = flow::parse_file(&flow_path).unwrap();

    let mut driver = FakeDriver::new();
    driver.custom_screenshot = Some(ref_img);
    let mut opts = default_opts(&parsed);
    opts.test_output_dir = Some(out.path().to_path_buf());
    let mut orch = Orchestra::new(&mut driver, opts.clone());
    assert!(orch.run_parsed_flow(&parsed).success);

    // Now supply a different screenshot -> should fail with AssertionError and write diff PNG.
    driver.custom_screenshot =
        Some(DynamicImage::ImageRgba8(RgbaImage::from_pixel(100, 100, Rgba([240, 10, 10, 255]))));
    let mut orch2 = Orchestra::new(&mut driver, opts);
    let res2 = orch2.run_parsed_flow(&parsed);
    assert!(!res2.success);
    assert!(is_assertion_failure(res2.error.as_ref().unwrap()));
    assert!(out.path().join("screenshot-diffs/ref_diff.png").exists());
}

#[test]
fn ai_commands_fail_cleanly_with_post_mvp_message() {
    let tmp = TempOut::new("ai_cmd");
    let flow_path = tmp.path().join("ai.yaml");
    std::fs::write(
        &flow_path,
        "appId: com.example.app\n---\n- assertWithAI:\n    assertion: looks good\n    optional: false\n",
    )
    .unwrap();
    let parsed = flow::parse_file(&flow_path).unwrap();
    let mut driver = FakeDriver::new();
    let mut orch = Orchestra::new(&mut driver, default_opts(&parsed));
    let res = orch.run_parsed_flow(&parsed);
    assert!(!res.success);
    let err = res.error.unwrap();
    assert!(err.downcast_ref::<MaestroError>().is_some_and(|m| m.kind == MaestroErrorKind::CloudApiKeyNotAvailable));
}

#[test]
fn repeat_loop_uses_compiled_js_cache_and_runs_fast() {
    let tmp = TempOut::new("perf_loop");
    let flow_path = tmp.path().join("loop.yaml");
    std::fs::write(
        &flow_path,
        r#"appId: com.example.app
---
- evalScript: ${output.i = 0}
- repeat:
    while:
      true: ${output.i < 50}
    commands:
      - evalScript: ${output.i = output.i + 1}
- assertTrue: ${output.i === 50}
"#,
    )
    .unwrap();
    let parsed = flow::parse_file(&flow_path).unwrap();
    let mut driver = FakeDriver::new();
    let mut orch = Orchestra::new(&mut driver, default_opts(&parsed));
    let res = orch.run_parsed_flow(&parsed);
    assert!(res.success, "{:?}", res.error);
    assert!(res.js_metrics.runtime_initialized);
    assert!(res.js_metrics.cache_hits >= 90, "expected >=90 cache hits, got {}", res.js_metrics.cache_hits);
    assert!(res.total_ms < 500.0, "50-iteration JS loop took {:.2} ms", res.total_ms);
}
