//! How a suite run ends when it does not end normally.
//!
//! A shard can fail to produce results in three ways: its setup returns an
//! error (device locked, `adb reverse` failed), its thread panics, or
//! `SIGINT`/`SIGTERM` arrives. None of them may discard what the other shards
//! (or the same shard's finished flows) measured, so each is turned into a
//! [`SuiteResult`] and every requested report is still written. Only then does
//! [`conclude_run`] decide how the process ends.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use serde_json::json;

use crate::flow::ParsedFlow;
use crate::interrupt::{InterruptedError, is_interrupted_error};
use crate::report::{self, FlowRunRecord, FlowStatus, ReportFormat, SuiteResult, TestExecutionSummary};
use crate::run::LaneStats;
use crate::run::suite::{flow_metadata, print_execution_summary, write_traces};
use crate::shard::ShardPlan;
use crate::trace::TraceOpts;
use crate::workspace;

/// What is known about a shard's device before the shard runs: enough to
/// report the shard when it never returns a [`SuiteResult`].
pub(crate) struct ShardMeta {
    pub device_id: String,
    pub device_description: String,
    pub platform: String,
}

/// Why a planned flow has no result of its own.
#[derive(Clone, Copy)]
pub(crate) enum UnrunFlow<'a> {
    /// Its shard errored or panicked; the message is the shard's error.
    Errored(&'a str),
    /// The run was interrupted before the flow started.
    NotRun,
}

/// Report record for a flow that did not run (to completion).
pub(crate) fn unrun_flow_record(flow: &ParsedFlow, why: UnrunFlow<'_>) -> FlowRunRecord {
    let name = workspace::flow_name(flow);
    let (status, json_status, failure_message) = match why {
        UnrunFlow::Errored(message) => (FlowStatus::Error, "errored", Some(message.to_string())),
        UnrunFlow::NotRun => (FlowStatus::NotRun, "notRun", None),
    };
    let mut raw_json = json!({
        "flow": name,
        "path": flow.path,
        "status": json_status,
        "totalMs": 0.0,
        "steps": [],
    });
    if let Some(message) = &failure_message {
        raw_json["error"] = json!(message);
    }
    let (properties, tags) = flow_metadata(flow);
    FlowRunRecord {
        name,
        path: flow.path.clone(),
        file: report::cwd_relative_path(&flow.path),
        status,
        duration_ms: 0.0,
        start_epoch_ms: now_epoch_ms(),
        failure_message,
        properties,
        tags,
        lane: LaneStats::default().lane_name().to_string(),
        flutter_steps: 0,
        native_steps: 0,
        blackbox: false,
        blackbox_reason: None,
        steps: Vec::new(),
        failure_diagnostics: None,
        raw_json,
        trace: None,
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64).unwrap_or(0)
}

/// Text of a panic payload (`panic!("…")` carries a `&str` or a `String`).
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

pub(crate) fn shard_panic_error(payload: &(dyn Any + Send)) -> anyhow::Error {
    anyhow!("shard worker thread panicked: {}", panic_message(payload))
}

/// Runs a shard on the calling thread, turning a panic into the same error a
/// panicking shard thread yields. The shard's RAII guards have already
/// dropped during the unwind.
pub(crate) fn catch_shard_panic<R>(work: impl FnOnce() -> Result<R>) -> Result<R> {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|payload| Err(shard_panic_error(payload.as_ref())))
}

/// The suite result of one shard, whatever became of it:
/// - `Ok`: its own result;
/// - interrupted during setup: every flow [`FlowStatus::NotRun`];
/// - any other error (or panic): every flow errored with the shard's error.
pub(crate) fn suite_from_shard_result(plan: &ShardPlan, meta: &ShardMeta, result: Result<SuiteResult>) -> SuiteResult {
    suite_from_shard_result_with(plan, meta, result, crate::interrupt::is_interrupted())
}

