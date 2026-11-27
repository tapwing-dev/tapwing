//! Shared selector polling, tap, assertion, swipe, and scroll operations
//! parameterized over a platform [`NativeSurface`].

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use crate::backend::{
    MaestroError, ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest, resolve_screen_point,
};
use crate::flow::ir::{Direction, ElementSelector, TapRepeat};
use crate::ui_tree::{
    Bounds, TreeNode, calculate_element_relative_point, describe_sel, find_in_tree, is_element_not_found,
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SETTLE_INTERVAL: Duration = Duration::from_millis(200);
const WAIT_UNTIL_VISIBLE: Duration = Duration::from_secs(10);

/// Armed for the first lookup after a `launchApp` in case the app has not
/// rendered any accessibility content yet.
#[derive(Clone, Debug)]
pub struct ColdStart {
    /// Package or bundle id of the launched app.
    pub app_id: String,
    /// Extra lookup window granted when the app never showed up.
    pub extra: Duration,
    /// Console prefix for the `warning:` line (`[shard N] ` when sharded).
    pub log_prefix: String,
}

/// Outcome of [`perform_tap`].
pub struct TapOutcome<H> {
    /// Whether the settled hierarchy changed compared to before the tap.
    pub changed: bool,
    /// The hierarchy the screen settled on after the tap.
    pub after: H,
}

/// Platform view hierarchy abstraction backed by [`TreeNode`].
pub trait NativeHierarchy {
    /// Root of the bounds-filtered UI tree.
    fn root(&self) -> &TreeNode;

    /// First element matching `selector`.
    fn find(&self, selector: &ElementSelector) -> Result<Option<TreeNode>> {
        find_in_tree(self.root(), selector)
    }

    /// Whether nothing is drawn over `node` at its center.
    fn is_visible(&self, node: &TreeNode) -> bool {
        self.root().is_topmost(node)
    }

    /// Whether a node outside `node`'s own subtree covers `node` at its center.
    fn is_covered_by_other(&self, node: &TreeNode) -> bool {
        self.root().is_covered_by_other(node)
    }

    /// Distinct visible texts (`text`, then `accessibility_text`), for error reporting.
    fn visible_texts(&self, limit: usize) -> Vec<String> {
        self.root().visible_texts(limit)
    }

    /// Structural fingerprint used to detect when the UI tree stopped changing.
    fn signature(&self) -> String {
        self.root().structure_signature()
    }

    /// Whether `package` has content on screen (used by cold-start retry on Android).
    fn shows_package(&self, _package: &str) -> bool {
        false
    }
}

/// Platform UI surface abstraction implemented by Android [`NativeOps`](crate::native_lane::ops::NativeOps)
/// and iOS [`IosNativeOps`](crate::ios_native_lane::ops::IosNativeOps).
pub trait NativeSurface {
    /// Platform hierarchy type.
    type Hierarchy: NativeHierarchy + Clone;

    /// Relative Y coordinate (`0.5` on Android, `0.9` on iOS) where an upward screen swipe starts.
    const SWIPE_UP_START_Y: f64;

    /// Screen dimensions in driver coordinate units (pixels on Android, logical points on iOS).
    fn screen(&mut self) -> Result<(i32, i32)>;

    /// Hierarchy snapshot used during positive polling loops (skips `waitForIdle` on Android).
    fn hierarchy_for_poll(&mut self) -> Result<Self::Hierarchy>;

    /// Full settled hierarchy snapshot used for negative assertions and tap-change baselines.
    fn hierarchy_for_negative(&mut self) -> Result<Self::Hierarchy>;

    /// Waits for UI movement/animations to settle, returning the settled hierarchy.
    fn settle(&mut self, initial: Option<Self::Hierarchy>, timeout_ms: Option<i32>) -> Result<Self::Hierarchy>;

    /// Performs a single tap or long press at `(x, y)` in driver coordinate units.
    fn tap_xy(&mut self, x: i32, y: i32, long_press: bool) -> Result<()>;

    /// Performs a raw swipe from `start` to `end` over `duration_ms` milliseconds.
    fn raw_swipe(&mut self, start: (i32, i32), end: (i32, i32), duration_ms: i64) -> Result<()>;

    /// Returns any pending [`ColdStart`] configuration for the next element poll.
    fn take_cold_start(&mut self) -> Option<ColdStart> {
        None
    }

    /// Records that a bounded cold-start retry window was granted.
    fn note_cold_start_retry(&mut self) {}
}

/// Single lookup; nodes without bounds cannot be tapped and count as missing.
pub fn find_once<S: NativeSurface>(
    surface: &mut S,
    sel: &ElementSelector,
    fast: bool,
) -> Result<(Option<TreeNode>, S::Hierarchy)> {
    let h = if fast { surface.hierarchy_for_poll()? } else { surface.hierarchy_for_negative()? };
    let found = h.find(sel)?.filter(|n| n.bounds.is_some());
    Ok((found, h))
}

/// Formats Maestro's element-not-found error including visible screen text snippets.
pub fn not_found<H: NativeHierarchy>(sel: &ElementSelector, h: &H) -> anyhow::Error {
    let seen = h.visible_texts(12).iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
    MaestroError::element_not_found(format!("Element not found: {}\n      on screen: {seen}", describe_sel(sel))).into()
}

/// Polls `surface` until `sel` matches an element with bounds or `timeout_ms` elapses
/// (extending once if a [`ColdStart`] window is armed and the app rendered no content yet).
pub fn poll_find<S: NativeSurface>(
    surface: &mut S,
    sel: &ElementSelector,
    timeout_ms: u64,
) -> Result<(TreeNode, S::Hierarchy)> {
    let started = Instant::now();
    let mut deadline = started + Duration::from_millis(timeout_ms);
    let mut cold = surface.take_cold_start();
    loop {
        let (found, h) = find_once(surface, sel, true)?;
        if let Some(node) = found {
            return Ok((node, h));
        }
        if cold.as_ref().is_some_and(|c| h.shows_package(&c.app_id)) {
            cold = None;
        }
        if Instant::now() >= deadline {
            if let Some(c) = cold.take() {
                eprintln!(
                    "{}warning: {} showed nothing on screen during the first {} ms lookup after launchApp \
                     (cold start); looking for {} once more for up to {} ms",
                    c.log_prefix,
                    c.app_id,
                    started.elapsed().as_millis(),
                    describe_sel(sel),
                    c.extra.as_millis()
                );
                surface.note_cold_start_retry();
                deadline = Instant::now() + c.extra;
                continue;
            }
            return Err(not_found(sel, &h));
        }
        crate::interrupt::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())))?;
    }
}

