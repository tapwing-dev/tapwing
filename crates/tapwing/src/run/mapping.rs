//! Pure stateless mappings between Maestro IR commands/selectors and the
//! Android / iOS Simulator driver wire formats.

use anyhow::Result;
use serde_json::Value;

use crate::backend::parse_index;
use crate::device::Platform;
use crate::driver::DriverError;
use crate::flow::ir::{Direction, ElementSelector, ElementTrait, KeyCode};

/// `scroll` duration: 400 ms on Android (`AndroidDriver.scrollVertical`),
/// 333 ms on iOS (`IOSDriver.scrollVertical`).
pub fn scroll_duration_ms(platform: Platform) -> u64 {
    match platform {
        Platform::Android => 400,
        Platform::Ios => 333,
    }
}

/// Maps driver-unit crop bounds to screenshot pixels like `Maestro.takeScreenshot`
/// (`Maestro.kt:651-663`): `scale = heightPixels / heightGrid`, origin and size
/// truncated separately. Identity on Android (`scale == 1`).
pub fn scale_crop_bounds(b: [i64; 4], scale: f64) -> [i64; 4] {
    if (scale - 1.0).abs() < f64::EPSILON {
        return b;
    }
    let sx = (b[0] as f64 * scale) as i64;
    let sy = (b[1] as f64 * scale) as i64;
    let w = ((b[2] - b[0]) as f64 * scale) as i64;
    let h = ((b[3] - b[1]) as f64 * scale) as i64;
    [sx, sy, sx + w, sy + h]
}

/// Keys routed through the Flutter engine boundary instead of `adb shell input keyevent`.
pub fn is_in_app_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab | KeyCode::Back | KeyCode::Escape)
}

/// Keys Maestro iOS types into the app (`IOSDriver.pressKey`: BACKSPACE ->
/// "delete", ENTER -> "return"); routed through the in-app driver.
pub fn is_in_app_key_ios(code: KeyCode) -> bool {
    matches!(code, KeyCode::Enter | KeyCode::Backspace)
}

/// How Maestro iOS handles a key `IOSDriver.pressKey` does not type into the
/// app: HOME/LOCK press hardware buttons through XCTest (not available
/// without a native lane), every other key is silently ignored.
pub fn ios_host_key(code: KeyCode) -> IosHostKey {
    match code {
        KeyCode::Home | KeyCode::Lock => IosHostKey::Unsupported,
        _ => IosHostKey::Ignored,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum IosHostKey {
    Unsupported,
    Ignored,
}

pub(super) fn ios_native_key_name(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Enter => "Enter",
        KeyCode::Backspace => "Backspace",
        KeyCode::Home => "Home",
        KeyCode::Lock => "Lock",
        _ => "",
    }
}

pub(super) fn in_app_key_name(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Enter => "ENTER",
        KeyCode::Backspace => "BACKSPACE",
        KeyCode::Tab => "TAB",
        KeyCode::Back => "BACK",
        KeyCode::Escape => "ESCAPE",
        _ => unreachable!("non in-app key passed to in_app_key_name: {code:?}"),
    }
}

/// Android `KeyEvent.KEYCODE_*` values matching `AndroidDriver.kt:287-325`.
pub fn android_keycode(code: KeyCode) -> u16 {
    match code {
        KeyCode::Enter => 66,
        KeyCode::Backspace => 67,
        KeyCode::Back => 4,
        KeyCode::Home => 3,
        KeyCode::Lock => 276,
        KeyCode::VolumeUp => 24,
        KeyCode::VolumeDown => 25,
        KeyCode::RemoteUp => 19,
        KeyCode::RemoteDown => 20,
        KeyCode::RemoteLeft => 21,
        KeyCode::RemoteRight => 22,
        KeyCode::RemoteCenter => 23,
        KeyCode::RemotePlayPause => 85,
        KeyCode::RemoteStop => 86,
        KeyCode::RemoteNext => 87,
        KeyCode::RemotePrevious => 88,
        KeyCode::RemoteRewind => 89,
        KeyCode::RemoteFastForward => 90,
        KeyCode::Escape => 111,
        KeyCode::Power => 26,
        KeyCode::Tab => 61,
        KeyCode::RemoteSystemNavigationUp => 280,
        KeyCode::RemoteSystemNavigationDown => 281,
        KeyCode::RemoteButtonA => 96,
        KeyCode::RemoteButtonB => 97,
        KeyCode::RemoteMenu => 82,
        KeyCode::TvInput => 178,
        KeyCode::TvInputHdmi1 => 243,
        KeyCode::TvInputHdmi2 => 244,
        KeyCode::TvInputHdmi3 => 245,
    }
}

