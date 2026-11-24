//! `POST /viewHierarchy` JSON into the shared selector engine.
//!
//! The runner answers `{ axElement, depth }` (`hierarchy.ViewHierarchy`); the
//! attribute mapping follows `IOSDriver.mapViewHierarchy` (Maestro 2.12.1):
//! `label` is the accessibility text, `title` (else `value`) the text,
//! `placeholderValue` the hint, `identifier` the id, `frame` the bounds in
//! logical points (floats truncated like Kotlin's `Float.toInt`). Selector
//! evaluation reuses the `Filters.kt` port in [`crate::fake_driver`].

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::fake_driver::{Bounds, TreeNode, find_in_tree};
use crate::flow::ir::ElementSelector;

/// `XCUIElement.ElementType.alert`: SpringBoard permission dialogs and native alerts.
const ELEMENT_ALERT: i64 = 7;
/// `XCUIElement.ElementType.keyboard`.
const ELEMENT_KEYBOARD: i64 = 19;
/// `IOSDriver.CHECKABLE_ELEMENTS`: checkbox, switch, toggle.
const CHECKABLE_ELEMENTS: [i64; 3] = [12, 40, 41];

/// `hierarchy.AXFrame`; the runner sends capitalized keys (`X`, `Y`, `Width`, `Height`).
#[derive(Debug, Default, Deserialize)]
struct AxFrame {
    #[serde(rename = "X", alias = "x", default)]
    x: f32,
    #[serde(rename = "Y", alias = "y", default)]
    y: f32,
    #[serde(rename = "Width", alias = "width", default)]
    width: f32,
    #[serde(rename = "Height", alias = "height", default)]
    height: f32,
}

impl AxFrame {
    /// `AXFrame.boundsString`: `[left,top][right,bottom]` with `Float.toInt()`
    /// truncation, right/bottom summed in `f32` first.
    fn bounds(&self) -> Bounds {
        Bounds::new(self.x as i32, self.y as i32, (self.x + self.width) as i32, (self.y + self.height) as i32)
    }
}

/// `hierarchy.AXElement` (only the fields Maestro reads).
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AxElement {
    label: Option<String>,
    title: Option<String>,
    value: Option<String>,
    placeholder_value: Option<String>,
    identifier: Option<String>,
    element_type: i64,
    enabled: bool,
    selected: bool,
    has_focus: bool,
    frame: Option<AxFrame>,
    children: Vec<AxElement>,
}

/// `hierarchy.ViewHierarchy`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AxHierarchy {
    ax_element: AxElement,
    #[serde(default)]
    depth: u32,
}

/// Empty strings are "no value", like the `isNotEmpty` filtering of Maestro's selectors.
fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref().filter(|s| !s.is_empty()).map(str::to_string)
}

#[derive(Default)]
struct Facts {
    next_uid: u64,
    alert: bool,
    keyboard: bool,
}

fn convert(el: &AxElement, facts: &mut Facts) -> TreeNode {
    facts.next_uid += 1;
    let uid = facts.next_uid;
    facts.alert |= el.element_type == ELEMENT_ALERT;
    facts.keyboard |= el.element_type == ELEMENT_KEYBOARD;
    // `title` wins when non-empty, otherwise `value`.
    let text = non_empty(&el.title).or_else(|| non_empty(&el.value));
    let checked = CHECKABLE_ELEMENTS.contains(&el.element_type) && el.value.as_deref() == Some("1");
    let children = el.children.iter().map(|c| convert(c, facts)).collect();
    TreeNode {
        uid,
        id: non_empty(&el.identifier),
        has_text_attr: text.is_some(),
        text,
        hint_text: non_empty(&el.placeholder_value),
        accessibility_text: non_empty(&el.label),
        error_text: None,
        bounds: el.frame.as_ref().map(AxFrame::bounds),
        clickable: false,
        enabled: Some(el.enabled),
        selected: Some(el.selected),
        checked: Some(checked),
        focused: Some(el.has_focus),
        matches_css_filter: None,
        ignore_bounds_filtering: false,
        children,
    }
}

/// One parsed, bounds-filtered iOS view hierarchy (`ViewHierarchy.from`).
#[derive(Clone, Debug)]
pub struct IosHierarchy {
    pub root: TreeNode,
    /// Nesting depth reported by the runner (Maestro warns past 61).
    pub depth: u32,
    alert: bool,
    keyboard: bool,
}

