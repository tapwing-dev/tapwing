//! Port of Maestro 2.12.1 `IntegrationTest.kt` (Cases 001–090 + Case 139)
//! running real Maestro YAML/JS fixtures through `tapwing::flow::parse_file`,
//! `Orchestra`, QuickJS-ng `JsEngine`, and in-memory `FakeDriver`.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use serde_json::json;
use tapwing::backend::{MaestroError, MaestroErrorKind};
use tapwing::fake_driver::{Bounds, FakeDriver, FakeEvent, FakeLayoutElement};
use tapwing::flow::ir::{Direction, KeyCode, ValueMap};
use tapwing::orchestra::{ErrorResolution, FlowResult, Orchestra, OrchestraCallbacks};

use crate::common::{
    TempDir as TempOut, assert_events_in_order, default_opts, is_assertion_failure, load_case, run_case,
};

fn run_case_env(case_name: &str, driver: &mut FakeDriver, env: &[(&str, &str)]) -> FlowResult {
    let flow = load_case(case_name);
    let mut opts = default_opts(&flow);
    opts.cli_env = env.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    let mut orch = Orchestra::new(driver, opts);
    orch.run_parsed_flow(&flow)
}

fn run_case_with_out(case_name: &str, driver: &mut FakeDriver, out: &Path, env: &[(&str, &str)]) -> FlowResult {
    let flow = load_case(case_name);
    let mut opts = default_opts(&flow);
    opts.test_output_dir = Some(out.to_path_buf());
    opts.cli_env = env.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    let mut orch = Orchestra::new(driver, opts);
    orch.run_parsed_flow(&flow)
}

fn is_element_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<MaestroError>().is_some_and(|m| m.kind == MaestroErrorKind::ElementNotFound)
}

fn is_unable_to_launch(err: &anyhow::Error) -> bool {
    err.downcast_ref::<MaestroError>().is_some_and(|m| m.kind == MaestroErrorKind::UnableToLaunchApp)
}

#[test]
fn case_001_assert_element_visible_by_id() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("element_id".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("001_assert_visible_by_id", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_002_assert_element_visible_by_text() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Element Text".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("002_assert_visible_by_text", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_003_assert_element_visible_by_size() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Element Text".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("003_assert_visible_by_size", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_004_assert_visible_no_element_with_id() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("another_id".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("004_assert_no_visible_element_with_id", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
}

#[test]
fn case_005_assert_visible_no_element_with_text() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Some other text".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("005_assert_no_visible_element_with_text", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
}

#[test]
fn case_006_assert_visible_no_element_with_size() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Some other text".into());
        e.bounds = Some(Bounds::new(0, 0, 101, 101));
    });
    let res = run_case("006_assert_no_visible_element_with_size", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
}

#[test]
fn case_007_assert_element_visible_by_size_with_tolerance() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Element Text".into());
        e.bounds = Some(Bounds::new(0, 0, 101, 101));
    });
    let res = run_case("007_assert_visible_by_size_with_tolerance", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_008_tap_on_element_no_retry_by_default() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Primary button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("008_tap_on_element", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
}

#[test]
fn case_008_tap_on_element_no_retry_if_hierarchy_changed() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Primary button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(|el| {
            el.borrow_mut().text = Some("Updated text".into());
            Ok(())
        }));
    });
    let res = run_case("008_tap_on_element", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
}

#[test]
fn case_008_tap_on_element_no_retry_if_screenshot_changed() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Primary button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(|el| {
            el.borrow_mut().color = [255, 0, 0, 255];
            Ok(())
        }));
    });
    let res = run_case("008_tap_on_element", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
}

#[test]
fn case_009_skip_optional_elements() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Non Optional".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("009_skip_optional_elements", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_010_scroll() {
    let mut driver = FakeDriver::new();
    let res = run_case("010_scroll", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Scroll));
}

#[test]
fn case_011_back_press() {
    let mut driver = FakeDriver::new();
    let res = run_case("011_back_press", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::BackPress));
}

#[test]
fn case_012_input_text() {
    let mut driver = FakeDriver::new();
    let res = run_case("012_input_text", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::InputText("Hello World".into())));
    assert!(driver.has_event(&FakeEvent::InputText("user@example.com".into())));
    assert_eq!(driver.current_text, "Hello Worlduser@example.com");
}

#[test]
fn case_013_launch_app() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("013_launch_app", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::StopApp("com.example.app".into()),
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
        ],
    );
}

#[test]
fn case_014_tap_on_point() {
    let mut driver = FakeDriver::new();
    let res = run_case("014_tap_on_point", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 100, y: 200 }));
}

