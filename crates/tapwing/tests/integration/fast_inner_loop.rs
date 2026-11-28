//! Integration, policy, report parity, and CLI E2E tests for Tapwing's
//! fast inner-loop features (`callHook`, `--attach`, `--start-from`).

use std::cell::Cell;
use std::process::{Command, Output};
use std::rc::Rc;

use serde_json::{Value, json};
use tapwing::driver::DriverError;
use tapwing::fake_driver::{FakeDriver, FakeEvent};
use tapwing::flow;
use tapwing::flow::ir::{MaestroCommand, Platform as IrPlatform};
use tapwing::graybox::GrayboxInfo;
use tapwing::orchestra::{FlowResult, Orchestra, OrchestraOpts};
use tapwing::report::{
    FlowRunRecord, FlowStatus, StepRecord, SuiteResult, TestExecutionSummary, html, junit, render_json_report,
};
use tapwing::trace::{self, TraceOpts, TraceScreenshots};
use tapwing::workspace;

use crate::common::TempDir;

fn parse_inline(dir: &TempDir, rel: &str, yaml: &str) -> flow::ParsedFlow {
    let p = dir.write(rel, yaml);
    flow::parse_file(&p).unwrap_or_else(|e| panic!("failed to parse {rel}: {}", e.render()))
}

fn run_with_opts(
    flow: &flow::ParsedFlow,
    driver: &mut FakeDriver,
    configure: impl FnOnce(&mut OrchestraOpts),
) -> FlowResult {
    let mut opts = OrchestraOpts {
        lookup_timeout_ms: 0,
        optional_lookup_timeout_ms: 0,
        flow_path: flow.path.clone(),
        ..Default::default()
    };
    configure(&mut opts);
    let mut orch = Orchestra::new(driver, opts);
    orch.run_parsed_flow(flow)
}

fn to_flow_record(name: &str, flow: &flow::ParsedFlow, result: &FlowResult) -> FlowRunRecord {
    let raw_json = result.to_report_json(name, &flow.path, "v2");
    let steps = result
        .steps
        .iter()
        .map(|s| StepRecord {
            index: s["index"].as_u64().unwrap_or(0) as usize,
            command: s["command"].as_str().unwrap_or("").to_string(),
            description: s["description"].as_str().unwrap_or("").to_string(),
            status: s["status"].as_str().unwrap_or("passed").to_string(),
            duration_ms: s["ms"].as_f64().unwrap_or(0.0),
            warning: s.get("warning").and_then(Value::as_str).map(str::to_string),
            error: s.get("error").and_then(Value::as_str).map(str::to_string),
            app_issues: Vec::new(),
        })
        .collect();
    FlowRunRecord {
        name: name.to_string(),
        path: flow.path.clone(),
        file: Some(flow.path.display().to_string()),
        status: if result.success { FlowStatus::Success } else { FlowStatus::Error },
        duration_ms: result.total_ms,
        start_epoch_ms: 1_764_167_405_000,
        failure_message: result.error.as_ref().map(|e| format!("{e:#}")),
        properties: Vec::new(),
        tags: Vec::new(),
        lane: "flutter".into(),
        flutter_steps: result.steps.len() as u32,
        native_steps: 0,
        blackbox: false,
        blackbox_reason: None,
        steps,
        failure_diagnostics: None,
        raw_json,
        trace: result.trace.clone(),
        graybox: result.graybox.clone(),
    }
}

fn wrap_summary(record: FlowRunRecord) -> TestExecutionSummary {
    let passed = record.passed();
    TestExecutionSummary {
        passed,
        suite_name: Some("InnerLoopSuite".into()),
        input_mode: "auto".into(),
        total_ms: record.duration_ms,
        utc_offset_secs: 0,
        interrupted: None,
        suites: vec![SuiteResult {
            shard_index: None,
            device_id: Some("sim-1".into()),
            device_description: Some("iPhone 18 Pro".into()),
            platform: "ios".into(),
            passed,
            setup_ms: 10.0,
            duration_ms: record.duration_ms,
            start_epoch_ms: 1_764_167_405_000,
            skipped_count: 0,
            error: None,
            flows: vec![record],
        }],
    }
}

fn tapwing_bin(args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tapwing"));
    cmd.args(args).env_remove("TAPWING_DRIVER_APK").env_remove("TAPWING_IOS_DRIVER_DIR").env("NO_COLOR", "1");
    cmd.output().expect("execute tapwing binary")
}