/// Polls until `sel` matches an uncovered (topmost at center) element or the 10 s cap expires.
pub fn wait_until_visible<S: NativeSurface>(
    surface: &mut S,
    sel: &ElementSelector,
    fallback: &S::Hierarchy,
) -> Result<S::Hierarchy> {
    let deadline = Instant::now() + WAIT_UNTIL_VISIBLE;
    let mut latest = fallback.clone();
    loop {
        if let Some(node) = latest.find(sel)?.filter(|n| n.bounds.is_some())
            && latest.is_visible(&node)
        {
            return Ok(latest);
        }
        if Instant::now() >= deadline {
            return Ok(latest);
        }
        crate::interrupt::sleep(SETTLE_INTERVAL.min(deadline.saturating_duration_since(Instant::now())))?;
        latest = surface.hierarchy_for_poll()?;
    }
}

/// Executes a tap (optionally repeated or long-pressed), settles, and retries once
/// if `retry_if_no_change` is enabled and the UI signature did not change.
pub fn perform_tap<S: NativeSurface>(
    surface: &mut S,
    (x, y): (i32, i32),
    long_press: bool,
    repeat: Option<&TapRepeat>,
    retry_if_no_change: bool,
    before: Option<S::Hierarchy>,
    settle_ms: Option<i32>,
) -> Result<TapOutcome<S::Hierarchy>> {
    let before = if retry_if_no_change {
        Some(match before {
            Some(h) => h,
            None => surface.hierarchy_for_negative()?,
        })
    } else {
        before
    };
    let attempts = if retry_if_no_change { 2 } else { 1 };
    let mut outcome = None;
    for _ in 0..attempts {
        match repeat {
            Some(r) if !long_press => {
                let count = r.repeat.max(1);
                for i in 0..count {
                    let t = Instant::now();
                    surface.tap_xy(x, y, false)?;
                    if i + 1 < count {
                        let spent = t.elapsed().as_millis().min(i64::MAX as u128) as i64;
                        crate::interrupt::sleep(Duration::from_millis(r.delay.saturating_sub(spent).max(0) as u64))?;
                    }
                }
            }
            _ => surface.tap_xy(x, y, long_press)?,
        }
        let after = surface.settle(None, settle_ms)?;
        let changed = before.as_ref().is_none_or(|b| after.signature() != b.signature());
        outcome = Some(TapOutcome { changed, after });
        if !retry_if_no_change || changed {
            break;
        }
    }
    outcome.ok_or_else(|| anyhow::anyhow!("no tap attempt executed"))
}

