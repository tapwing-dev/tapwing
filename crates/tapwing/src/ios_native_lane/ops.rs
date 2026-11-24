//! UI commands over the iOS native lane: Tapwing's XCTest driver behind HTTP,
//! with Maestro's waiting model (poll the hierarchy until the selector matches
//! or the timeout elapses) and the runner's `isScreenStatic` for settling.
//!
//! All coordinates are logical points (`IOSDriver` works in `widthGrid` /
//! `heightGrid`), matching the hierarchy bounds.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use super::client::IosNativeClient;
use super::hierarchy::IosHierarchy;
use crate::backend::{
    AssertionError, MaestroError, ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest,
    resolve_screen_point,
};
use crate::fake_driver::{Bounds, TreeNode, calculate_element_relative_point, describe_sel, is_element_not_found};
use crate::flow::ir::{Direction, ElementSelector};

/// `IOSDriver.longPress`: a 3 s press.
const LONG_PRESS_SECS: f64 = 3.0;
/// Pause between hierarchy polls while waiting for an element.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Pause between `isScreenStatic` probes that answered "still moving".
const STATIC_POLL: Duration = Duration::from_millis(50);
/// Settle budget when the command gives none (Maestro waits about 2 s).
const DEFAULT_SETTLE: Duration = Duration::from_millis(2000);
/// `Maestro.SCREEN_SETTLE_TIMEOUT_MS`: default of `waitForAnimationToEnd`.
const DEFAULT_ANIMATION_WAIT_MS: u64 = 3000;
/// Settle budget after `Home` / `Lock` / rotation (transitions are short).
const BUTTON_SETTLE: Duration = Duration::from_millis(1000);
/// Hierarchy-comparison fallback: comparisons 200 ms apart, like `waitForAppToSettle`.
const FALLBACK_ATTEMPTS: u32 = 10;
const FALLBACK_INTERVAL: Duration = Duration::from_millis(200);

/// `DeviceOrientation.camelCaseName` for the orientations of the flow language.
fn orientation_name(raw: &str) -> Option<&'static str> {
    match raw.trim().replace(['_', '-', ' '], "").to_ascii_lowercase().as_str() {
        "portrait" => Some("portrait"),
        "landscapeleft" => Some("landscapeLeft"),
        "landscaperight" => Some("landscapeRight"),
        "upsidedown" => Some("upsideDown"),
        _ => None,
    }
}

/// `IOSDriver.pressKey`: the runner key for a Maestro key, or hardware button.
enum KeyAction {
    Key(&'static str),
    Button(&'static str),
}

fn key_action(key: &str) -> Option<KeyAction> {
    match key.trim().to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(KeyAction::Key("return")),
        "backspace" | "delete" => Some(KeyAction::Key("delete")),
        "home" => Some(KeyAction::Button("home")),
        "lock" => Some(KeyAction::Button("lock")),
        _ => None,
    }
}

/// iOS native-lane command executor, borrowed per call from the warmed-up session.
pub struct IosNativeOps<'a> {
    client: &'a mut IosNativeClient,
    /// Bundle ids the runner may pick the foreground app from (the flow's app
    /// and `com.apple.springboard`).
    app_ids: &'a [String],
}

impl<'a> IosNativeOps<'a> {
    pub fn new(client: &'a mut IosNativeClient, app_ids: &'a [String]) -> Self {
        Self { client, app_ids }
    }

    /// Screen size in points.
    fn screen(&mut self) -> Result<(i32, i32)> {
        let d = self.client.device_info()?;
        Ok((d.width_points as i32, d.height_points as i32))
    }

    pub fn hierarchy(&mut self) -> Result<IosHierarchy> {
        let (w, h) = self.screen()?;
        let json = self.client.view_hierarchy(self.app_ids, false)?;
        IosHierarchy::parse(&json, w, h)
    }

    /// Whether a SpringBoard alert (permission dialog) is on screen: the
    /// routing signal for sending commands to this lane.
    pub fn has_springboard_alert(&mut self) -> Result<bool> {
        Ok(self.hierarchy()?.has_springboard_alert())
    }

    /// Bundle id of the foreground app, when the runner can tell.
    pub fn foreground_app_id(&mut self) -> Result<String> {
        self.client.running_app_id(self.app_ids)
    }

    /// Single lookup; nodes without bounds cannot be tapped and count as missing.
    fn find_once(&mut self, sel: &ElementSelector) -> Result<(Option<TreeNode>, IosHierarchy)> {
        let h = self.hierarchy()?;
        let found = h.find(sel)?.filter(|n| n.bounds.is_some());
        Ok((found, h))
    }

    fn not_found(sel: &ElementSelector, h: &IosHierarchy) -> anyhow::Error {
        let seen = h.visible_texts(12).iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
        MaestroError::element_not_found(format!("Element not found: {}\n      on screen: {seen}", describe_sel(sel)))
            .into()
    }

