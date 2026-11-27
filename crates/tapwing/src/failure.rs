//! F8 rich failure reports: "Did you mean" selector suggestions, compact UI
//! tree dumps, and failure artifact naming/formatting.
//!
//! Pure core module: takes [`TreeNode`] snapshots (produced by the Flutter
//! driver, Android/iOS native lanes, or test doubles)
//! and failed [`ElementSelector`]s with zero device I/O.

use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::flow::ir::{ElementSelector, MaestroCommand};
use crate::ui_tree::{Bounds, TreeNode};

/// Maximum hierarchy lines shown inline in console output.
pub const CONSOLE_HIERARCHY_MAX_LINES: usize = 25;

/// Maximum hierarchy lines stored inline in JSON/HTML/JUnit reports (full dump
/// is still written to `<test-output-dir>/failures/`).
pub const REPORT_HIERARCHY_MAX_LINES: usize = 80;

/// Maximum "Did you mean" suggestions attached to a failed selector step.
pub const MAX_SUGGESTIONS: usize = 5;

/// Maximum length of a single text/label attribute in the compact tree dump.
const MAX_ATTR_CHARS: usize = 120;

/// Rich diagnostics captured once at the failing leaf step of a flow.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureDiagnostics {
    /// Closest matching elements on screen when a selector lookup failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    /// Compact, pruned, indented UI hierarchy at the moment of failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy: Option<String>,
    /// Lane that produced [`Self::hierarchy`] (`"flutter"`, `"native"`, `"fake"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy_source: Option<String>,
    /// Total meaningful UI elements in the captured tree (before truncation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_elements: Option<usize>,
    /// Path to the failure screenshot PNG on disk, when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// Path to the full untruncated UI tree dump `.txt` file on disk, when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy_file: Option<String>,
}

impl FailureDiagnostics {
    /// Whether any diagnostic field was captured.
    pub fn is_empty(&self) -> bool {
        self.suggestions.is_empty()
            && self.hierarchy.is_none()
            && self.screenshot.is_none()
            && self.hierarchy_file.is_none()
    }
}

/// One candidate element suggested for a failed selector ("Did you mean …").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    /// Attribute name (`"text"`, `"accessibilityText"`, `"hintText"`, or `"id"`).
    pub field: String,
    /// Actual string value found on screen.
    pub value: String,
    /// Human-readable reason explaining why it did not match the selector.
    pub reason: String,
}

/// Converts a Flutter `tapwing_driver` `snapshot` RPC response
/// (`{"screen": [w, h], "root": {...}}`) into the shared [`TreeNode`] tree.
pub fn tree_from_flutter_snapshot(snap: &Value) -> Option<TreeNode> {
    let root = snap.get("root").filter(|v| v.is_object())?;
    let mut next_uid = 1u64;
    Some(parse_flutter_node(root, &mut next_uid))
}

fn parse_flutter_node(v: &Value, next_uid: &mut u64) -> TreeNode {
    let uid = v.get("id").and_then(Value::as_u64).unwrap_or_else(|| {
        let u = *next_uid;
        *next_uid += 1;
        u
    });
    let non_empty = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let bounds = v.get("bounds").and_then(Value::as_array).and_then(|arr| {
        if arr.len() != 4 {
            return None;
        }
        let l = arr[0].as_f64()?.round() as i32;
        let t = arr[1].as_f64()?.round() as i32;
        let r = arr[2].as_f64()?.round() as i32;
        let b = arr[3].as_f64()?.round() as i32;
        Some(Bounds::new(l, t, r, b))
    });
    let children = v
        .get("children")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(|c| parse_flutter_node(c, next_uid)).collect())
        .unwrap_or_default();
    // Flutter `KNode.toJson` omits `"enabled": true` and only writes `"enabled": false`.
    let enabled = Some(v.get("enabled").and_then(Value::as_bool).unwrap_or(true));
    TreeNode {
        uid,
        id: non_empty("resource-id"),
        has_text_attr: v.get("text").is_some_and(|t| !t.is_null()),
        text: non_empty("text"),
        hint_text: non_empty("hintText"),
        accessibility_text: non_empty("accessibilityText"),
        error_text: non_empty("error"),
        bounds,
        clickable: v.get("clickable").and_then(Value::as_bool).unwrap_or(false),
        enabled,
        selected: v.get("selected").and_then(Value::as_bool),
        checked: v.get("checked").and_then(Value::as_bool),
        focused: v.get("focused").and_then(Value::as_bool),
        matches_css_filter: None,
        ignore_bounds_filtering: false,
        children,
    }
}

