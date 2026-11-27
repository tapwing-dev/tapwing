//! [`HidOps`]: Maestro commands on a SpringBoard alert without the XCUITest runner.
//!
//! The hierarchy is the accessibility tree of the frontmost application mapped to the runner's
//! JSON ([`super::tree`]), taps and swipes are Indigo touches, and settling compares framebuffer
//! frames. The surface is portrait only: HID touches and the framebuffer use the unrotated space.
//!
//! Failure policy. A command that fails here without having sent input may run again on the
//! runner, so the router falls back. Once a touch went out, the command must not run again: the
//! tap may have dismissed the alert already. So after an input, a failed read of the new screen is
//! answered with an empty hierarchy, and an error that carries a possibly delivered event is final.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::tree::{self, FrontApp};
use super::{HidError, HidSession, Snapshot};
use crate::backend::{
    AssertionError, MaestroError, ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest,
};
use crate::flow::ir::ElementSelector;
use crate::ios_native_lane::hierarchy::IosHierarchy;
use crate::ios_native_lane::ops::{DEFAULT_ANIMATION_WAIT_MS, DEFAULT_SETTLE, LONG_PRESS_SECS, STATIC_POLL};
use crate::native_core::ops::{self as core_ops, NativeSurface};
use crate::ui_tree::Bounds;

/// A tap holds the finger down this long. Phase 1 registered holds of 0 to 50 ms 10 of 10 times.
pub const TAP_HOLD_MS: u32 = 10;
/// Swipes send a point about every 16 ms so a scroll view sees a drag (as the probe does).
const SWIPE_STEP_MS: i64 = 16;
/// The finger stays down this long at the start of a swipe before it moves.
pub const SWIPE_HOLD_MS: u32 = 30;
/// Bound for each accessibility request of a hierarchy read.
const SNAPSHOT_TIMEOUT_S: f64 = 3.0;

/// Points of a swipe: the start, evenly spaced steps, the end. At least two segments.
pub fn swipe_path(start: (f64, f64), end: (f64, f64), duration_ms: i64) -> (Vec<(f64, f64)>, u32) {
    let duration = duration_ms.max(1);
    let steps = (duration / SWIPE_STEP_MS).max(2);
    let path = (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            (start.0 + (end.0 - start.0) * t, start.1 + (end.1 - start.1) * t)
        })
        .collect();
    (path, u32::try_from(duration / steps).unwrap_or(u32::MAX))
}

/// A hold in seconds as milliseconds, capped at a minute.
pub fn hold_ms(secs: f64) -> u32 {
    (secs.max(0.0) * 1000.0).min(60_000.0) as u32
}

pub struct HidOps<'a> {
    session: &'a HidSession,
    /// An input event went out through this surface.
    input_sent: bool,
}

impl<'a> HidOps<'a> {
    pub fn new(session: &'a HidSession) -> Self {
        Self { session, input_sent: false }
    }

    /// Whether an input event went out, so the caller must not run the command again elsewhere.
    pub fn input_sent(&self) -> bool {
        self.input_sent
    }

    fn touch(&mut self, path: &[(f64, f64)], hold: u32, step: u32) -> Result<()> {
        match self.session.touch(path, hold, step) {
            Ok(()) => {
                self.input_sent = true;
                Ok(())
            }
            Err(e) => {
                self.input_sent |= e.input_maybe_sent();
                Err(anyhow!(e))
            }
        }
    }

    fn screen_points(&self) -> Result<(i32, i32)> {
        let s = self.session.screen()?;
        Ok((s.width as i32, s.height as i32))
    }

    /// The mapped tree of the frontmost application. Fails when the interface is not portrait.
    fn read(&self) -> Result<IosHierarchy> {
        let json = self.session.snapshot(&Snapshot { max_depth: 0, timeout_s: SNAPSHOT_TIMEOUT_S })?;
        let mapped = tree::map_snapshot(&json)?;
        if !mapped.portrait {
            return Err(HidError::Unsupported("a rotated interface").into());
        }
        let (w, h) = self.screen_points()?;
        IosHierarchy::parse(&mapped.document, w, h)
    }

    pub fn hierarchy(&mut self) -> Result<IosHierarchy> {
        self.read()
    }

    /// What replaces an unreadable screen after an input went out: a tree with nothing in it.
    fn empty_hierarchy(&self) -> Result<IosHierarchy> {
        let (w, h) = self.screen_points()?;
        let doc = json!({ "depth": 1, "axElement": {
            "label": "", "elementType": 0, "identifier": "", "enabled": true, "selected": false,
            "hasFocus": false, "children": [],
            "frame": { "X": 0.0, "Y": 0.0, "Width": w, "Height": h },
        } });
        IosHierarchy::parse(&doc, w, h)
    }

    /// Which application is in front, for a caller that wants to check before reading the tree.
    pub fn front_app(&self) -> Result<FrontApp> {
        Ok(self.session.front_app()?)
    }