/// Shared `tapOn` element execution with settle aiming and transient-overlay re-tap.
pub fn tap_on_element<S: NativeSurface>(surface: &mut S, req: &TapElementRequest) -> Result<Value> {
    let (node, hier) = poll_find(surface, &req.selector, req.timeout_ms)?;
    let settled = surface.settle(Some(hier), req.wait_to_settle_timeout_ms)?;
    let node = settled.find(&req.selector)?.filter(|n| n.bounds.is_some()).unwrap_or(node);
    let bounds = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
    let point = match &req.relative_point {
        Some(rel) => calculate_element_relative_point(bounds, rel)?,
        None => bounds.center(),
    };
    let covered_before_tap =
        req.wait_until_visible && req.relative_point.is_none() && settled.is_covered_by_other(&node);
    let before_sig = covered_before_tap.then(|| node.structure_signature());
    let outcome = perform_tap(
        surface,
        point,
        req.long_press,
        req.repeat.as_ref(),
        req.retry_if_no_change,
        Some(settled),
        req.wait_to_settle_timeout_ms,
    )?;
    let mut point = point;
    if req.wait_until_visible && req.relative_point.is_none() {
        let covered = outcome.after.find(&req.selector)?.filter(|n| n.bounds.is_some());
        if let Some(covered) = covered
            && ((!outcome.changed && !outcome.after.is_visible(&covered))
                || before_sig.as_deref().is_some_and(|sig| covered.structure_signature() == sig))
        {
            let uncovered = wait_until_visible(surface, &req.selector, &outcome.after)?;
            let target = uncovered.find(&req.selector)?.filter(|n| n.bounds.is_some()).unwrap_or(covered);
            point = target.bounds.map_or(point, |b| b.center());
            perform_tap(
                surface,
                point,
                req.long_press,
                req.repeat.as_ref(),
                false,
                Some(uncovered),
                req.wait_to_settle_timeout_ms,
            )?;
        }
    }
    Ok(json!({ "x": point.0, "y": point.1, "lane": "native" }))
}

/// Shared `tapOn` point execution.
pub fn tap_on_point<S: NativeSurface>(surface: &mut S, req: &TapPointRequest) -> Result<Value> {
    let (w, h) = surface.screen()?;
    let point = resolve_screen_point(&req.point, w, h)?;
    perform_tap(
        surface,
        point,
        req.long_press,
        req.repeat.as_ref(),
        req.retry_if_no_change,
        None,
        req.wait_to_settle_timeout_ms,
    )?;
    Ok(json!({ "x": point.0, "y": point.1, "lane": "native" }))
}

/// Shared `assertVisible` execution.
pub fn assert_visible<S: NativeSurface>(surface: &mut S, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
    let (node, _) = poll_find(surface, sel, timeout_ms)?;
    Ok(json!({ "text": node.text, "id": node.id, "lane": "native" }))
}

/// Shared `assertNotVisible` execution using settled negative hierarchies.
pub fn assert_not_visible<S: NativeSurface>(surface: &mut S, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let (found, _) = find_once(surface, sel, false)?;
        if found.is_none() {
            return Ok(json!({ "lane": "native" }));
        }
        if Instant::now() >= deadline {
            return Err(
                MaestroError::assertion(format!("Assertion is false: {} is not visible", describe_sel(sel))).into()
            );
        }
        crate::interrupt::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())))?;
    }
}

/// Shared `copyTextFrom` execution.
pub fn copy_text_from<S: NativeSurface>(
    surface: &mut S,
    sel: &ElementSelector,
    timeout_ms: u64,
) -> Result<(String, Value)> {
    let (node, _) = poll_find(surface, sel, timeout_ms)?;
    let text = node
        .resolved_copy_text()
        .ok_or_else(|| MaestroError::unable_to_copy("Element does not contain text to copy"))?;
    Ok((text.clone(), json!({ "text": text, "lane": "native" })))
}

