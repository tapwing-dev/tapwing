//! In-memory [`FlowDriver`] matching Maestro's `FakeDriver.kt`, `FakeLayoutElement.kt`,
//! `Filters.kt`, and `ViewHierarchy.kt`, enabling deterministic execution of the
//! ported Maestro `IntegrationTest.kt` suite without an Android device.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use anyhow::Result;
use fancy_regex::Regex;
use image::{DynamicImage, Rgba, RgbaImage};
use serde_json::{Value, json};

use crate::backend::{
    DeviceInfo, FlowDriver, LaunchRequest, MaestroError, ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest,
    TapPointRequest, TransportError, parse_index, parse_point,
};
use crate::flow::ir::{Direction, ElementSelector, ElementTrait, KeyCode, Platform, ValueMap};
use crate::screenshot;

/// Axis-aligned rectangle matching `FakeLayoutElement.Bounds`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    pub const fn of_size(width: i32, height: i32) -> Self {
        Self { left: 0, top: 0, right: width, bottom: height }
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub const fn center(&self) -> (i32, i32) {
        (self.left + self.width() / 2, self.top + self.height() / 2)
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x <= self.right && y >= self.top && y <= self.bottom
    }

    pub const fn translate(&self, dx: i32, dy: i32) -> Self {
        Self { left: self.left + dx, top: self.top + dy, right: self.right + dx, bottom: self.bottom + dy }
    }

    /// Exact port of `UiElement.getVisiblePercentage` (`UiElement.kt:31-47`).
    pub fn visible_percentage(&self, screen_w: i32, screen_h: i32) -> f64 {
        let w = self.width();
        let h = self.height();
        if w == 0 && h == 0 {
            return 0.0;
        }
        if self.left <= 0 && self.top <= 0 && self.right >= screen_w && self.bottom >= screen_h {
            return 1.0;
        }
        let vx = (self.right.min(screen_w) - self.left.max(0)).max(0);
        let vy = (self.bottom.min(screen_h) - self.top.max(0)).max(0);
        let vis_area = f64::from(vx) * f64::from(vy);
        let total_area = f64::from(w) * f64::from(h);
        // Preserves IEEE-754 `0.0 / 0.0 = NaN` (matching Kotlin) so zero-width
        // synthetic test bounds like `(0, 100, 0, 200)` are not pruned by `< 0.1`.
        vis_area / total_area
    }

    /// Exact port of `UiElement.isElementNearScreenCenter` (`UiElement.kt:49-68`).
    pub fn is_near_screen_center(&self, swipe_dir: Direction, screen_w: i32, screen_h: i32) -> bool {
        let cx = screen_w / 2;
        let cy = screen_h / 2;
        let (ecx, ecy) = self.center();
        let margin = match swipe_dir {
            Direction::Down | Direction::Up => screen_h / 5,
            Direction::Left | Direction::Right => screen_w / 5,
        };
        match swipe_dir {
            Direction::Right => ecx > cx - margin,
            Direction::Left => ecx < cx + margin,
            Direction::Up => ecy < cy + margin,
            Direction::Down => ecy > cy - margin,
        }
    }

    pub fn distance_to(&self, other: &Bounds) -> f64 {
        let (x1, y1) = self.center();
        let (x2, y2) = other.center();
        let dx = f64::from(x1 - x2);
        let dy = f64::from(y1 - y2);
        (dx * dx + dy * dy).sqrt()
    }
}

/// Shared mutable layout node mirroring `FakeLayoutElement.kt`.
pub type FakeElementRef = Rc<RefCell<FakeLayoutElement>>;

/// Click handler invoked when a tap lands inside an element's bounds.
/// May return `Err(...)` to simulate test-level or infrastructure errors on tap.
pub type OnClickFn = Box<dyn FnMut(&FakeElementRef) -> Result<()>>;

/// Dynamic text generator mirroring `FakeLayoutElement.mutatingText`.
pub type MutatingTextFn = Box<dyn FnMut() -> String>;

pub struct FakeLayoutElement {
    pub id: Option<String>,
    pub text: Option<String>,
    pub hint_text: Option<String>,
    pub accessibility_text: Option<String>,
    pub error_text: Option<String>,
    pub bounds: Option<Bounds>,
    pub clickable: bool,
    pub enabled: Option<bool>,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    pub focused: Option<bool>,
    pub color: [u8; 4],
    pub matches_css_filter: Option<String>,
    pub on_click: Option<OnClickFn>,
    pub mutating_text: Option<MutatingTextFn>,
    pub children: Vec<FakeElementRef>,
}

impl Default for FakeLayoutElement {
    fn default() -> Self {
        Self {
            id: None,
            text: None,
            hint_text: None,
            accessibility_text: None,
            error_text: None,
            bounds: None,
            clickable: false,
            enabled: Some(true),
            selected: Some(false),
            checked: Some(false),
            focused: Some(false),
            color: [0, 0, 0, 255],
            matches_css_filter: None,
            on_click: None,
            mutating_text: None,
            children: Vec::new(),
        }
    }
}

impl FakeLayoutElement {
    pub fn new_ref() -> FakeElementRef {
        Rc::new(RefCell::new(Self::default()))
    }

    pub fn add_child(parent: &FakeElementRef, configure: impl FnOnce(&mut FakeLayoutElement)) -> FakeElementRef {
        let child = Self::new_ref();
        configure(&mut child.borrow_mut());
        parent.borrow_mut().children.push(Rc::clone(&child));
        child
    }

    fn snapshot(&mut self, next_uid: &mut u64) -> TreeNode {
        let uid = *next_uid;
        *next_uid += 1;
        let resolved_text = if let Some(f) = self.mutating_text.as_mut() { Some(f()) } else { self.text.clone() };
        let children = self.children.iter().map(|c| c.borrow_mut().snapshot(next_uid)).collect();
        TreeNode {
            uid,
            id: self.id.clone(),
            has_text_attr: resolved_text.is_some(),
            text: resolved_text,
            hint_text: self.hint_text.clone(),
            accessibility_text: self.accessibility_text.clone(),
            error_text: self.error_text.clone(),
            bounds: self.bounds,
            clickable: self.clickable,
            enabled: self.enabled,
            selected: self.selected,
            checked: self.checked,
            focused: self.focused,
            matches_css_filter: self.matches_css_filter.clone(),
            ignore_bounds_filtering: false,
            children,
        }
    }

