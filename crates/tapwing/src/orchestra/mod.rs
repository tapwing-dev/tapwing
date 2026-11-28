//! Full Maestro 2.12.1 flow orchestrator (`Orchestra.kt` parity).
//!
//! Runs parsed [`ParsedFlow`] / [`MaestroCommand`] lists over any [`FlowDriver`]
//! backend using [`JsEngine`] for `${...}` expressions, `evalScript`, `runScript`,
//! `assertTrue`, `when`/`while` conditions, `runFlow`, `repeat`, `retry`,
//! `onFlowStart`/`onFlowComplete` hooks, `defineVariables`, timeouts
//! (`adjustedToLatestInteraction`), and `optional` downgrade semantics.

mod describe;
mod diagnostics;
mod dispatch;
mod eval;

pub use describe::{describe_command, is_optional, kind, settle_timeout_note, settles, settles_for_platform};
pub use dispatch::{adjusted_timeout, is_condition_truthy};
pub use eval::evaluate_command_scripts;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde_json::{Value, json};

use crate::app_health::{self as health, AppErrorsPolicy, AppHealthError, AppHealthOpts};
use crate::backend::{
    FlowDriver, LOOKUP_TIMEOUT_MS, OPTIONAL_LOOKUP_TIMEOUT_MS, is_optional_downgradable, is_transport_error,
};
use crate::failure;
use crate::flow::ParsedFlow;
use crate::flow::ir::{MaestroCommand, MaestroConfig};
use crate::js::{JsEngine, JsMetrics, default_http_agent};

use diagnostics::traces_screen;
use eval::evaluate_define_variables;

/// Reserved internal env vars controlled exclusively by Maestro (`Env.kt:102-105`).
pub const INTERNAL_ONLY_ENV_VARS: &[&str] = &["MAESTRO_SHARD_ID", "MAESTRO_SHARD_INDEX"];

/// Resolution returned by `on_command_failed` (`Orchestra.ErrorResolution`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorResolution {
    Fail,
    Continue,
}

/// Per-command metadata dispatched to `on_command_metadata_update` (`Orchestra.CommandMetadata`).
#[derive(Clone, Debug, Default)]
pub struct CommandMetadata {
    pub number_of_runs: Option<usize>,
    pub evaluated_command: Option<MaestroCommand>,
    pub log_messages: Vec<String>,
}

/// Callback invoked on command lifecycle events (`start`, `complete`, `warned`, `skipped`).
pub type CommandEventCallback = Box<dyn FnMut(usize, &MaestroCommand)>;

/// Callback invoked when a non-optional command fails.
pub type CommandFailedCallback = Box<dyn FnMut(usize, &MaestroCommand, &anyhow::Error) -> ErrorResolution>;

/// Callback invoked when command metadata (evaluated command, repeat count, JS logs) updates.
pub type CommandMetadataCallback = Box<dyn FnMut(&MaestroCommand, &CommandMetadata)>;

/// Optional lifecycle callbacks matching `Orchestra.kt:147-155`.
#[derive(Default)]
pub struct OrchestraCallbacks {
    pub on_command_start: Option<CommandEventCallback>,
    pub on_command_complete: Option<CommandEventCallback>,
    pub on_command_warned: Option<CommandEventCallback>,
    pub on_command_skipped: Option<CommandEventCallback>,
    pub on_command_failed: Option<CommandFailedCallback>,
    pub on_command_metadata_update: Option<CommandMetadataCallback>,
}

/// Configuration for an [`Orchestra`] run.
#[derive(Clone, Debug)]
pub struct OrchestraOpts {
    pub lookup_timeout_ms: u64,
    pub optional_lookup_timeout_ms: u64,
    /// `-e KEY=VALUE` from CLI; wins over top-level flow header `env:`.
    pub cli_env: HashMap<String, String>,
    /// Output directory for screenshots and diff artifacts (`--test-output-dir`).
    pub test_output_dir: Option<PathBuf>,
    /// Flow path for resolving `assertScreenshot` relative references and `MAESTRO_FILENAME`.
    pub flow_path: PathBuf,
    /// Optional device UDID injected into `MAESTRO_DEVICE_UDID`.
    pub device_id: Option<String>,
    /// Optional shard index (defaults to 0 -> `MAESTRO_SHARD_ID=1`, `MAESTRO_SHARD_INDEX=0`).
    pub shard_index: Option<usize>,
    /// Line prefix for thread-safe console output (`""` single-shard, `"[shard N] "` sharded).
    pub log_prefix: String,
    /// Whether to query `driver.check_busy_after()` after settling commands (`--check-settle`).
    pub check_settle: bool,
    pub settle_policy: String,
    /// Whether to print human-readable step lines to stdout (true for CLI, false for quiet tests).
    pub print_progress: bool,
    /// Whether `travel` should sleep between intermediate waypoints (false in fast fake-driver tests).
    pub travel_sleep: bool,
    /// Shared HTTP client reused across flows for connection pooling & keep-alive.
    pub http_agent: Option<Arc<ureq::Agent>>,
    /// F7 app health policy, ignore rules, and screenshots.
    pub app_health: AppHealthOpts,
    /// F8b `--trace`: per-step trace extras (`None` = off, no extra calls).
    pub trace: Option<crate::trace::TraceOpts>,
}