/// Exact screen-wide swipe start/end percentages (`AndroidDriver.kt:399-446`,
/// `IOSDriver.kt:284-338`; only `UP` differs: iOS starts at 90%).
pub fn screen_swipe_ratios(d: Direction, platform: Platform) -> (&'static str, &'static str) {
    match d {
        Direction::Up if platform == Platform::Ios => ("50%, 90%", "50%, 10%"),
        Direction::Up => ("50%, 50%", "50%, 10%"),
        Direction::Down => ("50%, 20%", "50%, 90%"),
        Direction::Right => ("10%, 50%", "90%, 50%"),
        Direction::Left => ("90%, 50%", "10%, 50%"),
    }
}

pub(super) fn direction(d: Direction) -> &'static str {
    match d {
        Direction::Up => "UP",
        Direction::Down => "DOWN",
        Direction::Left => "LEFT",
        Direction::Right => "RIGHT",
    }
}

fn trait_name(t: ElementTrait) -> &'static str {
    match t {
        ElementTrait::Text => "TEXT",
        ElementTrait::Square => "SQUARE",
        ElementTrait::LongText => "LONG_TEXT",
    }
}

/// The selector shape the Dart driver understands (`Selector.fromJson`).
/// Assumes `${...}` expressions have already been evaluated by [`crate::orchestra::Orchestra`].
pub fn driver_selector(sel: &ElementSelector) -> Result<Value> {
    let sub =
        |s: &Option<Box<ElementSelector>>| -> Result<Option<Value>> { s.as_deref().map(driver_selector).transpose() };
    let mut out = serde_json::Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            out.insert(k.to_string(), v);
        }
    };
    put("text", sel.text_regex.as_deref().map(Value::from));
    put("id", sel.id_regex.as_deref().map(Value::from));
    if let Some(index) = &sel.index {
        let parsed = parse_index(index)?;
        put("index", Some(parsed.into()));
    }
    put("enabled", sel.enabled.map(Value::from));
    put("checked", sel.checked.map(Value::from));
    put("focused", sel.focused.map(Value::from));
    put("selected", sel.selected.map(Value::from));
    if let Some(size) = &sel.size {
        put("width", size.width.map(Value::from));
        put("height", size.height.map(Value::from));
        put("tolerance", size.tolerance.map(Value::from));
    }
    if let Some(traits) = &sel.traits {
        put("traits", Some(Value::Array(traits.iter().map(|t| Value::from(trait_name(*t))).collect())));
    }
    put("css", sel.css.as_deref().map(Value::from));
    put("below", sub(&sel.below)?);
    put("above", sub(&sel.above)?);
    put("leftOf", sub(&sel.left_of)?);
    put("rightOf", sub(&sel.right_of)?);
    put("childOf", sub(&sel.child_of)?);
    put("containsChild", sub(&sel.contains_child)?);
    if let Some(list) = &sel.contains_descendants {
        put("containsDescendants", Some(Value::Array(list.iter().map(driver_selector).collect::<Result<_>>()?)));
    }
    Ok(Value::Object(out))
}

