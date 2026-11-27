//! The shim's accessibility JSON (`{"roots":[...]}`, `AXPTranslator` nodes) mapped to the runner's
//! `viewHierarchy` JSON, so `IosHierarchy` and the Maestro selector engine see the same structure
//! with either backend. Also the frontmost-process answer the hybrid router asks.
//!
//! The mapping follows what phase 1 measured on the Czech iPhone 18 Pro Max simulator:
//! * The tree has roles and subroles, not XCUI element types. [`element_type`] maps the roles to
//!   the types Tapwing reads (alert, keyboard, switch, checkbox, toggle) and to plain ones for the rest.
//!   The runner's `Alert` container has no counterpart, so alerts are found by their buttons and texts.
//! * `AXTitle` is always empty, so `title` stays null and the text comes from the value.
//! * An empty UIKit text field reports its placeholder as the value, the runner reports an empty
//!   value. The placeholder is dropped from the value so `text` selectors match the same elements.
//! * Frames are `[x, y, width, height]` in screen points, the runner's `X`, `Y`, `Width`, `Height`.

use serde_json::{Value, json};

use super::HidError;

/// `XCUIElement.ElementType` raw values used by `hierarchy.rs`, and plain ones for the other roles.
const OTHER: i64 = 1;
const APPLICATION: i64 = 2;
const GROUP: i64 = 3;
const WINDOW: i64 = 4;
const BUTTON: i64 = 9;
const RADIO_BUTTON: i64 = 10;
const CHECK_BOX: i64 = 12;
const POP_UP_BUTTON: i64 = 14;
const TABLE: i64 = 26;
const TABLE_ROW: i64 = 27;
const SLIDER: i64 = 33;
const PROGRESS_INDICATOR: i64 = 35;
const SWITCH: i64 = 40;
const TOGGLE: i64 = 41;
const LINK: i64 = 42;
const IMAGE: i64 = 43;
const SEARCH_FIELD: i64 = 45;
const SCROLL_VIEW: i64 = 46;
const STATIC_TEXT: i64 = 48;
const TEXT_FIELD: i64 = 49;
const SECURE_TEXT_FIELD: i64 = 50;
const TEXT_VIEW: i64 = 52;

/// Process name of SpringBoard, as the shim reports it for the process of a root.
const SPRINGBOARD_PROCESS: &str = "SpringBoard";

/// The runner element type for an `AXRole` and `AXSubrole`.
pub(super) fn element_type(role: &str, subrole: &str) -> i64 {
    match (role, subrole) {
        (_, "AXSearchField") => SEARCH_FIELD,
        (_, "AXSecureTextField") => SECURE_TEXT_FIELD,
        ("AXApplication", _) => APPLICATION,
        ("AXWindow", _) => WINDOW,
        ("AXGroup" | "AXGenericElement", _) => GROUP,
        ("AXButton", _) => BUTTON,
        ("AXRadioButton", _) => RADIO_BUTTON,
        ("AXCheckBox", _) => CHECK_BOX,
        ("AXSwitch", _) => SWITCH,
        ("AXToggle", _) => TOGGLE,
        ("AXPopUpButton", _) => POP_UP_BUTTON,
        ("AXTable" | "AXList", _) => TABLE,
        ("AXRow" | "AXCell", _) => TABLE_ROW,
        ("AXSlider", _) => SLIDER,
        ("AXProgressIndicator", _) => PROGRESS_INDICATOR,
        ("AXLink", _) => LINK,
        ("AXImage", _) => IMAGE,
        ("AXScrollArea", _) => SCROLL_VIEW,
        ("AXStaticText" | "AXHeading", _) => STATIC_TEXT,
        ("AXTextField", _) => TEXT_FIELD,
        ("AXTextArea", _) => TEXT_VIEW,
        _ => OTHER,
    }
}

fn is_text_input(element_type: i64) -> bool {
    matches!(element_type, TEXT_FIELD | SECURE_TEXT_FIELD | SEARCH_FIELD | TEXT_VIEW)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The shim writes `enabled`, `focused` and `selected` as numbers, tolerate booleans too.
fn flag(v: &Value, key: &str, default: bool) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        _ => default,
    }
}

/// A value as the runner's string: switches report `1` and `0` as numbers.
fn value_text(v: &Value) -> Option<String> {
    match v.get("value") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(u8::from(*b).to_string()),
        _ => None,
    }
}

fn frame(v: &Value) -> [f64; 4] {
    let mut out = [0.0; 4];
    if let Some(a) = v.get("frame").and_then(Value::as_array) {
        for (slot, n) in out.iter_mut().zip(a) {
            *slot = n.as_f64().filter(|n| n.is_finite()).unwrap_or(0.0);
        }
    }
    out
}

