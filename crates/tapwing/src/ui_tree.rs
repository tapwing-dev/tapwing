//! UI hierarchy node model (`Bounds`, `TreeNode`) and Maestro 2.12.1
//! `Filters.kt` / `Orchestra.buildFilter` selector evaluation, shared by the
//! Android/iOS native lanes, failure diagnostics, and test doubles.

use std::collections::HashSet;

use anyhow::Result;
use fancy_regex::Regex;

use crate::backend::{MaestroError, parse_index, parse_point};
use crate::flow::ir::{Direction, ElementSelector, ElementTrait};

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

/// Immutable UI hierarchy snapshot for selector evaluation (shared by native
/// lanes, failure diagnostics, and test doubles).
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

    /// Whether a node outside `node`'s own subtree is drawn over `node` at its centre
    /// (distinguishes an external covering overlay from `node`'s own child label).
    pub(crate) fn is_covered_by_other(&self, node: &TreeNode) -> bool {
        let Some(bounds) = node.bounds else {
            return false;
        };
        let (x, y) = bounds.center();
        self.element_at(x, y).is_none_or(|top| !node.contains_uid(top.uid))
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

    /// Distinct visible texts (`text`, then `accessibility_text`), for error messages.
    pub(crate) fn visible_texts(&self, limit: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in self.aggregate() {
            for t in [n.text.as_deref(), n.accessibility_text.as_deref()].into_iter().flatten() {
                if !t.is_empty() && !out.iter().any(|o| o == t) {
                    out.push(t.to_string());
                }
            }
            if out.len() >= limit {
                out.truncate(limit);
                break;
            }
        }
        out
    }

    /// Structural fingerprint ignoring UIDs, used by `retryTapIfNoChange` and `settle`.
    /// Includes `accessibility_text` (`content-desc` on Android, `label` on iOS) and
    /// `hint_text` because Flutter semantics labels live in `content-desc` while
    /// `text` stays empty.
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
