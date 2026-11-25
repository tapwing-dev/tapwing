//! F7 app health: step attribution, `--app-errors report|fail`, ignore rules,
//! trailing issues, crash diagnosis, and report summaries, over `FakeDriver`.

use std::path::Path;

use serde_json::{Value, json};
use tapwing::app_health::{AppErrorsPolicy, AppHealthOpts, IssueBatch, crash_issue};
use tapwing::backend::TransportError;
use tapwing::fake_driver::FakeDriver;
use tapwing::flow::Loader;
use tapwing::orchestra::{FlowResult, Orchestra, OrchestraOpts};

const FLOW: &str = "\
appId: dev.tapwing.sample_app
---
- evalScript: ${1}
- evalScript: ${2}
- evalScript: ${3}
";

fn issue(id: u64, kind: &str, message: &str) -> Value {
    json!({ "id": id, "seq": id, "kind": kind, "message": message, "count": 1, "request": id })
}

fn batch(current: Vec<Value>, late: Vec<Value>) -> IssueBatch {
    IssueBatch { current, late, dropped: 0 }
}

fn run(driver: &mut FakeDriver, source: &str, policy: AppErrorsPolicy, ignore: &[&str]) -> FlowResult {
    let flow = Loader::new().parse(Path::new("f7.yaml"), source).expect("flow parses");
    let ignore: Vec<String> = ignore.iter().map(|s| s.to_string()).collect();
    let opts = OrchestraOpts {
        lookup_timeout_ms: 0,
        optional_lookup_timeout_ms: 0,
        app_health: AppHealthOpts::new(policy, &ignore, false).unwrap(),
        ..Default::default()
    };
    Orchestra::new(driver, opts).run_parsed_flow(&flow)
}

fn kinds(step: &Value) -> Vec<String> {
    step["appIssues"].as_array().into_iter().flatten().map(|i| i["kind"].as_str().unwrap().to_string()).collect()
}

#[test]
fn report_attaches_issues_to_the_step_that_caused_them_and_keeps_pass_fail() {
    let mut driver = FakeDriver::new();
    driver.app_issue_batches = [
        batch(vec![], vec![]),
        batch(vec![issue(1, "layoutOverflow", "A RenderFlex overflowed by 12 pixels on the right.")], vec![]),
        // Fired after step 2's response: delivered during step 3, belongs to step 2.
        batch(vec![], vec![issue(2, "uncaughtError", "StateError: Bad state: late")]),
    ]
    .into();
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Report, &[]);
    assert!(res.success, "report never changes pass/fail: {:?}", res.error);
    assert!(res.steps[0].get("appIssues").is_none());
    assert_eq!(kinds(&res.steps[1]), ["layoutOverflow", "uncaughtError"]);
    assert!(res.steps[2].get("appIssues").is_none());

    let report = res.to_report_json("f7", Path::new("f7.yaml"), "v2");
    assert_eq!(
        report["appIssueSummary"],
        json!({ "total": 2, "ignored": 0, "byKind": { "layoutOverflow": 1, "uncaughtError": 1 } })
    );
}

#[test]
fn repeats_replace_the_record_by_id() {
    let mut driver = FakeDriver::new();
    let mut repeat = issue(1, "imageLoadError", "HTTP 404");
    repeat["count"] = json!(4);
    driver.app_issue_batches =
        [batch(vec![issue(1, "imageLoadError", "HTTP 404")], vec![]), batch(vec![], vec![repeat])].into();
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Report, &[]);
    let issues = res.steps[0]["appIssues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["count"], 4);
}

#[test]
fn fail_policy_fails_at_the_step_and_stops_the_flow() {
    let mut driver = FakeDriver::new();
    driver.app_issue_batches =
        [batch(vec![], vec![]), batch(vec![issue(1, "buildError", "boom in build")], vec![])].into();
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Fail, &[]);
    assert!(!res.success);
    assert_eq!(res.steps.len(), 2, "third command never ran");
    assert_eq!(res.steps[1]["status"], "failed");
    assert_eq!(
        res.steps[1]["error"],
        "App issue during `evalScript ${2}`: buildError: boom in build (--app-errors fail)"
    );
    assert_eq!(kinds(&res.steps[1]), ["buildError"]);
}

#[test]
fn fail_policy_blames_the_previous_step_for_late_issues() {
    let mut driver = FakeDriver::new();
    driver.app_issue_batches =
        [batch(vec![], vec![]), batch(vec![], vec![issue(1, "uncaughtError", "StateError: async")])].into();
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Fail, &[]);
    assert!(!res.success);
    assert_eq!(kinds(&res.steps[0]), ["uncaughtError"], "attached to the step that caused it");
    assert!(res.steps[1]["error"].as_str().unwrap().contains("during `evalScript ${1}`"));
}