    /// `Maestro.findElementWithTimeout`: poll until found, at least one attempt.
    fn poll_find(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<(TreeNode, IosHierarchy)> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let (found, h) = self.find_once(sel)?;
            if let Some(node) = found {
                return Ok((node, h));
            }
            if Instant::now() >= deadline {
                return Err(Self::not_found(sel, &h));
            }
            crate::interrupt::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())))?;
        }
    }

    /// Polls `isScreenStatic` until it reports a static screen (`true`) or
    /// `deadline` passes (`false`). Every probe is an HTTP round-trip that
    /// itself takes a screenshot comparison, so there is no busy loop.
    fn wait_until_static(&mut self, deadline: Instant) -> Result<bool> {
        loop {
            if self.client.is_screen_static()? {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            crate::interrupt::sleep(STATIC_POLL.min(remaining))?;
        }
    }

    /// Best-effort short wait after a system transition; never fails the command.
    fn settle_quietly(&mut self, budget: Duration) {
        let _ = self.wait_until_static(Instant::now() + budget);
    }

    /// Hierarchy-comparison settling, used when the runner cannot answer
    /// `isScreenStatic`: returns `(latest_hierarchy, settled)` where `settled`
    /// is `true` once two consecutive hierarchies are identical.
    fn settle_by_hierarchy(
        &mut self,
        initial: Option<IosHierarchy>,
        deadline: Option<Instant>,
    ) -> Result<(IosHierarchy, bool)> {
        let mut latest = match initial {
            Some(h) => h,
            None => self.hierarchy()?,
        };
        for attempt in 0..FALLBACK_ATTEMPTS {
            let after = self.hierarchy()?;
            if after.signature() == latest.signature() {
                return Ok((after, true));
            }
            latest = after;
            if attempt + 1 >= FALLBACK_ATTEMPTS {
                break;
            }
            match deadline {
                Some(d) => {
                    let remaining = d.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    crate::interrupt::sleep(FALLBACK_INTERVAL.min(remaining))?;
                }
                None => crate::interrupt::sleep(FALLBACK_INTERVAL)?,
            }
        }
        Ok((latest, false))
    }

    /// `ScreenshotUtils.waitForAppToSettle`: waits for the screen to stop
    /// moving, then returns a fresh hierarchy. `Some(ms)` with `ms <= 0`
    /// returns immediately without probing the runner.
    pub fn settle(&mut self, initial: Option<IosHierarchy>, timeout_ms: Option<i32>) -> Result<IosHierarchy> {
        if matches!(timeout_ms, Some(ms) if ms <= 0) {
            return match initial {
                Some(h) => Ok(h),
                None => self.hierarchy(),
            };
        }
        let budget = timeout_ms.map_or(DEFAULT_SETTLE, |ms| Duration::from_millis(ms as u64));
        let deadline = Instant::now() + budget;
        match self.wait_until_static(deadline) {
            Ok(_) => self.hierarchy(),
            Err(e) if crate::interrupt::is_interrupted_error(&e) => Err(e),
            // An old or confused runner: fall back to comparing hierarchies.
            Err(_) => self.settle_by_hierarchy(initial, Some(deadline)).map(|(h, _)| h),
        }
    }

    /// `waitForAnimationToEnd`: waits (at most `timeout_ms`) for a static screen.
    pub fn wait_for_animation_to_end(&mut self, timeout_ms: Option<u64>) -> Result<Value> {
        let budget = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_ANIMATION_WAIT_MS));
        let deadline = Instant::now() + budget;
        let settled = match self.wait_until_static(deadline) {
            Ok(s) => s,
            Err(e) if crate::interrupt::is_interrupted_error(&e) => return Err(e),
            Err(_) => {
                let before = self.hierarchy()?;
                let (_, stable) = self.settle_by_hierarchy(Some(before), Some(deadline))?;
                stable
            }
        };
        Ok(json!({ "lane": "native", "settled": settled }))
    }

    fn tap_xy(&mut self, x: i32, y: i32, long_press: bool) -> Result<()> {
        self.client.touch(f64::from(x), f64::from(y), long_press.then_some(LONG_PRESS_SECS))
    }

    /// `hierarchyBasedTap`: tap (optionally repeated), settle, and when
    /// `retry_if_no_change` and nothing changed, tap once more.
    fn perform_tap(
        &mut self,
        (x, y): (i32, i32),
        long_press: bool,
        repeat: Option<&crate::flow::ir::TapRepeat>,
        retry_if_no_change: bool,
        before: Option<IosHierarchy>,
        settle_ms: Option<i32>,
    ) -> Result<()> {
        let before = if retry_if_no_change {
            Some(match before {
                Some(h) => h,
                None => self.hierarchy()?,
            })
        } else {
            before
        };
        let attempts = if retry_if_no_change { 2 } else { 1 };
        for _ in 0..attempts {
            match repeat {
                Some(r) if !long_press => {
                    let count = r.repeat.max(1);
                    for i in 0..count {
                        let t = Instant::now();
                        self.tap_xy(x, y, false)?;
                        if i + 1 < count {
                            let spent = t.elapsed().as_millis().min(i64::MAX as u128) as i64;
                            crate::interrupt::sleep(
                                Duration::from_millis(r.delay.saturating_sub(spent).max(0) as u64),
                            )?;
                        }
                    }
                }
                _ => self.tap_xy(x, y, long_press)?,
            }
            let after = self.settle(None, settle_ms)?;
            if !retry_if_no_change || before.as_ref().is_none_or(|b| after.signature() != b.signature()) {
                break;
            }
        }
        Ok(())
    }

    pub fn tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value> {
        let (node, hier) = self.poll_find(&req.selector, req.timeout_ms)?;
        // Aim at the settled hierarchy: animating alerts move their buttons.
        let settled = self.settle(Some(hier), req.wait_to_settle_timeout_ms)?;
        let node = settled.find(&req.selector)?.filter(|n| n.bounds.is_some()).unwrap_or(node);
        let bounds = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
        let point = match &req.relative_point {
            Some(rel) => calculate_element_relative_point(bounds, rel)?,
            None => bounds.center(),
        };
        self.perform_tap(
            point,
            req.long_press,
            req.repeat.as_ref(),
            req.retry_if_no_change,
            Some(settled),
            req.wait_to_settle_timeout_ms,
        )?;
        Ok(json!({ "x": point.0, "y": point.1, "lane": "native" }))
    }

    pub fn tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value> {
        let (w, h) = self.screen()?;
        let point = resolve_screen_point(&req.point, w, h)?;
        self.perform_tap(
            point,
            req.long_press,
            req.repeat.as_ref(),
            req.retry_if_no_change,
            None,
            req.wait_to_settle_timeout_ms,
        )?;
        Ok(json!({ "x": point.0, "y": point.1, "lane": "native" }))
    }

    pub fn assert_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        let (node, _) = self.poll_find(sel, timeout_ms)?;
        Ok(json!({ "text": node.text, "id": node.id, "lane": "native" }))
    }

    /// Passes once the selector no longer matches; fails when it still does at the timeout.
    pub fn assert_not_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let (found, _) = self.find_once(sel)?;
            if found.is_none() {
                return Ok(json!({ "lane": "native" }));
            }
            if Instant::now() >= deadline {
                return Err(MaestroError::assertion(format!(
                    "Assertion is false: {} is not visible",
                    describe_sel(sel)
                ))
                .into());
            }
            crate::interrupt::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())))?;
        }
    }

    /// Element bounds scaled from logical points to physical screenshot pixels
    /// (`Maestro.takeScreenshot`, `Maestro.kt:651-663`: `scale = heightPixels / heightGrid`).
    pub fn find_element_bounds(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<[i64; 4]> {
        let (node, _) = self.poll_find(sel, timeout_ms)?;
        let b = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
        if b.width() <= 0 || b.height() <= 0 {
            return Err(AssertionError(format!(
                "Cannot crop screenshot: element has invalid dimensions (width: {}, height: {})",
                b.width(),
                b.height()
            ))
            .into());
        }
        let info = self.client.device_info()?;
        let scale =
            if info.height_points > 0 { f64::from(info.height_pixels) / f64::from(info.height_points) } else { 1.0 };
        Ok(crate::run::scale_crop_bounds(
            [i64::from(b.left), i64::from(b.top), i64::from(b.right), i64::from(b.bottom)],
            scale,
        ))
    }

    pub fn copy_text_from(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<(String, Value)> {
        let (node, _) = self.poll_find(sel, timeout_ms)?;
        let text = node
            .resolved_copy_text()
            .ok_or_else(|| MaestroError::unable_to_copy("Element does not contain text to copy"))?;
        Ok((text.clone(), json!({ "text": text, "lane": "native" })))
    }

    /// Types into the focused field (the runner types any Unicode text).
    pub fn input_text(&mut self, text: &str) -> Result<Value> {
        self.client.input_text(text, self.app_ids)?;
        Ok(json!({ "lane": "native" }))
    }

    pub fn erase_text(&mut self, characters: i32) -> Result<Value> {
        self.client.erase_text(characters.max(0) as u32, self.app_ids)?;
        Ok(json!({ "lane": "native" }))
    }

    /// `IOSDriver.hideKeyboard` with Maestro's strict outcome: fails when the
    /// keyboard stays up.
    pub fn hide_keyboard(&mut self) -> Result<Value> {
        self.hide_keyboard_with(true)
    }

    /// `IOSDriver.hideKeyboard`: returns immediately when no soft keyboard is
    /// visible, otherwise performs Maestro's dismiss swipes without submitting
    /// a form via `Return`. When the keyboard survives (a web-auth sheet or
    /// `WKWebView` ignores the swipes, and stock Maestro's own outcome there
    /// depends on timing), `strict` fails like Maestro's `Couldn't hide the
    /// keyboard`; otherwise the step passes with a compatibility warning, the
    /// same policy as the Flutter lane.
    pub fn hide_keyboard_with(&mut self, strict: bool) -> Result<Value> {
        if !self.client.keyboard_visible(self.app_ids)? {
            return Ok(json!({ "lane": "native", "alreadyHidden": true }));
        }
        let (w, h) = self.screen()?;
        let (wf, hf) = (f64::from(w), f64::from(h));
        let end_y = ((hf * 0.5) as i32) - 42;
        self.raw_swipe(((wf * 0.5) as i32, (hf * 0.5) as i32), ((wf * 0.5) as i32, end_y.max(0)), 50)?;
        if self.client.keyboard_visible(self.app_ids).unwrap_or(false) {
            let end_y_2 = ((hf * 0.5) as i32) + 42;
            self.raw_swipe(((wf * 0.5) as i32, (hf * 0.5) as i32), ((wf * 0.5) as i32, end_y_2.min(h)), 50)?;
        }
        let still_visible = self.client.keyboard_visible(self.app_ids).unwrap_or(false);
        if still_visible {
            if strict {
                return Err(MaestroError::assertion("Couldn't hide the keyboard").into());
            }
            return Ok(json!({
                "lane": "native",
                "warning": "the soft keyboard is still visible after Maestro's dismiss swipes; stock Maestro iOS \
                            fails or passes depending on timing here (--maestro-compat strict fails the step). \
                            Do not rely on hideKeyboard in this screen: tap a non-interactive element or reorder \
                            the fields instead"
            }));
        }
        Ok(json!({ "lane": "native" }))
    }

    /// `IOSDriver.pressKey`: `Enter` and `Backspace` are keyboard keys, `Home`
    /// and `Lock` hardware buttons; every other key is a silent no-op, as in Maestro.
    pub fn press_key(&mut self, key: &str) -> Result<Value> {
        match key_action(key) {
            Some(KeyAction::Key(k)) => self.client.press_key(k)?,
            Some(KeyAction::Button(b)) => return self.press_button(b),
            None => return Ok(json!({ "lane": "native", "ignored": key })),
        }
        Ok(json!({ "lane": "native" }))
    }

    /// `POST /pressButton`: `home` or `lock`.
    pub fn press_button(&mut self, button: &str) -> Result<Value> {
        let name = match key_action(button) {
            Some(KeyAction::Button(b)) => b,
            _ => {
                return Err(MaestroError::invalid_command(format!(
                    "Unknown iOS button: {button}. Valid buttons are: [Home, Lock]"
                ))
                .into());
            }
        };
        self.client.press_button(name)?;
        self.settle_quietly(BUTTON_SETTLE);
        Ok(json!({ "lane": "native" }))
    }

    /// `setOrientation` through `XCUIDevice.orientation`; width and height swap,
    /// so the cached screen size is dropped.
    pub fn set_orientation(&mut self, orientation: &str) -> Result<Value> {
        let name = orientation_name(orientation).ok_or_else(|| {
            MaestroError::invalid_command(format!(
                "Unknown orientation: {orientation}. Valid orientations are: [PORTRAIT, LANDSCAPE_LEFT, LANDSCAPE_RIGHT, UPSIDE_DOWN]"
            ))
        })?;
        self.client.set_orientation(name)?;
        self.settle_quietly(BUTTON_SETTLE);
        Ok(json!({ "lane": "native", "orientation": name }))
    }

    fn raw_swipe(&mut self, start: (i32, i32), end: (i32, i32), duration_ms: i64) -> Result<()> {
        let secs = duration_ms.max(1) as f64 / 1000.0;
        self.client.swipe_v2(
            None,
            (f64::from(start.0), f64::from(start.1)),
            (f64::from(end.0), f64::from(end.1)),
            secs,
            self.app_ids,
        )
    }

    /// Screen-wide swipe with `IOSDriver.swipe(direction)` start/end ratios.
    fn swipe_direction(&mut self, dir: Direction, duration_ms: i64) -> Result<()> {
        let (w, h) = self.screen()?;
        let p = |fx: f64, fy: f64| ((f64::from(w) * fx) as i32, (f64::from(h) * fy) as i32);
        let (start, end) = match dir {
            Direction::Up => (p(0.5, 0.9), p(0.5, 0.1)),
            Direction::Down => (p(0.5, 0.2), p(0.5, 0.9)),
            Direction::Right => (p(0.1, 0.5), p(0.9, 0.5)),
            Direction::Left => (p(0.9, 0.5), p(0.1, 0.5)),
        };
        self.raw_swipe(start, end, duration_ms)
    }

    /// `IOSDriver.swipe(point, direction)`: to 10% / 90% of the screen along the axis.
    fn swipe_from_point(&mut self, from: (i32, i32), dir: Direction, duration_ms: i64) -> Result<()> {
        let (w, h) = self.screen()?;
        let (w, h) = (f64::from(w), f64::from(h));
        let end = match dir {
            Direction::Up => (from.0, (h * 0.1) as i32),
            Direction::Down => (from.0, (h * 0.9) as i32),
            Direction::Right => ((w * 0.9) as i32, from.1),
            Direction::Left => ((w * 0.1) as i32, from.1),
        };
        self.raw_swipe(from, end, duration_ms)
    }

    /// `IOSDriver.scrollVertical`: 333 ms from the screen centre to 10% height.
    pub fn scroll(&mut self) -> Result<Value> {
        let (w, h) = self.screen()?;
        let (w, h) = (f64::from(w), f64::from(h));
        self.raw_swipe(((w * 0.5) as i32, (h * 0.5) as i32), ((w * 0.5) as i32, (h * 0.1) as i32), 333)?;
        self.settle(None, None)?;
        Ok(json!({ "lane": "native" }))
    }

    pub fn swipe(&mut self, req: &SwipeRequest) -> Result<Value> {
        let (w, h) = self.screen()?;
        if let (Some(sel), Some(dir)) = (&req.element_selector, req.direction) {
            let (node, _) = self.poll_find(sel, req.timeout_ms)?;
            let bounds = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
            let from = match &req.relative_point {
                Some(rel) => calculate_element_relative_point(bounds, rel)?,
                None => bounds.center(),
            };
            self.swipe_from_point(from, dir, req.duration_ms)?;
        } else if let (Some(sr), Some(er)) = (&req.start_relative, &req.end_relative) {
            let start = crate::backend::resolve_screen_point(sr, w, h)?;
            let end = crate::backend::resolve_screen_point(er, w, h)?;
            self.raw_swipe(start, end, req.duration_ms)?;
        } else if let Some(dir) = req.direction {
            self.swipe_direction(dir, req.duration_ms)?;
        } else if let (Some(s), Some(e)) = (&req.start_point, &req.end_point) {
            self.raw_swipe((s.x, s.y), (e.x, e.y), req.duration_ms)?;
        } else {
            return Err(MaestroError::invalid_command("Illegal arguments for swiping").into());
        }
        self.settle(None, req.wait_to_settle_timeout_ms)?;
        Ok(json!({ "lane": "native" }))
    }

    /// `Orchestra.scrollUntilVisible`: look (500 ms), then swipe from the centre.
    pub fn scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value> {
        let swipe_dir = match req.direction {
            Direction::Down => Direction::Up,
            Direction::Up => Direction::Down,
            Direction::Right => Direction::Left,
            Direction::Left => Direction::Right,
        };
        let (w, h) = self.screen()?;
        let threshold = f64::from(req.visibility_percentage) / 100.0;
        let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
        let (mut retry_center, max_retry_center) = (0, 4);
        loop {
            let poll_ms = 500_u64.min(deadline.saturating_duration_since(Instant::now()).as_millis() as u64);
            match self.poll_find(&req.selector, poll_ms) {
                Ok((node, _)) => {
                    let b = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
                    let visibility = b.visible_percentage(w, h);
                    if req.center_element && visibility > 0.1 && retry_center <= max_retry_center {
                        if b.is_near_screen_center(swipe_dir, w, h) {
                            return Ok(json!({ "lane": "native" }));
                        }
                        retry_center += 1;
                    } else if visibility >= threshold {
                        return Ok(json!({ "lane": "native" }));
                    }
                }
                Err(e) if is_element_not_found(&e) => {}
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                break;
            }
            self.swipe_from_point((w / 2, h / 2), swipe_dir, req.duration_ms as i64)?;
            self.settle(None, req.wait_to_settle_timeout_ms)?;
        }
        Err(MaestroError::element_not_found(format!(
            "Could not find a visible element matching selector: {}",
            describe_sel(&req.selector)
        ))
        .into())
    }

    /// PNG of the current screen through the driver (uncompressed capture).
    pub fn screenshot(&mut self) -> Result<Vec<u8>> {
        self.client.screenshot()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use super::super::client::mock::{MockRunner, Req, Resp};
    use super::*;
    use crate::backend::MaestroErrorKind;
    use crate::flow::ir::Point;

    fn el(element_type: i64, label: &str, frame: [f64; 4], children: Vec<Value>) -> Value {
        json!({
            "label": label, "elementType": element_type, "identifier": "", "enabled": true,
            "selected": false, "hasFocus": false, "children": children,
            "frame": { "X": frame[0], "Y": frame[1], "Width": frame[2], "Height": frame[3] }
        })
    }

    fn screen(children: Vec<Value>) -> String {
        json!({ "depth": 2, "axElement": el(0, "", [0.0, 0.0, 402.0, 874.0], children) }).to_string()
    }

    /// The location permission alert on top of the sample app.
    fn alert_screen() -> String {
        screen(vec![
            el(9, "Request Location Permission", [20.0, 500.0, 362.0, 44.0], vec![]),
            el(
                7,
                "Allow “Sample App” to use your location?",
                [66.0, 300.0, 270.0, 262.0],
                vec![
                    el(9, "Allow Once", [66.0, 430.0, 270.0, 44.0], vec![]),
                    el(9, "Allow While Using App", [66.0, 474.0, 270.0, 44.0], vec![]),
                    el(9, "Don’t Allow", [66.0, 518.0, 270.0, 44.0], vec![]),
                ],
            ),
        ])
    }

    fn app_screen() -> String {
        screen(vec![
            el(48, "Permission: granted", [20.0, 300.0, 362.0, 44.0], vec![]),
            el(9, "Request Location Permission", [20.0, 500.0, 362.0, 44.0], vec![]),
        ])
    }

    #[derive(Default)]
    struct Scene {
        hierarchy: String,
        /// A touch inside `[l, t, r, b]` replaces the hierarchy with `then`.
        on_touch: Option<([f64; 4], String)>,
        /// `/isScreenStatic` answers `false` this many times, then `true`.
        moving_probes: usize,
        static_broken: bool,
        screen: Option<(u32, u32)>,
        rotated: bool,
    }

    struct Fixture {
        runner: MockRunner,
        scene: Arc<Mutex<Scene>>,
        client: IosNativeClient,
        apps: Vec<String>,
    }

    fn fixture(initial: &str) -> Fixture {
        let scene = Arc::new(Mutex::new(Scene { hierarchy: initial.to_string(), ..Default::default() }));
        let s = scene.clone();
        let runner = MockRunner::start(Box::new(move |req: &Req| {
            let mut sc = s.lock().unwrap();
            match req.path() {
                "/deviceInfo" => {
                    let (w, h) = if sc.rotated { (874, 402) } else { sc.screen.unwrap_or((402, 874)) };
                    Resp::json(&format!(
                        r#"{{"widthPixels":{},"heightPixels":{},"widthPoints":{w},"heightPoints":{h}}}"#,
                        w * 3,
                        h * 3
                    ))
                }
                "/viewHierarchy" => Resp::json(&sc.hierarchy),
                "/touch" => {
                    let b = req.json();
                    let (x, y) = (b["x"].as_f64().unwrap(), b["y"].as_f64().unwrap());
                    if let Some((r, then)) = sc.on_touch.clone()
                        && x >= r[0]
                        && x <= r[2]
                        && y >= r[1]
                        && y <= r[3]
                    {
                        sc.hierarchy = then;
                    }
                    Resp::json("{}")
                }
                "/setOrientation" => {
                    sc.rotated = true;
                    Resp::json("{}")
                }
                "/isScreenStatic" if sc.static_broken => {
                    Resp { status: 500, content_type: "application/json", body: b"{}".to_vec() }
                }
                "/isScreenStatic" => {
                    if sc.moving_probes > 0 {
                        sc.moving_probes -= 1;
                        Resp::json(r#"{"isScreenStatic":false}"#)
                    } else {
                        Resp::json(r#"{"isScreenStatic":true}"#)
                    }
                }
                _ => Resp::json("{}"),
            }
        }));
        let client = IosNativeClient::connect(runner.port);
        let apps = vec!["dev.tapwing.sampleApp".to_string(), "com.apple.springboard".to_string()];
        Fixture { runner, scene, client, apps }
    }

    fn text(t: &str) -> ElementSelector {
        ElementSelector { text_regex: Some(t.into()), ..Default::default() }
    }

    fn tap_req(sel: ElementSelector, timeout_ms: u64) -> TapElementRequest {
        TapElementRequest {
            selector: sel,
            timeout_ms,
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

    fn touches(f: &Fixture) -> Vec<(f64, f64, Value)> {
        f.runner
            .calls("touch")
            .iter()
            .map(|r| {
                let b = r.json();
                (b["x"].as_f64().unwrap(), b["y"].as_f64().unwrap(), b["duration"].clone())
            })
            .collect()
    }

    #[test]
    fn taps_the_permission_alert_button_then_sees_the_app() {
        let mut f = fixture(&alert_screen());
        f.scene.lock().unwrap().on_touch = Some(([66.0, 474.0, 336.0, 518.0], app_screen()));
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);

        assert!(ops.has_springboard_alert().unwrap());
        ops.assert_visible(&text("Allow While Using App"), 1000).unwrap();
        let res = ops.tap_on_element(&tap_req(text("Allow While Using App"), 1000)).unwrap();
        // Centre of [66,474][336,518] in points.
        assert_eq!((res["x"].as_i64(), res["y"].as_i64()), (Some(201), Some(496)));
        assert_eq!(res["lane"], "native");

        ops.assert_not_visible(&text("Allow While Using App"), 500).unwrap();
        ops.assert_visible(&text("Permission: granted"), 500).unwrap();
        assert!(!ops.has_springboard_alert().unwrap());
        assert_eq!(touches(&f), vec![(201.0, 496.0, Value::Null)]);
        // Every request after the first reused one connection.
        assert_eq!(f.runner.connections.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn hierarchy_requests_carry_the_app_ids() {
        let mut f = fixture(&app_screen());
        IosNativeOps::new(&mut f.client, &f.apps).assert_visible(&text("Permission: granted"), 100).unwrap();
        let body = f.runner.calls("viewHierarchy")[0].json();
        assert_eq!(body["appIds"], json!(["dev.tapwing.sampleApp", "com.apple.springboard"]));
        assert_eq!(body["excludeKeyboardElements"], false);
    }

    #[test]
    fn missing_element_reports_what_is_on_screen() {
        let mut f = fixture(&alert_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let t = Instant::now();
        let err = ops.assert_visible(&text("Allow all the time"), 200).unwrap_err();
        assert!(t.elapsed() >= Duration::from_millis(150), "polled until the timeout");
        let me = err.downcast_ref::<MaestroError>().expect("MaestroError");
        assert_eq!(me.kind, MaestroErrorKind::ElementNotFound);
        assert!(me.message.contains("Allow all the time"), "{}", me.message);
        assert!(me.message.contains("\"Allow While Using App\""), "{}", me.message);
        assert!(is_element_not_found(&ops.tap_on_element(&tap_req(text("Nope"), 0)).unwrap_err()));
        assert!(touches(&f).is_empty());
    }

    #[test]
    fn assert_not_visible_fails_while_the_element_stays() {
        let mut f = fixture(&alert_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let err = ops.assert_not_visible(&text("Allow Once"), 100).unwrap_err();
        assert_eq!(err.downcast_ref::<MaestroError>().unwrap().kind, MaestroErrorKind::AssertionFailure);
        ops.assert_not_visible(&text("Something else"), 100).unwrap();
    }

    #[test]
    fn settle_waits_for_a_static_screen_and_zero_returns_at_once() {
        let mut f = fixture(&app_screen());
        f.scene.lock().unwrap().moving_probes = 3;
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        ops.settle(None, Some(2000)).unwrap();
        assert_eq!(f.runner.calls("isScreenStatic").len(), 4, "3 moving probes, then static");
        assert_eq!(f.runner.calls("viewHierarchy").len(), 1, "one hierarchy fetch once static");

        let before = f.runner.recorded().len();
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let initial = ops.hierarchy().unwrap();
        let after_fetch = f.runner.recorded().len();
        let t = Instant::now();
        ops.settle(Some(initial), Some(0)).unwrap();
        assert!(t.elapsed() < Duration::from_millis(100));
        assert_eq!(f.runner.recorded().len(), after_fetch, "settle(0) with a hierarchy sends nothing");
        assert!(after_fetch > before);
        ops.settle(None, Some(0)).unwrap();
        assert_eq!(f.runner.calls("isScreenStatic").len(), 4, "settle(0) never probes the runner");
    }

    #[test]
    fn settle_stops_at_its_budget_when_the_screen_never_settles() {
        let mut f = fixture(&app_screen());
        f.scene.lock().unwrap().moving_probes = usize::MAX;
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let t = Instant::now();
        ops.settle(None, Some(250)).unwrap();
        let spent = t.elapsed();
        assert!(spent >= Duration::from_millis(200) && spent < Duration::from_millis(1500), "{spent:?}");
        // Probes are paced (50 ms), not a busy loop.
        assert!(f.runner.calls("isScreenStatic").len() <= 8, "{}", f.runner.calls("isScreenStatic").len());
    }

    #[test]
    fn settle_falls_back_to_hierarchy_comparison_without_is_screen_static() {
        let mut f = fixture(&app_screen());
        f.scene.lock().unwrap().static_broken = true;
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let h = ops.settle(None, Some(1000)).unwrap();
        assert!(h.find(&text("Permission: granted")).unwrap().is_some());
        // Identical consecutive hierarchies: two fetches (initial + comparison).
        assert_eq!(f.runner.calls("viewHierarchy").len(), 2);
        let res = ops.wait_for_animation_to_end(Some(500)).unwrap();
        assert_eq!(res["settled"], true);
    }

    #[test]
    fn wait_for_animation_to_end_reports_whether_the_screen_settled() {
        let mut f = fixture(&app_screen());
        f.scene.lock().unwrap().moving_probes = 2;
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        assert_eq!(ops.wait_for_animation_to_end(Some(2000)).unwrap()["settled"], true);
        f.scene.lock().unwrap().moving_probes = usize::MAX;
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        assert_eq!(ops.wait_for_animation_to_end(Some(120)).unwrap()["settled"], false);
    }

    #[test]
    fn point_taps_long_press_text_input_erase_and_copy() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let tap = |point: &str, long_press: bool| TapPointRequest {
            point: point.into(),
            long_press,
            repeat: None,
            retry_if_no_change: false,
            wait_to_settle_timeout_ms: Some(0),
        };
        let res = ops.tap_on_point(&tap("50%, 25%", false)).unwrap();
        assert_eq!((res["x"].as_i64(), res["y"].as_i64()), (Some(201), Some(218)));
        ops.tap_on_point(&tap("30, 40", true)).unwrap();
        assert!(ops.tap_on_point(&tap("101%, 5%", false)).is_err());
        assert!(ops.tap_on_point(&tap("-1, 5", false)).is_err());

        ops.input_text("čau 42").unwrap();
        ops.erase_text(50).unwrap();
        ops.erase_text(-3).unwrap();

        let (copied, _) = ops.copy_text_from(&text("Permission: .*"), 500).unwrap();
        assert_eq!(copied, "Permission: granted");
        let bounds = ops.find_element_bounds(&text("Request Location Permission"), 500).unwrap();
        // Scaled from [20, 500, 382, 544] points to @3x screenshot pixels (1206x2622 / 402x874).
        assert_eq!(bounds, [60, 1500, 1146, 1632]);

        assert_eq!(touches(&f), vec![(201.0, 218.0, Value::Null), (30.0, 40.0, json!(3.0))]);
        assert_eq!(f.runner.calls("inputText")[0].json()["text"], "čau 42");
        assert_eq!(f.runner.calls("inputText")[0].json()["appIds"], json!(f.apps));
        let erased: Vec<_> =
            f.runner.calls("eraseText").iter().map(|r| r.json()["charactersToErase"].clone()).collect();
        assert_eq!(erased, vec![json!(50), json!(0)]);
    }

    #[test]
    fn retry_if_no_change_taps_again_only_when_the_screen_is_static() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let mut req = tap_req(text("Request Location Permission"), 500);
        req.retry_if_no_change = true;
        ops.tap_on_element(&req).unwrap();
        assert_eq!(touches(&f).len(), 2, "nothing changed, so the tap was repeated once");

        let mut f = fixture(&alert_screen());
        f.scene.lock().unwrap().on_touch = Some(([66.0, 474.0, 336.0, 518.0], app_screen()));
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let mut req = tap_req(text("Allow While Using App"), 500);
        req.retry_if_no_change = true;
        ops.tap_on_element(&req).unwrap();
        assert_eq!(touches(&f).len(), 1, "the screen changed, no retry");
    }

    #[test]
    fn keys_and_buttons_map_to_the_runner_endpoints() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        ops.press_key("Enter").unwrap();
        ops.press_key("Backspace").unwrap();
        let ignored = ops.press_key("Volume Up").unwrap();
        assert_eq!(ignored["ignored"], "Volume Up");
        ops.press_key("Home").unwrap();
        ops.press_key("Lock").unwrap();
        ops.press_button("home").unwrap();
        let err = ops.press_button("power").unwrap_err();
        assert_eq!(err.downcast_ref::<MaestroError>().unwrap().kind, MaestroErrorKind::InvalidCommand);

        let keys: Vec<_> = f.runner.calls("pressKey").iter().map(|r| r.json()["key"].clone()).collect();
        assert_eq!(keys, vec![json!("return"), json!("delete")]);
        let buttons: Vec<_> = f.runner.calls("pressButton").iter().map(|r| r.json()["button"].clone()).collect();
        assert_eq!(buttons, vec![json!("home"), json!("lock"), json!("home")]);
    }

    #[test]
    fn set_orientation_normalizes_names_and_refreshes_the_screen_size() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        ops.tap_on_point(&TapPointRequest {
            point: "50%, 50%".into(),
            long_press: false,
            repeat: None,
            retry_if_no_change: false,
            wait_to_settle_timeout_ms: Some(0),
        })
        .unwrap();
        let res = ops.set_orientation("LANDSCAPE_LEFT").unwrap();
        assert_eq!(res["orientation"], "landscapeLeft");
        // After the rotation 50%/50% is taken on the swapped (874x402) screen.
        ops.tap_on_point(&TapPointRequest {
            point: "50%, 50%".into(),
            long_press: false,
            repeat: None,
            retry_if_no_change: false,
            wait_to_settle_timeout_ms: Some(0),
        })
        .unwrap();
        assert_eq!(touches(&f), vec![(201.0, 437.0, Value::Null), (437.0, 201.0, Value::Null)]);
        assert_eq!(f.runner.calls("deviceInfo").len(), 2);
        let sent: Vec<_> = f.runner.calls("setOrientation").iter().map(|r| r.json()["orientation"].clone()).collect();
        assert_eq!(sent, vec![json!("landscapeLeft")]);

        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        for (raw, want) in [
            ("PORTRAIT", "portrait"),
            ("landscapeRight", "landscapeRight"),
            ("upside_down", "upsideDown"),
            ("LANDSCAPE_RIGHT", "landscapeRight"),
        ] {
            assert_eq!(ops.set_orientation(raw).unwrap()["orientation"], want);
        }
        let err = ops.set_orientation("SIDEWAYS").unwrap_err();
        assert_eq!(err.downcast_ref::<MaestroError>().unwrap().kind, MaestroErrorKind::InvalidCommand);
    }

    fn swipe_req() -> SwipeRequest {
        SwipeRequest {
            element_selector: None,
            direction: None,
            start_relative: None,
            end_relative: None,
            start_point: None,
            end_point: None,
            relative_point: None,
            duration_ms: 400,
            timeout_ms: 500,
            optional: false,
            wait_to_settle_timeout_ms: Some(0),
        }
    }

    fn swipes(f: &Fixture) -> Vec<(Vec<f64>, f64)> {
        f.runner
            .calls("swipeV2")
            .iter()
            .map(|r| {
                let b = r.json();
                let coords = ["startX", "startY", "endX", "endY"].map(|k| b[k].as_f64().unwrap()).to_vec();
                (coords, b["duration"].as_f64().unwrap())
            })
            .collect()
    }

    #[test]
    fn swipes_and_scroll_use_ios_driver_ratios_in_points() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        for dir in [Direction::Up, Direction::Down, Direction::Left, Direction::Right] {
            ops.swipe(&SwipeRequest { direction: Some(dir), ..swipe_req() }).unwrap();
        }
        ops.swipe(&SwipeRequest {
            start_relative: Some("50%, 75%".into()),
            end_relative: Some("50%, 35%".into()),
            ..swipe_req()
        })
        .unwrap();
        ops.swipe(&SwipeRequest {
            start_point: Some(Point { x: 200, y: 600 }),
            end_point: Some(Point { x: 200, y: 300 }),
            duration_ms: 300,
            ..swipe_req()
        })
        .unwrap();
        ops.scroll().unwrap();
        assert_eq!(
            ops.swipe(&swipe_req()).unwrap_err().downcast_ref::<MaestroError>().unwrap().kind,
            MaestroErrorKind::InvalidCommand
        );

        // 402 x 874 points; ratios of `IOSDriver.swipe(direction)` truncated to ints.
        let want: Vec<(Vec<f64>, f64)> = vec![
            (vec![201.0, 786.0, 201.0, 87.0], 0.4),
            (vec![201.0, 174.0, 201.0, 786.0], 0.4),
            (vec![361.0, 437.0, 40.0, 437.0], 0.4),
            (vec![40.0, 437.0, 361.0, 437.0], 0.4),
            (vec![201.0, 655.0, 201.0, 305.0], 0.4),
            (vec![200.0, 600.0, 200.0, 300.0], 0.3),
            (vec![201.0, 437.0, 201.0, 87.0], 0.333),
        ];
        assert_eq!(swipes(&f), want);
        assert!(f.runner.calls("swipeV2").iter().all(|r| r.json()["appId"].is_null()));
    }

    #[test]
    fn swipe_from_an_element_goes_towards_the_screen_edge() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        ops.swipe(&SwipeRequest {
            element_selector: Some(text("Request Location Permission")),
            direction: Some(Direction::Left),
            ..swipe_req()
        })
        .unwrap();
        // Element centre (201, 522) towards 10% of the width.
        assert_eq!(swipes(&f), vec![(vec![201.0, 522.0, 40.0, 522.0], 0.4)]);
    }

    #[test]
    fn scroll_until_visible_swipes_until_the_element_appears() {
        let below = screen(vec![el(48, "Top", [20.0, 100.0, 100.0, 44.0], vec![])]);
        let found = screen(vec![el(48, "Product 40", [20.0, 400.0, 200.0, 44.0], vec![])]);
        let mut f = fixture(&below);
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let req = ScrollUntilVisibleRequest {
            selector: text("Product 40"),
            direction: Direction::Down,
            duration_ms: 400,
            timeout_ms: 2000,
            visibility_percentage: 100,
            center_element: false,
            optional: false,
            wait_to_settle_timeout_ms: Some(0),
        };
        // The first swipe lands: reveal the element from then on.
        let scene = f.scene.clone();
        let flip = std::thread::spawn({
            let runner_requests = f.runner.requests.clone();
            move || {
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs(5) {
                    if runner_requests.lock().unwrap().iter().any(|r| r.path() == "/swipeV2") {
                        scene.lock().unwrap().hierarchy = found;
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        let res = ops.scroll_until_visible(&req).unwrap();
        flip.join().unwrap();
        assert_eq!(res["lane"], "native");
        let n = f.runner.calls("swipeV2").len();
        assert!((1..=3).contains(&n), "swiped {n} times");
        // `scrollUntilVisible: DOWN` swipes UP from the screen centre to 10% height.
        assert_eq!(swipes(&f)[0].0, vec![201.0, 437.0, 201.0, 87.0]);

        // Never visible: fails with the selector after the timeout.
        let mut f = fixture(&below);
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        let err = ops.scroll_until_visible(&ScrollUntilVisibleRequest { timeout_ms: 300, ..req }).unwrap_err();
        assert!(is_element_not_found(&err));
    }

    #[test]
    fn screenshot_and_foreground_app_go_through_the_runner() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        // The default mock answers `{}` for unknown endpoints: no `runningAppBundleId`.
        assert!(ops.foreground_app_id().is_err());
        assert!(ops.screenshot().unwrap().len() <= 2);
        assert_eq!(f.runner.calls("screenshot").len(), 1);
    }

    #[test]
    fn find_element_bounds_scales_points_to_screenshot_pixels() {
        let mut f = fixture(&app_screen());
        let mut ops = IosNativeOps::new(&mut f.client, &f.apps);
        // Element is [20, 500, 382, 544] points on a @3x simulator (1206x2622 / 402x874).
        let bounds = ops.find_element_bounds(&text("Request Location Permission"), 500).unwrap();
        assert_eq!(bounds, [60, 1500, 1146, 1632]);
    }

    #[test]
    fn hide_keyboard_checks_visibility_and_never_sends_return() {
        let kb_visible = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let kb_flag = kb_visible.clone();
        let runner = MockRunner::start(Box::new(move |req| match req.path() {
            "/deviceInfo" => {
                Resp::json(r#"{"widthPixels":1206,"heightPixels":2622,"widthPoints":402,"heightPoints":874}"#)
            }
            "/keyboard" => {
                let v = kb_flag.load(Ordering::Relaxed);
                Resp::json(&format!(r#"{{"isKeyboardVisible":{v}}}"#))
            }
            "/swipeV2" => {
                kb_flag.store(false, Ordering::Relaxed);
                Resp::json("{}")
            }
            _ => Resp::json("{}"),
        }));
        let mut client = IosNativeClient::connect(runner.port);
        let apps = vec!["dev.tapwing.sampleApp".to_string()];
        let mut ops = IosNativeOps::new(&mut client, &apps);

        // Already hidden: returns immediately with no swipe and no Enter key.
        let r1 = ops.hide_keyboard().unwrap();
        assert_eq!(r1["alreadyHidden"], true);
        assert!(runner.calls("swipeV2").is_empty());
        assert!(runner.calls("pressKey").is_empty());

        // Visible: dismisses via swipe (not Enter).
        kb_visible.store(true, Ordering::Relaxed);
        let r2 = ops.hide_keyboard().unwrap();
        assert_eq!(r2["lane"], "native");
        assert_eq!(runner.calls("swipeV2").len(), 1);
        assert!(runner.calls("pressKey").is_empty());
    }

    #[test]
    fn hide_keyboard_that_survives_the_swipes_warns_unless_strict() {
        let runner = MockRunner::start(Box::new(|req| match req.path() {
            "/deviceInfo" => {
                Resp::json(r#"{"widthPixels":1206,"heightPixels":2622,"widthPoints":402,"heightPoints":874}"#)
            }
            "/keyboard" => Resp::json(r#"{"isKeyboardVisible":true}"#),
            _ => Resp::json("{}"),
        }));
        let mut client = IosNativeClient::connect(runner.port);
        let apps = vec!["dev.tapwing.sampleApp".to_string()];
        let mut ops = IosNativeOps::new(&mut client, &apps);

        let lenient = ops.hide_keyboard_with(false).unwrap();
        assert!(lenient["warning"].as_str().unwrap().contains("still visible"));
        assert_eq!(runner.calls("swipeV2").len(), 2, "both Maestro dismiss swipes ran");

        let err = ops.hide_keyboard_with(true).unwrap_err();
        assert!(err.to_string().contains("Couldn't hide the keyboard"));
        assert!(ops.hide_keyboard().is_err(), "the plain entry point stays strict");
    }

    #[test]
    fn wait_for_animation_fallback_reports_settled_after_transition() {
        // Simulates A -> B -> B when `isScreenStatic` returns HTTP 500.
        let h_a = screen(vec![el(48, "Frame A", [0.0, 0.0, 100.0, 40.0], vec![])]);
        let h_b = screen(vec![el(48, "Frame B", [0.0, 0.0, 100.0, 40.0], vec![])]);
        let call = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_h = call.clone();
        let runner = MockRunner::start(Box::new(move |req| match req.path() {
            "/deviceInfo" => {
                Resp::json(r#"{"widthPixels":1206,"heightPixels":2622,"widthPoints":402,"heightPoints":874}"#)
            }
            "/isScreenStatic" => Resp { status: 500, content_type: "text/plain", body: b"unsupported".to_vec() },
            "/viewHierarchy" => {
                let n = call_h.fetch_add(1, Ordering::Relaxed);
                if n == 0 { Resp::json(&h_a) } else { Resp::json(&h_b) }
            }
            _ => Resp::json("{}"),
        }));
        let mut client = IosNativeClient::connect(runner.port);
        let apps = vec!["dev.tapwing.sampleApp".to_string()];
        let mut ops = IosNativeOps::new(&mut client, &apps);
        let res = ops.wait_for_animation_to_end(Some(1500)).unwrap();
        assert_eq!(res["settled"], true);
    }
}