    fn draw(&self, img: &mut RgbaImage, dpr_x: f64, dpr_y: f64) {
        if let Some(b) = self.bounds {
            let (w, h) = img.dimensions();
            let x0 = ((f64::from(b.left) * dpr_x).round() as i32).clamp(0, w as i32) as u32;
            let y0 = ((f64::from(b.top) * dpr_y).round() as i32).clamp(0, h as i32) as u32;
            let x1 = ((f64::from(b.right) * dpr_x).round() as i32).clamp(0, w as i32) as u32;
            let y1 = ((f64::from(b.bottom) * dpr_y).round() as i32).clamp(0, h as i32) as u32;
            let px = Rgba(self.color);
            for y in y0..y1 {
                for x in x0..x1 {
                    if x == x0 || x + 1 == x1 || y == y0 || y + 1 == y1 {
                        img.put_pixel(x, y, px);
                    }
                }
            }
        }
        for child in &self.children {
            child.borrow().draw(img, dpr_x, dpr_y);
        }
    }
}

fn dispatch_click(node: &FakeElementRef, x: i32, y: i32) -> Result<bool> {
    let children: Vec<FakeElementRef> = node.borrow().children.clone();
    for child in &children {
        if dispatch_click(child, x, y)? {
            return Ok(true);
        }
    }
    let contains = node.borrow().bounds.is_some_and(|b| b.contains(x, y));
    if contains {
        // Temporarily take `on_click` out so the handler can freely borrow/mutate
        // `node` or any sibling element without triggering a RefCell double-borrow.
        let cb = node.borrow_mut().on_click.take();
        if let Some(mut f) = cb {
            let res = f(node);
            if node.borrow().on_click.is_none() {
                node.borrow_mut().on_click = Some(f);
            }
            res?;
        }
        return Ok(true);
    }
    Ok(false)
}

fn translate_tree(node: &FakeElementRef, dx: i32, dy: i32) {
    {
        let mut n = node.borrow_mut();
        if let Some(b) = n.bounds {
            n.bounds = Some(b.translate(dx, dy));
        }
    }
    let children: Vec<FakeElementRef> = node.borrow().children.clone();
    for c in &children {
        translate_tree(c, dx, dy);
    }
}

/// Recorded interaction events mirroring `FakeDriver.Event` in Maestro.
#[derive(Clone, Debug, PartialEq)]
pub enum FakeEvent {
    Tap { x: i32, y: i32 },
    LongPress { x: i32, y: i32 },
    Scroll,
    BackPress,
    HideKeyboard,
    InputText(String),
    Swipe { start: (i32, i32), end: (i32, i32), duration_ms: i64 },
    SwipeWithDirection { direction: Direction, duration_ms: i64 },
    SwipeElementWithDirection { point: (i32, i32), direction: Direction, duration_ms: i64 },
    LaunchApp { app_id: String, launch_arguments: ValueMap },
    StopApp(String),
    KillApp(String),
    ClearState(String),
    OpenLink { link: String, app_id: Option<String>, auto_verify: bool },
    OpenBrowser { link: String },
    PressKey(KeyCode),
    SetOrientation(String),
    TakeScreenshot,
    ClearKeychain,
    SetLocation { latitude: f64, longitude: f64 },
    EraseAllText,
    SetPermissions { app_id: String, permissions: BTreeMap<String, String> },
    AddMedia(String),
    StartRecording,
    StopRecording,
}

/// Immutable UI hierarchy snapshot for selector evaluation (shared with the
/// Android native lane, which feeds it UiAutomator hierarchies).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub uid: u64,
    pub id: Option<String>,
    pub text: Option<String>,
    /// Maestro's `Filters.hasText` (the `text` trait) tests whether the node's
    /// `text` attribute *exists*, not whether it is non-empty. UiAutomator dumps
    /// give every node a `text` attribute (empty for a Flutter label that lives
    /// only in `content-desc`), but `text` above is `None` for empty values; this
    /// flag keeps the distinction.
    pub has_text_attr: bool,
    pub hint_text: Option<String>,
    pub accessibility_text: Option<String>,
    pub error_text: Option<String>,
    pub bounds: Option<Bounds>,
    pub clickable: bool,
    pub enabled: Option<bool>,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    pub focused: Option<bool>,
    pub matches_css_filter: Option<String>,
    /// `ignoreBoundsFiltering` (Android toasts are never pruned).
    pub ignore_bounds_filtering: bool,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// Deepest, last-painted node containing (`x`, `y`): `ViewHierarchy.getElementAt`
    /// (children are searched back to front, descendants before the node itself).
    pub(crate) fn element_at(&self, x: i32, y: i32) -> Option<&TreeNode> {
        for child in self.children.iter().rev() {
            if !child.children.is_empty()
                && let Some(deeper) = child.element_at(x, y)
            {
                return Some(deeper);
            }
            if child.bounds.is_some_and(|b| b.contains(x, y)) {
                return Some(child);
            }
        }
        None
    }

    /// `ViewHierarchy.isVisible`: `node` is the topmost element at its own centre,
    /// i.e. nothing is drawn over it (a node with children is hidden by its own
    /// descendants, exactly as in Maestro).
    pub(crate) fn is_topmost(&self, node: &TreeNode) -> bool {
        let Some(bounds) = node.bounds else {
            return false;
        };
        let (x, y) = bounds.center();
        self.element_at(x, y).is_some_and(|n| n.uid == node.uid)
    }

    pub(crate) fn aggregate(&self) -> Vec<TreeNode> {
        let mut out = vec![self.clone()];
        for child in &self.children {
            out.extend(child.aggregate());
        }
        out
    }

    pub(crate) fn filter_out_of_bounds(&self, width: i32, height: i32) -> Option<TreeNode> {
        // `ViewHierarchy.kt`: nodes flagged `ignoreBoundsFiltering` are kept as is.
        if self.ignore_bounds_filtering {
            return Some(self.clone());
        }
        let filtered: Vec<TreeNode> =
            self.children.iter().filter_map(|c| c.filter_out_of_bounds(width, height)).collect();
        let vis = self.bounds.map(|b| b.visible_percentage(width, height)).unwrap_or(0.0);
        if vis < 0.1 && filtered.is_empty() {
            return None;
        }
        let mut copy = self.clone();
        copy.children = filtered;
        Some(copy)
    }

    pub(crate) fn resolved_copy_text(&self) -> Option<String> {
        // Matches `Orchestra.resolveText` (`Orchestra.kt:1854-1867`).
        if let Some(t) = &self.text
            && !t.is_empty()
        {
            return Some(t.clone());
        }
        if let Some(t) = &self.hint_text
            && !t.is_empty()
        {
            return Some(t.clone());
        }
        if let Some(t) = &self.accessibility_text
            && !t.is_empty()
        {
            return Some(t.clone());
        }
        if let Some(t) = &self.error_text
            && !t.is_empty()
        {
            return Some(t.clone());
        }
        self.accessibility_text.clone()
    }

    /// Whether `uid` is this node or one of its descendants.
    pub(crate) fn contains_uid(&self, uid: u64) -> bool {
        self.uid == uid || self.children.iter().any(|c| c.contains_uid(uid))
    }

    /// Structural fingerprint ignoring UIDs, used by `retryTapIfNoChange` and `settle`.
    /// Includes `accessibility_text` (`content-desc` on Android) and `hint_text` because
    /// Flutter semantics labels live in `content-desc` while `text` stays empty.
    pub(crate) fn structure_signature(&self) -> String {
        let mut s = format!(
            "({:?},{:?},{:?},{:?},{:?},{},{:?},{:?},{:?},{:?};",
            self.id,
            self.text,
            self.accessibility_text,
            self.hint_text,
            self.bounds,
            self.clickable,
            self.enabled,
            self.selected,
            self.checked,
            self.focused
        );
        for c in &self.children {
            s.push_str(&c.structure_signature());
        }
        s.push(')');
        s
    }
}