// ---------------------------------------------------------------- YAML + config.yaml parsing

#[test]
fn call_hook_yaml_parsing_supports_shorthand_full_map_deferred_expr_and_validation() {
    let dir = TempDir::new("hook_yaml");

    let flow = parse_inline(
        &dir,
        "valid.yaml",
        r#"
appId: com.example.app
---
- callHook: reset_app
- callHook:
    name: seed_user
    args:
      email: "user@example.com"
      nested:
        role: admin
      tags:
        - "${TAG}"
        - beta
      promo: null
    timeout: "${HOOK_TIMEOUT}"
    settle: false
    waitToSettleTimeoutMs: "2500"
    outputVariable: "${OUT_VAR}"
    label: Seed user state
    optional: true
"#,
    );

    let hooks: Vec<&tapwing::flow::ir::CallHook> = flow
        .commands
        .iter()
        .filter_map(|c| match c {
            MaestroCommand::CallHook(h) => Some(h),
            _ => None,
        })
        .collect();
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[0].name, "reset_app");
    assert!(!hooks[0].optional);
    assert_eq!(hooks[1].name, "seed_user");
    assert_eq!(hooks[1].settle, Some(false));
    assert_eq!(hooks[1].wait_to_settle_timeout_ms.as_deref(), Some("2500"));
    assert_eq!(hooks[1].output_variable.as_deref(), Some("${OUT_VAR}"));
    assert_eq!(hooks[1].label.as_deref(), Some("Seed user state"));
    assert!(hooks[1].optional);

    for (rel, bad_yaml) in [
        ("empty_name.yaml", "appId: com.example.app\n---\n- callHook: \"   \"\n"),
        ("bad_timeout.yaml", "appId: com.example.app\n---\n- callHook:\n    name: x\n    timeout: 0\n"),
        ("bad_outvar.yaml", "appId: com.example.app\n---\n- callHook:\n    name: x\n    outputVariable: 123bad\n"),
        ("unknown_key.yaml", "appId: com.example.app\n---\n- callHook:\n    name: x\n    bogusKey: true\n"),
    ] {
        let p = dir.write(rel, bad_yaml);
        assert!(flow::parse_file(&p).is_err(), "{rel} should fail validation");
    }
}

#[test]
fn config_yaml_parses_tapwing_allow_hooks_and_rejects_non_bool() {
    let dir = TempDir::new("hook_cfg");
    let cfg_default = dir.write("default.yaml", "flows:\n  - smoke/*\n");
    assert_eq!(workspace::config::read(&cfg_default).unwrap().tapwing.allow_hooks, None);

    let cfg_true = dir.write("allow.yaml", "tapwing:\n  allowHooks: true\n");
    assert_eq!(workspace::config::read(&cfg_true).unwrap().tapwing.allow_hooks, Some(true));

    let cfg_false = dir.write("deny.yaml", "tapwing:\n  allowHooks: false\n");
    assert_eq!(workspace::config::read(&cfg_false).unwrap().tapwing.allow_hooks, Some(false));

    let cfg_bad = dir.write("bad.yaml", "tapwing:\n  allowHooks: \"yes\"\n");
    assert!(workspace::config::read(&cfg_bad).is_err());
}

// ---------------------------------------------------------------- Orchestra callHook semantics

