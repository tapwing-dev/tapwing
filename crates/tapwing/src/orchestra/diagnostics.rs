//! F7 app-health attribution, F8 failure diagnostics capture, and F8b trace recording helpers.

use serde_json::{Value, json};

use crate::app_health::{self as health, AppErrorsPolicy};
use crate::backend::{FlowDriver, is_maestro_error, is_optional_downgradable};
use crate::failure::{self, FailureDiagnostics};
use crate::flow::ir::MaestroCommand;

use super::describe::is_optional;
use super::{Orchestra, print_prefixed};

impl<'a, D: FlowDriver + ?Sized> Orchestra<'a, D> {
    /// Records a step entry, attaching its pending F8b trace extras.
    pub(super) fn push_step(&mut self, entry: Value) {
        if let Some(extra) = self.pending_trace.take() {
            self.trace_extras.insert(self.step_index, extra);
        }
        self.steps.push(entry);
        self.step_index += 1;
    }

    /// F8b: lane, nesting, `retry` attempt, HTTP requests, and the optional
    /// per-step capture for the command that just ran. Requests started by
    /// earlier steps go to the last recorded step.
    pub(super) fn trace_step_extras(&mut self, cmd: &MaestroCommand, start_ms: f64, capture: Option<Value>) -> Value {
        let mut batch = self.driver.trace_step();
        self.trace_http_dropped += batch.dropped;
        let late = std::mem::take(&mut batch.late);
        self.attach_trace_http(late);
        let wrapper = matches!(cmd, MaestroCommand::RunFlow(_) | MaestroCommand::Repeat(_) | MaestroCommand::Retry(_));
        let mut extra = json!({ "startMs": start_ms, "depth": self.subflow_depth });
        if let Some(attempt) = self.retry_attempts.last() {
            extra["attempt"] = Value::from(*attempt);
        }
        if is_optional(cmd) {
            extra["optional"] = Value::Bool(true);
        }
        if !wrapper && let Some(lane) = batch.lane {
            extra["lane"] = Value::from(lane);
        }
        let mut http = batch.current;
        http.iter_mut().for_each(crate::trace::sanitize_http_entry);
        extra["http"] = Value::Array(http);
        if let Some(Value::Object(cap)) = capture {
            for (k, v) in cap {
                extra[k] = v;
            }
        }
        extra
    }

    /// F8b: appends HTTP entries to the trace extras of the last recorded step.
    fn attach_trace_http(&mut self, mut entries: Vec<Value>) {
        if entries.is_empty() {
            return;
        }
        entries.iter_mut().for_each(crate::trace::sanitize_http_entry);
        let index = self.steps.last().and_then(|s| s["index"].as_u64()).map_or(0, |i| i as usize);
        let extra = self.trace_extras.entry(index).or_insert_with(|| json!({ "startMs": 0.0, "depth": 0 }));
        let mut list = extra["http"].as_array().cloned().unwrap_or_default();
        list.extend(entries);
        extra["http"] = Value::Array(list);
    }