/// Extracts the missing [`ElementSelector`] from an evaluated failing command,
/// when "Did you mean" suggestions make sense (i.e. lookup failed to find a
/// matching element; `assertNotVisible` is intentionally excluded).
pub fn extract_failed_selector(cmd: &MaestroCommand) -> Option<&ElementSelector> {
    match cmd {
        MaestroCommand::TapOnElement(c) => Some(&c.selector),
        MaestroCommand::AssertCondition(c) => c.condition.visible.as_ref(),
        MaestroCommand::CopyTextFrom(c) => Some(&c.selector),
        MaestroCommand::Swipe(c) => c.element_selector.as_ref(),
        MaestroCommand::ScrollUntilVisible(c) => Some(&c.selector),
        MaestroCommand::TakeScreenshot(c) => c.crop_on.as_ref(),
        MaestroCommand::AssertScreenshot(c) => c.crop_on.as_ref(),
        _ => None,
    }
}

/// Strips common regex anchors/wildcards (`^`, `$`, `.*`, `.+`) so substring
/// and fuzzy matching stay useful when flows use regex selectors.
fn normalize_selector_query(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        let before = s;
        if let Some(rest) = s.strip_prefix('^') {
            s = rest;
        }
        if let Some(rest) = s.strip_suffix('$') {
            s = rest;
        }
        if let Some(rest) = s.strip_prefix(".*") {
            s = rest;
        }
        if let Some(rest) = s.strip_suffix(".*") {
            s = rest;
        }
        if let Some(rest) = s.strip_prefix(".+") {
            s = rest;
        }
        if let Some(rest) = s.strip_suffix(".+") {
            s = rest;
        }
        if s == before {
            break;
        }
    }
    s.trim().to_string()
}

/// Returns candidate suggestions explaining why `selector` did not match any
/// element in `root` (state mismatches, cross-attribute hits, substring /
/// full-match regex misses, and Levenshtein typos).
pub fn suggest_similar(root: &TreeNode, selector: &ElementSelector, max_suggestions: usize) -> Vec<Suggestion> {
    if max_suggestions == 0 {
        return Vec::new();
    }
    let nodes = root.aggregate();
    let mut scored: Vec<(u8, usize, Suggestion)> = Vec::new();

    // Priority tiers (lower = better):
    // 0: Same text/id matched a node, but state flags / index / relative filters rejected it.
    // 1: Exact or case-insensitive match in the OTHER attribute (text vs id).
    // 2: Substring / full-match regex miss (very common in Maestro).
    // 3: Fuzzy Levenshtein typo match.

    if let Some(raw_text) = selector.text_regex.as_deref().filter(|s| !s.is_empty()) {
        collect_text_suggestions(&nodes, selector, raw_text, &mut scored);
    }
    if let Some(raw_id) = selector.id_regex.as_deref().filter(|s| !s.is_empty()) {
        collect_id_suggestions(&nodes, selector, raw_id, &mut scored);
    }

    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.value.cmp(&b.2.value)));

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (_, _, suggestion) in scored {
        let key = (suggestion.field.clone(), suggestion.value.clone());
        if seen.insert(key) {
            out.push(suggestion);
            if out.len() >= max_suggestions {
                break;
            }
        }
    }
    out
}