pub fn explain(e: &anyhow::Error) -> String {
    if let Some(d) = e.downcast_ref::<DriverError>()
        && d.code == "ELEMENT_NOT_FOUND"
    {
        let seen: Vec<String> = d.details["visibleTexts"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| format!("\"{s}\""))).take(12).collect())
            .unwrap_or_default();
        return format!("{}\n      on screen: {}", d.message, seen.join(", "));
    }
    format!("{e:#}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use anyhow::anyhow;
    use serde_json::json;

    use super::*;
    use crate::backend::{
        AssertionError, DEFAULT_SCROLL_TIMEOUT_MS, InvalidNumericError, is_optional_downgradable, scroll_timeout_ms,
        speed_to_duration,
    };
    use crate::flow::MaestroCommand;
    use crate::flow::ir::{Basic, Platform as IrPlatform, TakeScreenshot};
    use crate::native_lane::InputMode;
    use crate::orchestra::{adjusted_timeout, is_optional, settles_for_platform};
    use crate::run::{Opts, driver_config};

    #[test]
    fn speed_to_duration_matches_maestro() {
        assert_eq!(speed_to_duration("40").unwrap(), 601);
        assert_eq!(speed_to_duration("100").unwrap(), 1);
        assert_eq!(speed_to_duration("0").unwrap(), 1001);
        assert_eq!(speed_to_duration("50").unwrap(), 501);
        assert!(speed_to_duration("-1").unwrap_err().is::<InvalidNumericError>());
        assert!(speed_to_duration("101").unwrap_err().is::<InvalidNumericError>());
        assert!(speed_to_duration("fast").unwrap_err().is::<InvalidNumericError>());
    }

    #[test]
    fn scroll_timeout_negative_falls_back_to_default() {
        assert_eq!(scroll_timeout_ms("5000").unwrap(), 5000);
        assert_eq!(scroll_timeout_ms("-1").unwrap(), DEFAULT_SCROLL_TIMEOUT_MS);
        assert!(scroll_timeout_ms("NaN").unwrap_err().is::<InvalidNumericError>());
    }

    #[test]
    fn driver_selector_serializes_traits_and_css_and_validates_index() {
        let sel = ElementSelector {
            text_regex: Some("Hello".into()),
            traits: Some(vec![ElementTrait::Text, ElementTrait::Square, ElementTrait::LongText]),
            css: Some(".btn".into()),
            index: Some("2".into()),
            ..Default::default()
        };
        let v = driver_selector(&sel).unwrap();
        assert_eq!(v["text"], "Hello");
        assert_eq!(v["traits"], json!(["TEXT", "SQUARE", "LONG_TEXT"]));
        assert_eq!(v["css"], ".btn");
        assert_eq!(v["index"], 2);

        let bad_index = ElementSelector { index: Some("abc".into()), ..Default::default() };
        assert!(driver_selector(&bad_index).unwrap_err().is::<InvalidNumericError>());
    }

    #[test]
    fn key_routing_and_android_keycodes() {
        for k in [KeyCode::Enter, KeyCode::Backspace, KeyCode::Tab, KeyCode::Back, KeyCode::Escape] {
            assert!(is_in_app_key(k));
        }
        assert!(!is_in_app_key(KeyCode::Home));
        assert!(!is_in_app_key(KeyCode::VolumeUp));
        assert_eq!(android_keycode(KeyCode::Home), 3);
        assert_eq!(android_keycode(KeyCode::VolumeUp), 24);
        assert_eq!(android_keycode(KeyCode::VolumeDown), 25);
        assert_eq!(android_keycode(KeyCode::Power), 26);
        assert_eq!(android_keycode(KeyCode::Lock), 276);
        assert_eq!(android_keycode(KeyCode::TvInputHdmi3), 245);
    }

    #[test]
    fn screen_swipe_ratios_match_android_driver() {
        let a = Platform::Android;
        assert_eq!(screen_swipe_ratios(Direction::Up, a), ("50%, 50%", "50%, 10%"));
        assert_eq!(screen_swipe_ratios(Direction::Down, a), ("50%, 20%", "50%, 90%"));
        assert_eq!(screen_swipe_ratios(Direction::Right, a), ("10%, 50%", "90%, 50%"));
        assert_eq!(screen_swipe_ratios(Direction::Left, a), ("90%, 50%", "10%, 50%"));
    }

    #[test]
    fn ios_swipe_scroll_and_crop_match_ios_driver() {
        let ios = Platform::Ios;
        assert_eq!(screen_swipe_ratios(Direction::Up, ios), ("50%, 90%", "50%, 10%"));
        assert_eq!(screen_swipe_ratios(Direction::Down, ios), ("50%, 20%", "50%, 90%"));
        assert_eq!(screen_swipe_ratios(Direction::Right, ios), ("10%, 50%", "90%, 50%"));
        assert_eq!(screen_swipe_ratios(Direction::Left, ios), ("90%, 50%", "10%, 50%"));
        assert_eq!(scroll_duration_ms(ios), 333);
        assert_eq!(scroll_duration_ms(Platform::Android), 400);
        // Point bounds -> @3x screenshot pixels (Maestro.kt:651-663).
        assert_eq!(scale_crop_bounds([24, 342, 378, 390], 3.0), [72, 1026, 1134, 1170]);
        assert_eq!(scale_crop_bounds([10, 10, 20, 20], 1.0), [10, 10, 20, 20]);
    }

    #[test]
    fn ios_key_and_back_routing_matches_ios_driver() {
        assert!(is_in_app_key_ios(KeyCode::Enter));
        assert!(is_in_app_key_ios(KeyCode::Backspace));
        for k in [KeyCode::Back, KeyCode::Tab, KeyCode::Escape, KeyCode::VolumeUp] {
            assert!(!is_in_app_key_ios(k));
            assert_eq!(ios_host_key(k), IosHostKey::Ignored);
        }
        assert_eq!(ios_host_key(KeyCode::Home), IosHostKey::Unsupported);
        assert_eq!(ios_host_key(KeyCode::Lock), IosHostKey::Unsupported);

        let back = MaestroCommand::BackPress(Basic { label: None, optional: false });
        assert!(!settles_for_platform(&back, IrPlatform::Ios));
        assert!(settles_for_platform(&back, IrPlatform::Android));
    }

    #[test]
    fn driver_config_carries_platform_app_name_and_strict_mode() {
        let opts = Opts {
            settle_policy: "v2".into(),
            check_settle: false,
            settle_load: Default::default(),
            cli_env: HashMap::new(),
            test_output_dir: PathBuf::from("out"),
            strict_input: false,
            soft_keyboard_hint: Some(false),
            maestro_compat_strict: true,
            http_agent: None,
            input_mode: InputMode::Auto,
            shard_index: None,
            log_prefix: String::new(),
            app_errors: crate::app_health::AppErrorsPolicy::Report,
            trace: false,
            driver_probe: false,
        };
        let ios = driver_config(&opts, Platform::Ios, Some("Sample App"));
        assert_eq!(ios["platform"], "ios");
        assert_eq!(ios["appName"], "Sample App");
        assert_eq!(ios["strictHideKeyboard"], true);
        assert_eq!(ios["softKeyboard"], false);
        let android = driver_config(&opts, Platform::Android, None);
        assert_eq!(android["platform"], "android");
        assert_eq!(android["appName"], "");
        // Strict hideKeyboard emulation only exists for Maestro iOS.
        assert_eq!(android["strictHideKeyboard"], false);
        // F8b: the HTTP log is switched off explicitly without `--trace`.
        assert_eq!(android["httpLog"], false);
        // Settle load options are sent explicitly, defaults included.
        assert_eq!(android["settleLoadTimeoutMs"], 5000);
        assert_eq!(android["settleHttpIgnore"], serde_json::json!([]));
        assert_eq!(driver_config(&Opts { trace: true, ..opts }, Platform::Android, None)["httpLog"], true);
    }

    #[test]
    fn settle_load_options_merge_cli_over_config_and_reach_configure() {
        use crate::run::SettleLoadOpts;
        use crate::workspace::config::TapwingConfig;

        let config = TapwingConfig {
            settle_load_timeout_ms: Some(2000),
            settle_http_ignore: vec!["/poll".into()],
            ..Default::default()
        };
        assert_eq!(SettleLoadOpts::resolve(None, &[], &TapwingConfig::default()).unwrap(), SettleLoadOpts::default());
        assert_eq!(SettleLoadOpts::default().timeout_ms, 5000);
        assert_eq!(
            SettleLoadOpts::resolve(None, &[], &config).unwrap(),
            SettleLoadOpts { timeout_ms: 2000, http_ignore: vec!["/poll".into()] }
        );
        // The CLI timeout wins (0 removes the bound); CLI patterns are added.
        let merged = SettleLoadOpts::resolve(Some(0), &["analytics".to_string()], &config).unwrap();
        assert_eq!(merged, SettleLoadOpts { timeout_ms: 0, http_ignore: vec!["/poll".into(), "analytics".into()] });
        // Huge values that would overflow Dart Duration microseconds are rejected.
        let huge = SettleLoadOpts::resolve(Some(crate::run::MAX_SETTLE_LOAD_TIMEOUT_MS + 1), &[], &config);
        assert!(huge.is_err(), "expected timeout above 24h cap to fail");

        let opts = Opts {
            settle_policy: "v2".into(),
            check_settle: false,
            settle_load: merged.clone(),
            cli_env: HashMap::new(),
            test_output_dir: PathBuf::from("out"),
            strict_input: false,
            soft_keyboard_hint: None,
            maestro_compat_strict: false,
            http_agent: None,
            input_mode: InputMode::Auto,
            shard_index: None,
            log_prefix: String::new(),
            app_errors: crate::app_health::AppErrorsPolicy::Report,
            trace: false,
            driver_probe: false,
        };
        let cfg = driver_config(&opts, Platform::Android, None);
        assert_eq!(cfg["settleLoadTimeoutMs"], 0);
        assert_eq!(cfg["settleHttpIgnore"], serde_json::json!(["/poll", "analytics"]));

        // A driver that predates the options never echoes them: warn only
        // when the user asked for something other than the defaults.
        let old = serde_json::json!({ "settlePolicy": "v2", "pixelsPerUnit": 1 });
        let new = serde_json::json!({ "settlePolicy": "v2", "settleLoadTimeoutMs": 0, "settleHttpIgnore": 2 });
        assert!(merged.unsupported_warning(&old).unwrap().contains("--settle-http-ignore"));
        assert_eq!(merged.unsupported_warning(&new), None);
        assert_eq!(SettleLoadOpts::default().unsupported_warning(&old), None);
    }

    #[test]
    fn adjusted_timeout_subtracts_elapsed_and_clamps_to_zero() {
        let past = Instant::now() - Duration::from_millis(500);
        let adj = adjusted_timeout(17_000, past);
        assert!((16_300..=16_500).contains(&adj), "unexpected adjusted timeout: {adj}");
        assert_eq!(adjusted_timeout(200, past), 0);
    }

    #[test]
    fn optional_error_filtering_and_crop_on_optional_match_maestro() {
        assert!(!is_optional_downgradable(&anyhow::Error::new(InvalidNumericError("bad".into()))));
        assert!(!is_optional_downgradable(&anyhow!("connection to app lost")));
        assert!(!is_optional_downgradable(&anyhow::Error::new(DriverError {
            code: "INTERNAL".into(),
            message: "boom".into(),
            details: Value::Null,
        })));
        assert!(is_optional_downgradable(&anyhow::Error::new(AssertionError("mismatch".into()))));
        assert!(is_optional_downgradable(&anyhow::Error::new(DriverError {
            code: "ELEMENT_NOT_FOUND".into(),
            message: "missing".into(),
            details: Value::Null,
        })));

        let shot = MaestroCommand::TakeScreenshot(TakeScreenshot {
            path: "shot".into(),
            crop_on: Some(ElementSelector { optional: true, ..Default::default() }),
            label: None,
            optional: false,
        });
        assert!(!is_optional(&shot));
    }
}