fn suite_from_shard_result_with(
    plan: &ShardPlan,
    meta: &ShardMeta,
    result: Result<SuiteResult>,
    process_interrupted: bool,
) -> SuiteResult {
    let err = match result {
        Ok(suite) => return suite,
        Err(err) => err,
    };
    let error = (!(is_interrupted_error(&err) || process_interrupted)).then(|| format!("{err:#}"));
    let unrun = match &error {
        Some(message) => UnrunFlow::Errored(message),
        None => UnrunFlow::NotRun,
    };
    let flows = plan.sequence.iter().chain(&plan.flows).map(|flow| unrun_flow_record(flow, unrun)).collect();
    SuiteResult {
        shard_index: plan.shard_index,
        device_id: Some(meta.device_id.clone()),
        device_description: Some(meta.device_description.clone()),
        platform: meta.platform.clone(),
        passed: false,
        setup_ms: 0.0,
        duration_ms: 0.0,
        start_epoch_ms: now_epoch_ms(),
        skipped_count: 0,
        error,
        flows,
    }
}

/// Where the reports of a run go.
pub(crate) struct ReportSinks<'a> {
    pub format: ReportFormat,
    pub output: Option<&'a Path>,
    pub json: Option<&'a Path>,
    pub trace: Option<&'a TraceOpts>,
    pub trace_dirs: &'a [PathBuf],
}

impl ReportSinks<'_> {
    /// Writes every requested report. Each one is attempted even if an
    /// earlier one fails; the first failure is returned.
    fn write_all(&self, summary: &TestExecutionSummary) -> Result<()> {
        if let Some(trace_opts) = self.trace {
            write_traces(&summary.suites, self.trace_dirs, trace_opts);
        }
        let formatted = report::write_formatted_report(self.format, self.output, summary).map(|_| ());
        let json = match self.json {
            Some(path) => report::render_json_report(summary)
                .and_then(|rendered| report::write_file_creating_parents(path, &rendered)),
            None => Ok(()),
        };
        formatted.and(json)
    }

    fn any(&self) -> bool {
        self.format != ReportFormat::Noop || self.json.is_some() || self.trace.is_some()
    }
}

/// Prints the console summary, writes every requested report and decides how
/// the run ends:
/// - interrupted: a partial report, then [`InterruptedError`] (the CLI exits
///   with `128 + signal`);
/// - a shard errored: the full report including the other shards, then that
///   error (exit code `2`, as before reports survived it);
/// - otherwise `Ok(passed)`.
///
/// Called after every shard has returned, so device cleanup is already done
/// and only local rendering and file writes remain. A second signal still
/// exits the process immediately (see [`crate::interrupt`]).
pub(crate) fn conclude_run(summary: &TestExecutionSummary, sinks: &ReportSinks<'_>, elapsed: Duration) -> Result<bool> {
    let interrupted = summary.interrupted;
    if interrupted.is_none() && (summary.suites.len() > 1 || summary.total_executed() + summary.skipped_count() > 1) {
        print_execution_summary(summary, elapsed);
    }

    let written = sinks.write_all(summary);

    if let Some(signal) = interrupted {
        match written {
            Ok(()) if sinks.any() => eprintln!("partial report written (the run was interrupted)"),
            Ok(()) => {}
            Err(e) => eprintln!("warning: could not write the partial report: {e:#}"),
        }
        return Err(InterruptedError { signal }.into());
    }

    let shard_errors = summary.shard_errors();
    if !shard_errors.is_empty() {
        if let Err(e) = written {
            eprintln!("warning: {e:#}");
        }
        anyhow::bail!("{}", shard_errors.join("; "));
    }

    written?;
    Ok(summary.passed)
}

#[cfg(test)]
mod tests {
    use crate::run::Blackbox;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    use quick_xml::Reader;
    use quick_xml::events::Event;
    use serde_json::Value;