    /// F8b `--trace-screenshots steps`: screenshot (written to
    /// `<test-output-dir>/trace/`) and pruned UI tree after a step.
    pub(super) fn capture_trace_screen(&mut self) -> Option<Value> {
        let mut out = json!({});
        if let Some(dir) = self.opts.test_output_dir.clone()
            && let Some(png) = self.driver.capture_failure_screenshot()
        {
            let dir = dir.join(crate::trace::ASSETS_SUBDIR);
            let name = format!(
                "{}{}.png",
                crate::trace::asset_prefix(self.opts.shard_index, self.trace_ordinal),
                failure::artifact_stem(&self.opts.flow_path, self.step_index)
            );
            let path = dir.join(name);
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&path, png).is_ok() {
                out["screenshot"] = json!({ "path": path.to_string_lossy() });
            }
        }
        if let Some((source, root)) = self.driver.capture_failure_hierarchy() {
            let (tree, total) = failure::format_hierarchy(&root, Some(failure::REPORT_HIERARCHY_MAX_LINES));
            out["tree"] = Value::from(tree);
            out["treeSource"] = Value::from(source);
            out["treeElements"] = Value::from(total);
        }
        Some(out)
    }

    /// F8b: drains requests delivered after the last command (and those still
    /// in flight) into the last step, then merges step entries with their
    /// trace extras in start order.
    pub(super) fn finish_trace(&mut self) -> Value {
        let mut batch = self.driver.trace_drain();
        self.trace_http_dropped += batch.dropped;
        let mut tail = std::mem::take(&mut batch.late);
        tail.append(&mut batch.current);
        for mut entry in std::mem::take(&mut batch.in_flight) {
            entry["state"] = Value::from("inFlight");
            tail.push(entry);
        }
        self.attach_trace_http(tail);
        let mut steps: Vec<Value> = self
            .steps
            .iter()
            .map(|step| {
                let mut merged = step.clone();
                let index = step["index"].as_u64().unwrap_or(0) as usize;
                let extra =
                    self.trace_extras.get(&index).cloned().unwrap_or_else(|| json!({ "startMs": 0.0, "depth": 0 }));
                if let Value::Object(map) = extra {
                    for (k, v) in map {
                        merged[k] = v;
                    }
                }
                if merged.get("http").is_none() {
                    merged["http"] = json!([]);
                }
                merged
            })
            .collect();
        steps.sort_by(|a, b| {
            let (sa, sb) = (a["startMs"].as_f64().unwrap_or(0.0), b["startMs"].as_f64().unwrap_or(0.0));
            sa.total_cmp(&sb).then_with(|| a["index"].as_u64().cmp(&b["index"].as_u64()))
        });
        json!({
            "http": self.driver.trace_http_status(),
            "httpDropped": self.trace_http_dropped,
            "steps": steps,
        })
    }

    pub(super) fn record_dropped_issues(&mut self, dropped: u64) {
        if dropped == 0 {
            return;
        }
        self.dropped_app_issues += dropped;
        if self.opts.print_progress {
            print_prefixed(&self.opts.log_prefix, &format!("      {}", health::dropped_line(dropped)));
        }
    }

    /// Prints a step's app issues, screenshots new ones on request, and
    /// merges them into the step entry's `appIssues`.
    pub(super) fn attach_step_issues(&mut self, entry: &mut Value, mut issues: Vec<Value>) {
        if issues.is_empty() {
            return;
        }
        for issue in issues.iter_mut() {
            if self.opts.print_progress {
                print_prefixed(&self.opts.log_prefix, &format!("      {}", health::console_line(issue)));
            }
            self.screenshot_issue(issue, entry["index"].as_u64().unwrap_or(0));
        }
        let mut list: Vec<Value> = entry["appIssues"].as_array().cloned().unwrap_or_default();
        health::merge_issues(&mut list, issues);
        entry["appIssues"] = Value::Array(list);
    }

    /// Attaches issues caused by the previous command to the last recorded
    /// step. Returns the `--app-errors fail` message when one blocks.
    pub(super) fn attach_late_issues(&mut self, mut issues: Vec<Value>) -> Option<String> {
        if issues.is_empty() {
            return None;
        }
        self.opts.app_health.mark_ignored(&mut issues);
        let failure = (self.opts.app_health.policy == AppErrorsPolicy::Fail)
            .then(|| health::first_blocking(&issues).cloned())
            .flatten();
        let Some(mut last) = self.steps.pop() else {
            // No step yet (e.g. issues from an app attached before the flow).
            let msg = failure.map(|i| health::failure_message(&i, "app start"));
            let mut entry = json!({ "index": self.step_index, "command": "appStart", "description": "app start", "status": "passed", "ms": 0.0 });
            self.attach_step_issues(&mut entry, issues);
            self.steps.push(entry);
            self.step_index += 1;
            return msg;
        };
        let desc = last["description"].as_str().unwrap_or("previous step").to_string();
        if self.opts.print_progress {
            print_prefixed(&self.opts.log_prefix, &format!("      (after `{desc}`)"));
        }
        self.attach_step_issues(&mut last, issues);
        self.steps.push(last);
        failure.map(|i| health::failure_message(&i, &desc))
    }

    /// F8: captures rich diagnostics (suggestions, compact UI tree, failure
    /// screenshot, and full hierarchy file) once at the failing leaf step.
    /// Skipped during intermediate `retry` attempts that will be retried, and
    /// for downgradable errors inside `optional: true` `runFlow`/`repeat`.
    pub(super) fn capture_failure_diagnostics(
        &mut self,
        evaluated_cmd: &MaestroCommand,
        err: &anyhow::Error,
        crashed: bool,
        step_index: usize,
    ) -> Option<FailureDiagnostics> {
        if self.failure_captured {
            return None;
        }
        if (self.retry_attempts_remaining > 0 && is_maestro_error(err))
            || (self.optional_depth > 0 && is_optional_downgradable(err))
        {
            return None;
        }
        self.failure_captured = true;

        let mut diag = FailureDiagnostics::default();
        let mut full_tree_text: Option<String> = None;

        if !crashed && let Some((source, root)) = self.driver.capture_failure_hierarchy() {
            if let Some(selector) = failure::extract_failed_selector(evaluated_cmd) {
                diag.suggestions = failure::suggest_similar(&root, selector, failure::MAX_SUGGESTIONS);
            }
            let (compact, total) = failure::format_hierarchy(&root, Some(failure::REPORT_HIERARCHY_MAX_LINES));
            let (full, _) = failure::format_hierarchy(&root, None);
            diag.hierarchy = Some(compact);
            diag.hierarchy_source = Some(source);
            diag.total_elements = Some(total);
            full_tree_text = Some(full);
        }

        if let Some(dir) = self.opts.test_output_dir.clone() {
            let failures_dir = dir.join("failures");
            let stem = failure::artifact_stem(&self.opts.flow_path, step_index);
            if let Some(png) = self.driver.capture_failure_screenshot() {
                let path = failures_dir.join(format!("{stem}_failure.png"));
                if std::fs::create_dir_all(&failures_dir).is_ok() && crate::atomic_write::write(&path, png).is_ok() {
                    diag.screenshot = Some(path.to_string_lossy().to_string());
                }
            }
            if let Some(full) = full_tree_text {
                let path = failures_dir.join(format!("{stem}_hierarchy.txt"));
                if std::fs::create_dir_all(&failures_dir).is_ok() && crate::atomic_write::write(&path, full).is_ok() {
                    diag.hierarchy_file = Some(path.to_string_lossy().to_string());
                }
            }
        }

        if diag.is_empty() { None } else { Some(diag) }
    }

    /// `--app-errors-screenshot`: captures the first occurrence of each
    /// issue (by kind and message) into `<test-output-dir>/app-issues/`.
    fn screenshot_issue(&mut self, issue: &mut Value, step_index: u64) {
        if !self.opts.app_health.screenshot || issue["kind"] == "appCrash" || issue.get("screenshot").is_some() {
            return;
        }
        let Some(dir) = self.opts.test_output_dir.clone() else { return };
        let key = format!("{}|{}", issue["kind"].as_str().unwrap_or(""), issue["message"].as_str().unwrap_or(""));
        if !self.screenshotted_issues.insert(key) {
            return;
        }
        let Ok(png) = self.driver.screenshot_bytes() else { return };
        let path = dir.join("app-issues").join(format!(
            "{:03}_{}_{}.png",
            step_index + 1,
            issue["kind"].as_str().unwrap_or("issue"),
            issue["id"].as_u64().unwrap_or(0)
        ));
        if std::fs::create_dir_all(path.parent().unwrap_or(&dir)).is_ok() && std::fs::write(&path, png).is_ok() {
            issue["screenshot"] = Value::from(path.to_string_lossy().to_string());
        }
    }
}

/// F8b: whether `--trace-screenshots steps` captures the screen after `cmd`
/// (not for wrappers and pure JavaScript commands).
pub(super) fn traces_screen(cmd: &MaestroCommand) -> bool {
    !matches!(
        cmd,
        MaestroCommand::RunFlow(_)
            | MaestroCommand::Repeat(_)
            | MaestroCommand::Retry(_)
            | MaestroCommand::RunScript(_)
            | MaestroCommand::EvalScript(_)
            | MaestroCommand::DefineVariables(_)
            | MaestroCommand::ApplyConfiguration(_)
    )
}