fn add_grid_3x3(driver: &mut FakeDriver) {
    let cells = [
        ("Top Left", 0, 100),
        ("Top", 100, 100),
        ("Top Right", 200, 100),
        ("Left", 0, 200),
        ("Middle", 100, 200),
        ("Right", 200, 200),
        ("Bottom Left", 0, 300),
        ("Bottom", 100, 300),
        ("Bottom Right", 200, 300),
    ];
    for (text, x, y) in cells {
        driver.add_element(|e| {
            e.text = Some(text.into());
            e.bounds = Some(Bounds::new(x, y, x + 100, y + 100));
        });
    }
}

#[test]
fn case_015_tap_on_element_relative_position() {
    let mut driver = FakeDriver::new();
    add_grid_3x3(&mut driver);
    let res = run_case("015_element_relative_position", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::Tap { x: 150, y: 150 },
            FakeEvent::Tap { x: 150, y: 350 },
            FakeEvent::Tap { x: 50, y: 250 },
            FakeEvent::Tap { x: 250, y: 250 },
            FakeEvent::Tap { x: 50, y: 150 },
            FakeEvent::Tap { x: 250, y: 150 },
            FakeEvent::Tap { x: 50, y: 350 },
            FakeEvent::Tap { x: 250, y: 350 },
        ],
    );
}

#[test]
fn case_016_multiline_text() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Hello World\nHere is a second line".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("016_multiline_text", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 50 }));
}

#[test]
fn case_017_swipe() {
    let mut driver = FakeDriver::new();
    let res = run_case("017_swipe", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Swipe { start: (100, 500), end: (100, 200), duration_ms: 3000 }));
}

#[test]
fn case_018_contains_child() {
    let mut driver = FakeDriver::new();
    let parent = driver.add_element(|e| {
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    FakeLayoutElement::add_child(&parent, |e| {
        e.text = Some("Child".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("018_contains_child", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 100, y: 100 }));
}

#[test]
fn case_139_contains_child_with_relative_selectors_and_transient_hierarchy_changes() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("top side".into());
        e.bounds = Some(Bounds::new(100, 50, 200, 80));
    });
    driver.add_element(|e| {
        e.text = Some("bottom side".into());
        e.bounds = Some(Bounds::new(100, 500, 200, 530));
    });
    let l0 = driver.add_element(|e| {
        e.id = Some("level-0".into());
        e.bounds = Some(Bounds::new(50, 100, 300, 450));
    });
    let l1 = FakeLayoutElement::add_child(&l0, |e| {
        e.id = Some("level-1".into());
        e.bounds = Some(Bounds::new(70, 120, 280, 430));
    });
    FakeLayoutElement::add_child(&l1, |e| {
        e.id = Some("level-2".into());
        e.bounds = Some(Bounds::new(90, 140, 260, 410));
    });
    let counter = Rc::new(Cell::new(0usize));
    let c2 = Rc::clone(&counter);
    FakeLayoutElement::add_child(&l1, |e| {
        e.bounds = Some(Bounds::new(90, 140, 200, 170));
        e.mutating_text = Some(Box::new(move || {
            let n = c2.get();
            c2.set(n + 1);
            format!("transient_{n}")
        }));
    });
    let res = run_case("139_contains_child_with_relative_position", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_019_do_not_wait_until_visible() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    driver.add_element(|e| {
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("019_dont_wait_for_visibility", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 50 }));
}

#[test]
fn case_020_parse_config() {
    let flow = load_case("020_parse_config");
    assert_eq!(flow.config.app_id.as_deref(), Some("com.example.app"));
    assert_eq!(flow.commands.len(), 2);
}

#[test]
fn case_021_launch_app_with_clear_state() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("021_launch_app_with_clear_state", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::ClearState("com.example.app".into())));
    assert!(
        driver.has_event(&FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() })
    );
}

#[test]
fn case_022_launch_app_that_is_not_installed() {
    let mut driver = FakeDriver::new();
    let res = run_case("022_launch_app_that_is_not_installed", &mut driver);
    assert!(!res.success);
    assert!(is_unable_to_launch(res.error.as_ref().unwrap()));
}

