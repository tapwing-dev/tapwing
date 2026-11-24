//! Driver abstraction (`FlowDriver`) and Maestro-compatible error taxonomy.
//!
//! The orchestrator executes every UI/device command through [`FlowDriver`],
//! allowing the exact same orchestration, JS engine, and condition logic to
//! run against both:
//! - the real Android + `tapwing_driver` session ([`crate::run::AndroidFlutterBackend`])
//! - the in-memory [`crate::fake_driver::FakeDriver`] used by the ported
//!   Maestro integration test suite.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};
use image::DynamicImage;
use serde_json::Value;

use crate::driver::DriverError;
use crate::flow::ir::{Direction, ElementSelector, KeyCode, Platform, Point, TapRepeat, ValueMap};

/// Maestro's element lookup timeouts (`Orchestra.kt:142-143`): 17 s default, 7 s optional.
pub const LOOKUP_TIMEOUT_MS: u64 = 17_000;
pub const OPTIONAL_LOOKUP_TIMEOUT_MS: u64 = 7_000;

/// Default `scrollUntilVisible` timeout (`Commands.kt:182`).
pub const DEFAULT_SCROLL_TIMEOUT_MS: u64 = 20_000;

/// Maximum retries allowed by Maestro's `retry` command (`Orchestra.kt:1907`).
pub const MAX_RETRIES_ALLOWED: i64 = 3;

/// Syntax/range error on a numeric command field (`MaestroException.InvalidNumericFieldValue`).
///
/// Like Maestro (`Orchestra.kt:381`), this is NEVER downgraded to a warning by `optional: true`.
#[derive(Debug, Clone)]
pub struct InvalidNumericError(pub String);

impl fmt::Display for InvalidNumericError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidNumericError {}

/// Host-side Maestro assertion failure (`MaestroException.AssertionFailure`),
/// eligible for downgrade to warning when `optional: true` (`Orchestra.kt:376-383`).
#[derive(Debug, Clone)]
pub struct AssertionError(pub String);

impl fmt::Display for AssertionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AssertionError {}

/// Specific Maestro test-level error kinds (`MaestroException` subtypes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaestroErrorKind {
    ElementNotFound,
    AssertionFailure,
    UnableToLaunchApp,
    UnableToCopyText,
    HideKeyboardFailure,
    InvalidCommand,
    DestinationIsNotWritable,
    CloudApiKeyNotAvailable,
    NoFocus,
}

/// Test-level Maestro command failure (`MaestroException`).
///
/// Downgraded to a warning when `optional: true` and retried by `retry`.
#[derive(Debug, Clone)]
pub struct MaestroError {
    pub kind: MaestroErrorKind,
    pub message: String,
}

impl MaestroError {
    pub fn new(kind: MaestroErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub fn element_not_found(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::ElementNotFound, message)
    }

    pub fn assertion(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::AssertionFailure, message)
    }

    pub fn unable_to_launch(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::UnableToLaunchApp, message)
    }

    pub fn unable_to_copy(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::UnableToCopyText, message)
    }

    pub fn hide_keyboard(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::HideKeyboardFailure, message)
    }

    pub fn invalid_command(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::InvalidCommand, message)
    }

    pub fn not_writable(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::DestinationIsNotWritable, message)
    }

    pub fn cloud_api_key(message: impl Into<String>) -> Self {
        Self::new(MaestroErrorKind::CloudApiKeyNotAvailable, message)
    }
}

impl fmt::Display for MaestroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MaestroError {}

/// Infrastructure / transport failure (`DeviceConnectionException` / `DeviceUnreachableException`).
///
/// Matches `Orchestra.kt:397-398`: NEVER downgraded by `optional`, NEVER retried by `retry`,
/// and NEVER routed through `on_command_failed` — escapes immediately to the caller.
#[derive(Debug, Clone)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

/// JavaScript evaluation failure (`JsEvaluationException`).
///
/// Not a `MaestroException`: NEVER downgraded by `optional`, NEVER retried by `retry`,
/// but IS routed through `on_command_failed` so the failing step is marked.
#[derive(Debug, Clone)]
pub struct JsError {
    pub message: String,
    pub stack: Vec<String>,
}