fn collect_text_suggestions(
    nodes: &[TreeNode],
    selector: &ElementSelector,
    raw_text: &str,
    scored: &mut Vec<(u8, usize, Suggestion)>,
) {
    let query = normalize_selector_query(raw_text);
    let query_lower = query.to_lowercase();
    let regex = compile_maestro_regex(raw_text);
    let text_matched_count = nodes.iter().filter(|n| node_text_matches(n, raw_text, regex.as_ref())).count();

    for node in nodes {
        let text_fields = [
            ("text", node.text.as_deref()),
            ("accessibilityText", node.accessibility_text.as_deref()),
            ("hintText", node.hint_text.as_deref()),
        ];

        // 0. Text actually matched this node — explain which other filter rejected it.
        if let Some((field, val)) = text_fields
            .iter()
            .find_map(|(f, v)| v.filter(|s| str_matches(s, raw_text, regex.as_ref())).map(|s| (*f, s)))
        {
            if let Some(reason) = explain_filter_mismatch(node, selector, text_matched_count) {
                scored.push((
                    0,
                    0,
                    Suggestion { field: field.to_string(), value: compact_str(val, MAX_ATTR_CHARS), reason },
                ));
            }
            continue;
        }

        // 1. Cross-attribute hit: the requested `text` actually lives in `resource-id`.
        if let Some(id_val) = node.id.as_deref()
            && str_matches(id_val, raw_text, regex.as_ref())
        {
            scored.push((
                1,
                0,
                Suggestion {
                    field: "id".to_string(),
                    value: compact_str(id_val, MAX_ATTR_CHARS),
                    reason: "found in id, not text (try `id:` instead)".to_string(),
                },
            ));
        }

        if query_lower.is_empty() {
            continue;
        }

        // 2 & 3. Substring (full-match regex trap) or Levenshtein typo against text fields.
        for (field, maybe_val) in text_fields {
            let Some(val) = maybe_val else { continue };
            classify_string_similarity(field, val, &query, &query_lower, true, scored);
        }
    }
}

fn collect_id_suggestions(
    nodes: &[TreeNode],
    selector: &ElementSelector,
    raw_id: &str,
    scored: &mut Vec<(u8, usize, Suggestion)>,
) {
    let query = normalize_selector_query(raw_id);
    let query_lower = query.to_lowercase();
    let regex = compile_maestro_regex(raw_id);
    let id_matched_count =
        nodes.iter().filter(|n| n.id.as_deref().is_some_and(|s| str_matches(s, raw_id, regex.as_ref()))).count();

    for node in nodes {
        // 0. Id matched this node — explain which other filter rejected it.
        if let Some(id_val) = node.id.as_deref() {
            if str_matches(id_val, raw_id, regex.as_ref()) {
                if let Some(reason) = explain_filter_mismatch(node, selector, id_matched_count) {
                    scored.push((
                        0,
                        0,
                        Suggestion { field: "id".to_string(), value: compact_str(id_val, MAX_ATTR_CHARS), reason },
                    ));
                }
            } else if !query_lower.is_empty() {
                classify_string_similarity("id", id_val, &query, &query_lower, false, scored);
            }
        }

        // 1. Cross-attribute hit: requested `id` actually matches visible text.
        for (field, maybe_val) in [
            ("text", node.text.as_deref()),
            ("accessibilityText", node.accessibility_text.as_deref()),
            ("hintText", node.hint_text.as_deref()),
        ] {
            if let Some(val) = maybe_val
                && str_matches(val, raw_id, regex.as_ref())
            {
                scored.push((
                    1,
                    0,
                    Suggestion {
                        field: field.to_string(),
                        value: compact_str(val, MAX_ATTR_CHARS),
                        reason: format!("found in {field}, not id (try `text:` instead)"),
                    },
                ));
            }
        }
    }
}