#[test]
fn case_025_tap_on_element_relative_position_using_shortcut() {
    let mut driver = FakeDriver::new();
    add_grid_3x3(&mut driver);
    let res = run_case("025_element_relative_position_shortcut", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::Tap { x: 150, y: 150 },
            FakeEvent::Tap { x: 150, y: 350 },
            FakeEvent::Tap { x: 50, y: 250 },
            FakeEvent::Tap { x: 250, y: 250 },
            FakeEvent::Tap { x: 50, y: 150 },
            FakeEvent::Tap { x: 250, y: 150 },
            FakeEvent::Tap { x: 50, y: 350 },
            FakeEvent::Tap { x: 250, y: 350 },
        ],
    );
}

#[test]
fn case_026_assert_not_visible_no_element_with_id() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("another_id".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("026_assert_not_visible", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_026_assert_not_visible_element_present_fails() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("element_id".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("026_assert_not_visible", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
}

#[test]
fn case_027_open_link() {
    let mut driver = FakeDriver::new();
    let res = run_case("027_open_link", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::OpenLink {
            link: "https://example.com".into(),
            app_id: Some("com.example.app".into()),
            auto_verify: false,
        }],
    );
}

#[test]
fn case_028_env() {
    let out = TempOut::new("028");
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    driver.add_element(|e| {
        e.id = Some("button_id".into());
        e.text = Some("button_text".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case_with_out(
        "028_env",
        &mut driver,
        out.path(),
        &[
            ("APP_ID", "com.example.app"),
            ("BUTTON_ID", "button_id"),
            ("BUTTON_TEXT", "button_text"),
            ("PASSWORD", "testPassword"),
            ("NON_EXISTENT_TEXT", "nonExistentText"),
            ("NON_EXISTENT_ID", "nonExistentId"),
            ("URL", "secretUrl"),
            ("LAT", "37.82778"),
            ("LNG", "-122.48167"),
        ],
    );
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
            FakeEvent::Tap { x: 50, y: 50 },
            FakeEvent::Tap { x: 50, y: 50 },
            FakeEvent::InputText("${PASSWORD} is testPassword".into()),
            FakeEvent::OpenLink {
                link: "https://example.com/secretUrl".into(),
                app_id: Some("com.example.app".into()),
                auto_verify: false,
            },
            FakeEvent::SetLocation { latitude: 37.82778, longitude: -122.48167 },
            FakeEvent::StartRecording,
        ],
    );
    assert!(out.path().join("startRecording/028_env.mp4").exists());
}

#[test]
fn case_029_long_press_on_element() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Primary button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("029_long_press_on_element", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::LongPress { x: 50, y: 50 }));
}

#[test]
fn case_030_long_press_on_point() {
    let mut driver = FakeDriver::new();
    let res = run_case("030_long_press_on_point", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::LongPress { x: 100, y: 200 }));
}

#[test]
fn case_031_traits() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Text".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 100));
    });
    driver.add_element(|e| {
        e.text = Some("Square".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    driver.add_element(|e| {
        e.text = Some("a".repeat(500));
        e.bounds = Some(Bounds::new(0, 200, 200, 400));
    });
    let res = run_case("031_traits", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::Tap { x: 100, y: 50 }, FakeEvent::Tap { x: 50, y: 150 }, FakeEvent::Tap { x: 100, y: 300 }],
    );
}

#[test]
fn case_032_element_index() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Item 2".into());
        e.bounds = Some(Bounds::new(0, 200, 100, 300));
    });
    driver.add_element(|e| {
        e.text = Some("Item 1".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let res = run_case("032_element_index", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::Tap { x: 50, y: 150 }, FakeEvent::Tap { x: 50, y: 250 }]);
}

#[test]
fn case_033_text_with_number() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("2022".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("033_int_text", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 50 }));
}

#[test]
fn case_034_press_key() {
    let mut driver = FakeDriver::new();
    let res = run_case("034_press_key", &mut driver);
    assert!(res.success, "{:?}", res.error);
    for key in [
        KeyCode::Enter,
        KeyCode::Backspace,
        KeyCode::Home,
        KeyCode::Back,
        KeyCode::VolumeUp,
        KeyCode::VolumeDown,
        KeyCode::Lock,
        KeyCode::RemoteUp,
        KeyCode::RemoteDown,
        KeyCode::RemoteLeft,
        KeyCode::RemoteRight,
        KeyCode::RemoteCenter,
        KeyCode::RemotePlayPause,
        KeyCode::RemoteStop,
        KeyCode::RemoteNext,
        KeyCode::RemotePrevious,
        KeyCode::RemoteRewind,
        KeyCode::RemoteFastForward,
        KeyCode::Power,
        KeyCode::Tab,
        KeyCode::RemoteSystemNavigationUp,
        KeyCode::RemoteSystemNavigationDown,
        KeyCode::RemoteButtonA,
        KeyCode::RemoteButtonB,
        KeyCode::RemoteMenu,
        KeyCode::TvInput,
        KeyCode::TvInputHdmi1,
        KeyCode::TvInputHdmi2,
        KeyCode::TvInputHdmi3,
    ] {
        assert!(driver.has_event(&FakeEvent::PressKey(key)), "missing key {key:?}");
    }
}

