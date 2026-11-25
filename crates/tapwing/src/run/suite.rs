//! Single-shard and multi-shard suite execution, thread-safe progress logging,
//! generic [`FlowDriver`] test seam, and report output wiring.
//!
//! A shard that errors, panics or is interrupted still yields a
//! [`SuiteResult`] (see [`super::shard_outcome`]), so the other shards' results
//! and every requested report survive.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::backend::{FlowDriver, LOOKUP_TIMEOUT_MS, OPTIONAL_LOOKUP_TIMEOUT_MS};
use crate::device::{Device, Platform, SessionGuards};
use crate::driver::Driver;
use crate::fake_driver::FakeDriver;
use crate::flow::ParsedFlow;
use crate::js::default_http_agent;
use crate::native_lane::{InputMode, NativeLaneHandle};
use crate::orchestra::{Orchestra, OrchestraOpts, print_prefixed};
use crate::report::{self, FlowRunRecord, FlowStatus, ReportFormat, StepRecord, SuiteResult, TestExecutionSummary};
use crate::run::shard_outcome::{self, ReportSinks, ShardMeta, UnrunFlow, panic_message, unrun_flow_record};
use crate::run::{DeviceFlutterBackend, IosNativeSlot, LaneStats, Opts, Session, flows_need_ios_native};
use crate::shard::{self, ShardPlan};
use crate::workspace::{self, ExecutionPlan};

/// Port the app dials on an Android device (compiled into the driver and
/// reversed to the session's host port).
pub const DEVICE_PORT: u16 = 7781;

/// Full configuration for a `tapwing test` suite invocation.
pub struct SuiteRunConfig {
    pub plan: ExecutionPlan,
    pub shard_split: Option<usize>,
    pub shard_all: Option<usize>,
    pub shards: Option<usize>,
    pub device: Option<String>,
    pub platform: Option<Platform>,
    pub port: u16,
    pub report_json: Option<PathBuf>,
    pub report_format: ReportFormat,
    pub report_output: Option<PathBuf>,
    pub test_suite_name: Option<String>,
    pub test_output_dir: Option<PathBuf>,
    pub debug_output: Option<PathBuf>,
    pub flatten_debug_output: bool,
    pub cli_env: HashMap<String, String>,
    pub settle_policy: String,
    pub check_settle: bool,
    pub strict_input: bool,
    pub maestro_compat_strict: bool,
    pub input_mode: InputMode,
    /// F7 `--app-errors*` settings (CLI merged over `config.yaml` `tapwing:`).
    pub app_health: crate::app_health::AppHealthOpts,
    /// F8b `--trace`: writes `trace.json` + `trace.html` per shard.
    pub trace: Option<crate::trace::TraceOpts>,
}

fn now_epoch_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64).unwrap_or(0)
}

/// Executes a [`ShardPlan`] over any [`FlowDriver`] implementation (real device
/// backend or in-memory [`FakeDriver`]).
///
/// Always returns a [`SuiteResult`] covering every flow that started. After
/// `SIGINT`/`SIGTERM` the flows that never started are appended as
/// [`FlowStatus::NotRun`]; after a panic in `run_one` they are appended as
/// errored and [`SuiteResult::error`] is set.
pub fn execute_shard_with_driver<D, F>(
    shard_plan: &ShardPlan,
    base_opts: &OrchestraOpts,
    device_id: Option<&str>,
    device_description: Option<&str>,
    platform_name: &str,
    setup_ms: f64,
    run_one: F,
) -> SuiteResult
where
    D: FlowDriver,
    F: FnMut(&ParsedFlow, OrchestraOpts) -> (crate::orchestra::FlowResult, LaneStats, bool),
{
    let target = ShardTarget { device_id, device_description, platform_name, setup_ms };
    execute_shard(shard_plan, base_opts, &target, run_one, crate::interrupt::is_interrupted)
}

/// Device a shard runs on, as reported in its [`SuiteResult`].
pub(crate) struct ShardTarget<'a> {
    pub device_id: Option<&'a str>,
    pub device_description: Option<&'a str>,
    pub platform_name: &'a str,
    pub setup_ms: f64,
}

