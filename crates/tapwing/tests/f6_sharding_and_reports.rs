//! End-to-end integration tests for F6 Part 2:
//! 1. Multi-shard `--shard-split 2` over `FakeDriver` (round-robin assignment,
//!    `${MAESTRO_SHARD_INDEX}`, `${MAESTRO_SHARD_ID}`, `${MAESTRO_DEVICE_UDID}`,
//!    per-shard `shard-1/` & `shard-2/` screenshot isolation, merged JUnit XML,
//!    HTML-detailed report, and multi-shard JSON report).
//! 2. Multi-shard `--shard-all 2` over `FakeDriver` with an injected failure on
//!    shard 1 (`<failure>[shard 2] ...</failure>`).
//! 3. Single-shard `executionOrder` + `continueOnFailure: false` over
//!    `FakeDriver` (aborted sequence flows counted in `skipped_count` and
//!    omitted from `<testcase>`, while non-sequence flows still execute).
//! 4. CLI binary validation smoke tests (exit code 2 on invalid flag combinations).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tapwing::fake_driver::{Bounds, FakeDriver};
use tapwing::orchestra::OrchestraOpts;
use tapwing::report::{self, SuiteResult, TestExecutionSummary};
use tapwing::run::suite::execute_shard_on_fake_driver;
use tapwing::shard::{self, ShardMode};
use tapwing::workspace;