#[test]
fn call_hook_enforces_policy_even_with_optional_interpolates_args_and_binds_output() {
    let dir = TempDir::new("orch_hooks");
    let flow = parse_inline(
        &dir,
        "hooks.yaml",
        r#"
appId: com.example.app
env:
  USER_NAME: "ada"
---
- callHook:
    name: seed_profile
    optional: true
    args:
      profile:
        handle: "${USER_NAME}"
        roles:
          - "${USER_NAME}_admin"
          - reader
      count: 3
      note: null
    outputVariable: seeded
- assertTrue: ${output.seeded.handle == 'ada' && output.seeded.roles[0] == 'ada_admin' && output.seeded.count == 3}
"#,
    );

    // Policy blocked: must fail even though `optional: true` is set!
    let mut drv_blocked = FakeDriver::new().with_platform(IrPlatform::Ios);
    drv_blocked.register_hook("seed_profile", |_| Ok(json!({})));
    let blocked = run_with_opts(&flow, &mut drv_blocked, |o| o.allow_hooks = false);
    assert!(!blocked.success);
    assert!(blocked.error.as_ref().unwrap().to_string().contains("disabled"), "unexpected error: {:?}", blocked.error);
    assert!(
        !drv_blocked.events.iter().any(|e| matches!(e, FakeEvent::CallHook { .. })),
        "blocked callHook must not dispatch to driver"
    );

    // Policy allowed: args are recursively interpolated and result binds into `output.seeded`.
    let mut drv_ok = FakeDriver::new().with_platform(IrPlatform::Ios);
    drv_ok.register_hook("seed_profile", |args| {
        let a = args.expect("args present");
        assert_eq!(a["profile"]["handle"], "ada");
        assert_eq!(a["profile"]["roles"][0], "ada_admin");
        assert_eq!(a["count"], 3);
        assert!(a["note"].is_null());
        Ok(json!({
            "handle": a["profile"]["handle"],
            "roles": a["profile"]["roles"],
            "count": a["count"],
        }))
    });
    let ok = run_with_opts(&flow, &mut drv_ok, |o| o.allow_hooks = true);
    assert!(ok.success, "{:?}", ok.error);
    assert_eq!(ok.graybox.hooks_used, vec!["seed_profile"]);
    assert_eq!(ok.graybox.hook_calls, 1);
    assert!(ok.graybox.is_active());
}

#[test]
fn call_hook_errors_work_with_optional_and_retry() {
    let dir = TempDir::new("hook_errors");
    let flow = parse_inline(
        &dir,
        "retry_hook.yaml",
        r#"
appId: com.example.app
---
- callHook:
    name: missing_optional
    optional: true
- retry:
    maxRetries: 2
    commands:
      - callHook: flaky_hook
"#,
    );

    let attempts = Rc::new(Cell::new(0usize));
    let attempts_clone = Rc::clone(&attempts);
    let mut drv = FakeDriver::new();
    drv.register_hook("flaky_hook", move |_| {
        let n = attempts_clone.get() + 1;
        attempts_clone.set(n);
        if n == 1 {
            return Err(DriverError {
                code: "HOOK_FAILED".into(),
                message: "transient failure".into(),
                details: json!({}),
            }
            .into());
        }
        Ok(json!({ "ok": true }))
    });

    let res = run_with_opts(&flow, &mut drv, |o| o.allow_hooks = true);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(attempts.get(), 2);
    assert_eq!(res.steps[0]["status"], "warned");
    assert_eq!(res.graybox.hooks_used, vec!["missing_optional", "flaky_hook"]);
    assert_eq!(res.graybox.hook_calls, 3);
}

// ---------------------------------------------------------------- --attach & --start-from

#[test]
fn attach_replaces_leading_launch_app_and_warns_on_clear_state() {
    let dir = TempDir::new("attach_flow");
    let flow = parse_inline(
        &dir,
        "attach.yaml",
        r#"
appId: com.example.app
---
- launchApp:
    clearState: true
- assertVisible: "Home"
"#,
    );

    let mut drv = FakeDriver::new();
    drv.add_element(|e| e.text = Some("Home".into()));

    let res = run_with_opts(&flow, &mut drv, |o| o.attach = true);
    assert!(res.success, "{:?}", res.error);
    assert!(drv.events.contains(&FakeEvent::WarmAttach("com.example.app".into())));
    assert!(!drv.events.iter().any(|e| matches!(e, FakeEvent::LaunchApp { .. })));
    assert!(res.graybox.warm_attach);
    assert!(
        res.steps[0]["warning"].as_str().unwrap_or("").contains("Skipped clearState"),
        "expected clearState skip warning on step 0: {:?}",
        res.steps[0]
    );
}