impl JsError {
    pub fn new(message: impl Into<String>, stack: Vec<String>) -> Self {
        Self { message: message.into(), stack }
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsError {}

/// Whether `e` is a transport/device-death error that must bypass `on_command_failed`,
/// `optional`, and `retry`.
pub fn is_transport_error(e: &anyhow::Error) -> bool {
    e.is::<TransportError>() || crate::driver::is_rpc_silence(e)
}

/// Whether `e` is an `InvalidNumericError` (`MaestroException.InvalidNumericFieldValue`).
pub fn is_invalid_numeric_error(e: &anyhow::Error) -> bool {
    e.is::<InvalidNumericError>()
}

/// Whether `e` represents a Maestro test-level failure (`MaestroException`),
/// eligible for `retry` (`Orchestra.kt:1005`).
pub fn is_maestro_error(e: &anyhow::Error) -> bool {
    if e.is::<MaestroError>() || e.is::<AssertionError>() || e.is::<InvalidNumericError>() {
        return true;
    }
    if let Some(d) = e.downcast_ref::<DriverError>() {
        return matches!(
            d.code.as_str(),
            "ELEMENT_NOT_FOUND" | "ASSERTION" | "NO_FOCUS" | "UNABLE_TO_COPY_TEXT" | "INVALID_COMMAND"
        );
    }
    false
}

/// Whether an error is eligible to be downgraded to a warning by `optional: true`
/// (`Orchestra.kt:376-383, 397-399`).
///
/// Transport losses, `INTERNAL` / `UNKNOWN_METHOD` driver errors, JS errors,
/// and `InvalidNumericError` are NEVER downgraded.
pub fn is_optional_downgradable(e: &anyhow::Error) -> bool {
    if is_invalid_numeric_error(e) || is_transport_error(e) || e.is::<JsError>() {
        return false;
    }
    is_maestro_error(e)
}

/// Device screen geometry and platform identity (`maestro.DeviceInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub platform: Platform,
    pub width_pixels: i32,
    pub height_pixels: i32,
    pub width_grid: i32,
    pub height_grid: i32,
}

impl DeviceInfo {
    pub fn android_default() -> Self {
        Self {
            platform: Platform::Android,
            width_pixels: 1080,
            height_pixels: 1920,
            width_grid: 1080,
            height_grid: 1920,
        }
    }

    pub fn platform_js_name(&self) -> &'static str {
        match self.platform {
            Platform::Android => "android",
            Platform::Ios => "ios",
            Platform::Web => "web",
        }
    }
}

/// Evaluated parameters for `launchApp`.
#[derive(Clone, Debug)]
pub struct LaunchRequest {
    pub app_id: String,
    pub clear_state: bool,
    pub clear_keychain: bool,
    pub stop_app: bool,
    pub permissions: BTreeMap<String, String>,
    pub launch_arguments: ValueMap,
}

/// Evaluated parameters for `tapOn` with an element selector.
#[derive(Clone, Debug)]
pub struct TapElementRequest {
    pub selector: ElementSelector,
    pub timeout_ms: u64,
    pub optional: bool,
    pub long_press: bool,
    pub repeat: Option<TapRepeat>,
    pub relative_point: Option<String>,
    pub retry_if_no_change: bool,
    pub wait_until_visible: bool,
    pub wait_to_settle_timeout_ms: Option<i32>,
    pub app_id: Option<String>,
}

/// Evaluated parameters for `tapOn` with a point (`x,y` or `x%,y%`).
#[derive(Clone, Debug)]
pub struct TapPointRequest {
    pub point: String,
    pub long_press: bool,
    pub repeat: Option<TapRepeat>,
    pub retry_if_no_change: bool,
    pub wait_to_settle_timeout_ms: Option<i32>,
}

/// Evaluated parameters for `swipe`.
#[derive(Clone, Debug)]
pub struct SwipeRequest {
    pub element_selector: Option<ElementSelector>,
    pub direction: Option<Direction>,
    pub start_relative: Option<String>,
    pub end_relative: Option<String>,
    pub start_point: Option<Point>,
    pub end_point: Option<Point>,
    pub relative_point: Option<String>,
    pub duration_ms: i64,
    pub timeout_ms: u64,
    pub optional: bool,
    pub wait_to_settle_timeout_ms: Option<i32>,
}

/// Evaluated parameters for `scrollUntilVisible`.
#[derive(Clone, Debug)]
pub struct ScrollUntilVisibleRequest {
    pub selector: ElementSelector,
    pub direction: Direction,
    pub duration_ms: u64,
    pub timeout_ms: u64,
    pub visibility_percentage: i32,
    pub center_element: bool,
    pub optional: bool,
    pub wait_to_settle_timeout_ms: Option<i32>,
}

/// Backend contract implemented by both real device sessions and [`crate::fake_driver::FakeDriver`].
pub trait FlowDriver {
    fn device_info(&self) -> DeviceInfo;