/// Optional hook called before reading the hierarchy (allows tests to simulate
/// deferred elements, transient hierarchy flips, or scrolling momentum).
pub type HierarchyHook = Box<dyn FnMut(usize, &FakeElementRef)>;

/// In-memory test double implementing [`FlowDriver`] with Maestro 2.12.1 parity.
pub struct FakeDriver {
    pub info: DeviceInfo,
    pub root: FakeElementRef,
    pub installed_apps: HashSet<String>,
    pub events: Vec<FakeEvent>,
    pub current_text: String,
    pub airplane_mode: bool,
    pub dark_mode: bool,
    pub keyboard_remains_visible: bool,
    pub hide_keyboard_called: bool,
    /// Test seam: error thrown by `back_press()` (e.g. `TransportError` or generic error).
    pub command_error: Option<Box<dyn FnMut() -> anyhow::Error>>,
    /// Test seam: error thrown by `launch_app()`.
    pub launch_error: Option<Box<dyn FnMut() -> anyhow::Error>>,
    /// Test seam: error thrown by `start_recording()`.
    pub start_recording_error: Option<String>,
    /// Test seam: invoked on every hierarchy read (`read_count`, `&root`).
    pub on_hierarchy_read: Option<HierarchyHook>,
    pub hierarchy_read_count: usize,
    /// Last `appId` passed via `tap_on_element` or `open_link`.
    pub last_tap_app_id: Option<String>,
    pub last_open_link_app_id: Option<String>,
    recording_active: bool,
    /// Optional custom screenshot override (for testing `assertScreenshot`).
    pub custom_screenshot: Option<DynamicImage>,
    /// Test seam (F7): one batch returned per `take_app_issues()` call, in order.
    pub app_issue_batches: std::collections::VecDeque<crate::app_health::IssueBatch>,
    /// Test seam (F7): returned by `drain_app_issues()` at flow end.
    pub trailing_app_issues: crate::app_health::IssueBatch,
    /// Test seam (F7): returned by `diagnose_crash()` after a transport error.
    pub crash: Option<Value>,
    /// Test seam (F8b): one batch returned per `trace_step()` call, in order.
    pub trace_batches: std::collections::VecDeque<crate::trace::TraceBatch>,
    /// Test seam (F8b): returned by `trace_drain()` at flow end.
    pub trailing_trace: crate::trace::TraceBatch,
    /// Test seam (F8b): number of `trace_step()` / `trace_drain()` calls.
    pub trace_calls: usize,
    /// Test seam (F8b): reported by `trace_http_status()`.
    pub trace_http: &'static str,
}