#[test]
fn start_from_by_index_and_label_skips_on_flow_start_keeps_env_and_step_numbers() {
    let dir = TempDir::new("start_from");
    let flow = parse_inline(
        &dir,
        "flow.yaml",
        r#"
appId: com.example.app
env:
  TARGET_BTN: "Save"
onFlowStart:
  - back
onFlowComplete:
  - hideKeyboard
---
- launchApp
- tapOn:
    text: "StepTwo"
    label: Go to step 2
- tapOn:
    text: "${TARGET_BTN}"
    label: Target button
- assertVisible: "Saved"
"#,
    );

    let mut drv = FakeDriver::new();
    drv.add_element(|e| e.text = Some("Save".into()));
    drv.add_element(|e| e.text = Some("Saved".into()));

    // Start by label at step 3 (`Target button`), without explicitly passing `--attach`:
    let res = run_with_opts(&flow, &mut drv, |o| o.start_from = Some("Target button".into()));
    assert!(res.success, "{:?}", res.error);
    // Warm attach occurred automatically before step 3; onFlowStart's `back` was skipped; onFlowComplete's `hideKeyboard` ran!
    assert!(drv.events.contains(&FakeEvent::WarmAttach("com.example.app".into())));
    assert!(!drv.events.contains(&FakeEvent::BackPress));
    assert!(drv.events.contains(&FakeEvent::HideKeyboard));
    assert!(!drv.events.iter().any(|e| matches!(e, FakeEvent::LaunchApp { .. })));
    // Preserved 0-based indices 2 and 3 (rendered as steps 3 and 4), plus onFlowComplete (`hideKeyboard`).
    assert_eq!(res.steps.len(), 3);
    assert_eq!(res.steps[0]["index"], 2);
    assert_eq!(res.steps[1]["index"], 3);
    assert_eq!(res.steps[2]["command"], "hideKeyboard");
    assert!(res.graybox.warm_attach);
    let partial = res.graybox.partial_run.as_ref().expect("partial_run metadata");
    assert_eq!(partial.resolved_step, 3);
    assert_eq!(partial.skipped_steps, 2);
    assert!(partial.skipped_on_flow_start);

    // Start by 1-based step index 1 (`launchApp`) WITHOUT `--attach` performs normal cold launch, not warm attach.
    let mut drv2 = FakeDriver::new();
    drv2.add_installed_app("com.example.app");
    drv2.add_element(|e| e.text = Some("StepTwo".into()));
    drv2.add_element(|e| e.text = Some("Save".into()));
    drv2.add_element(|e| e.text = Some("Saved".into()));
    let res2 = run_with_opts(&flow, &mut drv2, |o| o.start_from = Some("1".into()));
    assert!(res2.success, "{:?}", res2.error);
    assert!(!drv2.events.iter().any(|e| matches!(e, FakeEvent::WarmAttach(_))));
    assert!(drv2.events.iter().any(|e| matches!(e, FakeEvent::LaunchApp { .. })));
    assert!(drv2.events.contains(&FakeEvent::BackPress), "step 1 should run onFlowStart");
    assert!(!res2.graybox.is_active());
}

// ---------------------------------------------------------------- Reports & Trace Parity