static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_test_dir(tag: &str) -> PathBuf {
    let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("tapwing-f6-test-{tag}-{}-{seq}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

fn write_flow(dir: &Path, filename: &str, body: &str) -> PathBuf {
    let p = dir.join(filename);
    fs::write(&p, body).expect("write flow file");
    p
}

#[test]
fn multi_shard_split_executes_in_parallel_isolates_artifacts_and_writes_all_reports() {
    let ws = temp_test_dir("shard-split");
    let out_root = ws.join("out");

    for (file, flow_name) in [("flow_0.yaml", "flow0"), ("flow_1.yaml", "flow1"), ("flow_2.yaml", "flow2")] {
        write_flow(
            &ws,
            file,
            &format!(
                "appId: com.example.app\n\
                 name: {flow_name}\n\
                 tags:\n  - smoke\n\
                 properties:\n  owner: qa\n\
                 ---\n\
                 - launchApp\n\
                 - inputText: \"${{MAESTRO_SHARD_INDEX}}:${{MAESTRO_SHARD_ID}}:${{MAESTRO_DEVICE_UDID}}\"\n\
                 - takeScreenshot: \"shot_{flow_name}\"\n"
            ),
        );
    }

    let plan = workspace::plan(std::slice::from_ref(&ws), &[], &[], None).expect("workspace plan");
    let req = shard::resolve_shard_request(Some(2), None, None, Some("emulator-5554,emulator-5556"), 0, 0, 3)
        .expect("shard request");
    assert_eq!(req.effective_shards, 2);

    let (_, shard_dirs) =
        shard::resolve_output_dirs_with_stamp(Some(&out_root), None, None, false, req.effective_shards, "stamp");
    let shard_plans = shard::partition_plan(plan, ShardMode::Split(2), 2);
    assert_eq!(shard_plans[0].flows.len(), 2); // flow0, flow2
    assert_eq!(shard_plans[1].flows.len(), 1); // flow1

    let devices = ["emulator-5554", "emulator-5556"];
    let (suites, recorded_texts): (Vec<SuiteResult>, Vec<String>) = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for ((sp, out_dir), dev_id) in shard_plans.iter().zip(shard_dirs.iter()).zip(devices.iter()) {
            handles.push(scope.spawn(move || {
                let mut drv = FakeDriver::new();
                drv.add_installed_app("com.example.app");
                let base_opts = OrchestraOpts {
                    lookup_timeout_ms: 0,
                    optional_lookup_timeout_ms: 0,
                    test_output_dir: Some(out_dir.clone()),
                    ..Default::default()
                };
                let res = execute_shard_on_fake_driver(&mut drv, sp, &base_opts, Some(dev_id));
                (res, drv.current_text)
            }));
        }
        handles.into_iter().map(|h| h.join().expect("shard thread join")).unzip()
    });

    // Shard 0 ran flow0 and flow2 -> inputText twice with "0:1:emulator-5554"
    assert_eq!(recorded_texts[0], "0:1:emulator-55540:1:emulator-5554");
    // Shard 1 ran flow1 -> inputText once with "1:2:emulator-5556"
    assert_eq!(recorded_texts[1], "1:2:emulator-5556");

    // Verify per-shard screenshot isolation under shard-1/ and shard-2/
    assert!(out_root.join("shard-1/screenshots/shot_flow0.png").is_file());
    assert!(out_root.join("shard-1/screenshots/shot_flow2.png").is_file());
    assert!(!out_root.join("shard-1/screenshots/shot_flow1.png").exists());
    assert!(out_root.join("shard-2/screenshots/shot_flow1.png").is_file());

    let summary = TestExecutionSummary {
        passed: suites.iter().all(|s| s.passed),
        suite_name: Some("Split Suite".into()),
        input_mode: "auto".into(),
        total_ms: 120.0,
        utc_offset_secs: 0,
        suites,
    };
    assert!(summary.passed);
    assert_eq!(summary.passed_count(), 3);
    assert_eq!(summary.failed_count(), 0);

    // JUnit XML report
    let junit_xml = report::junit::render_junit(&summary).expect("render junit");
    assert!(junit_xml.contains("<testsuite name=\"Split Suite\" device=\"emulator-5554\" tests=\"2\" failures=\"0\""));
    assert!(junit_xml.contains("<testsuite name=\"Split Suite\" device=\"emulator-5556\" tests=\"1\" failures=\"0\""));
    assert!(junit_xml.contains("<property name=\"owner\" value=\"qa\"/>"));
    assert!(junit_xml.contains("<property name=\"tags\" value=\"smoke\"/>"));

    // HTML-Detailed report
    let html_doc = report::html::render_html(&summary, true);
    assert!(html_doc.contains("Split Suite"));
    assert!(html_doc.contains("Shard 1"));
    assert!(html_doc.contains("Shard 2"));
    assert!(html_doc.contains("<table class=\"steps\">"));

    // Multi-shard JSON report
    let json_doc: Value = serde_json::from_str(&report::render_json_report(&summary).unwrap()).unwrap();
    assert_eq!(json_doc["summary"]["shardCount"], 2);
    assert_eq!(json_doc["summary"]["passedCount"], 3);
    assert_eq!(json_doc["shards"].as_array().unwrap().len(), 2);
    assert_eq!(json_doc["flows"].as_array().unwrap().len(), 3);
    assert_eq!(json_doc["flows"][0]["shardIndex"], 0);
    assert_eq!(json_doc["flows"][0]["shardId"], 1);
    assert_eq!(json_doc["flows"][2]["shardIndex"], 1);
    assert_eq!(json_doc["flows"][2]["shardId"], 2);
    assert_eq!(json_doc["flows"][2]["device"], "emulator-5556");

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn multi_shard_all_runs_all_flows_on_every_shard_and_prefixes_junit_failure() {
    let ws = temp_test_dir("shard-all");
    write_flow(
        &ws,
        "check_btn.yaml",
        "appId: com.example.app\n\
         name: checkBtn\n\
         ---\n\
         - launchApp\n\
         - assertVisible: \"Ready\"\n",
    );

    let plan = workspace::plan(std::slice::from_ref(&ws), &[], &[], None).expect("workspace plan");
    let shard_plans = shard::partition_plan(plan, ShardMode::All(2), 2);
    assert_eq!(shard_plans.len(), 2);

    let suites: Vec<SuiteResult> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (idx, sp) in shard_plans.iter().enumerate() {
            let dev_id = if idx == 0 { "emulator-5554" } else { "emulator-5556" };
            handles.push(scope.spawn(move || {
                let mut drv = FakeDriver::new();
                drv.add_installed_app("com.example.app");
                if idx == 0 {
                    drv.add_element(|el| {
                        el.text = Some("Ready".into());
                        el.bounds = Some(Bounds::new(0, 0, 200, 80));
                    });
                }
                let base_opts =
                    OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() };
                execute_shard_on_fake_driver(&mut drv, sp, &base_opts, Some(dev_id))
            }));
        }
        handles.into_iter().map(|h| h.join().expect("shard join")).collect()
    });

    assert!(suites[0].passed);
    assert!(!suites[1].passed);

    let summary = TestExecutionSummary {
        passed: false,
        suite_name: Some("Shard All Suite".into()),
        input_mode: "auto".into(),
        total_ms: 50.0,
        utc_offset_secs: 0,
        suites,
    };

    let junit_xml = report::junit::render_junit(&summary).expect("render junit");
    assert!(junit_xml.contains("<failure>[shard 2] Assertion is false:") && junit_xml.contains("Ready"), "{junit_xml}");

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn single_shard_execution_order_aborts_sequence_on_failure_and_runs_remaining_flows() {
    let ws = temp_test_dir("exec-order");
    fs::write(
        ws.join("config.yaml"),
        "executionOrder:\n\
         \x20 continueOnFailure: false\n\
         \x20 flowsOrder:\n\
         \x20   - seq1\n\
         \x20   - seq2\n\
         \x20   - seq3\n",
    )
    .unwrap();

    write_flow(&ws, "seq1.yaml", "appId: com.example.app\nname: seq1\n---\n- launchApp\n");
    write_flow(&ws, "seq2.yaml", "appId: com.example.app\nname: seq2\n---\n- assertVisible: \"MissingElement\"\n");
    write_flow(&ws, "seq3.yaml", "appId: com.example.app\nname: seq3\n---\n- launchApp\n");
    write_flow(&ws, "after.yaml", "appId: com.example.app\nname: after\n---\n- launchApp\n");

    let plan = workspace::plan(std::slice::from_ref(&ws), &[], &[], None).expect("plan");
    assert_eq!(plan.sequence.len(), 3);
    assert_eq!(plan.flows.len(), 1);
    assert!(!plan.continue_on_failure);

    let shard_plans = shard::partition_plan(plan, ShardMode::None, 1);
    let mut drv = FakeDriver::new();
    drv.add_installed_app("com.example.app");
    let base_opts = OrchestraOpts { lookup_timeout_ms: 0, optional_lookup_timeout_ms: 0, ..Default::default() };
    let suite = execute_shard_on_fake_driver(&mut drv, &shard_plans[0], &base_opts, Some("emulator-5554"));

    assert!(!suite.passed);
    assert_eq!(suite.skipped_count, 1); // seq3 was aborted
    let executed_names: Vec<&str> = suite.flows.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(executed_names, vec!["seq1", "seq2", "after"]);

    let summary = TestExecutionSummary {
        passed: suite.passed,
        suite_name: None,
        input_mode: "auto".into(),
        total_ms: 25.0,
        utc_offset_secs: 0,
        suites: vec![suite],
    };

    // JUnit XML must contain only executed flows (seq1, seq2, after) and omit aborted seq3.
    let junit_xml = report::junit::render_junit(&summary).unwrap();
    assert!(junit_xml.contains("tests=\"3\" failures=\"1\""));
    assert!(!junit_xml.contains("name=\"seq3\""));

    // Single-shard JSON report keeps legacy top-level shape { platform, inputMode, setupMs, flows }.
    let json_doc: Value = serde_json::from_str(&report::render_json_report(&summary).unwrap()).unwrap();
    assert!(json_doc.get("shards").is_none());
    assert_eq!(json_doc["flows"].as_array().unwrap().len(), 3);

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn cli_binary_rejects_invalid_flag_combinations_with_exit_code_2() {
    let ws = temp_test_dir("cli-smoke");
    let flow_path = write_flow(&ws, "flow.yaml", "appId: com.example.app\n---\n- launchApp\n");
    let _ = write_flow(&ws, "flow2.yaml", "appId: com.example.app\n---\n- launchApp\n");
    let bin = env!("CARGO_BIN_EXE_tapwing");

    // 1. --shard-split and --shard-all mutually exclusive
    let out = Command::new(bin)
        .args(["test", ws.to_str().unwrap(), "--shard-split", "2", "--shard-all", "2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Options --shard-split and --shard-all are mutually exclusive."), "{stderr}");

    // 2. Multiple --device values without shard flag
    let out =
        Command::new(bin).args(["test", flow_path.to_str().unwrap(), "--device", "emu-1,emu-2"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Multiple --device values require --shard-split or --shard-all."), "{stderr}");

    // 3. Fixed --port with multiple shards
    let out = Command::new(bin)
        .args(["test", ws.to_str().unwrap(), "--shard-split", "2", "--port", "7781"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--port cannot be fixed when running multiple shards"), "{stderr}");

    // 4. Non-existent --config file
    let missing_cfg = ws.join("missing_config.yaml");
    let out = Command::new(bin)
        .args(["test", flow_path.to_str().unwrap(), "--config", missing_cfg.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("The config file") && stderr.contains("does not exist."), "{stderr}");

    let _ = fs::remove_dir_all(&ws);
}