#[test]
fn case_035_ignore_duplicates_when_refreshing_item_position() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("icon".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    driver.add_element(|e| {
        e.text = Some("Item".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    driver.add_element(|e| {
        e.id = Some("icon".into());
        e.bounds = Some(Bounds::new(0, 200, 100, 300));
    });
    let res = run_case("035_refresh_position_ignore_duplicates", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 250 }));
}

#[test]
fn case_036_erase_text_with_numbers() {
    let mut driver = FakeDriver::new();
    let res = run_case("036_erase_text", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.current_text, "Hello");
}

#[test]
fn case_037_unicode_input_is_supported() {
    let mut driver = FakeDriver::new();
    let res = run_case("037_unicode_input", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(
        driver.current_text,
        "Tést inpütمرحبا بالعالم你好世界こんにちは世界안녕하세요Hello 👋 World 🌍Mixed مرحبا 你好"
    );
}

#[test]
fn case_038_partial_id_matching() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("com.google.android.inputmethod.latin:id/another_keyboard_area".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    driver.add_element(|e| {
        e.id = Some("com.google.android.inputmethod.latin:id/keyboard_area".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let res = run_case("038_partial_id", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::Tap { x: 50, y: 150 }, FakeEvent::Tap { x: 50, y: 50 }]);
}

#[test]
fn case_039_hide_keyboard() {
    let mut driver = FakeDriver::new();
    let res = run_case("039_hide_keyboard", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::HideKeyboard]);
}

#[test]
fn case_040_escape_regex_characters() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("+123456".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("040_escape_regex", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 50 }));
}

#[test]
fn case_041_take_screenshot() {
    let out = TempOut::new("041");
    let mut driver = FakeDriver::new();
    let res = run_case_with_out("041_take_screenshot", &mut driver, out.path(), &[]);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::TakeScreenshot]);
    assert!(out.path().join("screenshots/041_take_screenshot_with_filename.png").exists());
}

#[test]
fn case_042_extended_wait_until() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Item".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("042_extended_wait", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_042_extended_wait_until_element_not_found() {
    let mut driver = FakeDriver::new();
    let res = run_case("042_extended_wait", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
}

#[test]
fn case_043_stop_app() {
    let mut driver = FakeDriver::new();
    let res = run_case("043_stop_app", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::StopApp("com.example.app".into())));
    assert!(driver.has_event(&FakeEvent::StopApp("another.app".into())));
}

#[test]
fn case_044_clear_state() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    driver.add_installed_app("another.app");
    let res = run_case("044_clear_state", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::ClearState("com.example.app".into())));
    assert!(driver.has_event(&FakeEvent::ClearState("another.app".into())));
}

#[test]
fn case_045_clear_keychain() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("045_clear_keychain", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::ClearKeychain,
            FakeEvent::ClearKeychain,
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
        ],
    );
}

#[test]
fn case_046_run_flow() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    driver.add_installed_app("com.other.app");
    driver.add_element(|e| {
        e.text = Some("Primary button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    let res = run_case("046_run_flow", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
            FakeEvent::Tap { x: 50, y: 50 },
        ],
    );
}

#[test]
fn case_047_nested_run_flow() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    driver.add_installed_app("com.other.app");
    driver.add_element(|e| {
        e.text = Some("Primary button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    driver.add_element(|e| {
        e.text = Some("Secondary button".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
    });
    let res = run_case("047_run_flow_nested", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
            FakeEvent::Tap { x: 50, y: 50 },
            FakeEvent::Tap { x: 100, y: 100 },
        ],
    );
}

#[test]
fn case_048_tap_on_prioritises_clickable_elements() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 200, 200));
        e.clickable = true;
        e.on_click = Some(Box::new(|el| {
            el.borrow_mut().text = Some("Clicked".into());
            Ok(())
        }));
    });
    let res = run_case("048_tapOn_clickable", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::Tap { x: 100, y: 100 }]);
}