impl IosHierarchy {
    /// Parses the raw `viewHierarchy` JSON; `screen_w`/`screen_h` are the
    /// screen size in points, used to prune off-screen elements.
    pub fn parse(json: &Value, screen_w: i32, screen_h: i32) -> Result<Self> {
        let ax = AxHierarchy::deserialize(json).context("unexpected iOS viewHierarchy JSON")?;
        let mut facts = Facts::default();
        let raw = convert(&ax.ax_element, &mut facts);
        // `filtered ?: it`: a fully off-screen tree is kept unfiltered.
        let root = raw.filter_out_of_bounds(screen_w, screen_h).unwrap_or(raw);
        Ok(Self { root, depth: ax.depth, alert: facts.alert, keyboard: facts.keyboard })
    }

    pub fn parse_str(json: &str, screen_w: i32, screen_h: i32) -> Result<Self> {
        let value: Value = serde_json::from_str(json).context("malformed iOS viewHierarchy JSON")?;
        Self::parse(&value, screen_w, screen_h)
    }

    /// First element matching `selector` (`Filters` + `Orchestra.buildFilter`).
    pub fn find(&self, selector: &ElementSelector) -> Result<Option<TreeNode>> {
        find_in_tree(&self.root, selector)
    }

    /// Whether the screen shows an `XCUIElementTypeAlert`: the SpringBoard
    /// permission dialogs (location, photos, tracking, ...) and any native
    /// alert. The Flutter driver cannot see these, so they route to this lane.
    pub fn has_springboard_alert(&self) -> bool {
        self.alert
    }

    /// Whether the soft keyboard is part of the hierarchy.
    pub fn keyboard_visible(&self) -> bool {
        self.keyboard
    }

    /// Fingerprint used to detect "the screen stopped changing". Unlike
    /// `TreeNode::structure_signature` it covers the accessibility label and
    /// hint, which carry most of an iOS screen's text.
    pub fn signature(&self) -> String {
        fn walk(n: &TreeNode, out: &mut String) {
            use std::fmt::Write;
            let _ = write!(
                out,
                "({:?},{:?},{:?},{:?},{:?},{:?},{:?},{:?},{:?};",
                n.id, n.text, n.accessibility_text, n.hint_text, n.bounds, n.enabled, n.selected, n.checked, n.focused
            );
            for c in &n.children {
                walk(c, out);
            }
            out.push(')');
        }
        let mut s = String::new();
        walk(&self.root, &mut s);
        s
    }