    /// Polls two-frame comparison until the screen is still or `deadline` passes. A frame read that
    /// fails before any input fails the command, so the runner can take over. After an input it
    /// only ends the wait: the caller still gets a hierarchy and the command is not repeated.
    fn wait_until_static(&mut self, deadline: Instant) -> Result<bool> {
        loop {
            match self.session.screen_static() {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(_) if self.input_sent => return Ok(false),
                Err(e) => return Err(e.into()),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            crate::interrupt::sleep(STATIC_POLL.min(remaining))?;
        }
    }

    fn hierarchy_after_wait(&mut self) -> Result<IosHierarchy> {
        match self.hierarchy() {
            Err(e) if self.input_sent && !crate::interrupt::is_interrupted_error(&e) => self.empty_hierarchy(),
            other => other,
        }
    }

    pub fn settle(&mut self, initial: Option<IosHierarchy>, timeout_ms: Option<i32>) -> Result<IosHierarchy> {
        if matches!(timeout_ms, Some(ms) if ms <= 0) {
            return match initial {
                Some(h) => Ok(h),
                None => self.hierarchy_after_wait(),
            };
        }
        let budget = timeout_ms.map_or(DEFAULT_SETTLE, |ms| Duration::from_millis(u64::try_from(ms).unwrap_or(0)));
        self.wait_until_static(Instant::now() + budget)?;
        self.hierarchy_after_wait()
    }

    pub fn wait_for_animation_to_end(&mut self, timeout_ms: Option<u64>) -> Result<Value> {
        let budget = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_ANIMATION_WAIT_MS));
        let settled = self.wait_until_static(Instant::now() + budget)?;
        Ok(json!({ "lane": "native", "settled": settled }))
    }

    fn raw_swipe(&mut self, start: (i32, i32), end: (i32, i32), duration_ms: i64) -> Result<()> {
        let (path, step) =
            swipe_path((f64::from(start.0), f64::from(start.1)), (f64::from(end.0), f64::from(end.1)), duration_ms);
        self.touch(&path, SWIPE_HOLD_MS, step)
    }

    pub fn tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value> {
        core_ops::tap_on_element(self, req)
    }

    pub fn tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value> {
        core_ops::tap_on_point(self, req)
    }

    pub fn assert_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        core_ops::assert_visible(self, sel, timeout_ms)
    }

    pub fn assert_not_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        core_ops::assert_not_visible(self, sel, timeout_ms)
    }

    pub fn copy_text_from(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<(String, Value)> {
        core_ops::copy_text_from(self, sel, timeout_ms)
    }

    pub fn swipe(&mut self, req: &SwipeRequest) -> Result<Value> {
        core_ops::swipe(self, req)
    }

    pub fn scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value> {
        core_ops::scroll_until_visible(self, req)
    }

    /// `IOSDriver.scrollVertical`, as in `IosNativeOps::scroll`.
    pub fn scroll(&mut self) -> Result<Value> {
        let (w, h) = self.screen_points()?;
        let (w, h) = (f64::from(w), f64::from(h));
        self.raw_swipe(((w * 0.5) as i32, (h * 0.5) as i32), ((w * 0.5) as i32, (h * 0.1) as i32), 333)?;
        self.settle(None, None)?;
        Ok(json!({ "lane": "native" }))
    }

    /// Element bounds scaled to framebuffer pixels, like `IosNativeOps::find_element_bounds`.
    pub fn find_element_bounds(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<[i64; 4]> {
        let (node, _) = core_ops::poll_find(self, sel, timeout_ms)?;
        let b = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
        if b.width() <= 0 || b.height() <= 0 {
            return Err(AssertionError(format!(
                "Cannot crop screenshot: element has invalid dimensions (width: {}, height: {})",
                b.width(),
                b.height()
            ))
            .into());
        }
        let scale = self.session.screen()?.scale;
        Ok(crate::run::scale_crop_bounds(
            [i64::from(b.left), i64::from(b.top), i64::from(b.right), i64::from(b.bottom)],
            scale,
        ))
    }

    /// Not served here: typing needs the runner. Nothing is sent, so the router may fall back.
    pub fn unsupported<T>(&self, what: &'static str) -> Result<T> {
        Err(HidError::Unsupported(what).into())
    }
}

impl NativeSurface for HidOps<'_> {
    type Hierarchy = IosHierarchy;

    const SWIPE_UP_START_Y: f64 = 0.9;

    fn screen(&mut self) -> Result<(i32, i32)> {
        self.screen_points()
    }

    fn hierarchy_for_poll(&mut self) -> Result<IosHierarchy> {
        self.hierarchy()
    }

    fn hierarchy_for_negative(&mut self) -> Result<IosHierarchy> {
        self.hierarchy()
    }

    fn settle(&mut self, initial: Option<IosHierarchy>, timeout_ms: Option<i32>) -> Result<IosHierarchy> {
        HidOps::settle(self, initial, timeout_ms)
    }

    fn tap_xy(&mut self, x: i32, y: i32, long_press: bool) -> Result<()> {
        let hold = if long_press { hold_ms(LONG_PRESS_SECS) } else { TAP_HOLD_MS };
        self.touch(&[(f64::from(x), f64::from(y))], hold, 0)
    }

    fn raw_swipe(&mut self, start: (i32, i32), end: (i32, i32), duration_ms: i64) -> Result<()> {
        HidOps::raw_swipe(self, start, end, duration_ms)
    }
}

/// Whether an error from a command on the HID surface ends the command for good: a Maestro-level
/// outcome (element not found, assertion), an interrupt, or an input event that may have gone out.
pub fn is_final(e: &anyhow::Error, input_sent: bool) -> bool {
    input_sent
        || super::is_input_maybe_sent(e)
        || e.is::<MaestroError>()
        || e.is::<AssertionError>()
        || crate::interrupt::is_interrupted_error(e)
}

/// `Context` for the line that explains why a command was not repeated on the runner.
pub fn not_repeated(e: anyhow::Error) -> anyhow::Error {
    e.context("the command had already sent input through the HID backend, so it is not repeated on the runner")
}