#[test]
fn case_049_run_flow_conditionally() {
    let mut driver = FakeDriver::new();
    let indicator = driver.add_element(|e| {
        e.text = Some("Not Clicked".into());
        e.bounds = Some(Bounds::new(0, 100, 0, 200));
    });
    let ind_clone = Rc::clone(&indicator);
    driver.add_element(|e| {
        e.text = Some("button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            ind_clone.borrow_mut().text = Some("Clicked".into());
            Ok(())
        }));
    });
    let res = run_case_env("049_run_flow_conditionally", &mut driver, &[("NOT_CLICKED", "Not Clicked")]);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
}

#[test]
fn case_051_set_location() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("051_set_location", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
            FakeEvent::SetLocation { latitude: 12.5266, longitude: 78.2150 },
        ],
    );
}

#[test]
fn case_052_input_random() {
    let mut driver = FakeDriver::new();
    let res = run_case("052_text_random", &mut driver);
    assert!(res.success, "{:?}", res.error);
    let inputs: Vec<&str> = driver
        .events
        .iter()
        .filter_map(|e| match e {
            FakeEvent::InputText(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert!(!inputs.is_empty());
    assert!(inputs.iter().all(|t| t.len() >= 5));
    assert!(inputs.iter().any(|t| t.parse::<i32>().is_ok_and(|n| (10000..=99999).contains(&n))));
    assert!(inputs.iter().any(|t| t.contains('@')));
    assert!(inputs.iter().any(|t| t.contains(' ')));
}

#[test]
fn case_053_repeat_n_times() {
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
            ind2.borrow_mut().text = Some(n.to_string());
            Ok(())
        }));
    });
    let res = run_case("053_repeat_times", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 6);
}

#[test]
fn case_054_enabled_state() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(|el| {
            el.borrow_mut().enabled = Some(false);
            Ok(())
        }));
    });
    let res = run_case("054_enabled", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 1);
}

#[test]
fn case_055_tap_on_element_compare_regex() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("(Secondary button)".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let res = run_case("055_compare_regex", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 150 }));
}

#[test]
fn case_056_ignore_error_in_orchestra() {
    let flow = load_case("056_ignore_error");
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let cbs = OrchestraCallbacks {
        on_command_failed: Some(Box::new(|_, cmd, _| {
            if let tapwing::flow::ir::MaestroCommand::TapOnElement(t) = cmd
                && t.selector.text_regex.as_deref() == Some("Non existent text")
            {
                ErrorResolution::Continue
            } else {
                ErrorResolution::Fail
            }
        })),
        ..Default::default()
    };
    let mut orch = Orchestra::with_callbacks(&mut driver, default_opts(&flow), cbs);
    let res = orch.run_parsed_flow(&flow);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 150 }));
}

#[test]
fn case_057_pass_inner_env_variables_to_run_flow() {
    let mut driver = FakeDriver::new();
    let res = run_case_env("057_runFlow_env", &mut driver, &[("OUTER_ENV", "Outer Parameter")]);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::InputText("Inner Parameter".into())));
    assert!(driver.has_event(&FakeEvent::InputText("Outer Parameter".into())));
    assert!(driver.has_event(&FakeEvent::InputText("Overridden Parameter".into())));
}

#[test]
fn case_058_inline_env_parameters() {
    let mut driver = FakeDriver::new();
    let res = run_case("058_inline_env", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::InputText("Inline Parameter".into())));
    assert!(driver.has_event(&FakeEvent::InputText("Overridden Parameter".into())));
}

#[test]
fn case_059_directional_swipe_command() {
    let mut driver = FakeDriver::new();
    let res = run_case("059_directional_swipe_command", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::SwipeWithDirection { direction: Direction::Right, duration_ms: 500 }));
}

#[test]
fn case_060_pass_env_param_to_env_param() {
    let mut driver = FakeDriver::new();
    let res = run_case_env("060_pass_env_to_env", &mut driver, &[("PARAM", "Value")]);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::InputText("Value".into())), 3);
}

#[test]
fn case_061_launch_app_without_stopping_it() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("061_launchApp_withoutStopping", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(!driver.has_event(&FakeEvent::StopApp("com.example.app".into())));
    assert!(
        driver.has_event(&FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() })
    );
}

#[test]
fn case_062_copy_paste_text() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("com.google.android.inputmethod.latin:id/myId".into());
        e.text = Some("Some text to copy".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let res = run_case("062_copy_paste_text", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.current_text, "Some text to copy");
}

#[test]
fn case_063_javascript_injection() {
    let mut driver = FakeDriver::new();
    let res = run_case("063_js_injection", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::InputText("1".into()),
            FakeEvent::InputText("2".into()),
            FakeEvent::InputText("12".into()),
            FakeEvent::InputText("3".into()),
            FakeEvent::InputText("${A} ${B} 1 2".into()),
        ],
    );
}