#[test]
fn clean_run_emits_zero_graybox_metadata_while_graybox_run_flags_all_reports() {
    let dir = TempDir::new("reports");
    let clean_flow = parse_inline(&dir, "clean.yaml", "appId: com.example.app\n---\n- launchApp\n");
    let mut clean_drv = FakeDriver::new();
    clean_drv.add_installed_app("com.example.app");
    let clean_res = run_with_opts(&clean_flow, &mut clean_drv, |_| {});
    assert!(clean_res.success);
    assert_eq!(clean_res.graybox, GrayboxInfo::default());
    let clean_summary = wrap_summary(to_flow_record("clean", &clean_flow, &clean_res));
    let clean_json: Value = serde_json::from_str(&render_json_report(&clean_summary).unwrap()).unwrap();
    assert!(clean_json.get("graybox").is_none());
    assert!(clean_json["flows"][0].get("graybox").is_none());
    let clean_junit = junit::render_junit(&clean_summary).unwrap();
    assert!(!clean_junit.contains("tapwing.graybox"));
    assert!(!clean_junit.contains("Gray-box"));
    let clean_html = html::render_html_for_path(&clean_summary, true, None);
    assert!(!clean_html.contains("gray-box"));
    let clean_trace =
        trace::build_shard_trace(&clean_summary.suites[0], &TraceOpts { screenshots: TraceScreenshots::Failures });
    assert!(clean_trace["flows"][0].get("graybox").is_none());

    // Now gray-box run with hooks + warm attach + start-from:
    let gb_flow = parse_inline(
        &dir,
        "gb.yaml",
        r#"
appId: com.example.app
---
- launchApp
- callHook:
    name: seed_logged_in
    label: Seed state
- assertVisible: "Products"
"#,
    );
    let mut gb_drv = FakeDriver::new();
    gb_drv.register_hook("seed_logged_in", |_| Ok(json!({ "ok": true })));
    gb_drv.add_element(|e| e.text = Some("Products".into()));
    let gb_res = run_with_opts(&gb_flow, &mut gb_drv, |o| {
        o.allow_hooks = true;
        o.start_from = Some("Seed state".into());
        o.trace = Some(TraceOpts { screenshots: TraceScreenshots::Failures });
    });
    assert!(gb_res.success, "{:?}", gb_res.error);
    let gb_summary = wrap_summary(to_flow_record("gb", &gb_flow, &gb_res));

    let gb_json: Value = serde_json::from_str(&render_json_report(&gb_summary).unwrap()).unwrap();
    assert_eq!(gb_json["graybox"], true);
    assert_eq!(gb_json["flows"][0]["graybox"], true);
    assert_eq!(gb_json["flows"][0]["hooksUsed"], json!(["seed_logged_in"]));
    assert_eq!(gb_json["flows"][0]["warmAttach"], true);
    assert_eq!(gb_json["flows"][0]["partialRun"]["resolvedStep"], 2);

    let gb_junit = junit::render_junit(&gb_summary).unwrap();
    assert!(gb_junit.contains("tapwing.graybox"));
    assert!(gb_junit.contains("tapwing.hooks"));
    assert!(gb_junit.contains("tapwing.startFrom"));
    assert!(gb_junit.contains("tapwing.warmAttach"));
    assert!(gb_junit.contains("Gray-box:"));

    let gb_html = html::render_html_for_path(&gb_summary, true, None);
    assert!(gb_html.contains("contains gray-box runs"));
    assert!(gb_html.contains("gray-box (hook: seed_logged_in, partial: step 2, warm attach)"));
    assert!(gb_html.contains("<dt>Gray-box</dt>"));
    // Preserved 1-based step #2:
    assert!(gb_html.contains("<td class=\"n\">2</td>"));

    let gb_trace =
        trace::build_shard_trace(&gb_summary.suites[0], &TraceOpts { screenshots: TraceScreenshots::Failures });
    assert_eq!(gb_trace["flows"][0]["graybox"], true);
    assert_eq!(gb_trace["flows"][0]["hooksUsed"], json!(["seed_logged_in"]));
}

// ---------------------------------------------------------------- CLI E2E binary tests

#[test]
fn cli_check_and_test_validate_hooks_attach_and_start_from_before_device_selection() {
    let dir = TempDir::new("cli_e2e");
    let hook_flow = dir.write(
        "valid/one.yaml",
        "appId: com.example.app\n---\n- launchApp: { label: Start }\n- callHook: { name: seed, label: Seed }\n",
    );
    let second_flow = dir.write("valid/two.yaml", "appId: com.example.app\n---\n- launchApp\n");
    let bad_hook =
        dir.write("invalid/bad.yaml", "appId: com.example.app\n---\n- callHook: { name: seed, timeout: 0 }\n");

    let chk_ok = tapwing_bin(&["check", hook_flow.to_str().unwrap()]);
    assert!(chk_ok.status.success(), "{}", String::from_utf8_lossy(&chk_ok.stderr));

    let chk_bad = tapwing_bin(&["check", bad_hook.to_str().unwrap()]);
    assert!(!chk_bad.status.success());

    // `--start-from` fails before device selection on multi-flow directory:
    let multi = tapwing_bin(&["test", "--start-from", "2", dir.join("valid").to_str().unwrap()]);
    assert_eq!(multi.status.code(), Some(2));
    let multi_err = String::from_utf8_lossy(&multi.stderr);
    assert!(multi_err.contains("requires a single flow"), "stderr: {multi_err}");

    // `--start-from` fails before device selection on missing label:
    let missing = tapwing_bin(&["test", "--start-from", "no_such_label", second_flow.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(2));
    let missing_err = String::from_utf8_lossy(&missing.stderr);
    assert!(missing_err.contains("not found"), "stderr: {missing_err}");

    // `--allow-hooks` conflicts with `--no-hooks`:
    let conflict = tapwing_bin(&["test", "--allow-hooks", "--no-hooks", hook_flow.to_str().unwrap()]);
    assert!(!conflict.status.success());
    let conflict_err = String::from_utf8_lossy(&conflict.stderr);
    assert!(conflict_err.contains("--no-hooks"), "stderr: {conflict_err}");
}