#[test]
fn ignored_issues_are_reported_but_never_fail() {
    let mut driver = FakeDriver::new();
    driver.app_issue_batches =
        [batch(vec![issue(1, "layoutOverflow", "A RenderFlex overflowed by 3.0 pixels on the bottom.")], vec![])]
            .into();
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Fail, &["overflowed by [0-9.]+ pixels on the bottom"]);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(res.steps[0]["appIssues"][0]["ignored"], true);
    let report = res.to_report_json("f7", Path::new("f7.yaml"), "v2");
    assert_eq!(report["appIssueSummary"]["ignored"], 1);
}

#[test]
fn optional_does_not_downgrade_an_app_issue_failure() {
    let mut driver = FakeDriver::new();
    driver.app_issue_batches = [batch(vec![issue(1, "uncaughtError", "x")], vec![])].into();
    let source = "appId: a\n---\n- evalScript:\n    script: ${1}\n    optional: true\n";
    let res = run(&mut driver, source, AppErrorsPolicy::Fail, &[]);
    assert!(!res.success);
    assert_eq!(res.steps[0]["status"], "failed");
}

#[test]
fn trailing_issues_go_to_the_last_step_and_fail_under_fail() {
    let mut driver = FakeDriver::new();
    driver.trailing_app_issues = batch(vec![issue(9, "uncaughtError", "after the last step")], vec![]);
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Fail, &[]);
    assert!(!res.success);
    assert_eq!(kinds(&res.steps[2]), ["uncaughtError"]);
    assert!(res.error.unwrap().to_string().contains("during `evalScript ${3}`"));

    let mut driver = FakeDriver::new();
    driver.trailing_app_issues = batch(vec![issue(9, "uncaughtError", "after the last step")], vec![]);
    assert!(run(&mut driver, FLOW, AppErrorsPolicy::Report, &[]).success);
}

#[test]
fn crash_on_transport_error_names_the_crash_and_stays_a_transport_error() {
    let mut driver = FakeDriver::new();
    driver.command_error = Some(Box::new(|| TransportError("app closed the connection during `back`".into()).into()));
    driver.crash = Some(crash_issue(
        "App crashed",
        Some("AndroidRuntime: FATAL EXCEPTION: main\nAndroidRuntime: java.lang.IllegalStateException: boom"),
    ));
    let source = "appId: a\n---\n- evalScript: ${1}\n- back\n";
    let res = run(&mut driver, source, AppErrorsPolicy::Report, &[]);
    assert!(!res.success);
    let err = res.error.unwrap();
    assert!(err.is::<TransportError>(), "still a transport error for retry/runFlow semantics");
    assert_eq!(err.to_string(), "App crashed during `back`: AndroidRuntime: java.lang.IllegalStateException: boom");
    let step = &res.steps[1];
    assert_eq!(step["status"], "failed");
    assert_eq!(kinds(step), ["appCrash"]);
}

#[test]
fn transport_error_without_crash_is_unchanged() {
    let mut driver = FakeDriver::new();
    driver.command_error = Some(Box::new(|| TransportError("connection to app lost".into()).into()));
    let res = run(&mut driver, "appId: a\n---\n- back\n", AppErrorsPolicy::Report, &[]);
    assert_eq!(res.error.unwrap().to_string(), "connection to app lost");
    assert!(res.steps.is_empty());
}

#[test]
fn clean_flows_have_no_summary() {
    let mut driver = FakeDriver::new();
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Report, &[]);
    let report = res.to_report_json("f7", Path::new("f7.yaml"), "v2");
    assert!(report.get("appIssueSummary").is_none());
    assert!(res.steps.iter().all(|s| s.get("appIssues").is_none()));
}

#[test]
fn non_transport_errors_never_trigger_crash_diagnosis() {
    // Normal command/selector failures (ELEMENT_NOT_FOUND or plain non-transport errors)
    // must not invoke crash diagnosis, even if a lingering crash is available.
    let mut driver = FakeDriver::new();
    driver.crash = Some(crash_issue("App crashed", Some("AndroidRuntime: java.lang.IllegalStateException: old")));
    let res = run(&mut driver, "appId: a\n---\n- tapOn: MissingElement\n", AppErrorsPolicy::Report, &[]);
    assert!(!res.success);
    let err_str = res.error.as_ref().unwrap().to_string();
    assert!(!err_str.contains("App crashed"), "unexpected crash attribution: {err_str}");
    assert!(res.steps[0].get("appIssues").is_none());

    let mut driver = FakeDriver::new();
    driver.command_error = Some(Box::new(|| anyhow::anyhow!("plain runtime error")));
    driver.crash = Some(crash_issue("App crashed", Some("AndroidRuntime: java.lang.IllegalStateException: old")));
    let res = run(&mut driver, "appId: a\n---\n- back\n", AppErrorsPolicy::Report, &[]);
    assert!(!res.success);
    assert_eq!(res.steps[0]["error"], "plain runtime error");
    assert!(res.steps[0].get("appIssues").is_none());
}