#[test]
fn case_064_javascript_files() {
    let mut driver = FakeDriver::new();
    let res = run_case("064_js_files", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::InputText("Main".into()),
            FakeEvent::InputText("Sub".into()),
            FakeEvent::InputText("Sub".into()),
            FakeEvent::InputText("Main".into()),
            FakeEvent::InputText("Sub".into()),
            FakeEvent::InputText("064_js_files".into()),
            FakeEvent::InputText("Hello, Input Parameter!".into()),
            FakeEvent::InputText("Hello, Evaluated Parameter!".into()),
            FakeEvent::InputText("064_js_files".into()),
        ],
    );
}

#[test]
fn case_065_when_true_condition() {
    let mut driver = FakeDriver::new();
    let res = run_case("065_when_true", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(
        driver.events,
        vec![
            FakeEvent::InputText("True".into()),
            FakeEvent::InputText("String".into()),
            FakeEvent::InputText("Positive Int".into()),
            FakeEvent::InputText("Object".into()),
            FakeEvent::InputText("Array".into()),
        ]
    );
}

#[test]
fn case_066_copy_text_into_js_variable() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.id = Some("Field".into());
        e.text = Some("Maestro".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let res = run_case("066_copyText_jsVar", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::InputText("Hello, Maestro".into())]);
}

#[test]
fn case_067_assert_true_pass() {
    let mut driver = FakeDriver::new();
    let res = run_case("067_assertTrue_pass", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_067_assert_true_fail() {
    let mut driver = FakeDriver::new();
    let res = run_case("067_assertTrue_fail", &mut driver);
    assert!(!res.success);
    assert!(is_assertion_failure(res.error.as_ref().unwrap()));
}

#[test]
fn case_068_erase_all_text() {
    let mut driver = FakeDriver::new();
    let res = run_case("068_erase_all_text", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.current_text, "");
}

#[test]
fn case_069_wait_for_animation_to_end() {
    let mut driver = FakeDriver::new();
    let res = run_case("069_wait_for_animation_to_end", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::TakeScreenshot, FakeEvent::TakeScreenshot]);
}

#[test]
fn case_070_evaluate_js_inline() {
    let mut driver = FakeDriver::new();
    let res = run_case("070_evalScript", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::InputText("2".into()), FakeEvent::InputText("Result is: 2".into())],
    );
}

#[test]
fn case_071_tap_on_relative_point() {
    let mut driver = FakeDriver::new();
    let info = driver.device_info();
    let res = run_case("071_tapOnRelativePoint", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::Tap { x: 0, y: 0 },
            FakeEvent::Tap { x: info.width_grid, y: info.height_grid },
            FakeEvent::Tap { x: info.width_grid / 2, y: info.height_grid / 2 },
            FakeEvent::Tap { x: info.width_grid / 4, y: info.height_grid / 4 },
            FakeEvent::Tap { x: info.width_grid / 4, y: info.height_grid / 4 },
        ],
    );
}

#[test]
fn case_072_search_depth_first() {
    let mut driver = FakeDriver::new();
    let outer = driver.add_element(|e| {
        e.text = Some("Element".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    FakeLayoutElement::add_child(&outer, |e| {
        e.text = Some("Element".into());
        e.bounds = Some(Bounds::new(0, 0, 50, 50));
    });
    driver.add_element(|e| {
        e.text = Some("Element".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 200));
    });
    let res = run_case("072_searchDepthFirst", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 25, y: 25 }));
}

#[test]
fn case_073_handle_linebreaks() {
    let mut driver = FakeDriver::new();
    let ind = driver.add_element(|e| {
        e.text = Some("Indicator".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 100));
    });
    let ind2 = Rc::clone(&ind);
    driver.add_element(|e| {
        e.text = Some("Hello\nWorld".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            let mut b = ind2.borrow_mut();
            let cur = b.text.clone().unwrap_or_default();
            b.text = Some(format!("{cur}!"));
            Ok(())
        }));
    });
    let res = run_case("073_handle_linebreaks", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 2);
}

#[test]
fn case_074_directional_swipe_on_elements() {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("swiping element".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 100));
    });
    let res = run_case("074_directional_swipe_element", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::SwipeElementWithDirection {
        point: (50, 100),
        direction: Direction::Right,
        duration_ms: 400,
    }));
}