impl Default for OrchestraOpts {
    fn default() -> Self {
        Self {
            lookup_timeout_ms: LOOKUP_TIMEOUT_MS,
            optional_lookup_timeout_ms: OPTIONAL_LOOKUP_TIMEOUT_MS,
            cli_env: HashMap::new(),
            test_output_dir: None,
            flow_path: PathBuf::new(),
            device_id: None,
            shard_index: None,
            log_prefix: String::new(),
            check_settle: false,
            settle_policy: "v2".to_string(),
            print_progress: false,
            travel_sleep: false,
            http_agent: None,
            app_health: AppHealthOpts::default(),
            trace: None,
        }
    }
}

/// Writes `text` line-by-line to stdout under a single lock, prepending
/// `prefix` to each non-empty line (or every line when `prefix` is non-empty)
/// so parallel shards never interleave mid-line.
pub fn print_prefixed(prefix: &str, text: &str) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for line in text.split('\n') {
        if line.is_empty() && prefix.is_empty() {
            let _ = writeln!(handle);
        } else {
            let _ = writeln!(handle, "{prefix}{line}");
        }
    }
}

/// Outcome of running a flow through [`Orchestra`].
#[derive(Debug)]
pub struct FlowResult {
    pub success: bool,
    pub error: Option<anyhow::Error>,
    pub steps: Vec<Value>,
    pub settle_violations: usize,
    pub total_ms: f64,
    pub js_metrics: JsMetrics,
    pub js_closed: bool,
    pub dropped_app_issues: u64,
    /// F8b trace (`--trace` only): `{http, httpDropped, steps}` where steps are
    /// the step entries plus trace extras, in start order.
    pub trace: Option<Value>,
}

impl FlowResult {
    pub fn to_report_json(&self, flow_name: &str, path: &Path, settle_policy: &str) -> Value {
        json!({
            "flow": flow_name,
            "path": path,
            "status": if self.success { "passed" } else { "failed" },
            "totalMs": self.total_ms,
            "settlePolicy": settle_policy,
            "settleViolations": self.settle_violations,
            "js": {
                "runtimeInitialized": self.js_metrics.runtime_initialized,
                "evalCount": self.js_metrics.eval_count,
                "fastPathHits": self.js_metrics.fast_path_hits,
                "cacheHits": self.js_metrics.cache_hits,
                "evalMs": self.js_metrics.eval_ms,
                "httpRequests": self.js_metrics.http_requests,
                "httpMs": self.js_metrics.http_ms,
            },
            "steps": self.steps,
        })
        .tap_app_issue_summary(&self.steps, self.dropped_app_issues)
    }
}

trait TapAppIssueSummary {
    fn tap_app_issue_summary(self, steps: &[Value], dropped: u64) -> Value;
}

impl TapAppIssueSummary for Value {
    fn tap_app_issue_summary(mut self, steps: &[Value], dropped: u64) -> Value {
        if let Some(summary) = health::summarize_with_dropped(steps, dropped) {
            self["appIssueSummary"] = summary;
        }
        self
    }
}

/// Internal signal when a conditional command (`runFlow`, `runScript`) or
/// zero-iteration `repeat` is skipped (`Orchestra.CommandSkipped`).
#[derive(Debug)]
struct CommandSkipped;

impl std::fmt::Display for CommandSkipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("command skipped")
    }
}

impl std::error::Error for CommandSkipped {}