fn runner_frame(f: [f64; 4]) -> Value {
    json!({ "X": f[0], "Y": f[1], "Width": f[2], "Height": f[3] })
}

/// Maps one node and returns it with the depth of its subtree (a leaf is 1).
fn convert(v: &Value) -> (Value, u32) {
    let role = v.get("role").and_then(Value::as_str).unwrap_or("");
    let subrole = v.get("subrole").and_then(Value::as_str).unwrap_or("");
    let et = element_type(role, subrole);
    let placeholder = text(v, "placeholder").filter(|p| !p.is_empty());
    let mut value = value_text(v);
    if is_text_input(et) && value.is_some() && value == placeholder {
        value = Some(String::new());
    }
    let (children, depth): (Vec<Value>, u32) =
        v.get("children").and_then(Value::as_array).map_or((Vec::new(), 0), |kids| {
            let mapped: Vec<(Value, u32)> = kids.iter().map(convert).collect();
            let depth = mapped.iter().map(|m| m.1).max().unwrap_or(0);
            (mapped.into_iter().map(|m| m.0).collect(), depth)
        });
    let node = json!({
        "label": text(v, "label").unwrap_or_default(),
        "elementType": et,
        "identifier": text(v, "identifier").unwrap_or_default(),
        "enabled": flag(v, "enabled", true),
        "selected": flag(v, "selected", false),
        "hasFocus": flag(v, "focused", false),
        "title": text(v, "title"),
        "value": value,
        "placeholderValue": placeholder,
        "frame": runner_frame(frame(v)),
        "children": children,
    });
    (node, depth + 1)
}

fn roots(json_text: &str) -> Result<Vec<Value>, HidError> {
    let doc: Value = serde_json::from_str(json_text)
        .map_err(|e| HidError::Failed(format!("the accessibility tree is not JSON: {e}")))?;
    match doc.get("roots").and_then(Value::as_array) {
        Some(r) if !r.is_empty() => Ok(r.clone()),
        _ => Err(HidError::Failed("the accessibility tree has no application".into())),
    }
}

/// A mapped snapshot: the runner-shaped document and what the roots say about the interface.
#[derive(Debug)]
pub struct Mapped {
    /// `{ "depth", "axElement" }`, the runner's `viewHierarchy` answer.
    pub document: Value,
    /// The first root, the frontmost application.
    pub front: FrontApp,
    /// Whether the first root is taller than wide.
    pub portrait: bool,
}

/// Maps the shim's JSON. The applications become the children of one root without a type, as the
/// runner's tree has them.
pub fn map_snapshot(json_text: &str) -> Result<Mapped, HidError> {
    let roots = roots(json_text)?;
    let first = &roots[0];
    let first_frame = frame(first);
    let (children, depth): (Vec<Value>, u32) = {
        let mapped: Vec<(Value, u32)> = roots.iter().map(convert).collect();
        let depth = mapped.iter().map(|m| m.1).max().unwrap_or(0);
        (mapped.into_iter().map(|m| m.0).collect(), depth)
    };
    let document = json!({
        "depth": depth + 1,
        "axElement": {
            "label": "", "elementType": 0, "identifier": "", "enabled": true, "selected": false,
            "hasFocus": false, "title": null, "value": null, "placeholderValue": null,
            "frame": runner_frame(first_frame),
            "children": children,
        },
    });
    Ok(Mapped { document, front: front_of(first), portrait: first_frame[2] < first_frame[3] })
}

/// Which application owns the screen, as far as one accessibility read can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontApp {
    /// SpringBoard is in front, in portrait: a permission alert, the home screen, the lock screen.
    SpringBoard,
    /// Another process is in front, or SpringBoard in a rotated interface (the HID surface is portrait only).
    Other,
    /// The read gave nothing usable. Phase 1 saw this with the simulator's accessibility flags at 0.
    Unknown,
}

fn front_of(root: &Value) -> FrontApp {
    let f = frame(root);
    match root.get("process").and_then(Value::as_str) {
        Some(SPRINGBOARD_PROCESS) if f[2] < f[3] => FrontApp::SpringBoard,
        Some(_) => FrontApp::Other,
        // An old shim or a failed process lookup: the root cannot be told apart, so do not guess.
        None => FrontApp::Unknown,
    }
}

/// The frontmost application of one snapshot. Never errors: an unreadable tree is `Unknown`.
pub fn front_app(json_text: &str) -> FrontApp {
    roots(json_text).map_or(FrontApp::Unknown, |r| front_of(&r[0]))
}