#[test]
fn case_075_repeat_while() {
    let mut driver = FakeDriver::new();
    let counter = Rc::new(Cell::new(0i32));
    let counter_view = driver.add_element(|e| {
        e.text = Some("Value 0".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 100));
    });
    let c2 = Rc::clone(&counter);
    let cv2 = Rc::clone(&counter_view);
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            let n = c2.get() + 1;
            c2.set(n);
            cv2.borrow_mut().text = Some(format!("Value {n}"));
            Ok(())
        }));
    });
    let res = run_case("075_repeat_while", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 3);
}

#[test]
fn case_076_optional_assertion() {
    let mut driver = FakeDriver::new();
    let res = run_case("076_optional_assertion", &mut driver);
    assert!(res.success, "{:?}", res.error);
}

#[test]
fn case_077_env_special_characters() {
    let mut driver = FakeDriver::new();
    let special = "!@#$&*()_+{}|:\"<>?[]\\\\;',./";
    let res = run_case_env("077_env_special_characters", &mut driver, &[("OUTER", special)]);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::InputText(special.into()), FakeEvent::InputText(special.into())],
    );
}

#[test]
fn case_078_swipe_with_relative_coordinates() {
    let mut driver = FakeDriver::new();
    let info = driver.device_info();
    let res = run_case("078_swipe_relative", &mut driver);
    assert!(res.success, "{:?}", res.error);
    let expected_start = (info.width_grid / 2, info.height_grid * 30 / 100);
    let expected_end = (info.width_grid / 2, info.height_grid * 60 / 100);
    assert!(driver.has_event(&FakeEvent::Swipe { start: expected_start, end: expected_end, duration_ms: 3000 }));
}

#[test]
fn case_079_scroll_until_visible_no_view() {
    let mut driver = FakeDriver::new();
    let res = run_case("079_scroll_until_visible", &mut driver);
    assert!(!res.success);
    assert!(is_element_not_found(res.error.as_ref().unwrap()));
}

#[test]
fn case_079_2_scroll_until_visible_with_view() {
    let mut driver = FakeDriver::new();
    let h = driver.device_info().height_grid;
    driver.add_element(|e| {
        e.text = Some("Test".into());
        e.bounds = Some(Bounds::new(0, h, 100, h + 100));
    });
    let res = run_case("079_scroll_until_visible", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::SwipeElementWithDirection { point: (270, 480), direction: Direction::Up, duration_ms: 1 }],
    );
}

