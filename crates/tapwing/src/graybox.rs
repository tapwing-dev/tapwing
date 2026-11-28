//! Gray-box shortcut tracking and report metadata.
//!
//! Per `STRATEGY.md`, any shortcut that breaks the pure black-box flow
//! contract — in-app hooks (`callHook`), partial flow starts (`--start-from`),
//! or warm-app attach (`--attach`) — must be announced on stderr and flagged in
//! JSON, HTML, JUnit, and Trace reports so a gray-box run never masquerades as
//! a clean end-to-end black-box run.

use serde::Serialize;
use serde_json::{Value, json};

/// Metadata describing a partial flow run (`--start-from <step|label>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialRunInfo {
    /// Raw `--start-from` selector passed by the user (1-based index or label).
    pub start_from: String,
    /// 1-based top-level step number where execution began.
    pub resolved_step: usize,
    /// Number of leading top-level steps skipped.
    pub skipped_steps: usize,
    /// Whether `onFlowStart` was skipped because earlier steps were skipped.
    pub skipped_on_flow_start: bool,
}

/// Summary of all gray-box capabilities used during a single flow execution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrayboxInfo {
    /// Distinct hook names invoked via `callHook`, in first-use order.
    pub hooks_used: Vec<String>,
    /// Total number of `callHook` invocations attempted/executed.
    pub hook_calls: usize,
    /// Present when `--start-from` skipped one or more leading steps.
    pub partial_run: Option<PartialRunInfo>,
    /// True when the flow attached to an already-running app (`--attach` or
    /// auto-attach on `--start-from`).
    pub warm_attach: bool,
}

impl GrayboxInfo {
    /// Whether any gray-box shortcut was active during the flow.
    pub fn is_active(&self) -> bool {
        !self.hooks_used.is_empty() || self.partial_run.is_some() || self.warm_attach
    }

    /// Records a `callHook` invocation under `name`.
    pub fn record_hook(&mut self, name: &str) {
        self.hook_calls += 1;
        if !self.hooks_used.iter().any(|existing| existing == name) {
            self.hooks_used.push(name.to_string());
        }
    }

    /// Human-readable reasons suitable for JSON, HTML, JUnit, and Trace reports.
    pub fn reasons(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.hooks_used.is_empty() {
            let names = self.hooks_used.join(", ");
            if self.hooks_used.len() == 1 && self.hook_calls == 1 {
                out.push(format!("in-app hook: {names}"));
            } else {
                let plural = if self.hook_calls == 1 { "call" } else { "calls" };
                out.push(format!("in-app hooks: {names} ({} {plural})", self.hook_calls));
            }
        }
        if let Some(p) = &self.partial_run {
            let on_start_note = if p.skipped_on_flow_start { ", skipped onFlowStart" } else { "" };
            let step_word = if p.skipped_steps == 1 { "step" } else { "steps" };
            out.push(format!(
                "partial run: started from step {} (`{}`, skipped {} {step_word}{on_start_note})",
                p.resolved_step, p.start_from, p.skipped_steps
            ));
        }
        if self.warm_attach {
            out.push("warm attach: reused running app".to_string());
        }
        out
    }

    /// Compact badge label for HTML report subtitles.
    pub fn short_badge(&self) -> String {
        let mut parts = Vec::new();
        if !self.hooks_used.is_empty() {
            let label = if self.hooks_used.len() == 1 { "hook" } else { "hooks" };
            parts.push(format!("{label}: {}", self.hooks_used.join(", ")));
        }
        if let Some(p) = &self.partial_run {
            parts.push(format!("partial: step {}", p.resolved_step));
        }
        if self.warm_attach {
            parts.push("warm attach".to_string());
        }
        if parts.is_empty() { "gray-box".to_string() } else { format!("gray-box ({})", parts.join(", ")) }
    }