/// Maestro 2.12.1 flow orchestrator.
pub struct Orchestra<'a, D: FlowDriver + ?Sized> {
    driver: &'a mut D,
    opts: OrchestraOpts,
    callbacks: OrchestraCallbacks,
    js: JsEngine,
    copied_text: Option<String>,
    last_interaction: Instant,
    steps: Vec<Value>,
    step_index: usize,
    settle_violations: usize,
    subflow_depth: usize,
    recording_open: bool,
    /// Keys supplied via CLI `-e`; protected from top-level flow `env:` overwrites.
    cli_env_keys: Vec<String>,
    /// Buffered log messages captured during the currently executing command.
    pending_logs: Arc<std::sync::Mutex<Vec<String>>>,
    /// F7: issue fingerprints already screenshotted in this flow.
    screenshotted_issues: std::collections::HashSet<String>,
    /// F7: undelivered issues evicted from the driver ring buffer during this flow.
    dropped_app_issues: u64,
    /// F8: number of enclosing `retry` blocks that still have attempts left
    /// (suppresses failure screenshot/tree capture on intermediate retried attempts).
    retry_attempts_remaining: usize,
    /// F8: number of enclosing `optional: true` `runFlow` / `repeat` blocks
    /// (suppresses failure screenshot/tree capture for downgradable errors inside).
    optional_depth: usize,
    /// F8: true once rich failure diagnostics have been captured for the failing
    /// leaf command so outer `runFlow`/`repeat`/`retry` wrappers do not duplicate them.
    failure_captured: bool,
    /// F7: flow's active `appId`, used to match intentional `stopApp`/`killApp`/`clearState`.
    flow_app_id: Option<String>,
    /// F7: true after an intentional `stopApp`/`killApp`/`clearState` until `launchApp`.
    app_intentionally_stopped: bool,
    /// F8b: flow start (trace `startMs` offsets).
    flow_t0: Instant,
    /// F8b: trace extras per step index.
    trace_extras: HashMap<usize, Value>,
    /// F8b: extras for the step about to be recorded by [`Self::push_step`].
    pending_trace: Option<Value>,
    /// F8b: 1-based attempt of each enclosing `retry` block.
    retry_attempts: Vec<usize>,
    /// F8b: HTTP log entries the driver dropped during this flow.
    trace_http_dropped: u64,
    /// F8b: per-run flow ordinal for unique asset names.
    trace_ordinal: usize,
}

impl<'a, D: FlowDriver + ?Sized> Orchestra<'a, D> {
    pub fn new(driver: &'a mut D, opts: OrchestraOpts) -> Self {
        Self::with_callbacks(driver, opts, OrchestraCallbacks::default())
    }

    pub fn with_callbacks(driver: &'a mut D, opts: OrchestraOpts, callbacks: OrchestraCallbacks) -> Self {
        let platform = crate::backend::platform_js_name(driver.platform()).to_string();
        let agent = opts.http_agent.clone().unwrap_or_else(default_http_agent);
        let js = JsEngine::with_http_agent(platform, agent);
        let cli_env_keys: Vec<String> = opts.cli_env.keys().cloned().collect();
        Self {
            driver,
            opts,
            callbacks,
            js,
            copied_text: None,
            last_interaction: Instant::now(),
            steps: Vec::new(),
            step_index: 0,
            settle_violations: 0,
            subflow_depth: 0,
            recording_open: false,
            cli_env_keys,
            pending_logs: Arc::new(std::sync::Mutex::new(Vec::new())),
            screenshotted_issues: std::collections::HashSet::new(),
            dropped_app_issues: 0,
            retry_attempts_remaining: 0,
            optional_depth: 0,
            failure_captured: false,
            flow_app_id: None,
            app_intentionally_stopped: false,
            flow_t0: Instant::now(),
            trace_extras: HashMap::new(),
            pending_trace: None,
            retry_attempts: Vec::new(),
            trace_http_dropped: 0,
            trace_ordinal: 0,
        }
    }

    /// Runs a parsed flow file (preserving full Maestro `Orchestra.runFlow` lifecycle).
    pub fn run_parsed_flow(&mut self, flow: &ParsedFlow) -> FlowResult {
        if self.opts.flow_path.as_os_str().is_empty() {
            self.opts.flow_path = flow.path.clone();
        }
        self.run_commands(&flow.commands)
    }