/// Screen-wide swipe across `surface` along `dir`.
pub fn swipe_direction<S: NativeSurface>(surface: &mut S, dir: Direction, duration_ms: i64) -> Result<()> {
    let (w, h) = surface.screen()?;
    let (w, h) = (f64::from(w), f64::from(h));
    let p = |fx: f64, fy: f64| ((w * fx) as i32, (h * fy) as i32);
    let (start, end) = match dir {
        Direction::Up => (p(0.5, S::SWIPE_UP_START_Y), p(0.5, 0.1)),
        Direction::Down => (p(0.5, 0.2), p(0.5, 0.9)),
        Direction::Right => (p(0.1, 0.5), p(0.9, 0.5)),
        Direction::Left => (p(0.9, 0.5), p(0.1, 0.5)),
    };
    surface.raw_swipe(start, end, duration_ms)
}

/// Directional swipe starting from `from` to 10% / 90% of the screen axis.
pub fn swipe_from_point<S: NativeSurface>(
    surface: &mut S,
    from: (i32, i32),
    dir: Direction,
    duration_ms: i64,
) -> Result<()> {
    let (w, h) = surface.screen()?;
    let (w, h) = (f64::from(w), f64::from(h));
    let end = match dir {
        Direction::Up => (from.0, (h * 0.1) as i32),
        Direction::Down => (from.0, (h * 0.9) as i32),
        Direction::Right => ((w * 0.9) as i32, from.1),
        Direction::Left => ((w * 0.1) as i32, from.1),
    };
    surface.raw_swipe(from, end, duration_ms)
}

/// Shared `swipe` execution supporting element-anchored, relative, directional, and absolute points.
pub fn swipe<S: NativeSurface>(surface: &mut S, req: &SwipeRequest) -> Result<Value> {
    let (w, h) = surface.screen()?;
    if let (Some(sel), Some(dir)) = (&req.element_selector, req.direction) {
        let (node, _) = poll_find(surface, sel, req.timeout_ms)?;
        let bounds = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
        let from = match &req.relative_point {
            Some(rel) => calculate_element_relative_point(bounds, rel)?,
            None => bounds.center(),
        };
        swipe_from_point(surface, from, dir, req.duration_ms)?;
    } else if let (Some(sr), Some(er)) = (&req.start_relative, &req.end_relative) {
        let start = resolve_screen_point(sr, w, h)?;
        let end = resolve_screen_point(er, w, h)?;
        surface.raw_swipe(start, end, req.duration_ms)?;
    } else if let Some(dir) = req.direction {
        swipe_direction(surface, dir, req.duration_ms)?;
    } else if let (Some(s), Some(e)) = (&req.start_point, &req.end_point) {
        surface.raw_swipe((s.x, s.y), (e.x, e.y), req.duration_ms)?;
    } else {
        return Err(MaestroError::invalid_command("Illegal arguments for swiping").into());
    }
    surface.settle(None, req.wait_to_settle_timeout_ms)?;
    Ok(json!({ "lane": "native" }))
}

/// Shared `scrollUntilVisible` execution.
pub fn scroll_until_visible<S: NativeSurface>(surface: &mut S, req: &ScrollUntilVisibleRequest) -> Result<Value> {
    let swipe_dir = match req.direction {
        Direction::Down => Direction::Up,
        Direction::Up => Direction::Down,
        Direction::Right => Direction::Left,
        Direction::Left => Direction::Right,
    };
    let (w, h) = surface.screen()?;
    let threshold = f64::from(req.visibility_percentage) / 100.0;
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
    let (mut retry_center, max_retry_center) = (0, 4);
    loop {
        let poll_ms = 500_u64.min(deadline.saturating_duration_since(Instant::now()).as_millis() as u64);
        match poll_find(surface, &req.selector, poll_ms) {
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
        swipe_from_point(surface, (w / 2, h / 2), swipe_dir, req.duration_ms as i64)?;
        surface.settle(None, req.wait_to_settle_timeout_ms)?;
    }
    Err(MaestroError::element_not_found(format!(
        "Could not find a visible element matching selector: {}",
        describe_sel(&req.selector)
    ))
    .into())
}