/// [`execute_shard_with_driver`] with the interrupt check injected, so tests
/// can interrupt a shard without touching the process-wide signal state.
pub(crate) fn execute_shard<F, I>(
    shard_plan: &ShardPlan,
    base_opts: &OrchestraOpts,
    target: &ShardTarget<'_>,
    mut run_one: F,
    is_interrupted: I,
) -> SuiteResult
where
    F: FnMut(&ParsedFlow, OrchestraOpts) -> (crate::orchestra::FlowResult, LaneStats, bool),
    I: Fn() -> bool,
{
    let suite_start_epoch_ms = now_epoch_ms();
    let t0 = Instant::now();
    let prefix = shard_plan.log_prefix();
    let device_id = target.device_id;
    let mut records = Vec::new();
    let mut all_ok = true;
    let mut skipped_count = 0usize;
    let mut error = None;

    // Sequential flows first, then the rest; `next` is the first flow not started yet.
    let sequence_len = shard_plan.sequence.len();
    let planned: Vec<&ParsedFlow> = shard_plan.sequence.iter().chain(&shard_plan.flows).collect();
    let mut next = 0usize;
    let mut stopped = false;
    while let Some(flow) = planned.get(next).copied() {
        if is_interrupted() {
            stopped = true;
            break;
        }
        let in_sequence = next < sequence_len;
        next += 1;
        let rec = match execute_single_flow(flow, base_opts, shard_plan.shard_index, &prefix, device_id, &mut run_one) {
            Ok(rec) => rec,
            // The driver session is in an unknown state after a panic: record
            // the flow as errored and stop the shard instead of the process.
            Err(panic) => {
                let message = format!("panicked while running flow {}: {panic}", workspace::flow_name(flow));
                records.push(unrun_flow_record(flow, UnrunFlow::Errored(&message)));
                error = Some(message);
                stopped = true;
                break;
            }
        };
        let ok = rec.passed();
        all_ok &= ok;
        records.push(rec);
        if is_interrupted() {
            stopped = true;
            break;
        }
        if in_sequence && !ok && !shard_plan.continue_on_failure {
            if base_opts.print_progress {
                print_prefixed(
                    &prefix,
                    &format!(
                        "Flow {} failed and continueOnFailure is set to false, aborting running sequential Flows",
                        workspace::flow_name(flow)
                    ),
                );
            }
            skipped_count = sequence_len.saturating_sub(next);
            next = sequence_len;
        }
    }

    if stopped {
        all_ok = false;
        let unrun = match &error {
            Some(message) => UnrunFlow::Errored(message),
            None => UnrunFlow::NotRun,
        };
        records.extend(planned.iter().skip(next).map(|flow| unrun_flow_record(flow, unrun)));
    }

    SuiteResult {
        shard_index: shard_plan.shard_index,
        device_id: device_id.map(str::to_string),
        device_description: target.device_description.or(device_id).map(str::to_string),
        platform: target.platform_name.to_string(),
        passed: all_ok,
        setup_ms: target.setup_ms,
        duration_ms: t0.elapsed().as_secs_f64() * 1000.0,
        start_epoch_ms: suite_start_epoch_ms,
        skipped_count,
        error,
        flows: records,
    }
}

/// Convenience test seam executing a [`ShardPlan`] against a [`FakeDriver`].
pub fn execute_shard_on_fake_driver(
    driver: &mut FakeDriver,
    shard_plan: &ShardPlan,
    base_opts: &OrchestraOpts,
    device_id: Option<&str>,
) -> SuiteResult {
    let platform_name = match driver.device_info().platform {
        crate::flow::ir::Platform::Android => "android",
        crate::flow::ir::Platform::Ios => "ios",
        crate::flow::ir::Platform::Web => "web",
    };
    execute_shard_with_driver::<FakeDriver, _>(
        shard_plan,
        base_opts,
        device_id,
        device_id,
        platform_name,
        0.0,
        |flow, opts| {
            let mut orchestra = Orchestra::new(driver, opts);
            let res = orchestra.run_parsed_flow(flow);
            let steps_len = res.steps.len() as u32;
            (res, LaneStats { flutter: steps_len, native: 0, ..Default::default() }, false)
        },
    )
}