impl Default for FakeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDriver {
    /// Creates a `FakeDriver` with Maestro's exact default `DeviceInfo`
    /// (`Platform.IOS`, 1080×1920 pixels, 540×960 grid).
    pub fn new() -> Self {
        Self {
            info: DeviceInfo {
                platform: Platform::Ios,
                width_pixels: 1080,
                height_pixels: 1920,
                width_grid: 540,
                height_grid: 960,
            },
            root: FakeLayoutElement::new_ref(),
            installed_apps: HashSet::new(),
            events: Vec::new(),
            current_text: String::new(),
            airplane_mode: false,
            dark_mode: false,
            keyboard_remains_visible: false,
            hide_keyboard_called: false,
            command_error: None,
            launch_error: None,
            start_recording_error: None,
            on_hierarchy_read: None,
            hierarchy_read_count: 0,
            last_tap_app_id: None,
            last_open_link_app_id: None,
            recording_active: false,
            custom_screenshot: None,
            app_issue_batches: std::collections::VecDeque::new(),
            trailing_app_issues: crate::app_health::IssueBatch::default(),
            crash: None,
            trace_batches: std::collections::VecDeque::new(),
            trailing_trace: crate::trace::TraceBatch::default(),
            trace_calls: 0,
            trace_http: "enabled",
        }
    }

    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.info.platform = platform;
        self
    }

    pub fn add_installed_app(&mut self, app_id: impl Into<String>) {
        self.installed_apps.insert(app_id.into());
    }

    pub fn add_element(&mut self, configure: impl FnOnce(&mut FakeLayoutElement)) -> FakeElementRef {
        FakeLayoutElement::add_child(&self.root, configure)
    }

    pub fn set_command_transport_error(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        self.command_error = Some(Box::new(move || TransportError(m.clone()).into()));
    }

    pub fn set_command_runtime_error(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        self.command_error = Some(Box::new(move || anyhow::anyhow!("{}", m)));
    }

    pub fn set_launch_transport_error(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        self.launch_error = Some(Box::new(move || TransportError(m.clone()).into()));
    }

    pub fn event_count(&self, target: &FakeEvent) -> usize {
        self.events.iter().filter(|e| *e == target).count()
    }

    pub fn has_event(&self, target: &FakeEvent) -> bool {
        self.events.contains(target)
    }

    fn snapshot_hierarchy(&mut self) -> TreeNode {
        self.hierarchy_read_count += 1;
        if let Some(mut hook) = self.on_hierarchy_read.take() {
            hook(self.hierarchy_read_count, &self.root);
            if self.on_hierarchy_read.is_none() {
                self.on_hierarchy_read = Some(hook);
            }
        }
        let mut next_uid = 1;
        let raw = self.root.borrow_mut().snapshot(&mut next_uid);
        raw.filter_out_of_bounds(self.info.width_grid, self.info.height_grid).unwrap_or(raw)
    }

    fn find_element_once(&mut self, selector: &ElementSelector) -> Result<TreeNode> {
        let root = self.snapshot_hierarchy();
        find_in_tree(&root, selector)?.ok_or_else(|| {
            MaestroError::element_not_found(format!("Element not found: {}", describe_sel(selector))).into()
        })
    }

    fn find_element_with_poll(&mut self, selector: &ElementSelector, timeout_ms: u64) -> Result<TreeNode> {
        // First immediate attempt (also validates numeric fields like `index`).
        match self.find_element_once(selector) {
            Ok(node) => return Ok(node),
            Err(e) if !is_element_not_found(&e) => return Err(e),
            Err(_) => {}
        }
        // When `timeout_ms > 0` or a dynamic hierarchy hook is installed, poll a
        // bounded number of fresh hierarchy snapshots (supports deferred child/parent
        // tests like Cases 149 and 150 deterministically without sleeping).
        let max_attempts = if timeout_ms > 0 || self.on_hierarchy_read.is_some() { 10 } else { 0 };
        for _ in 0..max_attempts {
            match self.find_element_once(selector) {
                Ok(node) => return Ok(node),
                Err(e) if !is_element_not_found(&e) => return Err(e),
                Err(_) => {}
            }
        }
        Err(MaestroError::element_not_found(format!("Element not found: {}", describe_sel(selector))).into())
    }

    fn render_screen_image(&self) -> DynamicImage {
        if let Some(img) = &self.custom_screenshot {
            return img.clone();
        }
        let w = self.info.width_pixels.max(1) as u32;
        let h = self.info.height_pixels.max(1) as u32;
        let mut img = RgbaImage::from_pixel(w, h, Rgba([255, 255, 255, 255]));
        let dpr_x = f64::from(self.info.width_pixels) / f64::from(self.info.width_grid.max(1));
        let dpr_y = f64::from(self.info.height_pixels) / f64::from(self.info.height_grid.max(1));
        self.root.borrow().draw(&mut img, dpr_x, dpr_y);
        DynamicImage::ImageRgba8(img)
    }

    fn perform_tap_at(&mut self, x: i32, y: i32, long_press: bool, repeat: i32) -> Result<()> {
        let count = repeat.max(1);
        for _ in 0..count {
            if long_press {
                self.events.push(FakeEvent::LongPress { x, y });
            } else {
                dispatch_click(&self.root, x, y)?;
                self.events.push(FakeEvent::Tap { x, y });
            }
        }
        Ok(())
    }
}

impl FlowDriver for FakeDriver {
    fn take_app_issues(&mut self) -> crate::app_health::IssueBatch {
        self.app_issue_batches.pop_front().unwrap_or_default()
    }

    fn drain_app_issues(&mut self) -> crate::app_health::IssueBatch {
        std::mem::take(&mut self.trailing_app_issues)
    }

    fn diagnose_crash(&mut self) -> Option<Value> {
        self.crash.take()
    }

    fn trace_step(&mut self) -> crate::trace::TraceBatch {
        self.trace_calls += 1;
        let mut batch = self.trace_batches.pop_front().unwrap_or_default();
        batch.lane.get_or_insert("flutter");
        batch
    }

    fn trace_drain(&mut self) -> crate::trace::TraceBatch {
        self.trace_calls += 1;
        std::mem::take(&mut self.trailing_trace)
    }