    use super::*;
    use crate::fake_driver::FakeDriver;
    use crate::orchestra::{FlowResult, Orchestra, OrchestraOpts};
    use crate::run::suite::{ShardTarget, execute_shard, execute_shard_on_fake_driver, run_shards_in_parallel};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tapwing-outcome-{tag}-{}-{seq}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn json(&self, name: &str) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.0.join(name)).unwrap()).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn flow(name: &str) -> ParsedFlow {
        crate::flow::Loader::new()
            .parse(
                Path::new(&format!("{name}.yaml")),
                &format!("appId: com.example.app\nname: {name}\n---\n- launchApp\n"),
            )
            .unwrap()
    }

    fn plan(shard_index: Option<usize>, sequence: &[&str], flows: &[&str]) -> ShardPlan {
        ShardPlan {
            shard_index,
            sequence: sequence.iter().map(|n| flow(n)).collect(),
            flows: flows.iter().map(|n| flow(n)).collect(),
            continue_on_failure: false,
        }
    }

    fn meta(device: &str) -> ShardMeta {
        ShardMeta { device_id: device.into(), device_description: device.into(), platform: "android".into() }
    }

    fn opts() -> OrchestraOpts {
        OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() }
    }

    fn fake_driver() -> FakeDriver {
        let mut driver = FakeDriver::new();
        driver.add_installed_app("com.example.app");
        driver
    }

    fn run_on_fake(
        driver: &mut FakeDriver,
        flow: &ParsedFlow,
        opts: OrchestraOpts,
    ) -> (FlowResult, LaneStats, Blackbox) {
        let result = Orchestra::new(driver, opts).run_parsed_flow(flow);
        (result, LaneStats::default(), Blackbox::default())
    }

    fn target(device: &str) -> ShardTarget<'_> {
        ShardTarget {
            device_id: Some(device),
            device_description: Some(device),
            platform_name: "android",
            setup_ms: 0.0,
        }
    }

    fn summary(suites: Vec<SuiteResult>, interrupted: Option<i32>) -> TestExecutionSummary {
        TestExecutionSummary {
            passed: interrupted.is_none() && suites.iter().all(|s| s.passed),
            suite_name: None,
            input_mode: "auto".into(),
            total_ms: 10.0,
            utc_offset_secs: 0,
            interrupted,
            suites,
        }
    }

    fn statuses(doc: &Value) -> Vec<&str> {
        doc["flows"].as_array().unwrap().iter().map(|f| f["status"].as_str().unwrap()).collect()
    }

    /// Asserts `xml` is well-formed and returns `(name, status, child elements)` per `<testcase>`.
    fn junit_testcases(xml: &str) -> Vec<(String, String, Vec<String>)> {
        let mut reader = Reader::from_str(xml);
        let mut cases: Vec<(String, String, Vec<String>)> = Vec::new();
        let mut depth = 0usize;
        let note = |e: &quick_xml::events::BytesStart<'_>, cases: &mut Vec<(String, String, Vec<String>)>| {
            let tag = e.name().into_inner().to_string();
            if tag == "testcase" {
                let attr = |key: &str| e.try_get_attribute(key).unwrap().map(|a| a.value.into_owned()).unwrap();
                cases.push((attr("name"), attr("status"), Vec::new()));
            } else if let Some(case) = cases.last_mut().filter(|_| tag != "testsuite" && tag != "testsuites") {
                case.2.push(tag);
            }
        };
        loop {
            match reader.read_event().expect("well-formed JUnit XML") {
                Event::Start(e) => {
                    note(&e, &mut cases);
                    depth += 1;
                }
                Event::Empty(e) => note(&e, &mut cases),
                Event::End(_) => depth = depth.checked_sub(1).expect("unbalanced closing tag"),
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(depth, 0, "unclosed XML elements remain");
        cases
    }

    #[test]
    fn errored_shard_keeps_other_shards_results_writes_every_report_and_fails_the_run() {
        let scratch = Scratch::new("shard-error");
        let plans = [plan(Some(0), &[], &["a", "b"]), plan(Some(1), &[], &["c", "d"])];
        let metas = [meta("emu-1"), meta("emu-2")];

        let results = run_shards_in_parallel(plans.iter(), |sp| match sp.shard_index {
            Some(0) => Ok(execute_shard_on_fake_driver(&mut fake_driver(), sp, &opts(), Some("emu-1"))),
            _ => Err(anyhow!("adb reverse failed").context("setting up emu-2")),
        });
        let suites: Vec<SuiteResult> = plans
            .iter()
            .zip(&metas)
            .zip(results)
            .map(|((plan, meta), result)| suite_from_shard_result(plan, meta, result))
            .collect();
        let summary = summary(suites, None);

        let trace_dirs = [scratch.0.join("shard-1"), scratch.0.join("shard-2")];
        let trace_opts = TraceOpts::default();
        let junit_path = scratch.0.join("report.xml");
        let json_path = scratch.0.join("run.json");
        let sinks = ReportSinks {
            format: ReportFormat::Junit,
            output: Some(&junit_path),
            json: Some(&json_path),
            trace: Some(&trace_opts),
            trace_dirs: &trace_dirs,
        };
        let err = conclude_run(&summary, &sinks, Duration::from_millis(10)).unwrap_err();
        assert_eq!(err.to_string(), "[shard 2] setting up emu-2: adb reverse failed", "the run must fail");
        assert!(!is_interrupted_error(&err));

        let doc = scratch.json("run.json");
        assert_eq!(statuses(&doc), ["passed", "passed", "errored", "errored"]);
        assert_eq!(doc["flows"][2]["error"], "setting up emu-2: adb reverse failed");
        assert_eq!(doc["flows"][2]["device"], "emu-2");
        assert_eq!(doc["shards"][0]["passed"], true);
        assert_eq!(doc["shards"][0]["passedCount"], 2);
        assert!(doc["shards"][0].get("error").is_none());
        assert_eq!(doc["shards"][1]["passed"], false);
        assert_eq!(doc["shards"][1]["failedCount"], 2);
        assert_eq!(doc["shards"][1]["error"], "setting up emu-2: adb reverse failed");
        assert_eq!(
            doc["shardErrors"],
            json!([{ "shardIndex": 1, "shardId": 2, "device": "emu-2", "error": "setting up emu-2: adb reverse failed" }])
        );
        assert_eq!(doc["summary"]["passed"], false);
        assert_eq!(doc["summary"]["passedCount"], 2);
        assert_eq!(doc["summary"]["failedCount"], 2);
        assert!(doc.get("interrupted").is_none());

        let junit = std::fs::read_to_string(&junit_path).unwrap();
        let cases = junit_testcases(&junit);
        let shown: Vec<(&str, &str)> = cases.iter().map(|(n, s, _)| (n.as_str(), s.as_str())).collect();
        assert_eq!(shown, [("a", "SUCCESS"), ("b", "SUCCESS"), ("c", "ERROR"), ("d", "ERROR")]);
        assert_eq!(cases[2].2, ["failure"]);
        assert!(junit.contains("device=\"emu-2\" tests=\"2\" failures=\"2\""), "{junit}");
        assert!(junit.contains("<failure>[shard 2] setting up emu-2: adb reverse failed</failure>"), "{junit}");

        let html = report::html::render_html(&summary, true);
        assert!(html.contains("setting up emu-2: adb reverse failed"));

        assert!(scratch.0.join("shard-1/trace-shard-1.html").is_file());
        let trace = scratch.json("shard-2/trace-shard-2.json");
        assert_eq!(trace["shard"]["error"], "setting up emu-2: adb reverse failed");
        assert_eq!(trace["flows"][0]["status"], "failed");
    }

    #[test]
    fn panicking_shard_worker_is_reported_like_an_errored_shard() {
        let plans = [plan(Some(0), &[], &["a"]), plan(Some(1), &[], &["b"])];
        let metas = [meta("emu-1"), meta("emu-2")];
        let results = run_shards_in_parallel(plans.iter(), |sp| match sp.shard_index {
            Some(0) => Ok(execute_shard_on_fake_driver(&mut fake_driver(), sp, &opts(), Some("emu-1"))),
            _ => panic!("boom in setup"),
        });
        let suites: Vec<SuiteResult> = plans
            .iter()
            .zip(&metas)
            .zip(results)
            .map(|((plan, meta), result)| suite_from_shard_result(plan, meta, result))
            .collect();
        let summary = summary(suites, None);

        let doc: Value = serde_json::from_str(&report::render_json_report(&summary).unwrap()).unwrap();
        assert_eq!(statuses(&doc), ["passed", "errored"]);
        assert_eq!(doc["flows"][1]["error"], "shard worker thread panicked: boom in setup");
        junit_testcases(&report::junit::render_junit(&summary).unwrap());

        let sinks = ReportSinks { format: ReportFormat::Noop, output: None, json: None, trace: None, trace_dirs: &[] };
        let err = conclude_run(&summary, &sinks, Duration::ZERO).unwrap_err();
        assert_eq!(err.to_string(), "[shard 2] shard worker thread panicked: boom in setup");

        // The single-shard path runs on the calling thread and must not unwind out of it.
        let caught = catch_shard_panic(|| -> Result<()> { panic!("boom on main") }).unwrap_err();
        assert_eq!(caught.to_string(), "shard worker thread panicked: boom on main");
    }

    #[test]
    fn panic_inside_a_flow_keeps_finished_flows_and_errors_the_rest_of_the_shard() {
        let scratch = Scratch::new("flow-panic");
        let shard_plan = plan(None, &[], &["a", "b", "c"]);
        let mut driver = fake_driver();
        let suite = execute_shard(
            &shard_plan,
            &opts(),
            &target("emu-1"),
            |flow, opts| {
                if workspace::flow_name(flow) == "b" {
                    panic!("driver state corrupted");
                }
                run_on_fake(&mut driver, flow, opts)
            },
            || false,
        );

        let message = "panicked while running flow b: driver state corrupted";
        assert_eq!(suite.error.as_deref(), Some(message));
        assert!(!suite.passed);
        assert_eq!((suite.passed_count(), suite.failed_count(), suite.total_executed()), (1, 2, 3));

        let summary = summary(vec![suite], None);
        let json_path = scratch.0.join("run.json");
        let sinks = ReportSinks {
            format: ReportFormat::Noop,
            output: None,
            json: Some(&json_path),
            trace: None,
            trace_dirs: &[],
        };
        let err = conclude_run(&summary, &sinks, Duration::ZERO).unwrap_err();
        assert_eq!(err.to_string(), message, "single-shard errors carry no shard prefix");

        // Single-shard JSON keeps its legacy shape and gains `shardErrors`.
        let doc = scratch.json("run.json");
        assert_eq!(statuses(&doc), ["passed", "errored", "errored"]);
        assert_eq!(doc["flows"][2]["error"], message);
        assert_eq!(doc["shardErrors"][0]["error"], message);
        assert!(doc.get("shards").is_none());
        junit_testcases(&report::junit::render_junit(&summary).unwrap());
    }

    fn interrupted_flow_result(signal: i32) -> FlowResult {
        FlowResult {
            success: false,
            error: Some(InterruptedError { signal }.into()),
            steps: Vec::new(),
            settle_violations: 0,
            total_ms: 5.0,
            js_metrics: Default::default(),
            js_closed: true,
            dropped_app_issues: 0,
            trace: None,
        }
    }

    #[test]
    fn interrupt_mid_suite_writes_a_partial_report_and_keeps_finished_flows() {
        let scratch = Scratch::new("interrupt");
        let shard_plan = plan(None, &["a", "b", "c"], &["d"]);
        let mut driver = fake_driver();
        let interrupted = Cell::new(false);
        let suite = execute_shard(
            &shard_plan,
            &opts(),
            &target("emu-1"),
            |flow, opts| {
                if workspace::flow_name(flow) == "b" {
                    // SIGTERM arrives while `b` runs: the orchestrator unwinds with InterruptedError.
                    interrupted.set(true);
                    return (interrupted_flow_result(15), LaneStats::default(), Blackbox::default());
                }
                run_on_fake(&mut driver, flow, opts)
            },
            || interrupted.get(),
        );

        assert!(!suite.passed);
        assert_eq!(suite.error, None, "an interrupt is not a shard error");
        assert_eq!(suite.skipped_count, 0);
        let shown: Vec<FlowStatus> = suite.flows.iter().map(|f| f.status).collect();
        assert_eq!(shown, [FlowStatus::Success, FlowStatus::Interrupted, FlowStatus::NotRun, FlowStatus::NotRun]);
        assert_eq!(
            (suite.passed_count(), suite.failed_count(), suite.not_run_count(), suite.total_executed()),
            (1, 1, 2, 2)
        );

        let summary = summary(vec![suite], Some(15));
        let junit_path = scratch.0.join("out/report.xml");
        let json_path = scratch.0.join("out/run.json");
        let sinks = ReportSinks {
            format: ReportFormat::Junit,
            output: Some(&junit_path),
            json: Some(&json_path),
            trace: None,
            trace_dirs: &[],
        };
        let err = conclude_run(&summary, &sinks, Duration::ZERO).unwrap_err();
        assert_eq!(err.downcast_ref::<InterruptedError>(), Some(&InterruptedError { signal: 15 }));

        let doc = scratch.json("out/run.json");
        assert_eq!(doc["interrupted"], true);
        assert_eq!(doc["interruptSignal"], "SIGTERM");
        assert_eq!(statuses(&doc), ["passed", "interrupted", "notRun", "notRun"]);
        assert_eq!(doc["flows"][0]["steps"].as_array().unwrap().len(), 1, "the finished flow keeps its steps");
        assert!(doc.get("shardErrors").is_none());

        let junit = std::fs::read_to_string(&junit_path).unwrap();
        let cases = junit_testcases(&junit);
        let shown: Vec<(&str, &str)> = cases.iter().map(|(n, s, _)| (n.as_str(), s.as_str())).collect();
        assert_eq!(shown, [("a", "SUCCESS"), ("b", "STOPPED"), ("c", "CANCELED"), ("d", "CANCELED")]);
        assert_eq!(cases[1].2, ["failure"]);
        assert_eq!(cases[2].2, ["skipped"]);
        assert!(junit.contains("tests=\"4\" failures=\"1\" skipped=\"2\""), "{junit}");
        assert!(junit.contains("<failure>terminated by SIGTERM</failure>"), "{junit}");
        assert!(junit.contains("<skipped message=\"not run: terminated by SIGTERM\"/>"), "{junit}");

        let html = report::html::render_html(&summary, true);
        assert!(html.contains("interrupted by SIGTERM, partial results"), "{html}");
        assert!(html.contains("1 of 2 passed, 2 not run"), "{html}");

        let trace = crate::trace::build_shard_trace(&summary.suites[0], &TraceOpts::default());
        assert_eq!(trace["interrupted"], true);
        assert_eq!(trace["flows"][1]["status"], "failed");
        assert_eq!(trace["flows"][2]["status"], "skipped");
    }

    #[test]
    fn interrupt_between_flows_and_during_setup_marks_every_remaining_flow_not_run() {
        // Signal already pending when the shard starts its first flow.
        let shard_plan = plan(Some(0), &[], &["a", "b"]);
        let suite = execute_shard(
            &shard_plan,
            &opts(),
            &target("emu-1"),
            |_, _| unreachable!("no flow may start after the interrupt"),
            || true,
        );
        assert!(!suite.passed);
        assert!(suite.flows.iter().all(FlowRunRecord::not_run));
        assert_eq!(suite.flows.len(), 2);

        // Shard setup itself aborted with InterruptedError (even wrapped in context).
        let setup_err = anyhow::Error::new(InterruptedError { signal: 2 }).context("waiting for the device");
        let suite = suite_from_shard_result(&plan(Some(1), &[], &["c"]), &meta("emu-2"), Err(setup_err));
        assert_eq!(suite.error, None);
        assert_eq!(suite.device_id.as_deref(), Some("emu-2"));
        assert!(!suite.passed);
        assert_eq!(suite.flows[0].status, FlowStatus::NotRun);

        // Child process died from the process-group SIGINT first and surfaced as a plain error.
        let transport_err = anyhow::anyhow!("adb reverse failed: connection reset by peer");
        let suite = suite_from_shard_result_with(&plan(Some(1), &[], &["c"]), &meta("emu-2"), Err(transport_err), true);
        assert_eq!(suite.error, None);
        assert_eq!(suite.flows[0].status, FlowStatus::NotRun);

        let summary = summary(vec![suite], Some(2));
        let doc: Value = serde_json::from_str(&report::render_json_report(&summary).unwrap()).unwrap();
        assert_eq!(doc["interruptSignal"], "SIGINT");
        assert_eq!(statuses(&doc), ["notRun"]);
        let junit = report::junit::render_junit(&summary).unwrap();
        assert_eq!(junit_testcases(&junit)[0].2, ["skipped"]);
        assert!(junit.contains("tests=\"1\" failures=\"0\" skipped=\"1\""), "{junit}");
    }

    #[test]
    fn normal_run_is_unchanged_and_reports_its_result() {
        let shard_plan = plan(None, &[], &["a", "b"]);
        let suite = execute_shard_on_fake_driver(&mut fake_driver(), &shard_plan, &opts(), Some("emu-1"));
        let summary = summary(vec![suite], None);
        let sinks = ReportSinks { format: ReportFormat::Noop, output: None, json: None, trace: None, trace_dirs: &[] };
        assert!(conclude_run(&summary, &sinks, Duration::ZERO).unwrap());

        let doc: Value = serde_json::from_str(&report::render_json_report(&summary).unwrap()).unwrap();
        let keys: Vec<&str> = doc.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["platform", "inputMode", "setupMs", "flows"]);
        let junit = report::junit::render_junit(&summary).unwrap();
        assert!(!junit.contains("skipped"), "{junit}");
    }
}