fn execute_single_flow<F>(
    flow: &ParsedFlow,
    base_opts: &OrchestraOpts,
    shard_index: Option<usize>,
    prefix: &str,
    device_id: Option<&str>,
    run_one: &mut F,
) -> Result<FlowRunRecord, String>
where
    F: FnMut(&ParsedFlow, OrchestraOpts) -> (crate::orchestra::FlowResult, LaneStats, bool),
{
    let name = workspace::flow_name(flow);
    if base_opts.print_progress {
        if prefix.is_empty() {
            print_prefixed("", &format!("\n▶ {name}"));
        } else {
            print_prefixed(prefix, &format!("▶ {name}"));
        }
    }

    let flow_start_epoch_ms = now_epoch_ms();
    let mut opts = base_opts.clone();
    opts.flow_path = flow.path.clone();
    opts.shard_index = shard_index;
    opts.log_prefix = prefix.to_string();
    if let Some(id) = device_id {
        opts.device_id = Some(id.to_string());
    }

    let (result, lanes, blackbox) = match catch_unwind(AssertUnwindSafe(|| run_one(flow, opts))) {
        Ok(outcome) => outcome,
        Err(payload) => {
            let message = panic_message(payload.as_ref());
            if base_opts.print_progress {
                print_prefixed(prefix, &format!("ERROR {name}: panicked: {message}"));
            }
            return Err(message);
        }
    };

    if base_opts.print_progress {
        print_prefixed(
            prefix,
            &format!("{} {name} in {:.0} ms", if result.success { "PASSED" } else { "FAILED" }, result.total_ms),
        );
        if base_opts.check_settle {
            print_prefixed(
                prefix,
                &format!("settle check ({}): {} violation(s)", base_opts.settle_policy, result.settle_violations),
            );
        }
    }

    Ok(build_flow_record(flow, &name, result, flow_start_epoch_ms, &base_opts.settle_policy, lanes, blackbox))
}

/// Report `properties` and `tags` declared in the flow's config section.
pub(crate) fn flow_metadata(flow: &ParsedFlow) -> (Vec<(String, String)>, Vec<String>) {
    let properties = flow
        .config
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
        .collect();
    (properties, flow.config.tags.clone().unwrap_or_default())
}

pub(crate) fn build_flow_record(
    flow: &ParsedFlow,
    name: &str,
    result: crate::orchestra::FlowResult,
    start_epoch_ms: u64,
    settle_policy: &str,
    lanes: LaneStats,
    blackbox: bool,
) -> FlowRunRecord {
    // A flow cut short by SIGINT/SIGTERM is not a test failure: it gets its own
    // status even when a child process (`adb`, `iproxy`, `xcrun`) died from the
    // process-group signal first and surfaced as a transport error.
    let interrupted = !result.success
        && (result.error.as_ref().is_some_and(crate::interrupt::is_interrupted_error)
            || crate::interrupt::is_interrupted());
    let mut raw_json = result.to_report_json(name, &flow.path, settle_policy);
    if interrupted {
        raw_json["status"] = json!("interrupted");
    }
    raw_json["lane"] = json!(lanes.lane_name());
    raw_json["laneStats"] = json!({ "flutter": lanes.flutter, "native": lanes.native });
    if lanes.cold_start_retries > 0 {
        raw_json["coldStartRetries"] = json!(lanes.cold_start_retries);
    }
    if blackbox {
        raw_json["blackbox"] = json!(true);
    }

    let steps: Vec<StepRecord> = result
        .steps
        .iter()
        .map(|v| {
            let status = v["status"].as_str().unwrap_or("passed").to_string();
            let warning = v.get("warning").and_then(Value::as_str).map(str::to_string).or_else(|| {
                if status == "warned" { v.get("error").and_then(Value::as_str).map(str::to_string) } else { None }
            });
            let error =
                if status == "failed" { v.get("error").and_then(Value::as_str).map(str::to_string) } else { None };
            StepRecord {
                index: v["index"].as_u64().unwrap_or(0) as usize,
                command: v["command"].as_str().unwrap_or("").to_string(),
                description: v["description"].as_str().unwrap_or("").to_string(),
                status,
                duration_ms: v["ms"].as_f64().unwrap_or(0.0),
                warning,
                error,
                app_issues: v["appIssues"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| crate::app_health::console_line(i).trim_start_matches("⚠ ").to_string())
                    .collect(),
            }
        })
        .collect();

    let failure_message = if result.success {
        None
    } else if let Some(err) = &result.error {
        Some(err.to_string())
    } else {
        steps.iter().rev().find_map(|s| s.error.clone()).or_else(|| Some("Flow failed".to_string()))
    };

    let (properties, tags) = flow_metadata(flow);

    let trace = result.trace.clone();
    let failure_diagnostics = if result.success {
        None
    } else {
        result.steps.iter().rev().find_map(|s| s.get("failure").cloned()).and_then(|v| serde_json::from_value(v).ok())
    };

    FlowRunRecord {
        name: name.to_string(),
        path: flow.path.clone(),
        file: report::cwd_relative_path(&flow.path),
        status: if result.success {
            FlowStatus::Success
        } else if interrupted {
            FlowStatus::Interrupted
        } else {
            FlowStatus::Error
        },
        duration_ms: result.total_ms,
        start_epoch_ms,
        failure_message,
        properties,
        tags,
        lane: lanes.lane_name().to_string(),
        flutter_steps: lanes.flutter,
        native_steps: lanes.native,
        blackbox,
        steps,
        failure_diagnostics,
        raw_json,
        trace,
    }
}