    /// Attaches gray-box fields to a JSON flow/trace object when active.
    ///
    /// Leaves clean black-box objects untouched.
    pub fn apply_to_json(&self, obj: &mut Value) {
        if !self.is_active() {
            return;
        }
        obj["graybox"] = json!(true);
        obj["grayboxReasons"] = json!(self.reasons());
        if !self.hooks_used.is_empty() {
            obj["hooksUsed"] = json!(self.hooks_used);
            obj["hookCalls"] = json!(self.hook_calls);
        }
        if let Some(partial) = &self.partial_run {
            obj["partialRun"] = serde_json::to_value(partial).unwrap_or(Value::Null);
        }
        if self.warm_attach {
            obj["warmAttach"] = json!(true);
        }
    }

    /// JUnit `<property>` pairs when active; empty for clean black-box flows.
    pub fn junit_properties(&self) -> Vec<(String, String)> {
        if !self.is_active() {
            return Vec::new();
        }
        let mut props = vec![("tapwing.graybox".to_string(), "true".to_string())];
        if !self.hooks_used.is_empty() {
            props.push(("tapwing.hooks".to_string(), self.hooks_used.join(", ")));
        }
        if let Some(p) = &self.partial_run {
            props.push(("tapwing.startFrom".to_string(), format!("step {} ({})", p.resolved_step, p.start_from)));
        }
        if self.warm_attach {
            props.push(("tapwing.warmAttach".to_string(), "true".to_string()));
        }
        props
    }

    /// JUnit `<system-err>` line when active; `None` for clean black-box flows.
    pub fn junit_system_err(&self) -> Option<String> {
        self.is_active().then(|| format!("Gray-box: {}", self.reasons().join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_graybox_leaves_json_and_junit_untouched() {
        let g = GrayboxInfo::default();
        assert!(!g.is_active());
        assert!(g.reasons().is_empty());
        assert!(g.junit_properties().is_empty());
        assert_eq!(g.junit_system_err(), None);

        let mut v = json!({ "status": "passed" });
        g.apply_to_json(&mut v);
        assert_eq!(v, json!({ "status": "passed" }));
    }

    #[test]
    fn active_graybox_renders_reasons_json_and_junit_metadata() {
        let mut g = GrayboxInfo::default();
        g.record_hook("seed_logged_in");
        g.record_hook("seed_logged_in");
        g.record_hook("open_cart");
        g.warm_attach = true;
        g.partial_run = Some(PartialRunInfo {
            start_from: "Cart screen".into(),
            resolved_step: 4,
            skipped_steps: 3,
            skipped_on_flow_start: true,
        });

        assert!(g.is_active());
        assert_eq!(g.short_badge(), "gray-box (hooks: seed_logged_in, open_cart, partial: step 4, warm attach)");
        assert_eq!(
            g.reasons(),
            vec![
                "in-app hooks: seed_logged_in, open_cart (3 calls)",
                "partial run: started from step 4 (`Cart screen`, skipped 3 steps, skipped onFlowStart)",
                "warm attach: reused running app",
            ]
        );

        let mut v = json!({ "status": "passed" });
        g.apply_to_json(&mut v);
        assert_eq!(v["graybox"], true);
        assert_eq!(v["hooksUsed"], json!(["seed_logged_in", "open_cart"]));
        assert_eq!(v["hookCalls"], 3);
        assert_eq!(v["warmAttach"], true);
        assert_eq!(v["partialRun"]["resolvedStep"], 4);
        assert_eq!(v["partialRun"]["skippedOnFlowStart"], true);

        assert_eq!(
            g.junit_properties(),
            vec![
                ("tapwing.graybox".into(), "true".into()),
                ("tapwing.hooks".into(), "seed_logged_in, open_cart".into()),
                ("tapwing.startFrom".into(), "step 4 (Cart screen)".into()),
                ("tapwing.warmAttach".into(), "true".into()),
            ]
        );
        assert!(g.junit_system_err().unwrap().starts_with("Gray-box: in-app hooks:"));
    }
}
