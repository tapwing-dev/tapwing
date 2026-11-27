//! UiAutomator XML hierarchy parsing and Maestro selector evaluation for the
//! native lane.
//!
//! The XML comes from `MaestroDriver.viewHierarchy`. Attribute mapping follows
//! `AndroidDriver.mapHierarchy` (Maestro 2.12.1); selector evaluation reuses the
//! `Filters.kt` port in [`crate::ui_tree`] so both lanes answer
//! `text` / `id` / `below` / `enabled` / `index` / ... identically.

use anyhow::{Context, Result, bail};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::native_core::ops::NativeHierarchy;
use crate::ui_tree::{Bounds, TreeNode};

/// Class of Android toasts; `ViewHierarchy.filterOutOfBounds` never prunes them.
const TOAST_CLASS_NAME: &str = "android.widget.Toast";

/// Resource-id prefix of Gboard, whose elements Maestro treats as the soft keyboard.
const KEYBOARD_RESOURCE_PREFIX: &str = "com.google.android.inputmethod.latin:id/";

/// Parses `[l,t][r,b]` (`UiElement.toUiElementOrNull`).
pub fn parse_bounds(raw: &str) -> Option<Bounds> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
    let nums: Vec<i32> = inner.replace("][", ",").split(',').map(|n| n.trim().parse().ok()).collect::<Option<_>>()?;
    match nums.as_slice() {
        [l, t, r, b] => Some(Bounds::new(*l, *t, *r, *b)),
        _ => None,
    }
}

/// Parses one `<node>`, plus its `package` attribute.
fn node_from_element(e: &BytesStart<'_>, uid: u64) -> Result<(TreeNode, Option<String>)> {
    let mut node = TreeNode {
        uid,
        id: None,
        text: None,
        has_text_attr: false,
        hint_text: None,
        accessibility_text: None,
        error_text: None,
        bounds: None,
        clickable: false,
        enabled: None,
        selected: None,
        checked: None,
        focused: None,
        matches_css_filter: None,
        ignore_bounds_filtering: false,
        children: Vec::new(),
    };
    let mut class = None;
    let mut package = None;
    for attr in e.attributes() {
        let attr = attr.context("malformed XML attribute in view hierarchy")?;
        let key = attr.key.as_ref().to_string();
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .context("bad escape in view hierarchy attribute")?
            .into_owned();
        let non_empty = |s: String| (!s.is_empty()).then_some(s);
        let flag = || value == "true";
        match key.as_str() {
            "text" => {
                node.has_text_attr = true;
                node.text = non_empty(value);
            }
            "content-desc" => node.accessibility_text = non_empty(value),
            "hintText" => node.hint_text = non_empty(value),
            "error" => node.error_text = non_empty(value),
            "resource-id" => node.id = non_empty(value),
            "bounds" => node.bounds = parse_bounds(&value),
            "clickable" => node.clickable = flag(),
            "enabled" => node.enabled = Some(flag()),
            "focused" => node.focused = Some(flag()),
            "checked" => node.checked = Some(flag()),
            "selected" => node.selected = Some(flag()),
            "class" => class = Some(value),
            "package" => package = non_empty(value),
            _ => {}
        }
    }
    node.ignore_bounds_filtering = class.as_deref() == Some(TOAST_CLASS_NAME);
    Ok((node, package))
}

/// Whether a selector could match anything on `node`: text, description, id, hint text or
/// error text (the same attributes [`Hierarchy::find`] matches).
fn has_content(node: &TreeNode) -> bool {
    node.text.is_some()
        || node.accessibility_text.is_some()
        || node.id.is_some()
        || node.hint_text.is_some()
        || node.error_text.is_some()
}

/// Parses a UiAutomator dump into a tree (document root = `<hierarchy>`).
/// UIDs are assigned in document order, matching [`crate::ui_tree::TreeNode`] snapshots.
pub fn parse_hierarchy(xml: &str) -> Result<TreeNode> {
    parse_with_packages(xml).map(|(root, _)| root)
}