/// Validates sharding and device flags, runs all shards (on the calling thread
/// when 1 shard, or in parallel via `std::thread::scope` when `> 1`), prints
/// the console summary, and writes all requested reports (`--format` and
/// `--report`).
///
/// A shard that errors or panics, and an interrupt, still produce the reports:
/// see [`shard_outcome::conclude_run`] for how the run then ends.
pub fn run_test_suite(cfg: SuiteRunConfig) -> Result<bool> {
    let total_flows = cfg.plan.sequence.len() + cfg.plan.flows.len();
    let shard_req = shard::resolve_shard_request(
        cfg.shard_split,
        cfg.shard_all,
        cfg.shards,
        cfg.device.as_deref(),
        cfg.port,
        cfg.plan.sequence.len(),
        total_flows,
    )?;
    for w in &shard_req.warnings {
        eprintln!("{w}");
    }

    let devices = Device::select_many(
        &shard_req.requested_devices,
        shard_req.requested_shards,
        shard_req.effective_shards,
        cfg.platform,
    )?;

    let disable_android_animations = cfg.plan.config.platform.android.disable_animations;
    if cfg.plan.config.platform.ios.disable_animations && devices.iter().any(|d| d.platform() == Platform::Ios) {
        eprintln!(
            "note: platform.ios.disableAnimations is a Maestro Cloud setting and has no local simctl equivalent; ignored"
        );
    }

    let (_, shard_out_dirs) = shard::resolve_output_dirs(
        cfg.test_output_dir.as_deref(),
        cfg.plan.config.test_output_dir.as_deref(),
        cfg.debug_output.as_deref(),
        cfg.flatten_debug_output,
        shard_req.effective_shards,
    );

    let trace_dirs = shard_out_dirs.clone();
    let shard_plans = shard::partition_plan(cfg.plan, shard_req.mode, shard_req.effective_shards);
    let http_agent = default_http_agent();
    let t0 = Instant::now();

    let shared = ShardSessionSettings {
        port: cfg.port,
        input_mode: cfg.input_mode,
        settle_policy: &cfg.settle_policy,
        check_settle: cfg.check_settle,
        strict_input: cfg.strict_input,
        maestro_compat_strict: cfg.maestro_compat_strict,
        cli_env: &cfg.cli_env,
        disable_android_animations,
        app_health: &cfg.app_health,
        trace: cfg.trace,
    };

    // Captured before the devices move into their shards, so a shard that
    // never returns a result can still be reported.
    let metas: Vec<ShardMeta> = devices
        .iter()
        .map(|dev| ShardMeta {
            device_id: dev.id().to_string(),
            device_description: dev.describe(),
            platform: dev.platform().wire_name().to_string(),
        })
        .collect();

    let results: Vec<Result<SuiteResult>> = if shard_req.effective_shards <= 1 {
        let dev = devices.into_iter().next().ok_or_else(|| anyhow!("no target device resolved"))?;
        let sp = shard_plans.first().ok_or_else(|| anyhow!("no shard plan resolved"))?;
        let out_dir = shard_out_dirs.into_iter().next().unwrap_or_else(|| PathBuf::from(".tapwing"));
        vec![shard_outcome::catch_shard_panic(|| run_device_shard(dev, sp, out_dir, &shared, Arc::clone(&http_agent)))]
    } else {
        run_shards_in_parallel(devices.into_iter().zip(&shard_plans).zip(shard_out_dirs), |((dev, sp), out_dir)| {
            run_device_shard(dev, sp, out_dir, &shared, Arc::clone(&http_agent))
        })
    };

    // Every shard has returned, so all device RAII guards have dropped: the
    // device is clean before any report is rendered, interrupted or not.
    let suites: Vec<SuiteResult> = shard_plans
        .iter()
        .zip(&metas)
        .zip(results)
        .map(|((plan, meta), result)| shard_outcome::suite_from_shard_result(plan, meta, result))
        .collect();
    let interrupted = crate::interrupt::signal();

    let elapsed = t0.elapsed();
    let passed = interrupted.is_none() && suites.iter().all(|s| s.passed);
    let utc_offset_secs = if cfg.report_format == ReportFormat::Noop { 0 } else { report::local_utc_offset_secs() };
    let summary = TestExecutionSummary {
        passed,
        suite_name: cfg.test_suite_name,
        input_mode: format!("{:?}", cfg.input_mode).to_lowercase(),
        total_ms: elapsed.as_secs_f64() * 1000.0,
        utc_offset_secs,
        interrupted,
        suites,
    };

    let sinks = ReportSinks {
        format: cfg.report_format,
        output: cfg.report_output.as_deref(),
        json: cfg.report_json.as_deref(),
        trace: cfg.trace.as_ref(),
        trace_dirs: &trace_dirs,
    };
    shard_outcome::conclude_run(&summary, &sinks, elapsed)
}