    fn launch_app(&mut self, req: &LaunchRequest) -> Result<Value>;
    fn stop_app(&mut self, app_id: &str) -> Result<Value>;
    fn kill_app(&mut self, app_id: &str) -> Result<Value>;
    fn clear_state(&mut self, app_id: &str) -> Result<Value>;
    fn clear_keychain(&mut self) -> Result<Value>;
    fn set_permissions(&mut self, app_id: &str, permissions: &BTreeMap<String, String>) -> Result<Value>;

    fn tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value>;
    fn tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value>;
    fn assert_visible(&mut self, selector: &ElementSelector, timeout_ms: u64, optional: bool) -> Result<Value>;
    fn assert_not_visible(&mut self, selector: &ElementSelector, timeout_ms: u64, optional: bool) -> Result<Value>;
    fn find_element_bounds(&mut self, selector: &ElementSelector, timeout_ms: u64, optional: bool) -> Result<[i64; 4]>;

    fn input_text(&mut self, text: &str) -> Result<Value>;
    fn erase_text(&mut self, characters: i32) -> Result<Value>;
    fn copy_text_from(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        optional: bool,
    ) -> Result<(String, Value)>;

    fn back_press(&mut self) -> Result<Value>;
    fn hide_keyboard(&mut self) -> Result<Value>;
    fn press_key(&mut self, code: KeyCode) -> Result<Value>;

    fn scroll(&mut self) -> Result<Value>;
    fn swipe(&mut self, req: &SwipeRequest) -> Result<Value>;
    fn scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value>;
    fn wait_for_animation_to_end(&mut self, timeout_ms: Option<u64>) -> Result<Value>;

    fn screenshot_bytes(&mut self) -> Result<Vec<u8>>;
    fn screenshot_image(&mut self) -> Result<DynamicImage> {
        let png = self.screenshot_bytes()?;
        image::load_from_memory(&png).context("decoding screenshot PNG")
    }

    fn open_link(&mut self, link: &str, app_id: Option<&str>, auto_verify: bool, browser: bool) -> Result<Value>;
    fn set_location(&mut self, latitude: f64, longitude: f64) -> Result<Value>;
    fn set_orientation(&mut self, orientation: &str) -> Result<Value>;
    fn add_media(&mut self, media_paths: &[String]) -> Result<Value>;
    fn start_recording(&mut self, out_path: &Path) -> Result<Value>;
    fn stop_recording(&mut self) -> Result<Value>;

    fn is_airplane_mode_enabled(&mut self) -> Result<bool>;
    fn set_airplane_mode(&mut self, enabled: bool) -> Result<Value>;
    fn is_dark_mode_enabled(&mut self) -> Result<bool>;
    fn set_dark_mode(&mut self, enabled: bool) -> Result<Value>;

    /// Optional hook for `--check-settle` diagnostics on real Flutter drivers.
    fn check_busy_after(&mut self) -> Option<Value> {
        None
    }

    /// Optional hint for the backend about the current flow's `appId`.
    fn set_flow_app_id(&mut self, _app_id: &str) {}

    /// F7: app health issues delivered while the last command ran, split by
    /// the command that caused them. Called after every command.
    fn take_app_issues(&mut self) -> crate::app_health::IssueBatch {
        crate::app_health::IssueBatch::default()
    }

    /// F7: issues still buffered in the app when the flow ends.
    fn drain_app_issues(&mut self) -> crate::app_health::IssueBatch {
        self.take_app_issues()
    }

    /// F7: after the connection to the app was lost, an `appCrash` issue when
    /// the app process died (with a crash log excerpt when available).
    fn diagnose_crash(&mut self) -> Option<Value> {
        None
    }

    /// F8: captures the current UI hierarchy for rich failure diagnostics
    /// (`(source_lane, root_node)`). Only called when a non-optional step fails.
    fn capture_failure_hierarchy(&mut self) -> Option<(String, crate::fake_driver::TreeNode)> {
        None
    }

    /// F8b `--trace`: the lane that ran the last command and the HTTP log
    /// entries delivered since the previous call. Only called with `--trace`.
    fn trace_step(&mut self) -> crate::trace::TraceBatch {
        crate::trace::TraceBatch::default()
    }

    /// F8b `--trace`: entries still buffered at flow end, plus requests in flight.
    fn trace_drain(&mut self) -> crate::trace::TraceBatch {
        self.trace_step()
    }

    /// F8b `--trace`: `enabled`, `unsupported` (no driver log), or `off`.
    fn trace_http_status(&self) -> &'static str {
        "off"
    }

    /// F8: captures a PNG screenshot when a non-optional step fails.
    /// Defaults to [`Self::screenshot_bytes`] on real devices; overridden by
    /// `FakeDriver` so failure artifacts do not pollute recorded user events.
    fn capture_failure_screenshot(&mut self) -> Option<Vec<u8>> {
        self.screenshot_bytes().ok()
    }
}