    /// Runs a raw list of [`MaestroCommand`]s (matching `Orchestra.runFlow(commands)`).
    pub fn run_commands(&mut self, commands: &[MaestroCommand]) -> FlowResult {
        let t0 = Instant::now();
        self.last_interaction = Instant::now();
        self.retry_attempts_remaining = 0;
        self.optional_depth = 0;
        self.failure_captured = false;
        self.flow_app_id = None;
        self.app_intentionally_stopped = false;
        self.flow_t0 = t0;
        self.trace_extras.clear();
        self.pending_trace = None;
        self.retry_attempts.clear();
        self.trace_http_dropped = 0;
        if self.opts.trace.is_some() {
            self.trace_ordinal = crate::trace::next_flow_ordinal();
        }

        let config = extract_config(commands);

        // Defense-in-depth for removed Rhino JS engine (`Orchestra.kt:163-166`).
        if let Some(cfg) = &config
            && cfg.ext.get("jsEngine").and_then(Value::as_str) == Some("rhino")
        {
            return FlowResult {
                success: false,
                error: Some(anyhow::anyhow!(
                    "The Rhino JS engine has been removed. Remove `jsEngine: rhino` from your config."
                )),
                steps: Vec::new(),
                settle_violations: 0,
                total_ms: t0.elapsed().as_secs_f64() * 1000.0,
                js_metrics: self.js.metrics(),
                js_closed: true,
                dropped_app_issues: 0,
                trace: None,
            };
        }

        // Wire `console.log` capture into `pending_logs`.
        let logs_sink = Arc::clone(&self.pending_logs);
        self.js.set_log_callback(move |line| {
            if let Ok(mut buf) = logs_sink.lock() {
                buf.push(line);
            }
        });

        // Inject default env vars (`MAESTRO_FILENAME`, `MAESTRO_DEVICE_UDID`,
        // `MAESTRO_SHARD_ID`, `MAESTRO_SHARD_INDEX`) and CLI `-e` vars first.
        self.inject_initial_env();

        let mut flow_success = false;
        let mut primary_error: Option<anyhow::Error> = None;

        // 1. Execute top-level `DefineVariables` BEFORE `onFlowStart` (`Orchestra.kt:243`, Case 107).
        let define_vars_res = self.execute_define_variables(commands, config.as_ref(), true);
        match define_vars_res {
            Ok(true) => {
                // Evaluate flow appId after variables are defined (`Orchestra.kt:245`).
                let eval_app_id = self.evaluated_app_id(config.as_ref());
                if let Some(app) = &eval_app_id {
                    self.flow_app_id = Some(app.clone());
                    self.driver.set_flow_app_id(app);
                }

                // Filter out top-level `DefineVariables` so they don't run twice (`Orchestra.kt:249`).
                let filtered: Vec<MaestroCommand> =
                    commands.iter().filter(|c| !matches!(c, MaestroCommand::DefineVariables(_))).cloned().collect();

                // 2. Execute `onFlowStart` hook (`Orchestra.kt:251-257`).
                let on_start_res = if let Some(hook) = config.as_ref().and_then(|c| c.on_flow_start.as_ref()) {
                    self.execute_command_list(&hook.commands, config.as_ref(), false)
                } else {
                    Ok(true)
                };

                match on_start_res {
                    Ok(true) => {
                        // 3. Execute main flow commands (`Orchestra.kt:260-267`).
                        match self.execute_command_list(&filtered, config.as_ref(), false) {
                            Ok(ok) => {
                                flow_success = ok;
                            }
                            Err(e) => {
                                primary_error = Some(e);
                            }
                        }
                        if self.recording_open {
                            if !crate::interrupt::is_interrupted() {
                                let _ = self.driver.stop_recording();
                            }
                            self.recording_open = false;
                        }
                    }
                    Ok(false) => {
                        flow_success = false;
                    }
                    Err(e) => {
                        primary_error = Some(e);
                    }
                }
            }
            Ok(false) => {
                flow_success = false;
            }
            Err(e) => {
                primary_error = Some(e);
            }
        }

        let interrupted = crate::interrupt::is_interrupted()
            || primary_error.as_ref().is_some_and(crate::interrupt::is_interrupted_error);
        if interrupted {
            let js_metrics = self.js.metrics();
            self.js.close();
            let js_closed = self.js.is_closed();
            return FlowResult {
                success: false,
                error: primary_error.or_else(|| Some(crate::interrupt::InterruptedError::current().into())),
                steps: std::mem::take(&mut self.steps),
                settle_violations: self.settle_violations,
                total_ms: t0.elapsed().as_secs_f64() * 1000.0,
                js_metrics,
                js_closed,
                dropped_app_issues: self.dropped_app_issues,
                trace: None,
            };
        }

        // 4. ALWAYS execute `onFlowComplete` hook in finally (`Orchestra.kt:274-296`, Cases 104, 108, 109).
        let on_complete_success = if let Some(hook) = config.as_ref().and_then(|c| c.on_flow_complete.as_ref()) {
            match self.execute_command_list(&hook.commands, config.as_ref(), false) {
                Ok(ok) => ok,
                Err(e) => {
                    // Body failure wins over onFlowComplete failure (`Orchestra.kt:290`).
                    if primary_error.is_none() {
                        primary_error = Some(e);
                    }
                    false
                }
            }
        } else {
            true
        };

        // F7: issues still buffered in the app (e.g. an async error after the
        // last command) belong to the last step.
        let mut trailing = self.driver.drain_app_issues();
        self.record_dropped_issues(trailing.dropped);
        let mut tail = std::mem::take(&mut trailing.late);
        tail.append(&mut trailing.current);
        let tail_failure = self.attach_late_issues(tail);
        let mut health_failed = false;
        if let Some(msg) = tail_failure
            && primary_error.is_none()
            && flow_success
            && on_complete_success
        {
            if self.opts.print_progress {
                print_prefixed(&self.opts.log_prefix, &format!("  ✗ {msg}"));
            }
            primary_error = Some(AppHealthError(msg).into());
            health_failed = true;
        }

        // F8b: requests still open or delivered after the last command.
        let trace = self.opts.trace.is_some().then(|| self.finish_trace());

        let js_metrics = self.js.metrics();
        self.js.close();
        let js_closed = self.js.is_closed();

        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let success = primary_error.is_none() && flow_success && on_complete_success && !health_failed;

        FlowResult {
            success,
            error: primary_error,
            steps: std::mem::take(&mut self.steps),
            settle_violations: self.settle_violations,
            total_ms,
            js_metrics,
            js_closed,
            dropped_app_issues: self.dropped_app_issues,
            trace,
        }
    }