/// F8b: writes one trace per shard into its output dir and prints the paths.
/// A failed write is reported but never changes the run's result.
pub fn write_traces(suites: &[SuiteResult], dirs: &[PathBuf], opts: &crate::trace::TraceOpts) {
    for (idx, suite) in suites.iter().enumerate() {
        let dir = suite.shard_index.and_then(|i| dirs.get(i)).or_else(|| dirs.get(idx)).cloned().unwrap_or_default();
        match crate::trace::write_shard_trace(suite, &dir, opts) {
            Ok(files) => print_prefixed("", &format!("trace: {}", files.html.display())),
            Err(e) => eprintln!("warning: could not write trace: {e:#}"),
        }
    }
}

/// Runs `work` for each item concurrently via `std::thread::scope`, joining
/// **every** worker before returning so all RAII guards drop cleanly. Returns
/// one result per item, in order; a panicking worker becomes an `Err`, so one
/// shard's failure never discards the others' results.
pub(crate) fn run_shards_in_parallel<T, R, F>(items: impl IntoIterator<Item = T>, work: F) -> Vec<Result<R>>
where
    T: Send,
    R: Send,
    F: Fn(T) -> Result<R> + Sync,
{
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for item in items {
            let f = &work;
            handles.push(scope.spawn(move || f(item)));
        }
        handles
            .into_iter()
            .map(|h| match h.join() {
                Ok(result) => result,
                Err(payload) => Err(shard_outcome::shard_panic_error(payload.as_ref())),
            })
            .collect()
    })
}

#[derive(Clone, Copy)]
struct ShardSessionSettings<'a> {
    port: u16,
    input_mode: InputMode,
    settle_policy: &'a str,
    check_settle: bool,
    strict_input: bool,
    maestro_compat_strict: bool,
    cli_env: &'a HashMap<String, String>,
    disable_android_animations: bool,
    app_health: &'a crate::app_health::AppHealthOpts,
    trace: Option<crate::trace::TraceOpts>,
}