    /// Distinct visible texts (text, then accessibility label), for error messages.
    pub fn visible_texts(&self, limit: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in self.root.aggregate() {
            for t in [n.text.as_deref(), n.accessibility_text.as_deref()].into_iter().flatten() {
                if !t.is_empty() && !out.iter().any(|o| o == t) {
                    out.push(t.to_string());
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// iOS 17+ location permission alert as the runner reports it (trimmed),
    /// with the app screen below it.
    fn alert_screen() -> Value {
        json!({
            "depth": 4,
            "axElement": {
                "label": "", "elementType": 0, "identifier": "", "enabled": true, "selected": false,
                "hasFocus": false, "title": null, "value": null, "placeholderValue": null,
                "frame": { "X": 0.0, "Y": 0.0, "Width": 402.0, "Height": 874.0 },
                "children": [
                    {
                        "label": "Sample App", "elementType": 2, "identifier": "dev.tapwing.sampleApp",
                        "enabled": true, "selected": false, "hasFocus": false,
                        "frame": { "X": 0.0, "Y": 0.0, "Width": 402.0, "Height": 874.0 },
                        "children": [{
                            "label": "Request Location Permission", "elementType": 9, "identifier": "",
                            "enabled": true, "selected": false, "hasFocus": false,
                            "frame": { "X": 20.0, "Y": 300.0, "Width": 362.0, "Height": 44.0 },
                            "children": []
                        }]
                    },
                    {
                        "label": "Allow “Sample App” to use your location?", "elementType": 7,
                        "identifier": "", "enabled": true, "selected": false, "hasFocus": false,
                        "frame": { "X": 66.0, "Y": 300.5, "Width": 270.0, "Height": 260.75 },
                        "children": [
                            {
                                "label": "Allow “Sample App” to use your location?", "elementType": 48,
                                "identifier": "", "enabled": true, "selected": false, "hasFocus": false,
                                "title": "Allow “Sample App” to use your location?",
                                "frame": { "X": 82.0, "Y": 316.0, "Width": 238.0, "Height": 40.0 },
                                "children": []
                            },
                            {
                                "label": "Allow Once", "elementType": 9, "identifier": "", "enabled": true,
                                "selected": false, "hasFocus": false,
                                "frame": { "X": 66.0, "Y": 430.0, "Width": 270.0, "Height": 44.0 },
                                "children": []
                            },
                            {
                                "label": "Allow While Using App", "elementType": 9, "identifier": "",
                                "enabled": true, "selected": false, "hasFocus": false,
                                "frame": { "X": 66.0, "Y": 474.0, "Width": 270.0, "Height": 44.0 },
                                "children": []
                            },
                            {
                                "label": "Don’t Allow", "elementType": 9, "identifier": "", "enabled": true,
                                "selected": false, "hasFocus": false,
                                "frame": { "X": 66.0, "Y": 518.0, "Width": 270.0, "Height": 44.0 },
                                "children": []
                            }
                        ]
                    }
                ]
            }
        })
    }

    fn label(t: &str) -> ElementSelector {
        ElementSelector { text_regex: Some(t.into()), ..Default::default() }
    }

    fn el(element_type: i64, extra: Value) -> Value {
        let mut base = json!({
            "label": "", "elementType": element_type, "identifier": "", "enabled": true,
            "selected": false, "hasFocus": false, "children": [],
            "frame": { "X": 0.0, "Y": 0.0, "Width": 100.0, "Height": 50.0 }
        });
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        base
    }

    fn doc(children: Vec<Value>) -> Value {
        json!({ "depth": 1, "axElement": el(0, json!({
            "frame": { "X": 0.0, "Y": 0.0, "Width": 402.0, "Height": 874.0 },
            "children": children
        })) })
    }

    #[test]
    fn selects_springboard_alert_buttons_by_label() {
        let h = IosHierarchy::parse(&alert_screen(), 402, 874).unwrap();
        assert!(h.has_springboard_alert());
        assert!(!h.keyboard_visible());
        assert_eq!(h.depth, 4);
        // `accessibilityText` (label) is what `tapOn: "text"` matches on iOS.
        let allow = h.find(&label("Allow While Using App")).unwrap().expect("alert button");
        assert_eq!(allow.bounds, Some(Bounds::new(66, 474, 336, 518)));
        assert_eq!(allow.bounds.unwrap().center(), (201, 496));
        // Regex, curly apostrophe preserved.
        assert!(h.find(&label("Don’t Allow")).unwrap().is_some());
        assert!(h.find(&label("Allow (Once|While.*)")).unwrap().is_some());
        // The app below is part of the same tree.
        assert!(h.find(&label("Request Location Permission")).unwrap().is_some());
    }

    #[test]
    fn frames_truncate_like_kotlin_float_to_int() {
        let screen = doc(vec![el(
            9,
            json!({ "label": "Frac", "frame": { "X": 10.7, "Y": 20.5, "Width": 100.6, "Height": 50.9 } }),
        )]);
        let h = IosHierarchy::parse(&screen, 402, 874).unwrap();
        // left/top truncate; right = 10.7 + 100.6 = 111.3 -> 111, bottom = 20.5 + 50.9 = 71.4 -> 71.
        assert_eq!(h.root.children[0].bounds, Some(Bounds::new(10, 20, 111, 71)));
        let h = IosHierarchy::parse(&alert_screen(), 402, 874).unwrap();
        let once = h.find(&label("Allow Once")).unwrap().unwrap();
        assert_eq!(once.bounds, Some(Bounds::new(66, 430, 336, 474)));
    }

    #[test]
    fn plain_app_screen_has_no_alert() {
        let h = IosHierarchy::parse(&doc(vec![el(9, json!({ "label": "Login" }))]), 402, 874).unwrap();
        assert!(!h.has_springboard_alert());
        assert!(h.find(&label("Login")).unwrap().is_some());
    }

    #[test]
    fn maps_text_hint_id_and_state_like_ios_driver() {
        let screen = doc(vec![
            // title wins over value
            el(48, json!({ "title": "Title", "value": "Value", "label": "Label" })),
            // empty title falls back to the value
            el(
                49,
                json!({ "title": "", "value": "typed", "placeholderValue": "Email", "identifier": "email_field", "hasFocus": true }),
            ),
            // null / empty strings are absent
            el(1, json!({ "title": null, "value": "", "label": "" })),
            el(9, json!({ "enabled": false, "selected": true, "label": "Off" })),
        ]);
        let h = IosHierarchy::parse(&screen, 402, 874).unwrap();
        let kids = &h.root.children;
        assert_eq!(kids[0].text.as_deref(), Some("Title"));
        assert_eq!(kids[0].accessibility_text.as_deref(), Some("Label"));
        assert_eq!(kids[1].text.as_deref(), Some("typed"));
        assert_eq!(kids[1].hint_text.as_deref(), Some("Email"));
        assert_eq!(kids[1].id.as_deref(), Some("email_field"));
        assert_eq!(kids[1].focused, Some(true));
        assert_eq!((kids[2].text.clone(), kids[2].accessibility_text.clone(), kids[2].id.clone()), (None, None, None));
        assert_eq!((kids[3].enabled, kids[3].selected, kids[3].focused), (Some(false), Some(true), Some(false)));
        assert!(!kids[3].clickable);
        // Hint text is selectable like Maestro's `hintText`.
        assert!(h.find(&label("Email")).unwrap().is_some());
        let disabled = ElementSelector { enabled: Some(false), ..Default::default() };
        assert_eq!(h.find(&disabled).unwrap().unwrap().accessibility_text.as_deref(), Some("Off"));
    }

    #[test]
    fn checked_only_for_checkable_types_with_value_one() {
        let screen = doc(vec![
            el(40, json!({ "label": "Switch on", "value": "1" })),
            el(40, json!({ "label": "Switch off", "value": "0" })),
            el(12, json!({ "label": "Checkbox on", "value": "1" })),
            el(41, json!({ "label": "Toggle on", "value": "1" })),
            el(48, json!({ "label": "Text one", "value": "1" })),
        ]);
        let h = IosHierarchy::parse(&screen, 402, 874).unwrap();
        let checked: Vec<_> = h.root.children.iter().map(|c| c.checked).collect();
        assert_eq!(checked, vec![Some(true), Some(false), Some(true), Some(true), Some(false)]);
        let sel = ElementSelector { checked: Some(true), ..Default::default() };
        assert_eq!(h.find(&sel).unwrap().unwrap().accessibility_text.as_deref(), Some("Switch on"));
    }

    #[test]
    fn detects_keyboard_and_prunes_off_screen_elements() {
        let screen = doc(vec![
            el(19, json!({ "frame": { "X": 0.0, "Y": 600.0, "Width": 402.0, "Height": 274.0 } })),
            el(9, json!({ "label": "Way below", "frame": { "X": 0.0, "Y": 5000.0, "Width": 100.0, "Height": 40.0 } })),
        ]);
        let h = IosHierarchy::parse(&screen, 402, 874).unwrap();
        assert!(h.keyboard_visible());
        assert!(h.find(&label("Way below")).unwrap().is_none(), "off-screen element pruned");
        assert_eq!(h.root.children.len(), 1);
    }

    #[test]
    fn missing_frame_means_no_bounds_and_uids_follow_document_order() {
        // A container without a frame survives through its on-screen child.
        let framed = el(9, json!({ "label": "Child" }));
        let container = json!({ "label": "No frame", "elementType": 1, "enabled": true, "children": [framed] });
        let h = IosHierarchy::parse(&doc(vec![container]), 402, 874).unwrap();
        assert_eq!(h.root.children[0].bounds, None);
        assert!(h.root.children[0].children[0].bounds.is_some());
        let uids: Vec<u64> = h.root.aggregate().iter().map(|n| n.uid).collect();
        assert_eq!(uids, vec![1, 2, 3]);
        // A frameless leaf has nothing to show and is pruned like any off-screen element.
        let leaf = json!({ "label": "Ghost", "elementType": 1, "enabled": true, "children": [] });
        let h = IosHierarchy::parse(&doc(vec![leaf]), 402, 874).unwrap();
        assert!(h.root.children.is_empty());
    }

    #[test]
    fn visible_texts_dedupe_and_signature_tracks_the_screen() {
        let h = IosHierarchy::parse(&alert_screen(), 402, 874).unwrap();
        let texts = h.visible_texts(50);
        assert_eq!(texts.iter().filter(|t| t.as_str() == "Allow Once").count(), 1);
        assert_eq!(texts.iter().filter(|t| t.starts_with("Allow “Sample App”")).count(), 1);
        assert_eq!(h.visible_texts(2).len(), 2);
        let sig = |extra: Value| IosHierarchy::parse(&doc(vec![el(9, extra)]), 402, 874).unwrap().signature();
        // Label, title and focus changes all change the fingerprint.
        assert_ne!(sig(json!({ "label": "A" })), sig(json!({ "label": "B" })));
        assert_ne!(sig(json!({ "title": "A" })), sig(json!({ "title": "B" })));
        assert_ne!(sig(json!({})), sig(json!({ "hasFocus": true })));
        assert_eq!(sig(json!({ "label": "A" })), sig(json!({ "label": "A" })));
    }

    #[test]
    fn lowercase_frame_keys_are_accepted_and_garbage_is_rejected() {
        let screen = json!({ "axElement": {
            "enabled": true, "children": [],
            "frame": { "x": 1.0, "y": 2.0, "width": 10.0, "height": 20.0 }
        } });
        let h = IosHierarchy::parse(&screen, 402, 874).unwrap();
        assert_eq!(h.root.bounds, Some(Bounds::new(1, 2, 11, 22)));
        assert!(IosHierarchy::parse(&json!({ "nope": 1 }), 402, 874).is_err());
        assert!(IosHierarchy::parse_str("not json", 402, 874).is_err());
        assert!(IosHierarchy::parse_str(&alert_screen().to_string(), 402, 874).is_ok());
    }
}