#[test]
fn case_080_hierarchy_pruning_assert_visible() {
    let mut driver = FakeDriver::new();
    let info = driver.device_info();
    let root = driver.add_element(|e| {
        e.id = Some("root".into());
        e.bounds = Some(Bounds::new(0, 0, 500, 500));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("visible_1".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("visible_2".into());
        e.bounds = Some(Bounds::new(info.width_grid - 50, 0, info.width_grid + 100, 100));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("visible_3".into());
        e.bounds = Some(Bounds::new(0, info.height_grid - 50, 100, info.height_grid + 100));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("visible_4".into());
        e.bounds = Some(Bounds::new(-100, -100, info.width_grid + 200, info.height_grid + 200));
    });
    let res = run_case("080_hierarchy_pruning_assert_visible", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_081_hierarchy_pruning_assert_not_visible() {
    let mut driver = FakeDriver::new();
    let info = driver.device_info();
    let root = driver.add_element(|e| {
        e.id = Some("root".into());
        e.bounds = Some(Bounds::new(0, 0, 500, 500));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("not_visible_1".into());
        e.bounds = Some(Bounds::new(-100, -100, 0, 0));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("not_visible_2".into());
        e.bounds = Some(Bounds::new(info.width_grid, 0, info.width_grid + 100, 100));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("not_visible_3".into());
        e.bounds = Some(Bounds::new(0, info.height_grid, 100, info.height_grid + 100));
    });
    FakeLayoutElement::add_child(&root, |e| {
        e.id = Some("not_visible_4".into());
        e.bounds = Some(Bounds::new(0, info.height_grid - 10, 100, info.height_grid + 100));
    });
    let res = run_case("081_hierarchy_pruning_assert_not_visible", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.events.is_empty());
}

#[test]
fn case_082_repeat_while_true() {
    let mut driver = FakeDriver::new();
    let counter = Rc::new(Cell::new(0i32));
    let cv = driver.add_element(|e| {
        e.text = Some("Value 0".into());
        e.bounds = Some(Bounds::new(0, 100, 100, 100));
    });
    let c2 = Rc::clone(&counter);
    let cv2 = Rc::clone(&cv);
    driver.add_element(|e| {
        e.text = Some("Button".into());
        e.bounds = Some(Bounds::new(0, 0, 100, 100));
        e.on_click = Some(Box::new(move |_| {
            let n = c2.get() + 1;
            c2.set(n);
            cv2.borrow_mut().text = Some(format!("Value {n}"));
            Ok(())
        }));
    });
    let res = run_case("082_repeat_while_true", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(driver.event_count(&FakeEvent::Tap { x: 50, y: 50 }), 3);
}

#[test]
fn case_083_assert_on_properties() {
    let mut driver = FakeDriver::new();
    let field = driver.add_element(|e| {
        e.text = Some("Field".into());
        e.checked = Some(true);
        e.selected = Some(true);
        e.focused = Some(true);
        e.bounds = Some(Bounds::of_size(100, 100));
    });
    let f2 = Rc::clone(&field);
    driver.add_element(|e| {
        e.text = Some("Flip".into());
        e.bounds = Some(Bounds::of_size(100, 100).translate(0, 100));
        e.on_click = Some(Box::new(move |_| {
            let mut b = f2.borrow_mut();
            b.checked = b.checked.map(|v| !v);
            b.selected = b.selected.map(|v| !v);
            b.enabled = b.enabled.map(|v| !v);
            b.focused = b.focused.map(|v| !v);
            Ok(())
        }));
    });
    let res = run_case("083_assert_properties", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert!(driver.has_event(&FakeEvent::Tap { x: 50, y: 150 }));
}

#[test]
fn case_084_open_browser() {
    let mut driver = FakeDriver::new();
    let res = run_case("084_open_browser", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(&driver.events, &[FakeEvent::OpenBrowser { link: "https://example.com".into() }]);
}

#[test]
fn case_085_open_link_with_auto_verify() {
    let mut driver = FakeDriver::new();
    let res = run_case("085_open_link_auto_verify", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[FakeEvent::OpenLink {
            link: "https://example.com".into(),
            app_id: Some("com.example.app".into()),
            auto_verify: true,
        }],
    );
}

#[test]
fn case_086_launch_app_sets_all_permissions_to_allow() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("086_launchApp_sets_all_permissions_to_allow", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::SetPermissions {
                app_id: "com.example.app".into(),
                permissions: BTreeMap::from([("all".into(), "allow".into())]),
            },
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
        ],
    );
}

#[test]
fn case_087_launch_app_with_all_permissions_to_deny() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("087_launchApp_with_all_permissions_to_deny", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::SetPermissions {
                app_id: "com.example.app".into(),
                permissions: BTreeMap::from([("all".into(), "deny".into())]),
            },
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
        ],
    );
}

#[test]
fn case_088_launch_app_deny_all_and_allow_notifications() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("088_launchApp_with_all_permissions_to_deny_and_notification_to_allow", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::SetPermissions {
                app_id: "com.example.app".into(),
                permissions: BTreeMap::from([("all".into(), "deny".into()), ("notifications".into(), "allow".into())]),
            },
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
        ],
    );
}

#[test]
fn case_089_launch_app_with_sms_permissions() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("089_launchApp_with_sms_permission_group_to_allow", &mut driver);
    assert!(res.success, "{:?}", res.error);
    assert_events_in_order(
        &driver.events,
        &[
            FakeEvent::SetPermissions {
                app_id: "com.example.app".into(),
                permissions: BTreeMap::from([("sms".into(), "allow".into())]),
            },
            FakeEvent::LaunchApp { app_id: "com.example.app".into(), launch_arguments: ValueMap::new() },
        ],
    );
}

#[test]
fn case_090_travel() {
    let mut driver = FakeDriver::new();
    driver.add_installed_app("com.example.app");
    let res = run_case("090_travel", &mut driver);
    assert!(res.success, "{:?}", res.error);
    let locs: Vec<(f64, f64)> = driver
        .events
        .iter()
        .filter_map(|e| match e {
            FakeEvent::SetLocation { latitude, longitude } => Some((*latitude, *longitude)),
            _ => None,
        })
        .collect();
    let expected = [(0.0, 0.0), (0.1, 0.0), (0.1, 0.1), (0.0, 0.1)];
    let mut idx = 0;
    for (elat, elon) in expected {
        let found = locs[idx..].iter().position(|(lat, lon)| (lat - elat).abs() < 1e-6 && (lon - elon).abs() < 1e-6);
        assert!(found.is_some(), "missing waypoint ({elat}, {elon}) in {locs:?}");
        idx += found.unwrap() + 1;
    }
    let _ = json!({});
}