fn run_device_shard(
    mut dev: Device,
    shard_plan: &ShardPlan,
    test_output_dir: PathBuf,
    settings: &ShardSessionSettings<'_>,
    http_agent: Arc<ureq::Agent>,
) -> Result<SuiteResult> {
    let t0 = Instant::now();
    let mut drv = Driver::listen(settings.port)?;
    drv.expect_platform(dev.platform().wire_name());
    drv.set_liveness_probe(match &dev {
        Device::Android(a) => a.liveness_probe(),
        Device::Ios(_) => crate::device::ios::liveness_probe(),
    });
    let host_port = drv.port()?;
    let prefix = shard_plan.log_prefix();

    let mut all_flows: Vec<ParsedFlow> = Vec::with_capacity(shard_plan.sequence.len() + shard_plan.flows.len());
    all_flows.extend(shard_plan.sequence.iter().cloned());
    all_flows.extend(shard_plan.flows.iter().cloned());

    let mut native_lane = match (&dev, settings.input_mode) {
        (Device::Android(a), InputMode::Auto | InputMode::Native) => Some(NativeLaneHandle::spawn(&a.serial)),
        _ => None,
    };
    let mut ios_native_lane = match (&dev, settings.input_mode) {
        (Device::Ios(sim), InputMode::Native) => Some(IosNativeSlot::spawn(&sim.udid)),
        (Device::Ios(sim), InputMode::Auto) if flows_need_ios_native(&all_flows) => {
            Some(IosNativeSlot::spawn(&sim.udid))
        }
        (Device::Ios(sim), InputMode::Auto) => Some(IosNativeSlot::lazy(&sim.udid)),
        _ => None,
    };

    let (_guards, soft_keyboard_hint) = match &dev {
        Device::Android(a) => {
            let disable_anim = settings.disable_android_animations;
            let (reverse_res, awake_res, anim_res) = std::thread::scope(|scope| {
                let h_rev = scope.spawn(move || a.reverse(DEVICE_PORT, host_port));
                let h_awk = scope.spawn(move || a.keep_awake());
                let h_anim = if disable_anim { Some(scope.spawn(move || a.disable_animations())) } else { None };
                // Best effort: leftovers of a hard-killed earlier run never fail this one.
                let h_sweep = scope.spawn(move || a.sweep_orphan_recordings());
                if let Ok(Err(e)) = h_sweep.join() {
                    eprintln!("{prefix}warning: could not remove leftover screen recordings of an earlier run: {e:#}");
                }
                (
                    h_rev.join().map_err(|_| anyhow!("adb reverse setup thread panicked")),
                    h_awk.join().map_err(|_| anyhow!("keep_awake setup thread panicked")),
                    h_anim.map(|h| h.join().map_err(|_| anyhow!("disable_animations setup thread panicked"))),
                )
            });
            let reverse = reverse_res??;
            let awake_setup = awake_res??;
            let anim_guard = match anim_res {
                Some(r) => Some(r??),
                None => None,
            };
            (Some(SessionGuards::new(reverse, awake_setup.guard, anim_guard)), awake_setup.soft_keyboard_hint)
        }
        Device::Ios(sim) => (None, Some(!sim.hardware_keyboard_connected())),
    };

    let opts = Opts {
        settle_policy: settings.settle_policy.to_string(),
        check_settle: settings.check_settle,
        cli_env: settings.cli_env.clone(),
        test_output_dir: test_output_dir.clone(),
        strict_input: settings.strict_input,
        soft_keyboard_hint,
        maestro_compat_strict: settings.maestro_compat_strict,
        http_agent: Some(Arc::clone(&http_agent)),
        input_mode: settings.input_mode,
        shard_index: shard_plan.shard_index,
        log_prefix: prefix.clone(),
        app_errors: settings.app_health.policy,
        trace: settings.trace.is_some(),
    };

    let setup_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let dev_id = dev.id().to_string();
    let dev_desc = dev.describe();
    let platform_wire = dev.platform().wire_name().to_string();
    crate::interrupt::check()?;
    print_prefixed(&prefix, &format!("device {dev_desc} · setup {setup_ms:.0} ms"));

    let base_orch_opts = OrchestraOpts {
        lookup_timeout_ms: LOOKUP_TIMEOUT_MS,
        optional_lookup_timeout_ms: OPTIONAL_LOOKUP_TIMEOUT_MS,
        cli_env: settings.cli_env.clone(),
        test_output_dir: Some(test_output_dir),
        flow_path: PathBuf::new(),
        device_id: Some(dev_id.clone()),
        shard_index: shard_plan.shard_index,
        log_prefix: prefix,
        check_settle: settings.check_settle,
        settle_policy: settings.settle_policy.to_string(),
        print_progress: true,
        travel_sleep: true,
        http_agent: Some(http_agent),
        app_health: settings.app_health.clone(),
        trace: settings.trace,
    };

    let mut session = Session {
        dev: &mut dev,
        drv: &mut drv,
        opts: &opts,
        units_scale: 1.0,
        native: native_lane.as_mut(),
        ios_native: ios_native_lane.as_mut(),
    };

    Ok(execute_shard_with_driver::<DeviceFlutterBackend<'_>, _>(
        shard_plan,
        &base_orch_opts,
        Some(&dev_id),
        Some(&dev_desc),
        &platform_wire,
        setup_ms,
        |flow, orch_opts| {
            let mut backend = DeviceFlutterBackend::new(&mut session, flow.app_id().to_string());
            let mut orchestra = Orchestra::new(&mut backend, orch_opts);
            let res = orchestra.run_parsed_flow(flow);
            (res, backend.lane_stats(), backend.ran_blackbox())
        },
    ))
}