    fn trace_http_status(&self) -> &'static str {
        self.trace_http
    }

    fn device_info(&self) -> DeviceInfo {
        self.info.clone()
    }

    fn launch_app(&mut self, req: &LaunchRequest) -> Result<Value> {
        if let Some(err_fn) = self.launch_error.as_mut() {
            return Err(err_fn());
        }
        if req.clear_keychain {
            self.events.push(FakeEvent::ClearKeychain);
        }
        if req.clear_state && self.installed_apps.contains(&req.app_id) {
            self.events.push(FakeEvent::ClearState(req.app_id.clone()));
        }
        self.events
            .push(FakeEvent::SetPermissions { app_id: req.app_id.clone(), permissions: req.permissions.clone() });
        if req.stop_app {
            self.events.push(FakeEvent::StopApp(req.app_id.clone()));
        }
        if !self.installed_apps.contains(&req.app_id) {
            return Err(MaestroError::unable_to_launch(format!("App {} is not installed", req.app_id)).into());
        }
        self.events
            .push(FakeEvent::LaunchApp { app_id: req.app_id.clone(), launch_arguments: req.launch_arguments.clone() });
        Ok(json!({ "appId": req.app_id }))
    }

    fn stop_app(&mut self, app_id: &str) -> Result<Value> {
        self.events.push(FakeEvent::StopApp(app_id.to_string()));
        Ok(Value::Null)
    }

    fn kill_app(&mut self, app_id: &str) -> Result<Value> {
        self.events.push(FakeEvent::KillApp(app_id.to_string()));
        Ok(Value::Null)
    }

    fn clear_state(&mut self, app_id: &str) -> Result<Value> {
        if self.installed_apps.contains(app_id) {
            self.events.push(FakeEvent::ClearState(app_id.to_string()));
        }
        self.events.push(FakeEvent::SetPermissions {
            app_id: app_id.to_string(),
            permissions: BTreeMap::from([("all".to_string(), "unset".to_string())]),
        });
        Ok(Value::Null)
    }

    fn clear_keychain(&mut self) -> Result<Value> {
        self.events.push(FakeEvent::ClearKeychain);
        Ok(Value::Null)
    }

    fn set_permissions(&mut self, app_id: &str, permissions: &BTreeMap<String, String>) -> Result<Value> {
        self.events.push(FakeEvent::SetPermissions { app_id: app_id.to_string(), permissions: permissions.clone() });
        Ok(Value::Null)
    }

    fn tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value> {
        self.last_tap_app_id = req.app_id.clone();
        let node = self.find_element_with_poll(&req.selector, req.timeout_ms)?;
        let bounds = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));

        let (tap_x, tap_y) = if let Some(rel) = &req.relative_point {
            calculate_element_relative_point(bounds, rel)?
        } else {
            bounds.center()
        };

        let repeat_count = req.repeat.as_ref().map(|r| r.repeat).unwrap_or(1);
        let sig_before =
            if req.retry_if_no_change { Some(self.snapshot_hierarchy().structure_signature()) } else { None };
        let img_before = if req.retry_if_no_change { Some(self.render_screen_image().to_rgba8()) } else { None };

        self.perform_tap_at(tap_x, tap_y, req.long_press, repeat_count)?;

        if req.retry_if_no_change {
            let sig_after = self.snapshot_hierarchy().structure_signature();
            let hierarchy_changed = sig_before.as_deref() != Some(sig_after.as_str());
            let screenshot_changed = if !hierarchy_changed {
                let img_after = self.render_screen_image().to_rgba8();
                img_before.as_ref() != Some(&img_after)
            } else {
                true
            };
            if !hierarchy_changed && !screenshot_changed {
                self.perform_tap_at(tap_x, tap_y, req.long_press, repeat_count)?;
            }
        }

        Ok(json!({ "x": tap_x, "y": tap_y }))
    }

    fn tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value> {
        let (tap_x, tap_y) =
            crate::backend::resolve_screen_point(&req.point, self.info.width_grid, self.info.height_grid)?;
        let repeat_count = req.repeat.as_ref().map(|r| r.repeat).unwrap_or(1);
        self.perform_tap_at(tap_x, tap_y, req.long_press, repeat_count)?;
        Ok(json!({ "x": tap_x, "y": tap_y }))
    }

    fn assert_visible(&mut self, selector: &ElementSelector, timeout_ms: u64, _optional: bool) -> Result<Value> {
        let node = self.find_element_with_poll(selector, timeout_ms)?;
        Ok(json!({ "text": node.text, "id": node.id }))
    }

    fn assert_not_visible(&mut self, selector: &ElementSelector, _timeout_ms: u64, _optional: bool) -> Result<Value> {
        match self.find_element_once(selector) {
            Ok(_) => Err(MaestroError::assertion(format!("Element is visible: {}", describe_sel(selector))).into()),
            Err(e) if is_element_not_found(&e) => Ok(Value::Null),
            Err(e) => Err(e),
        }
    }

    fn find_element_bounds(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        _optional: bool,
    ) -> Result<[i64; 4]> {
        let node = self.find_element_with_poll(selector, timeout_ms)?;
        let b = node.bounds.ok_or_else(|| MaestroError::assertion("cropOn element has no bounds"))?;
        // Scale from grid coordinates to screenshot pixels matching `Maestro.kt` / Case 138.
        let dpr_x = f64::from(self.info.width_pixels) / f64::from(self.info.width_grid.max(1));
        let dpr_y = f64::from(self.info.height_pixels) / f64::from(self.info.height_grid.max(1));
        Ok([
            (f64::from(b.left) * dpr_x).round() as i64,
            (f64::from(b.top) * dpr_y).round() as i64,
            (f64::from(b.right) * dpr_x).round() as i64,
            (f64::from(b.bottom) * dpr_y).round() as i64,
        ])
    }

    fn input_text(&mut self, text: &str) -> Result<Value> {
        self.current_text.push_str(text);
        self.events.push(FakeEvent::InputText(text.to_string()));
        Ok(json!({ "text": text }))
    }

    fn erase_text(&mut self, characters: i32) -> Result<Value> {
        if characters == 50 {
            self.current_text.clear();
        } else if characters > 0 {
            let count = characters as usize;
            let len = self.current_text.chars().count();
            self.current_text = self.current_text.chars().take(len.saturating_sub(count)).collect();
        }
        self.events.push(FakeEvent::EraseAllText);
        Ok(Value::Null)
    }

    fn copy_text_from(
        &mut self,
        selector: &ElementSelector,
        timeout_ms: u64,
        _optional: bool,
    ) -> Result<(String, Value)> {
        let node = self.find_element_with_poll(selector, timeout_ms)?;
        let text = node
            .resolved_copy_text()
            .ok_or_else(|| MaestroError::unable_to_copy("Element does not contain text to copy"))?;
        Ok((text.clone(), json!({ "text": text })))
    }

    fn back_press(&mut self) -> Result<Value> {
        if let Some(err_fn) = self.command_error.as_mut() {
            return Err(err_fn());
        }
        self.events.push(FakeEvent::BackPress);
        Ok(Value::Null)
    }

    fn hide_keyboard(&mut self) -> Result<Value> {
        self.hide_keyboard_called = true;
        self.events.push(FakeEvent::HideKeyboard);
        if self.keyboard_remains_visible {
            return Err(MaestroError::hide_keyboard("Couldn't hide the keyboard.").into());
        }
        Ok(Value::Null)
    }

    fn press_key(&mut self, code: KeyCode) -> Result<Value> {
        if code == KeyCode::Backspace {
            self.current_text.pop();
        }
        self.events.push(FakeEvent::PressKey(code));
        Ok(Value::Null)
    }

    fn scroll(&mut self) -> Result<Value> {
        self.events.push(FakeEvent::Scroll);
        Ok(Value::Null)
    }

    fn swipe(&mut self, req: &SwipeRequest) -> Result<Value> {
        if let (Some(sel), Some(dir)) = (&req.element_selector, req.direction) {
            let node = self.find_element_with_poll(sel, req.timeout_ms)?;
            let bounds = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
            let point = if let Some(rel) = &req.relative_point {
                calculate_element_relative_point(bounds, rel)?
            } else {
                bounds.center()
            };
            let (dx, dy) = match dir {
                Direction::Up => (0, -300),
                Direction::Down => (0, 300),
                Direction::Right => (-300, 0),
                Direction::Left => (300, 0),
            };
            translate_tree(&self.root, dx, dy);
            self.events.push(FakeEvent::SwipeElementWithDirection {
                point,
                direction: dir,
                duration_ms: req.duration_ms,
            });
            return Ok(Value::Null);
        }

        if let (Some(sr), Some(er)) = (&req.start_relative, &req.end_relative) {
            let (sx, sy, sp) = parse_point(sr)?;
            let (ex, ey, ep) = parse_point(er)?;
            let start = if sp { (self.info.width_grid * sx / 100, self.info.height_grid * sy / 100) } else { (sx, sy) };
            let end = if ep { (self.info.width_grid * ex / 100, self.info.height_grid * ey / 100) } else { (ex, ey) };
            self.events.push(FakeEvent::Swipe { start, end, duration_ms: req.duration_ms });
            return Ok(Value::Null);
        }

        if let Some(dir) = req.direction {
            self.events.push(FakeEvent::SwipeWithDirection { direction: dir, duration_ms: req.duration_ms });
            return Ok(Value::Null);
        }

        if let (Some(s), Some(e)) = (&req.start_point, &req.end_point) {
            self.events.push(FakeEvent::Swipe { start: (s.x, s.y), end: (e.x, e.y), duration_ms: req.duration_ms });
            return Ok(Value::Null);
        }

        Err(MaestroError::invalid_command("Illegal arguments for swiping").into())
    }

    fn scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value> {
        let swipe_dir = match req.direction {
            Direction::Down => Direction::Up,
            Direction::Up => Direction::Down,
            Direction::Right => Direction::Left,
            Direction::Left => Direction::Right,
        };
        let vis_threshold = f64::from(req.visibility_percentage) / 100.0;
        let center_point = (self.info.width_grid / 2, self.info.height_grid / 2);
        let mut retry_center_count = 0;
        let max_retry_center = 4;

        // Deterministic bounded scroll iterations (mirrors `FakeTimer` fast execution in Maestro tests).
        let max_steps = 20;
        for _ in 0..max_steps {
            match self.find_element_once(&req.selector) {
                Ok(node) => {
                    let b = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
                    let vis = b.visible_percentage(self.info.width_grid, self.info.height_grid);
                    if req.center_element && vis > 0.1 && retry_center_count <= max_retry_center {
                        if b.is_near_screen_center(swipe_dir, self.info.width_grid, self.info.height_grid) {
                            return Ok(Value::Null);
                        }
                        retry_center_count += 1;
                    } else if vis >= vis_threshold {
                        return Ok(Value::Null);
                    }
                }
                Err(e) if !is_element_not_found(&e) => return Err(e),
                Err(_) => {}
            }

            let (dx, dy) = match swipe_dir {
                Direction::Up => (0, -300),
                Direction::Down => (0, 300),
                Direction::Right => (-300, 0),
                Direction::Left => (300, 0),
            };
            translate_tree(&self.root, dx, dy);
            self.events.push(FakeEvent::SwipeElementWithDirection {
                point: center_point,
                direction: swipe_dir,
                duration_ms: req.duration_ms as i64,
            });
        }

        Err(MaestroError::element_not_found(format!("No visible element found: {}", describe_sel(&req.selector)))
            .into())
    }

    fn wait_for_animation_to_end(&mut self, _timeout_ms: Option<u64>) -> Result<Value> {
        // Matches Maestro `ScreenshotUtils.waitUntilScreenIsStatic`: captures two
        // consecutive screenshots and sees they match (`IntegrationTest` Case 069).
        self.events.push(FakeEvent::TakeScreenshot);
        self.events.push(FakeEvent::TakeScreenshot);
        Ok(Value::Null)
    }

    fn screenshot_bytes(&mut self) -> Result<Vec<u8>> {
        self.events.push(FakeEvent::TakeScreenshot);
        let img = self.render_screen_image();
        screenshot::encode_png(&img)
    }

    fn open_link(&mut self, link: &str, app_id: Option<&str>, auto_verify: bool, browser: bool) -> Result<Value> {
        self.last_open_link_app_id = app_id.map(str::to_string);
        if browser {
            self.events.push(FakeEvent::OpenBrowser { link: link.to_string() });
        } else {
            self.events.push(FakeEvent::OpenLink {
                link: link.to_string(),
                app_id: app_id.map(str::to_string),
                auto_verify,
            });
        }
        Ok(Value::Null)
    }

    fn set_location(&mut self, latitude: f64, longitude: f64) -> Result<Value> {
        self.events.push(FakeEvent::SetLocation { latitude, longitude });
        Ok(Value::Null)
    }

    fn set_orientation(&mut self, orientation: &str) -> Result<Value> {
        self.events.push(FakeEvent::SetOrientation(orientation.to_string()));
        Ok(Value::Null)
    }

    fn add_media(&mut self, media_paths: &[String]) -> Result<Value> {
        for p in media_paths {
            self.events.push(FakeEvent::AddMedia(p.clone()));
        }
        Ok(Value::Null)
    }

    fn start_recording(&mut self, out_path: &Path) -> Result<Value> {
        if let Some(err) = &self.start_recording_error {
            return Err(anyhow::anyhow!("{}", err));
        }
        if !self.recording_active {
            if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(out_path, b"Screen recording")?;
            self.recording_active = true;
            self.events.push(FakeEvent::StartRecording);
        }
        Ok(json!({ "path": out_path }))
    }

    fn stop_recording(&mut self) -> Result<Value> {
        if self.recording_active {
            self.recording_active = false;
            self.events.push(FakeEvent::StopRecording);
        }
        Ok(Value::Null)
    }

    fn is_airplane_mode_enabled(&mut self) -> Result<bool> {
        Ok(self.airplane_mode)
    }

    fn set_airplane_mode(&mut self, enabled: bool) -> Result<Value> {
        self.airplane_mode = enabled;
        Ok(json!({ "airplaneMode": enabled }))
    }

    fn is_dark_mode_enabled(&mut self) -> Result<bool> {
        Ok(self.dark_mode)
    }

    fn set_dark_mode(&mut self, enabled: bool) -> Result<Value> {
        self.dark_mode = enabled;
        Ok(json!({ "darkMode": enabled }))
    }

    fn capture_failure_hierarchy(&mut self) -> Option<(String, TreeNode)> {
        Some(("fake".to_string(), self.snapshot_hierarchy()))
    }

    fn capture_failure_screenshot(&mut self) -> Option<Vec<u8>> {
        let img = self.render_screen_image();
        screenshot::encode_png(&img).ok()
    }
}