#[test]
fn crash_on_rpc_silence_error_is_diagnosed() {
    let mut driver = FakeDriver::new();
    driver.command_error = Some(Box::new(|| {
        tapwing::driver::RpcSilenceError { method: "back".into(), silence: std::time::Duration::from_secs(10) }.into()
    }));
    driver.crash = Some(crash_issue(
        "App crashed",
        Some("AndroidRuntime: FATAL EXCEPTION: main\nAndroidRuntime: java.lang.IllegalStateException: boom"),
    ));
    let res = run(&mut driver, "appId: a\n---\n- back\n", AppErrorsPolicy::Report, &[]);
    assert!(!res.success);
    let err = res.error.unwrap();
    assert!(tapwing::backend::is_transport_error(&err));
    assert_eq!(
        res.steps[0]["error"],
        "App crashed during `back`: AndroidRuntime: java.lang.IllegalStateException: boom"
    );
    assert_eq!(kinds(&res.steps[0]), ["appCrash"]);
}

#[test]
fn intentional_app_stop_suppresses_crash_diagnosis_until_relaunch() {
    for stop_cmd in
        ["stopApp: dev.tapwing.sample_app", "killApp: dev.tapwing.sample_app", "clearState: dev.tapwing.sample_app"]
    {
        let mut driver = FakeDriver::new();
        driver.add_installed_app("dev.tapwing.sample_app");
        driver.command_error = Some(Box::new(|| TransportError("app is not connected".into()).into()));
        driver.crash = Some(crash_issue("App crashed", Some("AndroidRuntime: java.lang.IllegalStateException: old")));

        let stopped_flow = format!("appId: dev.tapwing.sample_app\n---\n- launchApp\n- {stop_cmd}\n- back\n");
        let res = run(&mut driver, &stopped_flow, AppErrorsPolicy::Report, &[]);
        assert!(!res.success);
        let err = res.error.unwrap();
        assert!(err.is::<TransportError>());
        assert_eq!(err.to_string(), "app is not connected", "stop cmd `{stop_cmd}` should skip crash diagnosis");

        // Relaunching clears the intentional-stop guard so a subsequent crash IS diagnosed.
        let relaunched_flow =
            format!("appId: dev.tapwing.sample_app\n---\n- launchApp\n- {stop_cmd}\n- launchApp\n- back\n");
        let res = run(&mut driver, &relaunched_flow, AppErrorsPolicy::Report, &[]);
        assert!(!res.success);
        let err = res.error.unwrap();
        assert!(err.is::<TransportError>());
        assert!(err.to_string().contains("App crashed during `back`"), "{err}");
    }
}

#[test]
fn transport_errors_bypass_retry_and_optional_downgrade() {
    for make_err in [
        || TransportError("app is not connected".into()).into(),
        || TransportError("invalid JSON from driver during `back`: expected value".into()).into(),
    ] as [fn() -> anyhow::Error; 2]
    {
        let mut driver = FakeDriver::new();
        driver.command_error = Some(Box::new(make_err));
        let source = "\
appId: dev.tapwing.sample_app
---
- retry:
    maxRetries: 2
    commands:
      - back
";
        let res = run(&mut driver, source, AppErrorsPolicy::Report, &[]);
        assert!(!res.success);
        assert!(res.error.as_ref().unwrap().is::<TransportError>());
        // Must escape immediately on the first attempt without retrying or recording failure diagnostics.
        assert!(res.steps.is_empty(), "expected no retried/failed steps recorded: {:?}", res.steps);

        let opt_source = "\
appId: dev.tapwing.sample_app
---
- runFlow:
    optional: true
    commands:
      - back
";
        let res = run(&mut driver, opt_source, AppErrorsPolicy::Report, &[]);
        assert!(!res.success);
        assert!(res.error.as_ref().unwrap().is::<TransportError>());
    }
}

#[test]
fn dropped_issues_are_surfaced_in_summary() {
    let mut driver = FakeDriver::new();
    driver.app_issue_batches = [
        IssueBatch { current: vec![issue(1, "uncaughtError", "flood #1")], late: vec![], dropped: 1200 },
        IssueBatch { current: vec![], late: vec![], dropped: 300 },
    ]
    .into();
    let res = run(&mut driver, FLOW, AppErrorsPolicy::Report, &[]);
    assert!(res.success, "{:?}", res.error);
    assert_eq!(res.dropped_app_issues, 1500);
    let report = res.to_report_json("f7", Path::new("f7.yaml"), "v2");
    assert_eq!(
        report["appIssueSummary"],
        json!({ "total": 1, "ignored": 0, "dropped": 1500, "byKind": { "uncaughtError": 1 } })
    );
}