    fn inject_initial_env(&mut self) {
        // 1. CLI `-e` env (strip reserved internal shard vars like `Env.kt:130`).
        let cli_items: Vec<(String, String)> = self
            .opts
            .cli_env
            .iter()
            .filter(|(k, _)| !INTERNAL_ONLY_ENV_VARS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in cli_items {
            self.js.put_env(k, v);
        }

        // 2. Default env vars (`Env.kt:116-132`).
        if let Some(stem) = self.opts.flow_path.file_stem().and_then(|s| s.to_str())
            && !stem.is_empty()
        {
            self.js.put_env("MAESTRO_FILENAME", stem);
        }
        if let Some(udid) = self.opts.device_id.as_deref().filter(|s| !s.trim().is_empty()) {
            self.js.put_env("MAESTRO_DEVICE_UDID", udid);
        }
        let shard_idx = self.opts.shard_index.unwrap_or(0);
        self.js.put_env("MAESTRO_SHARD_ID", (shard_idx + 1).to_string());
        self.js.put_env("MAESTRO_SHARD_INDEX", shard_idx.to_string());
    }

    fn evaluated_app_id(&mut self, config: Option<&MaestroConfig>) -> Option<String> {
        let raw = config?.app_id.as_deref()?;
        self.js.evaluate_scripts(raw).ok()
    }

    fn execute_define_variables(
        &mut self,
        commands: &[MaestroCommand],
        config: Option<&MaestroConfig>,
        respect_cli_precedence: bool,
    ) -> Result<bool> {
        let defines: Vec<MaestroCommand> =
            commands.iter().filter(|c| matches!(c, MaestroCommand::DefineVariables(_))).cloned().collect();
        if defines.is_empty() {
            return Ok(true);
        }
        for cmd in &defines {
            let MaestroCommand::DefineVariables(d) = cmd else {
                continue;
            };
            let evaluated = evaluate_define_variables(d, &mut self.js)?;
            for (k, v) in &evaluated.env {
                if respect_cli_precedence && self.cli_env_keys.contains(k) {
                    continue;
                }
                if let Some(s) = v.as_str() {
                    self.js.put_env(k.clone(), s.to_string());
                }
            }
            let _ = config;
        }
        Ok(true)
    }

    /// Executes a command list at top-level (`is_subflow == false`, returning `Ok(false)`
    /// or `Err` on failure) or inside a subflow (`is_subflow == true`, returning `Ok(mutated)`
    /// and propagating errors to parent `runFlow`/`retry`).
    fn execute_command_list(
        &mut self,
        commands: &[MaestroCommand],
        config: Option<&MaestroConfig>,
        is_subflow: bool,
    ) -> Result<bool> {
        if is_subflow {
            self.subflow_depth += 1;
        }
        let mut any_mutated = false;

        let res = (|| -> Result<bool> {
            for (idx, cmd) in commands.iter().enumerate() {
                crate::interrupt::check()?;
                if let Some(cb) = self.callbacks.on_command_start.as_mut() {
                    cb(idx, cmd);
                }

                // Clear pending log buffer for this command.
                if let Ok(mut buf) = self.pending_logs.lock() {
                    buf.clear();
                }

                let js_before = self.js.metrics();
                let ts = Instant::now();
                let start_ms = ts.duration_since(self.flow_t0).as_secs_f64() * 1000.0;
                let mut evaluated_cmd: Option<MaestroCommand> = None;

                let exec_res: Result<(bool, Value, MaestroCommand)> = (|| {
                    let evaluated = evaluate_command_scripts(cmd, &mut self.js)?;
                    evaluated_cmd = Some(evaluated.clone());
                    let meta = CommandMetadata {
                        number_of_runs: None,
                        evaluated_command: Some(evaluated.clone()),
                        log_messages: Vec::new(),
                    };
                    if let Some(cb) = self.callbacks.on_command_metadata_update.as_mut() {
                        cb(cmd, &meta);
                    }
                    let (mutated, extra) = self.execute_single_command(&evaluated, cmd, config)?;
                    Ok((mutated, extra, evaluated))
                })();

                // Flush any console.log messages captured during this command.
                let captured_logs = self.pending_logs.lock().map(|mut b| std::mem::take(&mut *b)).unwrap_or_default();
                if !captured_logs.is_empty() {
                    let meta =
                        CommandMetadata { number_of_runs: None, evaluated_command: None, log_messages: captured_logs };
                    if let Some(cb) = self.callbacks.on_command_metadata_update.as_mut() {
                        cb(cmd, &meta);
                    }
                }

                let ms = ts.elapsed().as_secs_f64() * 1000.0;
                let js_delta = self.js.metrics().delta_since(&js_before);
                let step_kind = kind(cmd);
                let is_step =
                    !matches!(cmd, MaestroCommand::ApplyConfiguration(_) | MaestroCommand::DefineVariables(_));

                if let Err(e) = &exec_res {
                    if crate::interrupt::is_interrupted_error(e) {
                        return Err(crate::interrupt::InterruptedError::current().into());
                    }
                    crate::interrupt::check()?;
                }

                // F8b `--trace-screenshots steps`: capture before taking the
                // step's issues and HTTP entries so the capture RPCs count
                // toward this step.
                let trace_capture = match self.opts.trace {
                    Some(t)
                        if is_step
                            && t.screenshots == crate::trace::TraceScreenshots::Steps
                            && exec_res.is_ok()
                            && traces_screen(cmd) =>
                    {
                        self.capture_trace_screen()
                    }
                    _ => None,
                };

                // F7: issues the app reported while this command ran; late
                // ones were caused by the previous command.
                let mut batch =
                    if matches!(cmd, MaestroCommand::ApplyConfiguration(_) | MaestroCommand::DefineVariables(_)) {
                        // Not device commands and never recorded as steps.
                        health::IssueBatch::default()
                    } else {
                        self.driver.take_app_issues()
                    };
                self.record_dropped_issues(batch.dropped);
                self.opts.app_health.mark_ignored(&mut batch.current);
                let late_failure = self.attach_late_issues(std::mem::take(&mut batch.late));
                let mut step_issues = batch.current;
                self.pending_trace = None;
                if self.opts.trace.is_some() && is_step {
                    self.pending_trace = Some(self.trace_step_extras(cmd, start_ms, trace_capture));
                }
                let exec_res = match exec_res {
                    Ok(_) if self.opts.app_health.policy == AppErrorsPolicy::Fail => {
                        match (late_failure, health::first_blocking(&step_issues)) {
                            (Some(msg), _) => Err(AppHealthError(msg).into()),
                            (None, Some(issue)) => {
                                Err(AppHealthError(health::failure_message(issue, &describe_command(cmd))).into())
                            }
                            (None, None) => exec_res,
                        }
                    }
                    other => other,
                };

                // F7: when a transport error occurs (and the app was not
                // intentionally stopped), check whether the app process crashed
                // and surface the crash log instead of a generic socket error.
                // Never run crash diagnosis on normal command/selector errors
                // such as ELEMENT_NOT_FOUND.
                let mut crashed = false;
                let exec_res = match exec_res {
                    Err(e) if is_transport_error(&e) && !self.app_intentionally_stopped => {
                        match self.driver.diagnose_crash() {
                            Some(crash) => {
                                let what = crash["message"].as_str().unwrap_or("App crashed").to_string();
                                let desc = describe_command(cmd);
                                let msg = match health::crash_headline(&crash) {
                                    Some(line) => format!("{what} during `{desc}`: {line}"),
                                    None => format!("{what} during `{desc}`"),
                                };
                                step_issues.push(crash);
                                crashed = true;
                                Err(e.context(msg))
                            }
                            None => Err(e),
                        }
                    }
                    other => other,
                };

                match exec_res {
                    Ok((mutated, mut extra, evaluated)) => {
                        if mutated {
                            self.last_interaction = Instant::now();
                            any_mutated = true;
                        }
                        if js_delta.eval_count > 0 || js_delta.http_requests > 0 {
                            attach_js_metrics(&mut extra, &js_delta);
                        }
                        let desc = describe_command(&evaluated);
                        if self.opts.print_progress && !matches!(cmd, MaestroCommand::ApplyConfiguration(_)) {
                            print_prefixed(&self.opts.log_prefix, &format!("  ✓ {ms:>7.1} ms  {desc}"));
                            if let Some(w) = extra.get("warning").and_then(Value::as_str) {
                                print_prefixed(&self.opts.log_prefix, &format!("      ⚠ {w}"));
                            }
                            if let Some(note) = settle_timeout_note(&extra) {
                                print_prefixed(&self.opts.log_prefix, &format!("      ⚠ {note}"));
                            }
                        }
                        if !matches!(cmd, MaestroCommand::ApplyConfiguration(_) | MaestroCommand::DefineVariables(_)) {
                            let mut entry = json!({
                                "index": self.step_index,
                                "command": step_kind,
                                "description": desc,
                                "status": "passed",
                                "ms": ms,
                                "driver": extra,
                            });
                            if let Some(w) = extra.get("warning").and_then(Value::as_str) {
                                entry["warning"] = Value::from(w);
                            }
                            if self.opts.check_settle
                                && settles_for_platform(cmd, self.driver.platform())
                                && let Some(busy) = self.driver.check_busy_after()
                            {
                                let blocking: Vec<Value> = busy["animations"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .filter(|a| a["infinite"].as_bool() != Some(true))
                                    .collect();
                                if !blocking.is_empty() {
                                    self.settle_violations += 1;
                                    if self.opts.print_progress {
                                        let what: Vec<String> = blocking
                                            .iter()
                                            .map(|a| {
                                                format!(
                                                    "{} {:.0}% {}",
                                                    a["widget"].as_str().unwrap_or("?"),
                                                    a["fraction"].as_f64().unwrap_or(0.0) * 100.0,
                                                    a["status"].as_str().unwrap_or("")
                                                )
                                            })
                                            .collect();
                                        print_prefixed(
                                            &self.opts.log_prefix,
                                            &format!("      ⚠ returned while still animating: {}", what.join(", ")),
                                        );
                                    }
                                }
                                entry["busyAfter"] = busy;
                            }
                            self.attach_step_issues(&mut entry, std::mem::take(&mut step_issues));
                            self.push_step(entry);
                        }
                        if let Some(cb) = self.callbacks.on_command_complete.as_mut() {
                            cb(idx, cmd);
                        }
                    }
                    Err(e) if e.is::<CommandSkipped>() => {
                        let desc = describe_command(cmd);
                        if self.opts.print_progress {
                            print_prefixed(&self.opts.log_prefix, &format!("  - {ms:>7.1} ms  {desc}  (skipped)"));
                        }
                        let mut entry = json!({
                            "index": self.step_index,
                            "command": step_kind,
                            "description": desc,
                            "status": "skipped",
                            "ms": ms,
                        });
                        self.attach_step_issues(&mut entry, std::mem::take(&mut step_issues));
                        self.push_step(entry);
                        if let Some(cb) = self.callbacks.on_command_skipped.as_mut() {
                            cb(idx, cmd);
                        }
                    }
                    Err(e) if is_transport_error(&e) => {
                        // `Orchestra.kt:397-398`: transport death NEVER routes through
                        // `onCommandFailed` or `optional` — propagate immediately.
                        // F7: record the step when the app crashed.
                        if crashed {
                            let desc = describe_command(cmd);
                            let msg = e.to_string();
                            let diag = self.capture_failure_diagnostics(
                                evaluated_cmd.as_ref().unwrap_or(cmd),
                                &e,
                                true,
                                self.step_index,
                            );
                            if self.opts.print_progress {
                                print_prefixed(
                                    &self.opts.log_prefix,
                                    &format!("  ✗ {ms:>7.1} ms  {desc}\n      {msg}"),
                                );
                                if let Some(d) = &diag {
                                    let formatted = failure::format_console(d);
                                    if !formatted.is_empty() {
                                        print_prefixed(&self.opts.log_prefix, &formatted);
                                    }
                                }
                            }
                            let mut entry = json!({
                                "index": self.step_index,
                                "command": step_kind,
                                "description": desc,
                                "status": "failed",
                                "ms": ms,
                                "error": msg,
                            });
                            if let Some(d) = &diag
                                && let Ok(v) = serde_json::to_value(d)
                            {
                                entry["failure"] = v;
                            }
                            self.attach_step_issues(&mut entry, std::mem::take(&mut step_issues));
                            self.push_step(entry);
                        }
                        return Err(e);
                    }
                    Err(e) if is_optional(cmd) && is_optional_downgradable(&e) => {
                        self.failure_captured = false;
                        let desc = describe_command(cmd);
                        if self.opts.print_progress {
                            print_prefixed(
                                &self.opts.log_prefix,
                                &format!("  ~ {ms:>7.1} ms  {desc}  (optional: {e})"),
                            );
                        }
                        let mut entry = json!({
                            "index": self.step_index,
                            "command": step_kind,
                            "description": desc,
                            "status": "warned",
                            "ms": ms,
                            "error": e.to_string(),
                        });
                        self.attach_step_issues(&mut entry, std::mem::take(&mut step_issues));
                        self.push_step(entry);
                        if let Some(cb) = self.callbacks.on_command_warned.as_mut() {
                            cb(idx, cmd);
                        }
                    }
                    Err(e) => {
                        let desc = describe_command(cmd);
                        let diag = self.capture_failure_diagnostics(
                            evaluated_cmd.as_ref().unwrap_or(cmd),
                            &e,
                            crashed,
                            self.step_index,
                        );
                        if self.opts.print_progress {
                            let err_text = if crashed || diag.as_ref().is_some_and(|d| d.hierarchy.is_some()) {
                                e.to_string()
                            } else {
                                crate::run::explain(&e)
                            };
                            print_prefixed(
                                &self.opts.log_prefix,
                                &format!("  ✗ {ms:>7.1} ms  {desc}\n      {err_text}"),
                            );
                            if let Some(d) = &diag {
                                let formatted = failure::format_console(d);
                                if !formatted.is_empty() {
                                    print_prefixed(&self.opts.log_prefix, &formatted);
                                }
                            }
                        }
                        let mut entry = json!({
                            "index": self.step_index,
                            "command": step_kind,
                            "description": desc,
                            "status": "failed",
                            "ms": ms,
                            "error": e.to_string(),
                        });
                        if let Some(d) = &diag
                            && let Ok(v) = serde_json::to_value(d)
                        {
                            entry["failure"] = v;
                        }
                        self.attach_step_issues(&mut entry, std::mem::take(&mut step_issues));
                        self.push_step(entry);
                        if let Some(cb) = self.callbacks.on_command_failed.as_mut() {
                            match cb(idx, cmd, &e) {
                                ErrorResolution::Continue => continue,
                                ErrorResolution::Fail => {
                                    if is_subflow {
                                        return Err(e);
                                    } else {
                                        return Ok(false);
                                    }
                                }
                            }
                        }
                        return Err(e);
                    }
                }
            }
            Ok(if is_subflow { any_mutated } else { true })
        })();

        if is_subflow {
            self.subflow_depth -= 1;
        }
        res
    }

    fn run_sub_flow(
        &mut self,
        commands: &[MaestroCommand],
        parent_config: Option<&MaestroConfig>,
        subflow_config: Option<&MaestroConfig>,
    ) -> Result<bool> {
        self.js.enter_env_scope();
        let res = (|| -> Result<bool> {
            self.execute_define_variables(commands, parent_config, false)?;
            let filtered: Vec<MaestroCommand> =
                commands.iter().filter(|c| !matches!(c, MaestroCommand::DefineVariables(_))).cloned().collect();

            let active_config = subflow_config.or(parent_config);
            let mut mutated = false;
            let mut body_err: Option<anyhow::Error> = None;

            let start_ok = if let Some(hook) = subflow_config.and_then(|c| c.on_flow_start.as_ref()) {
                match self.execute_command_list(&hook.commands, active_config, true) {
                    Ok(m) => {
                        mutated |= m;
                        true
                    }
                    Err(e) => {
                        body_err = Some(e);
                        false
                    }
                }
            } else {
                true
            };

            if start_ok {
                match self.execute_command_list(&filtered, active_config, true) {
                    Ok(m) => mutated |= m,
                    Err(e) => body_err = Some(e),
                }
            }

            // Always run subflow's `onFlowComplete` hook (`Orchestra.kt:1247-1255`, Case 106),
            // unless aborted by SIGINT/SIGTERM.
            let interrupted = crate::interrupt::is_interrupted()
                || body_err.as_ref().is_some_and(crate::interrupt::is_interrupted_error);
            if !interrupted && let Some(hook) = subflow_config.and_then(|c| c.on_flow_complete.as_ref()) {
                match self.execute_command_list(&hook.commands, active_config, true) {
                    Ok(m) => mutated |= m,
                    Err(e) => {
                        if body_err.is_none() {
                            body_err = Some(e);
                        }
                    }
                }
            }

            if let Some(err) = body_err {
                return Err(err);
            }
            Ok(mutated)
        })();
        self.js.leave_env_scope();
        res
    }
}

fn extract_config(commands: &[MaestroCommand]) -> Option<MaestroConfig> {
    commands.iter().find_map(|c| match c {
        MaestroCommand::ApplyConfiguration(a) => Some(a.config.clone()),
        _ => None,
    })
}

fn attach_js_metrics(extra: &mut Value, delta: &JsMetrics) {
    let js_info = json!({
        "evalCount": delta.eval_count,
        "fastPathHits": delta.fast_path_hits,
        "cacheHits": delta.cache_hits,
        "evalMs": delta.eval_ms,
        "httpRequests": delta.http_requests,
        "httpMs": delta.http_ms,
    });
    match extra {
        Value::Object(map) => {
            map.insert("js".to_string(), js_info);
        }
        Value::Null => {
            *extra = json!({ "js": js_info });
        }
        other => {
            let prev = other.take();
            *other = json!({ "result": prev, "js": js_info });
        }
    }
}