fn classify_string_similarity(
    field: &str,
    candidate: &str,
    query: &str,
    query_lower: &str,
    is_text: bool,
    scored: &mut Vec<(u8, usize, Suggestion)>,
) {
    let cand_flat = candidate.replace('\n', " ");
    let cand_lower = cand_flat.to_lowercase();

    // Exact match after trimming whitespace.
    if cand_lower.trim() == query_lower {
        scored.push((
            2,
            0,
            Suggestion {
                field: field.to_string(),
                value: compact_str(candidate, MAX_ATTR_CHARS),
                reason: "differs only in surrounding whitespace".to_string(),
            },
        ));
        return;
    }

    // Substring match (Maestro regexes are full-match, so partial strings fail).
    if query_lower.chars().count() >= 2 && cand_lower.contains(query_lower) {
        let diff = cand_lower.chars().count().saturating_sub(query_lower.chars().count());
        let hint = if is_text {
            format!("contains \"{query}\" (Maestro matches full string — try \".*{query}.*\")")
        } else {
            format!("contains \"{query}\" (try \".*{query}.*\")")
        };
        scored.push((
            2,
            diff,
            Suggestion { field: field.to_string(), value: compact_str(candidate, MAX_ATTR_CHARS), reason: hint },
        ));
        return;
    }
    if cand_lower.chars().count() >= 3 && query_lower.contains(&cand_lower) {
        let diff = query_lower.chars().count().saturating_sub(cand_lower.chars().count());
        scored.push((
            2,
            diff + 10,
            Suggestion {
                field: field.to_string(),
                value: compact_str(candidate, MAX_ATTR_CHARS),
                reason: "shorter prefix/substring of selector".to_string(),
            },
        ));
        return;
    }

    // Fuzzy typo match via case-insensitive Levenshtein distance.
    let q_len = query_lower.chars().count();
    let c_len = cand_lower.chars().count();
    if q_len < 2 || c_len < 2 {
        return;
    }
    let max_dist = (q_len / 3).clamp(1, 3);
    if q_len.abs_diff(c_len) > max_dist {
        return;
    }
    let dist = levenshtein(query_lower, &cand_lower);
    if dist > 0 && dist <= max_dist {
        scored.push((
            3,
            dist,
            Suggestion {
                field: field.to_string(),
                value: compact_str(candidate, MAX_ATTR_CHARS),
                reason: format!("similar (edit distance {dist})"),
            },
        ));
    }
}

fn explain_filter_mismatch(node: &TreeNode, sel: &ElementSelector, total_matches: usize) -> Option<String> {
    let mut reasons = Vec::new();
    if let Some(want) = sel.enabled
        && node.enabled != Some(want)
    {
        reasons.push(format!("enabled={}, expected {want}", fmt_opt_bool(node.enabled)));
    }
    if let Some(want) = sel.checked
        && node.checked != Some(want)
    {
        reasons.push(format!("checked={}, expected {want}", fmt_opt_bool(node.checked)));
    }
    if let Some(want) = sel.selected
        && node.selected != Some(want)
    {
        reasons.push(format!("selected={}, expected {want}", fmt_opt_bool(node.selected)));
    }
    if let Some(want) = sel.focused
        && node.focused != Some(want)
    {
        reasons.push(format!("focused={}, expected {want}", fmt_opt_bool(node.focused)));
    }
    if let Some(raw_idx) = sel.index.as_deref()
        && let Ok(idx) = crate::backend::parse_index(raw_idx)
        && (idx >= total_matches as i64 || idx < -(total_matches as i64))
    {
        reasons.push(format!("requested index {idx}, but only {total_matches} matching element(s) exist"));
    }
    if reasons.is_empty()
        && (sel.below.is_some()
            || sel.above.is_some()
            || sel.left_of.is_some()
            || sel.right_of.is_some()
            || sel.child_of.is_some()
            || sel.contains_child.is_some()
            || sel.contains_descendants.is_some()
            || sel.size.is_some()
            || sel.traits.is_some())
    {
        reasons.push("text/id matched, but another selector filter (relative/size/traits) did not".to_string());
    }
    if reasons.is_empty() { None } else { Some(reasons.join("; ")) }
}