/// Parses an element `index` field (`NumericFields.parseIndex`).
pub fn parse_index(raw: &str) -> Result<i64> {
    let parsed = raw.trim().parse::<f64>().ok().filter(|f| f.is_finite()).ok_or_else(|| {
        anyhow::Error::new(InvalidNumericError(format!("Invalid index value \"{raw}\": index must be a whole number.")))
    })?;
    Ok(parsed as i64)
}

/// Parses a `x,y` or `x%,y%` point string (`NumericFields.parsePoint`).
/// Returns `(x, y, is_percent)`.
pub fn parse_point(raw: &str) -> Result<(i32, i32, bool)> {
    let is_percent = raw.contains('%');
    let stripped = raw.replace('%', "");
    let parts: Vec<&str> = stripped.split(',').collect();
    if parts.len() < 2 {
        return Err(InvalidNumericError(format!(
            "Invalid point value \"{raw}\": expected two whole-number coordinates like \"x,y\" or \"x%,y%\"."
        ))
        .into());
    }
    let parse_coord = |s: &str| {
        s.trim().parse::<i32>().map_err(|_| {
            anyhow::Error::new(InvalidNumericError(format!(
                "Invalid point value \"{raw}\": expected two whole-number coordinates like \"x,y\" or \"x%,y%\"."
            )))
        })
    };
    let x = parse_coord(parts[0])?;
    let y = parse_coord(parts[1])?;
    Ok((x, y, is_percent))
}

/// Parses and validates a screen point (`"x,y"` or `"x%,y%"`) against a
/// `(width, height)` viewport, returning resolved `(x, y)` coordinates.
pub fn resolve_screen_point(spec: &str, width: i32, height: i32) -> Result<(i32, i32)> {
    let (x, y, is_percent) = parse_point(spec)?;
    if is_percent {
        if !(0..=100).contains(&x) || !(0..=100).contains(&y) {
            return Err(MaestroError::invalid_command(format!("Invalid point: {spec}")).into());
        }
        Ok((width * x / 100, height * y / 100))
    } else {
        if x < 0 || y < 0 {
            return Err(MaestroError::invalid_command(format!(
                "Invalid point: {spec}. Coordinates must not be negative."
            ))
            .into());
        }
        Ok((x, y))
    }
}

/// Converts Maestro's `scrollUntilVisible` `speed` (`0..=100`) to drag duration in ms
/// (`Commands.kt:158-162`, `NumericFields.kt:47-57`).
pub fn speed_to_duration(raw: &str) -> Result<u64> {
    let parsed = raw.trim().parse::<i64>().map_err(|_| {
        anyhow::Error::new(InvalidNumericError(format!("scroll speed `{raw}` must be an integer 0..100")))
    })?;
    if !(0..=100).contains(&parsed) {
        return Err(anyhow::Error::new(InvalidNumericError(format!(
            "scroll speed `{raw}` is out of range (expected 0..100)"
        ))));
    }
    Ok(((1000.0 * (100 - parsed) as f64) / 100.0) as u64 + 1)
}

/// Parses `scrollUntilVisible` `timeout` (`Commands.kt:164-169`): negative falls back to 20 000 ms.
pub fn scroll_timeout_ms(raw: &str) -> Result<u64> {
    let clean = raw.trim().replace('_', "");
    let parsed = clean
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| anyhow::Error::new(InvalidNumericError(format!("timeout `{raw}` is not a number"))))?;
    if parsed < 0.0 { Ok(DEFAULT_SCROLL_TIMEOUT_MS) } else { Ok(parsed as u64) }
}

/// Parses a millisecond timeout string (supporting `_` separators as in Maestro's `parseTimeoutMs`).
pub fn parse_timeout_ms(raw: &str, what: &str) -> Result<u64> {
    let clean = raw.trim().replace('_', "");
    let parsed = clean
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| anyhow::Error::new(InvalidNumericError(format!("{what} `{raw}` is not a number"))))?;
    Ok(parsed.max(0.0) as u64)
}

/// Parses `waitForAnimationToEnd` `timeout`: negative returns `None` (`Commands.kt:1061-1065`).
pub fn parse_wait_animation_timeout(raw: &str) -> Result<Option<u64>> {
    let clean = raw.trim().replace('_', "");
    let parsed = clean
        .parse::<i64>()
        .map_err(|_| anyhow::Error::new(InvalidNumericError(format!("timeout `{raw}` is not a number"))))?;
    if parsed < 0 { Ok(None) } else { Ok(Some(parsed as u64)) }
}