/// [`parse_hierarchy`], plus the packages that own at least one node with
/// content (see [`Hierarchy::shows_package`]).
fn parse_with_packages(xml: &str) -> Result<(TreeNode, Vec<String>)> {
    let mut packages: Vec<String> = Vec::new();
    let mut note = |node: &TreeNode, package: Option<String>| {
        if let Some(p) = package
            && has_content(node)
            && !packages.contains(&p)
        {
            packages.push(p);
        }
    };
    let mut reader = Reader::from_str(xml);
    let mut next_uid = 1_u64;
    // Open elements, innermost last; the bottom entry is the document root.
    let mut stack: Vec<TreeNode> = Vec::new();
    let mut root: Option<TreeNode> = None;

    fn attach(stack: &mut [TreeNode], root: &mut Option<TreeNode>, done: TreeNode) {
        match stack.last_mut() {
            Some(parent) => parent.children.push(done),
            None => *root = Some(done),
        }
    }

    loop {
        match reader.read_event().context("malformed view hierarchy XML")? {
            Event::Start(e) => {
                let (node, package) = node_from_element(&e, next_uid)?;
                note(&node, package);
                stack.push(node);
                next_uid += 1;
            }
            Event::Empty(e) => {
                let (node, package) = node_from_element(&e, next_uid)?;
                note(&node, package);
                next_uid += 1;
                attach(&mut stack, &mut root, node);
            }
            Event::End(_) => {
                let done = stack.pop().context("unbalanced view hierarchy XML")?;
                attach(&mut stack, &mut root, done);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        bail!("truncated view hierarchy XML");
    }
    Ok((root.context("empty view hierarchy XML")?, packages))
}

/// One parsed, bounds-filtered view hierarchy (`ViewHierarchy.from`).
#[derive(Clone, Debug)]
pub struct Hierarchy {
    pub root: TreeNode,
    /// Packages owning at least one node with text, a description or an id.
    content_packages: Vec<String>,
}

impl Hierarchy {
    pub fn parse(xml: &str, screen_w: i32, screen_h: i32) -> Result<Self> {
        let (raw, content_packages) = parse_with_packages(xml)?;
        // `filtered ?: it`: a fully off-screen tree is kept unfiltered.
        let root = raw.filter_out_of_bounds(screen_w, screen_h).unwrap_or(raw);
        Ok(Self { root, content_packages })
    }

    /// Whether `package` shows anything a selector could match. A cold
    /// Flutter app is either missing from the window roots or a bare
    /// `FlutterView` without semantics children; both count as not shown.
    pub fn shows_package(&self, package: &str) -> bool {
        self.content_packages.iter().any(|p| p == package)
    }

    /// Soft-keyboard check (`AndroidDriver.isKeyboardVisible`): any Gboard element.
    pub fn keyboard_visible(&self) -> bool {
        self.root.aggregate().iter().any(|n| n.id.as_deref().is_some_and(|i| i.starts_with(KEYBOARD_RESOURCE_PREFIX)))
    }
}

impl NativeHierarchy for Hierarchy {
    fn root(&self) -> &TreeNode {
        &self.root
    }

    fn shows_package(&self, package: &str) -> bool {
        Hierarchy::shows_package(self, package)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::ir::ElementSelector;

    /// Android 14 runtime-permission dialog (permission controller), trimmed.
    const PERMISSION_DIALOG: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
<hierarchy rotation="0">
  <node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.google.android.permissioncontroller" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2400]">
    <node index="0" text="" resource-id="com.android.permissioncontroller:id/grant_dialog" class="android.widget.LinearLayout" package="com.google.android.permissioncontroller" content-desc="" clickable="false" enabled="true" focused="false" checked="false" selected="false" bounds="[108,900][972,1500]">
      <node index="0" text="Allow Sample App to access this device&apos;s location?" resource-id="com.android.permissioncontroller:id/permission_message" class="android.widget.TextView" package="com.google.android.permissioncontroller" content-desc="" clickable="false" enabled="true" focused="false" checked="false" selected="false" bounds="[160,950][920,1100]" />
      <node index="1" text="While using the app" resource-id="com.android.permissioncontroller:id/permission_allow_foreground_only_button" class="android.widget.Button" package="com.google.android.permissioncontroller" content-desc="" clickable="true" enabled="true" focused="false" checked="false" selected="false" bounds="[160,1150][920,1250]" />
      <node index="2" text="Only this time" resource-id="com.android.permissioncontroller:id/permission_allow_one_time_button" class="android.widget.Button" package="com.google.android.permissioncontroller" content-desc="" clickable="true" enabled="true" focused="false" checked="false" selected="false" bounds="[160,1260][920,1360]" />
      <node index="3" text="Don&apos;t allow" resource-id="com.android.permissioncontroller:id/permission_deny_button" class="android.widget.Button" package="com.google.android.permissioncontroller" content-desc="" clickable="true" enabled="true" focused="false" checked="false" selected="false" bounds="[160,1370][920,1470]" />
    </node>
  </node>
</hierarchy>"#;

    fn text_sel(t: &str) -> ElementSelector {
        ElementSelector { text_regex: Some(t.into()), ..Default::default() }
    }

    fn dialog() -> Hierarchy {
        Hierarchy::parse(PERMISSION_DIALOG, 1080, 2400).unwrap()
    }

    #[test]
    fn a_package_shows_only_with_selectable_content() {
        assert!(dialog().shows_package("com.google.android.permissioncontroller"));
        assert!(!dialog().shows_package("com.example.app"));
        // A cold Flutter app: its window is there, its semantics are not.
        let bare = r#"<hierarchy rotation="0">
  <node text="" resource-id="" content-desc="" package="com.example.app" bounds="[0,0][1080,2400]">
    <node text="" resource-id="" content-desc="" package="com.example.app" bounds="[0,0][1080,2400]"/>
  </node>
</hierarchy>"#;
        assert!(!Hierarchy::parse(bare, 1080, 2400).unwrap().shows_package("com.example.app"));
        // Hint and error text are matched by `find`, so they count as content too (an empty
        // text field with only a hint is a screen that is up).
        for attr in [
            r#"text="Hi""#,
            r#"content-desc="Hi""#,
            r#"resource-id="hi""#,
            r#"hintText="Search""#,
            r#"error="Required""#,
        ] {
            let xml = format!(
                r#"<hierarchy rotation="0"><node {attr} package="com.example.app" bounds="[0,0][10,10]"/></hierarchy>"#
            );
            assert!(Hierarchy::parse(&xml, 1080, 2400).unwrap().shows_package("com.example.app"), "{attr}");
        }
    }

    #[test]
    fn parses_bounds_and_rejects_garbage() {
        assert_eq!(parse_bounds("[10,20][110,220]"), Some(Bounds::new(10, 20, 110, 220)));
        assert_eq!(parse_bounds("[0,0][1080,2400]"), Some(Bounds::new(0, 0, 1080, 2400)));
        assert_eq!(parse_bounds("[-5,0][10,10]"), Some(Bounds::new(-5, 0, 10, 10)));
        assert_eq!(parse_bounds("10,20,30,40"), None);
        assert_eq!(parse_bounds("[1,2][3]"), None);
        assert_eq!(parse_bounds("[a,b][c,d]"), None);
    }

    #[test]
    fn parses_permission_dialog_tree_and_unescapes_entities() {
        let h = dialog();
        let all = h.root.aggregate();
        // hierarchy + FrameLayout + LinearLayout + message + 3 buttons
        assert_eq!(all.len(), 7);
        assert!(h.root.bounds.is_none(), "<hierarchy> has no bounds");
        assert_eq!(h.root.children.len(), 1);
        let dont = h.find(&text_sel("Don't allow")).unwrap().expect("`Don&apos;t allow` unescaped");
        assert_eq!(dont.id.as_deref(), Some("com.android.permissioncontroller:id/permission_deny_button"));
        assert_eq!(dont.bounds, Some(Bounds::new(160, 1370, 920, 1470)));
        assert!(dont.clickable);
        assert_eq!(dont.enabled, Some(true));
        // UIDs follow document order.
        let uids: Vec<u64> = all.iter().map(|n| n.uid).collect();
        assert_eq!(uids, vec![1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn selects_permission_buttons_by_text_id_and_index() {
        let h = dialog();
        let allow = h.find(&text_sel("While using the app")).unwrap().unwrap();
        assert_eq!(allow.bounds.unwrap().center(), (540, 1200));
        let once = h.find(&text_sel("Only this time")).unwrap().unwrap();
        assert_eq!(once.bounds.unwrap().center(), (540, 1310));
        // `id` matches the full resource id and its suffix after `/`.
        let by_id = ElementSelector { id_regex: Some("permission_deny_button".into()), ..Default::default() };
        assert_eq!(h.find(&by_id).unwrap().unwrap().text.as_deref(), Some("Don't allow"));
        // Regex, case-insensitive, full match.
        assert!(h.find(&text_sel("while.*APP")).unwrap().is_some());
        assert!(h.find(&text_sel("While using")).unwrap().is_none());
        // index selects among clickable matches by position.
        let any_button =
            ElementSelector { text_regex: Some(".+".into()), index: Some("1".into()), ..Default::default() };
        assert_eq!(h.find(&any_button).unwrap().unwrap().text.as_deref(), Some("While using the app"));
    }

    #[test]
    fn relative_selectors_and_state_flags() {
        let h = dialog();
        let below = ElementSelector {
            text_regex: Some(".*(allow|time).*".into()),
            below: Some(Box::new(text_sel("While using the app"))),
            ..Default::default()
        };
        assert_eq!(h.find(&below).unwrap().unwrap().text.as_deref(), Some("Only this time"));
        let above = ElementSelector {
            text_regex: Some("While using the app".into()),
            above: Some(Box::new(text_sel("Don't allow"))),
            ..Default::default()
        };
        assert!(h.find(&above).unwrap().is_some());
        let disabled =
            ElementSelector { text_regex: Some("Don't allow".into()), enabled: Some(false), ..Default::default() };
        assert!(h.find(&disabled).unwrap().is_none());
        let checked =
            ElementSelector { text_regex: Some("Only this time".into()), checked: Some(false), ..Default::default() };
        assert!(h.find(&checked).unwrap().is_some());
    }

    #[test]
    fn deepest_matching_element_and_clickable_first() {
        let xml = r#"<hierarchy rotation="0">
  <node text="" class="A" clickable="true" enabled="true" bounds="[0,0][1000,200]">
    <node text="Login" class="B" clickable="false" enabled="true" bounds="[10,10][400,100]"/>
  </node>
  <node text="Login" class="C" clickable="true" enabled="true" bounds="[0,300][1000,500]"/>
</hierarchy>"#;
        let h = Hierarchy::parse(xml, 1080, 2400).unwrap();
        // Both "Login" nodes match; the clickable one is preferred (`clickableFirst`).
        let hit = h.find(&text_sel("Login")).unwrap().unwrap();
        assert_eq!(hit.bounds, Some(Bounds::new(0, 300, 1000, 500)));

        // A parent whose text also matches yields to its deepest matching child.
        let nested = r#"<hierarchy rotation="0">
  <node text="Save" clickable="true" enabled="true" bounds="[0,0][500,200]">
    <node text="Save" clickable="false" enabled="true" bounds="[10,10][100,100]"/>
  </node>
</hierarchy>"#;
        let h = Hierarchy::parse(nested, 1080, 2400).unwrap();
        let hit = h.find(&text_sel("Save")).unwrap().unwrap();
        assert_eq!(hit.bounds, Some(Bounds::new(10, 10, 100, 100)));
    }

    #[test]
    fn matches_hint_content_desc_and_error_text() {
        let xml = r#"<hierarchy rotation="0">
  <node text="" hintText="Email address" class="android.widget.EditText" enabled="true" focused="true" bounds="[0,0][500,100]"/>
  <node text="" content-desc="Open menu" clickable="true" enabled="true" bounds="[0,200][100,300]"/>
  <node text="" error="Required field" enabled="true" bounds="[0,400][500,500]"/>
</hierarchy>"#;
        let h = Hierarchy::parse(xml, 1080, 2400).unwrap();
        assert!(h.find(&text_sel("Email address")).unwrap().is_some());
        assert!(h.find(&text_sel("Open menu")).unwrap().is_some());
        assert!(h.find(&text_sel("Required field")).unwrap().is_some());
        let focused = ElementSelector { focused: Some(true), ..Default::default() };
        assert_eq!(h.find(&focused).unwrap().unwrap().hint_text.as_deref(), Some("Email address"));
    }

    #[test]
    fn newlines_numeric_entities_and_off_screen_pruning() {
        let xml = "<hierarchy rotation=\"0\">\
  <node text=\"Line one&#10;line two &amp; more\" enabled=\"true\" bounds=\"[0,0][500,100]\"/>\
  <node text=\"Way below\" enabled=\"true\" bounds=\"[0,5000][500,5100]\"/>\
</hierarchy>";
        let h = Hierarchy::parse(xml, 1080, 2400).unwrap();
        // `\n` is matched as a space, like `Filters.textMatches`.
        assert!(h.find(&text_sel("Line one line two & more")).unwrap().is_some());
        // Elements fully outside the screen are pruned (`filterOutOfBounds`).
        assert!(h.find(&text_sel("Way below")).unwrap().is_none());
        assert_eq!(h.visible_texts(10), vec!["Line one\nline two & more".to_string()]);
    }

    #[test]
    fn detects_gboard_keyboard_and_toasts_survive_bounds_filtering() {
        let xml = r#"<hierarchy rotation="0">
  <node text="" resource-id="com.google.android.inputmethod.latin:id/key_pos_0_0" enabled="true" bounds="[0,1500][100,1600]"/>
  <node text="Saved!" class="android.widget.Toast" enabled="true" bounds="[0,9000][100,9100]"/>
</hierarchy>"#;
        let h = Hierarchy::parse(xml, 1080, 2400).unwrap();
        assert!(h.keyboard_visible());
        assert!(h.find(&text_sel("Saved!")).unwrap().is_some(), "toast kept despite being off-screen");
        let none = Hierarchy::parse("<hierarchy rotation=\"0\"/>", 1080, 2400).unwrap();
        assert!(!none.keyboard_visible());
    }

    #[test]
    fn empty_xml_attributes_become_none() {
        let h = dialog();
        let frame = &h.root.children[0];
        assert_eq!(frame.text, None);
        assert_eq!(frame.id, None);
        assert_eq!(frame.accessibility_text, None);
        assert_eq!(frame.resolved_copy_text(), None);
        // Empty values are `None`, but the attribute itself was present.
        assert!(frame.has_text_attr);
        assert!(!h.root.has_text_attr, "<hierarchy> has no text attribute");
    }

    #[test]
    fn text_trait_tests_attribute_presence_like_maestro() {
        use crate::flow::ir::ElementTrait;
        // `Filters.hasText` is `attributes["text"] != null`: every UiAutomator node
        // carries `text` (even `""`), so a Flutter label that only has `content-desc`
        // (here "Traits Ready") still has the TEXT trait. Verified against Maestro 2.12.1.
        let xml = r#"<hierarchy rotation="0">
  <node text="" content-desc="Traits Ready" resource-id="" class="android.view.View" bounds="[48,367][1296,427]" />
  <node text="" content-desc="" resource-id="container" class="android.view.View" bounds="[48,500][1296,600]" />
</hierarchy>"#;
        let h = Hierarchy::parse(xml, 1344, 2992).unwrap();
        let text_trait = Some(vec![ElementTrait::Text]);
        let label = ElementSelector {
            text_regex: Some("Traits Ready".into()),
            traits: text_trait.clone(),
            ..Default::default()
        };
        assert!(h.find(&label).unwrap().is_some());
        let container =
            ElementSelector { id_regex: Some("container".into()), traits: text_trait, ..Default::default() };
        assert!(h.find(&container).unwrap().is_some(), "an empty `text` attribute still counts");
        // LONG_TEXT only looks at the (non-empty) text length.
        let long = ElementSelector {
            text_regex: Some("Traits Ready".into()),
            traits: Some(vec![ElementTrait::LongText]),
            ..Default::default()
        };
        assert!(h.find(&long).unwrap().is_none());
    }

    #[test]
    fn is_visible_means_topmost_at_the_centre_like_maestro() {
        let xml = r#"<hierarchy rotation="0">
  <node text="Below" bounds="[0,0][100,100]"/>
  <node text="Above" bounds="[0,0][100,100]"/>
  <node text="Parent" bounds="[0,200][100,300]"><node text="Child" bounds="[0,200][100,300]"/></node>
  <node text="Alone" bounds="[0,400][100,500]"/>
</hierarchy>"#;
        let h = Hierarchy::parse(xml, 1080, 2400).unwrap();
        let get = |t: &str| h.find(&text_sel(t)).unwrap().unwrap();
        assert!(!h.is_visible(&get("Below")), "a later sibling is drawn over it");
        assert!(h.is_visible(&get("Above")));
        assert!(!h.is_visible(&get("Parent")), "descendants win over their parent");
        assert!(h.is_visible(&get("Child")));
        assert!(h.is_visible(&get("Alone")));
    }

    #[test]
    fn rejects_malformed_xml() {
        assert!(parse_hierarchy("<hierarchy><node></hierarchy>").is_err());
        assert!(parse_hierarchy("<hierarchy><node").is_err());
        assert!(parse_hierarchy("").is_err());
    }

    #[test]
    fn signature_changes_with_the_screen() {
        let a =
            Hierarchy::parse("<hierarchy><node text=\"A\" bounds=\"[0,0][10,10]\"/></hierarchy>", 100, 100).unwrap();
        let b =
            Hierarchy::parse("<hierarchy><node text=\"B\" bounds=\"[0,0][10,10]\"/></hierarchy>", 100, 100).unwrap();
        assert_ne!(a.signature(), b.signature());
        assert_eq!(a.signature(), a.clone().signature());
    }
}