/// Prints the Maestro-style end-of-run summary (with per-shard breakdown when sharded).
pub fn print_execution_summary(summary: &TestExecutionSummary, elapsed: std::time::Duration) {
    let passed = summary.passed_count();
    let total = summary.total_executed();
    let skipped = summary.skipped_count();
    let skipped_part = if skipped > 0 { format!(", {skipped} skipped") } else { String::new() };

    if summary.suites.len() <= 1 {
        print_prefixed("", &format!("\n{passed}/{total} flows passed{skipped_part} in {:.1} s", elapsed.as_secs_f64()));
        for suite in &summary.suites {
            for flow in suite.flows.iter().filter(|f| f.failed()) {
                print_prefixed("", &format!("  FAILED {}", flow.name));
            }
        }
        return;
    }

    print_prefixed(
        "",
        &format!(
            "\n{passed}/{total} flows passed{skipped_part} across {} shards in {:.1} s",
            summary.suites.len(),
            elapsed.as_secs_f64()
        ),
    );
    for (idx, suite) in summary.suites.iter().enumerate() {
        let shard_num = suite.shard_index.unwrap_or(idx) + 1;
        let dev = suite.device_description.as_deref().or(suite.device_id.as_deref()).unwrap_or("device");
        print_prefixed(
            "",
            &format!(
                "  [shard {shard_num}] {dev}: {}/{} passed ({:.1} s)",
                suite.passed_count(),
                suite.total_executed(),
                suite.duration_ms / 1000.0
            ),
        );
    }
    for (idx, suite) in summary.suites.iter().enumerate() {
        let shard_num = suite.shard_index.unwrap_or(idx) + 1;
        for flow in suite.flows.iter().filter(|f| f.failed()) {
            print_prefixed("", &format!("  FAILED [shard {shard_num}] {}", flow.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct DropCounter<'a>(&'a AtomicUsize);
    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn run_shards_in_parallel_joins_all_shards_drops_guards_and_keeps_every_result() {
        let drops = AtomicUsize::new(0);
        let results = run_shards_in_parallel([0usize, 1, 2], |idx| {
            let _guard = DropCounter(&drops);
            match idx {
                0 => Err(anyhow!("device locked")),
                1 => Ok("ok"),
                _ => Err(anyhow!("adb reverse failed")),
            }
        });

        assert_eq!(drops.load(Ordering::SeqCst), 3, "every shard must run to completion and drop its guards");
        let shown: Vec<String> =
            results.iter().map(|r| r.as_ref().map_or_else(|e| e.to_string(), |v| v.to_string())).collect();
        assert_eq!(shown, ["device locked", "ok", "adb reverse failed"], "one result per shard, in shard order");
    }

    #[test]
    fn run_shards_in_parallel_converts_worker_panic_into_error_and_keeps_other_results() {
        let results = run_shards_in_parallel([0usize, 1, 2], |idx| -> Result<usize> {
            match idx {
                0 => Err(anyhow!("setup failed")),
                1 => panic!("boom"),
                _ => Ok(7),
            }
        });

        assert_eq!(results[0].as_ref().unwrap_err().to_string(), "setup failed");
        assert_eq!(results[1].as_ref().unwrap_err().to_string(), "shard worker thread panicked: boom");
        assert_eq!(*results[2].as_ref().unwrap(), 7);
    }

    #[test]
    fn cold_start_retries_json_schema_single_and_multi_shard_and_reports() {
        let flow = crate::flow::Loader::new()
            .parse(std::path::Path::new("smoke.yaml"), "appId: com.example.app\nname: smoke\n---\n- launchApp\n")
            .unwrap();

        let make_result = |handshake_retries: u32| {
            let mut phases = json!({ "acceptMs": 10.0, "configureMs": 5.0, "waitIdleMs": 5.0 });
            if handshake_retries > 0 {
                phases["handshakeRetries"] = json!(handshake_retries);
            }
            crate::orchestra::FlowResult {
                success: true,
                error: None,
                steps: vec![json!({
                    "index": 0,
                    "command": "launchApp",
                    "description": "launchApp com.example.app",
                    "status": "passed",
                    "ms": 20.0,
                    "driver": { "phases": phases },
                })],
                settle_violations: 0,
                total_ms: 20.0,
                js_metrics: Default::default(),
                js_closed: true,
                dropped_app_issues: 0,
                trace: Some(json!({
                    "http": "off",
                    "httpDropped": 0,
                    "steps": [{
                        "index": 0,
                        "command": "launchApp",
                        "description": "launchApp com.example.app",
                        "status": "passed",
                        "ms": 20.0,
                        "startMs": 0.0,
                        "depth": 0,
                        "http": [],
                        "driver": { "phases": phases },
                    }],
                })),
            }
        };

        let clean = build_flow_record(
            &flow,
            "clean",
            make_result(0),
            1_700_000_000_000,
            "v2",
            LaneStats { flutter: 1, native: 0, cold_start_retries: 0 },
            false,
        );
        let retried = build_flow_record(
            &flow,
            "retried",
            make_result(1),
            1_700_000_000_000,
            "v2",
            LaneStats { flutter: 1, native: 1, cold_start_retries: 2 },
            false,
        );

        assert!(clean.raw_json.get("coldStartRetries").is_none(), "omitted when 0");
        assert!(clean.raw_json["steps"][0]["driver"]["phases"].get("handshakeRetries").is_none());
        assert_eq!(retried.raw_json["coldStartRetries"], 2);
        assert_eq!(retried.raw_json["steps"][0]["driver"]["phases"]["handshakeRetries"], 1);

        let make_suite = |shard_index, flows| SuiteResult {
            shard_index,
            device_id: Some("emu-1".into()),
            device_description: Some("emu-1".into()),
            platform: "android".into(),
            passed: true,
            setup_ms: 5.0,
            duration_ms: 40.0,
            start_epoch_ms: 1_700_000_000_000,
            skipped_count: 0,
            error: None,
            flows,
        };

        // Single-shard JSON + JUnit + HTML + trace.
        let single_suite = make_suite(None, vec![clean.clone(), retried.clone()]);
        let single_summary = TestExecutionSummary {
            passed: true,
            suite_name: Some("ColdStart".into()),
            input_mode: "auto".into(),
            total_ms: 40.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![single_suite.clone()],
        };
        let single_json: Value = serde_json::from_str(&report::render_json_report(&single_summary).unwrap()).unwrap();
        assert!(single_json["flows"][0].get("coldStartRetries").is_none());
        assert_eq!(single_json["flows"][1]["coldStartRetries"], 2);
        let junit = report::junit::render_junit(&single_summary).unwrap();
        assert!(junit.contains("name=\"retried\"") && junit.contains("status=\"SUCCESS\""));
        let html = report::html::render_html(&single_summary, true);
        assert!(html.contains("retried") && html.contains("launchApp com.example.app"));

        let trace = crate::trace::build_shard_trace(&single_suite, &crate::trace::TraceOpts::default());
        assert!(trace["flows"][0].get("coldStartRetries").is_none());
        assert!(
            trace["flows"][1].get("coldStartRetries").is_none(),
            "flow-level trace does not carry coldStartRetries"
        );
        assert_eq!(trace["flows"][1]["steps"][0]["driver"]["phases"]["handshakeRetries"], 1);

        // Multi-shard merged JSON keeps coldStartRetries in both flattened `flows` and `shards[].flows`.
        let multi_summary = TestExecutionSummary {
            passed: true,
            suite_name: Some("ColdStart".into()),
            input_mode: "auto".into(),
            total_ms: 40.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![make_suite(Some(0), vec![clean]), make_suite(Some(1), vec![retried])],
        };
        let multi_json: Value = serde_json::from_str(&report::render_json_report(&multi_summary).unwrap()).unwrap();
        assert!(multi_json["flows"][0].get("coldStartRetries").is_none());
        assert_eq!(multi_json["flows"][1]["coldStartRetries"], 2);
        assert!(multi_json["shards"][0]["flows"][0].get("coldStartRetries").is_none());
        assert_eq!(multi_json["shards"][1]["flows"][0]["coldStartRetries"], 2);
    }
}