fn fmt_opt_bool(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "true",
        Some(false) => "false",
        None => "unset",
    }
}

fn compile_maestro_regex(pattern: &str) -> Option<fancy_regex::Regex> {
    fancy_regex::Regex::new(&format!("(?ims)^(?:{pattern})$")).ok()
}

fn str_matches(candidate: &str, raw: &str, re: Option<&fancy_regex::Regex>) -> bool {
    if candidate == raw || candidate.replace('\n', " ") == raw {
        return true;
    }
    if let Some(r) = re
        && (r.is_match(candidate).unwrap_or(false) || r.is_match(&candidate.replace('\n', " ")).unwrap_or(false))
    {
        return true;
    }
    false
}

fn node_text_matches(node: &TreeNode, raw: &str, re: Option<&fancy_regex::Regex>) -> bool {
    [node.text.as_deref(), node.accessibility_text.as_deref(), node.hint_text.as_deref()]
        .into_iter()
        .flatten()
        .any(|s| str_matches(s, raw, re))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Renders a compact, pruned, indented UI hierarchy suitable for console logs,
/// HTML/JUnit reports, and `.txt` artifacts.
///
/// Pass-through container nodes without text, labels, ids, or notable state are
/// collapsed so every line carries signal. Returns `(formatted_tree, total_meaningful_nodes)`.
pub fn format_hierarchy(root: &TreeNode, max_lines: Option<usize>) -> (String, usize) {
    let mut lines = Vec::new();
    collect_hierarchy_lines(root, 0, &mut lines);
    let total = lines.len();
    if total == 0 {
        return ("(empty UI tree)".to_string(), 0);
    }
    let Some(limit) = max_lines else {
        return (lines.join("\n"), total);
    };
    if total <= limit {
        return (lines.join("\n"), total);
    }
    let mut out = lines[..limit].join("\n");
    let remaining = total - limit;
    let _ = write!(out, "\n… and {remaining} more element{}", if remaining == 1 { "" } else { "s" });
    (out, total)
}

fn is_meaningful_node(node: &TreeNode) -> bool {
    node.text.is_some()
        || node.accessibility_text.is_some()
        || node.hint_text.is_some()
        || node.id.is_some()
        || node.error_text.is_some()
        || node.clickable
        || node.focused == Some(true)
        || node.checked == Some(true)
        || node.selected == Some(true)
        || node.enabled == Some(false)
}

fn collect_hierarchy_lines(node: &TreeNode, depth: usize, out: &mut Vec<String>) {
    let meaningful = is_meaningful_node(node);
    if meaningful {
        let indent = "  ".repeat(depth);
        out.push(format!("{indent}- {}", format_node_summary(node)));
    }
    let next_depth = if meaningful { depth + 1 } else { depth };
    for child in &node.children {
        collect_hierarchy_lines(child, next_depth, out);
    }
}

fn format_node_summary(node: &TreeNode) -> String {
    let mut parts = Vec::new();
    if let Some(t) = node.text.as_deref() {
        parts.push(format!("text=\"{}\"", compact_str(t, MAX_ATTR_CHARS)));
    }
    if let Some(a) = node.accessibility_text.as_deref() {
        parts.push(format!("a11y=\"{}\"", compact_str(a, MAX_ATTR_CHARS)));
    }
    if let Some(h) = node.hint_text.as_deref() {
        parts.push(format!("hint=\"{}\"", compact_str(h, MAX_ATTR_CHARS)));
    }
    if let Some(err) = node.error_text.as_deref() {
        parts.push(format!("error=\"{}\"", compact_str(err, MAX_ATTR_CHARS)));
    }
    if let Some(id) = node.id.as_deref() {
        parts.push(format!("[id=\"{}\"]", compact_str(id, MAX_ATTR_CHARS)));
    }
    if let Some(b) = node.bounds {
        parts.push(format!("[{},{}][{},{}]", b.left, b.top, b.right, b.bottom));
    }
    let mut flags = Vec::new();
    if node.clickable {
        flags.push("clickable");
    }
    if node.enabled == Some(false) {
        flags.push("disabled");
    }
    if node.focused == Some(true) {
        flags.push("focused");
    }
    if node.checked == Some(true) {
        flags.push("checked");
    }
    if node.selected == Some(true) {
        flags.push("selected");
    }
    if !flags.is_empty() {
        parts.push(format!("({})", flags.join(", ")));
    }
    if parts.is_empty() { "(node)".to_string() } else { parts.join(" ") }
}

fn compact_str(raw: &str, max_chars: usize) -> String {
    let escaped = raw.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
    let count = escaped.chars().count();
    if count <= max_chars {
        escaped
    } else {
        let mut s: String = escaped.chars().take(max_chars.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

/// Maximum length of the human-readable slug prefix in [`artifact_stem`].
const ARTIFACT_SLUG_MAX_CHARS: usize = 64;

/// Formats a filesystem-safe, collision-resistant artifact stem
/// (`<flow_slug>_<path_hash>_step<NNN>`).
pub fn artifact_stem(flow_path: &Path, step_index: usize) -> String {
    let raw = flow_path.file_stem().and_then(|s| s.to_str()).unwrap_or("flow");
    let sanitized: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(ARTIFACT_SLUG_MAX_CHARS)
        .collect();
    let trimmed = sanitized.trim_matches('_');
    let slug = if trimmed.chars().any(|c| c.is_ascii_alphanumeric()) { trimmed } else { "flow" };
    let hash = short_path_hash(flow_path);
    format!("{slug}_{hash}_step{:03}", step_index + 1)
}

fn short_path_hash(path: &Path) -> String {
    let mut hasher = Sha256::new();
    for comp in path.components() {
        if matches!(comp, std::path::Component::CurDir) {
            continue;
        }
        hasher.update(comp.as_os_str().as_encoded_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    format!("{:02x}{:02x}{:02x}{:02x}", digest[0], digest[1], digest[2], digest[3])
}

/// Formats failure diagnostics for terminal output (6-space indented under the
/// failing step line).
pub fn format_console(diag: &FailureDiagnostics) -> String {
    let mut lines = Vec::new();
    if !diag.suggestions.is_empty() {
        lines.push("      Did you mean:".to_string());
        for s in &diag.suggestions {
            lines.push(format!("        • {}={} ({})", s.field, format_quoted(&s.value), s.reason));
        }
    }
    if let Some(path) = &diag.screenshot {
        lines.push(format!("      Screenshot: {path}"));
    }
    if let Some(tree) = &diag.hierarchy {
        let mut meta = Vec::new();
        if let Some(src) = &diag.hierarchy_source {
            meta.push(src.clone());
        }
        if let Some(n) = diag.total_elements {
            meta.push(format!("{n} element{}", if n == 1 { "" } else { "s" }));
        }
        if let Some(file) = &diag.hierarchy_file {
            meta.push(format!("full dump: {file}"));
        }
        let header = if meta.is_empty() {
            "      UI tree:".to_string()
        } else {
            format!("      UI tree ({}):", meta.join(", "))
        };
        lines.push(header);
        let tree_lines: Vec<&str> = tree.lines().collect();
        let truncated = tree_lines.len() > CONSOLE_HIERARCHY_MAX_LINES;
        let shown = if truncated { &tree_lines[..CONSOLE_HIERARCHY_MAX_LINES] } else { &tree_lines[..] };
        for l in shown {
            lines.push(format!("        {l}"));
        }
        if truncated {
            let extra = tree_lines.len() - CONSOLE_HIERARCHY_MAX_LINES;
            lines.push(format!("        … ({extra} more line(s) omitted from console)"));
        }
    }
    lines.join("\n")
}

/// Formats failure diagnostics for JUnit `<system-err>`.
pub fn format_system_err(diag: &FailureDiagnostics) -> String {
    let mut lines = vec!["Failure diagnostics:".to_string()];
    if !diag.suggestions.is_empty() {
        lines.push("Did you mean:".to_string());
        for s in &diag.suggestions {
            lines.push(format!("  - {}={} ({})", s.field, format_quoted(&s.value), s.reason));
        }
    }
    if let Some(path) = &diag.screenshot {
        lines.push(format!("Screenshot: {path}"));
    }
    if let Some(file) = &diag.hierarchy_file {
        lines.push(format!("Hierarchy file: {file}"));
    }
    if let Some(tree) = &diag.hierarchy {
        let mut meta = Vec::new();
        if let Some(src) = &diag.hierarchy_source {
            meta.push(src.clone());
        }
        if let Some(n) = diag.total_elements {
            meta.push(format!("{n} elements"));
        }
        if meta.is_empty() {
            lines.push("UI tree:".to_string());
        } else {
            lines.push(format!("UI tree ({}):", meta.join(", ")));
        }
        lines.push(tree.clone());
    }
    lines.join("\n")
}

fn format_quoted(v: &str) -> String {
    format!("\"{v}\"")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn node(uid: u64) -> TreeNode {
        TreeNode {
            uid,
            id: None,
            text: None,
            has_text_attr: false,
            hint_text: None,
            accessibility_text: None,
            error_text: None,
            bounds: Some(Bounds::new(0, 0, 100, 40)),
            clickable: false,
            enabled: Some(true),
            selected: None,
            checked: None,
            focused: None,
            matches_css_filter: None,
            ignore_bounds_filtering: false,
            children: Vec::new(),
        }
    }

    fn sel_text(t: &str) -> ElementSelector {
        ElementSelector { text_regex: Some(t.to_string()), ..Default::default() }
    }

    fn sel_id(id: &str) -> ElementSelector {
        ElementSelector { id_regex: Some(id.to_string()), ..Default::default() }
    }

    #[test]
    fn suggests_substring_typo_cross_attr_and_state_mismatch() {
        let mut root = node(1);
        root.bounds = Some(Bounds::new(0, 0, 1080, 2400));

        let mut google = node(2);
        google.text = Some("Sign in with Google".into());
        google.clickable = true;

        let mut typo = node(3);
        typo.text = Some("Submit".into());
        typo.enabled = Some(false);

        let mut id_only = node(4);
        id_only.id = Some("login_btn".into());
        id_only.accessibility_text = Some("Continue".into());

        root.children = vec![google, typo, id_only];

        // 1. Substring / Maestro full-match regex trap
        let s = suggest_similar(&root, &sel_text("Sign in"), 5);
        assert_eq!(s[0].field, "text");
        assert_eq!(s[0].value, "Sign in with Google");
        assert!(s[0].reason.contains("full string"), "{:?}", s[0]);

        // 2. Typo (Levenshtein)
        let s = suggest_similar(&root, &sel_text("Submt"), 5);
        assert_eq!(s[0].value, "Submit");
        assert!(s[0].reason.contains("edit distance 1"), "{:?}", s[0]);

        // 3. State mismatch (text matches, enabled=false)
        let mut sel = sel_text("Submit");
        sel.enabled = Some(true);
        let s = suggest_similar(&root, &sel, 5);
        assert_eq!(s[0].value, "Submit");
        assert!(s[0].reason.contains("enabled=false, expected true"), "{:?}", s[0]);

        // 4. Cross-attribute hit (looked for text="login_btn", exists in id)
        let s = suggest_similar(&root, &sel_text("login_btn"), 5);
        assert_eq!(s[0].field, "id");
        assert_eq!(s[0].value, "login_btn");
        assert!(s[0].reason.contains("found in id"), "{:?}", s[0]);

        // 5. Cross-attribute hit (looked for id="Continue", exists in accessibilityText)
        let s = suggest_similar(&root, &sel_id("Continue"), 5);
        assert_eq!(s[0].field, "accessibilityText");
        assert_eq!(s[0].value, "Continue");
        assert!(s[0].reason.contains("not id"), "{:?}", s[0]);
    }

    #[test]
    fn parses_flutter_snapshot_json_and_prunes_empty_wrappers() {
        let snap = json!({
            "screen": [1080, 2400],
            "root": {
                "id": 0,
                "bounds": [0, 0, 1080, 2400],
                "children": [
                    {
                        "id": 1,
                        "bounds": [0, 0, 1080, 2400],
                        "children": [
                            {
                                "id": 2,
                                "text": "Hello\nWorld",
                                "resource-id": "greeting",
                                "bounds": [10, 20, 300, 80],
                                "clickable": true,
                                "enabled": false
                            }
                        ]
                    }
                ]
            }
        });
        let root = tree_from_flutter_snapshot(&snap).expect("parsed tree");
        let (tree, total) = format_hierarchy(&root, Some(20));
        assert_eq!(total, 1, "empty wrapper nodes collapsed:\n{tree}");
        assert_eq!(tree, "- text=\"Hello\\nWorld\" [id=\"greeting\"] [10,20][300,80] (clickable, disabled)");
    }

    #[test]
    fn truncates_hierarchy_cleanly_at_max_lines() {
        let mut root = node(1);
        for i in 0..5 {
            let mut c = node(i + 2);
            c.text = Some(format!("Item {i}"));
            root.children.push(c);
        }
        let (tree, total) = format_hierarchy(&root, Some(2));
        assert_eq!(total, 5);
        assert!(tree.contains("- text=\"Item 0\""), "{tree}");
        assert!(tree.contains("- text=\"Item 1\""), "{tree}");
        assert!(tree.contains("… and 3 more elements"), "{tree}");
    }

    #[test]
    fn artifact_stem_sanitizes_flow_name() {
        let stem = artifact_stem(Path::new("/tmp/my flow (v2).yaml"), 3);
        assert!(stem.starts_with("my_flow__v2_"), "{stem}");
        assert!(stem.ends_with("_step004"), "{stem}");
        assert!(stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{stem}");
        assert_eq!(stem, artifact_stem(Path::new("/tmp/my flow (v2).yaml"), 3));
    }

    #[test]
    fn artifact_stem_distinguishes_same_basename_in_different_dirs() {
        let a = artifact_stem(Path::new("login/smoke.yaml"), 0);
        let b = artifact_stem(Path::new("checkout/smoke.yaml"), 0);
        let c = artifact_stem(Path::new("a/b/smoke.yaml"), 0);
        let d = artifact_stem(Path::new("a_b/smoke.yaml"), 0);
        assert!(a.starts_with("smoke_") && a.ends_with("_step001"), "{a}");
        assert!(b.starts_with("smoke_") && b.ends_with("_step001"), "{b}");
        assert_ne!(a, b);
        assert_ne!(c, d);
        assert_eq!(a, artifact_stem(Path::new("./login/smoke.yaml"), 0));
    }

    #[test]
    fn artifact_stem_distinguishes_non_ascii_flow_names() {
        let jp = artifact_stem(Path::new("flows/日本語.yaml"), 0);
        let kr = artifact_stem(Path::new("flows/한국어.yaml"), 0);
        let cafe1 = artifact_stem(Path::new("flows/café.yaml"), 0);
        let cafe2 = artifact_stem(Path::new("flows/cafè.yaml"), 0);
        for stem in [&jp, &kr, &cafe1, &cafe2] {
            assert!(stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{stem}");
            assert!(stem.ends_with("_step001"), "{stem}");
        }
        assert!(jp.starts_with("flow_"), "{jp}");
        assert!(kr.starts_with("flow_"), "{kr}");
        assert_ne!(jp, kr);
        assert_ne!(cafe1, cafe2);
    }
}