/// Exact port of `calculateElementRelativePoint` (`ElementCoordinateUtil.kt:15-39`).
pub fn calculate_element_relative_point(bounds: Bounds, point: &str) -> Result<(i32, i32)> {
    let (px, py, is_percent) = parse_point(point)?;
    if is_percent {
        if !(0..=100).contains(&px) || !(0..=100).contains(&py) {
            return Err(MaestroError::invalid_command(format!(
                "Invalid element-relative point: {point}. Percentages must be between 0 and 100."
            ))
            .into());
        }
        Ok((bounds.left + (bounds.width() * px / 100), bounds.top + (bounds.height() * py / 100)))
    } else {
        if px < 0 || py < 0 || px >= bounds.width() || py >= bounds.height() {
            return Err(MaestroError::invalid_command(format!(
                "Invalid element-relative point: {point}. Coordinates must be within element bounds."
            ))
            .into());
        }
        Ok((bounds.left + px, bounds.top + py))
    }
}

pub(crate) fn is_element_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<MaestroError>().is_some_and(|m| m.kind == crate::backend::MaestroErrorKind::ElementNotFound)
}

pub(crate) fn describe_sel(s: &ElementSelector) -> String {
    match (&s.text_regex, &s.id_regex) {
        (Some(t), _) => format!("\"{t}\""),
        (_, Some(i)) => format!("id: {i}"),
        _ => serde_json::to_string(s).unwrap_or_default(),
    }
}

// --- Full 1:1 Maestro `Filters.kt` + `Orchestra.buildFilter` implementation ---

pub(crate) fn find_in_tree(root: &TreeNode, selector: &ElementSelector) -> Result<Option<TreeNode>> {
    if let Some(parent_sel) = &selector.child_of {
        let Some(parent_node) = resolve_parent_hierarchy(parent_sel, root)? else {
            return Ok(None);
        };
        let mut child_sel = selector.clone();
        child_sel.child_of = None;
        let matches = apply_selector(&parent_node.aggregate(), &child_sel)?;
        return Ok(matches.into_iter().next());
    }
    let matches = apply_selector(&root.aggregate(), selector)?;
    Ok(matches.into_iter().next())
}

fn resolve_parent_hierarchy(selector: &ElementSelector, root: &TreeNode) -> Result<Option<TreeNode>> {
    let scope_node = if let Some(grandparent) = &selector.child_of {
        match resolve_parent_hierarchy(grandparent, root)? {
            Some(gp) => gp,
            None => return Ok(None),
        }
    } else {
        root.clone()
    };
    let mut own = selector.clone();
    own.child_of = None;
    let matches = apply_selector(&scope_node.aggregate(), &own)?;
    Ok(matches.into_iter().next())
}

fn apply_selector(nodes: &[TreeNode], selector: &ElementSelector) -> Result<Vec<TreeNode>> {
    // 1. Basic filters (wrapped in `deepestMatchingElement`, matching `Orchestra.kt:1755-1759`).
    let has_basic = selector.text_regex.is_some()
        || selector.id_regex.is_some()
        || selector.size.is_some()
        || selector.traits.as_ref().is_some_and(|t| !t.is_empty())
        || selector.enabled.is_some()
        || selector.selected.is_some()
        || selector.checked.is_some()
        || selector.focused.is_some()
        || selector.css.is_some();

    let mut candidate_sets: Vec<Vec<TreeNode>> = Vec::new();
    if has_basic {
        let compiled = CompiledBasicSelector::compile(selector);
        let basic_matched = deepest_matching_elements(nodes, |n| matches_basic(n, &compiled));
        candidate_sets.push(basic_matched);
    } else {
        candidate_sets.push(nodes.to_vec());
    }

    // 2. Relative filters (`below`, `above`, `leftOf`, `rightOf`, `containsChild`, `containsDescendants`).
    if let Some(other) = &selector.below {
        candidate_sets.push(filter_relative(nodes, other, |a, b| a.top > b.top)?);
    }
    if let Some(other) = &selector.above {
        candidate_sets.push(filter_relative(nodes, other, |a, b| a.top < b.top)?);
    }
    if let Some(other) = &selector.left_of {
        candidate_sets.push(filter_relative(nodes, other, |a, b| a.left < b.left)?);
    }
    if let Some(other) = &selector.right_of {
        candidate_sets.push(filter_relative(nodes, other, |a, b| a.left > b.left)?);
    }
    if let Some(child_sel) = &selector.contains_child {
        let matching_children: HashSet<u64> = apply_selector(nodes, child_sel)?.into_iter().map(|n| n.uid).collect();
        let matched =
            nodes.iter().filter(|n| n.children.iter().any(|c| matching_children.contains(&c.uid))).cloned().collect();
        candidate_sets.push(matched);
    }
    if let Some(desc_sels) = &selector.contains_descendants {
        let mut matched = Vec::new();
        for n in nodes {
            let mut all_ok = true;
            for dsel in desc_sels {
                let mut found = false;
                for c in &n.children {
                    if subtree_matches(c, dsel)? {
                        found = true;
                        break;
                    }
                }
                if !found {
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
                matched.push(n.clone());
            }
        }
        candidate_sets.push(matched);
    }

    // Preserve distance ordering (closest first) whenever any directional
    // relative filter (`below`/`above`/`leftOf`/`rightOf`) is present.
    let has_directional = selector.below.is_some()
        || selector.above.is_some()
        || selector.left_of.is_some()
        || selector.right_of.is_some();

    let mut result = if candidate_sets.is_empty() {
        nodes.to_vec()
    } else if has_directional {
        // Intersect all sets while ordering by the directional relative set (closest first).
        let sets: Vec<HashSet<u64>> = candidate_sets.iter().map(|list| list.iter().map(|n| n.uid).collect()).collect();
        let common: HashSet<u64> =
            sets.into_iter().reduce(|a, b| a.intersection(&b).copied().collect()).unwrap_or_default();
        // Use the last directional list for distance ordering (or sum of distances across all directional filters).
        let dir_lists: Vec<&Vec<TreeNode>> = candidate_sets.iter().skip(1).collect();
        let mut ordered: Vec<TreeNode> = Vec::new();
        let mut seen = HashSet::new();
        // Score each common node by its rank across directional filters (closest first).
        let mut scored: Vec<(usize, TreeNode)> = Vec::new();
        for n in &candidate_sets[0] {
            if common.contains(&n.uid) && seen.insert(n.uid) {
                let rank: usize =
                    dir_lists.iter().map(|l| l.iter().position(|x| x.uid == n.uid).unwrap_or(usize::MAX / 4)).sum();
                scored.push((rank, n.clone()));
            }
        }
        scored.sort_by_key(|(rank, _)| *rank);
        for (_, n) in scored {
            ordered.push(n);
        }
        ordered
    } else {
        let mut iter = candidate_sets.into_iter();
        let first = iter.next().unwrap_or_default();
        let rest: Vec<HashSet<u64>> = iter.map(|l| l.into_iter().map(|n| n.uid).collect()).collect();
        let mut seen = HashSet::new();
        first.into_iter().filter(|n| seen.insert(n.uid) && rest.iter().all(|s| s.contains(&n.uid))).collect()
    };

    // 3. Index or clickableFirst (`Orchestra.kt:1764-1774`).
    if let Some(raw_index) = &selector.index {
        let idx = parse_index(raw_index)?;
        result.sort_by_key(|n| {
            let b = n.bounds.map(|b| (b.top, b.left)).unwrap_or((i32::MAX, i32::MAX));
            (b.0, b.1)
        });
        let len = result.len() as i64;
        let resolved = if idx >= 0 { idx } else { len + idx };
        if resolved < 0 || resolved >= len {
            return Ok(Vec::new());
        }
        return Ok(vec![result.swap_remove(resolved as usize)]);
    }

    // `Filters.clickableFirst`: stable sort putting clickable elements first.
    result.sort_by_key(|n| !n.clickable);
    Ok(result)
}

fn subtree_matches(node: &TreeNode, selector: &ElementSelector) -> Result<bool> {
    if !apply_selector(std::slice::from_ref(node), selector)?.is_empty() {
        return Ok(true);
    }
    for c in &node.children {
        if subtree_matches(c, selector)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn deepest_matching_elements<F: Fn(&TreeNode) -> bool>(nodes: &[TreeNode], pred: F) -> Vec<TreeNode> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for node in nodes {
        collect_deepest(node, &pred, &mut out, &mut seen);
    }
    out
}

fn collect_deepest<F: Fn(&TreeNode) -> bool>(
    node: &TreeNode,
    pred: &F,
    out: &mut Vec<TreeNode>,
    seen: &mut HashSet<u64>,
) -> bool {
    let mut any_child = false;
    for child in &node.children {
        if collect_deepest(child, pred, out, seen) {
            any_child = true;
        }
    }
    if any_child {
        return true;
    }
    if pred(node) {
        if seen.insert(node.uid) {
            out.push(node.clone());
        }
        return true;
    }
    false
}

fn filter_relative(
    nodes: &[TreeNode],
    other_sel: &ElementSelector,
    predicate: impl Fn(Bounds, Bounds) -> bool,
) -> Result<Vec<TreeNode>> {
    let others: Vec<Bounds> = apply_selector(nodes, other_sel)?.into_iter().filter_map(|n| n.bounds).collect();
    let mut pairs: Vec<(f64, TreeNode)> = Vec::new();
    for node in nodes {
        let Some(b) = node.bounds else {
            continue;
        };
        for &ob in &others {
            if predicate(b, ob) {
                pairs.push((b.distance_to(&ob), node.clone()));
            }
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(pairs.into_iter().map(|(_, n)| n).collect())
}

/// Precompiled regexes for basic selector matching so a hierarchy scan compiles
/// each pattern once per `apply_selector` call instead of once per tree node.
struct CompiledBasicSelector<'a> {
    sel: &'a ElementSelector,
    text_re: Option<Regex>,
    id_re: Option<Regex>,
}

impl<'a> CompiledBasicSelector<'a> {
    fn compile(sel: &'a ElementSelector) -> Self {
        Self {
            sel,
            text_re: sel.text_regex.as_deref().and_then(compile_maestro_regex),
            id_re: sel.id_regex.as_deref().and_then(compile_maestro_regex),
        }
    }
}

fn matches_basic(node: &TreeNode, compiled: &CompiledBasicSelector<'_>) -> bool {
    let sel = compiled.sel;
    if let Some(pattern) = &sel.text_regex {
        let re = compiled.text_re.as_ref();
        let text_ok = |val: Option<&str>| -> bool {
            let Some(v) = val else {
                return false;
            };
            let stripped = v.replace('\n', " ");
            regex_full_match(re, pattern, v) || regex_full_match(re, pattern, &stripped)
        };
        let err_ok = node.error_text.as_deref().filter(|v| !v.is_empty()).is_some_and(|v| {
            let stripped = v.replace('\n', " ");
            regex_full_match(re, pattern, v) || regex_full_match(re, pattern, &stripped)
        });
        if !(text_ok(node.text.as_deref())
            || text_ok(node.hint_text.as_deref())
            || text_ok(node.accessibility_text.as_deref())
            || err_ok)
        {
            return false;
        }
    }

    if let Some(pattern) = &sel.id_regex {
        let Some(rid) = node.id.as_deref() else {
            return false;
        };
        let re = compiled.id_re.as_ref();
        let suffix = rid.rsplit('/').next().unwrap_or(rid);
        if !(regex_full_match(re, pattern, rid) || regex_full_match(re, pattern, suffix)) {
            return false;
        }
    }

    if let Some(size) = &sel.size {
        let Some(b) = node.bounds else {
            return false;
        };
        let tol = size.tolerance.unwrap_or(0);
        if let Some(w) = size.width
            && (b.width() - w).abs() > tol
        {
            return false;
        }
        if let Some(h) = size.height
            && (b.height() - h).abs() > tol
        {
            return false;
        }
    }

    if let Some(traits) = &sel.traits {
        for t in traits {
            let ok = match t {
                ElementTrait::Text => node.text.is_some() || node.has_text_attr,
                ElementTrait::LongText => node.text.as_ref().is_some_and(|s| s.chars().count() > 200),
                ElementTrait::Square => node
                    .bounds
                    .is_some_and(|b| b.height() > 0 && (1.0 - (b.width() as f32 / b.height() as f32)).abs() < 0.03),
            };
            if !ok {
                return false;
            }
        }
    }

    if let Some(en) = sel.enabled
        && node.enabled != Some(en)
    {
        return false;
    }
    if let Some(s) = sel.selected
        && node.selected != Some(s)
    {
        return false;
    }
    if let Some(c) = sel.checked
        && node.checked != Some(c)
    {
        return false;
    }
    if let Some(f) = sel.focused
        && node.focused != Some(f)
    {
        return false;
    }
    if let Some(css) = &sel.css
        && node.matches_css_filter.as_deref() != Some(css.as_str())
    {
        return false;
    }

    true
}

/// Compiles a Maestro regex with `IGNORE_CASE | DOT_MATCHES_ALL | MULTILINE` (`(?ism)`),
/// anchored for full match (`\A(?:...)\z`). Validates `pattern` standalone first so
/// unbalanced parentheses like `a)(b` fall back to literal equality (`StringUtils.toRegexSafe`).
fn compile_maestro_regex(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()?;
    Regex::new(&format!(r"(?ism)\A(?:{pattern})\z")).ok()
}

fn regex_full_match(re: Option<&Regex>, literal: &str, value: &str) -> bool {
    if value == literal {
        return true;
    }
    re.is_some_and(|r| r.is_match(value).unwrap_or(false))
}
