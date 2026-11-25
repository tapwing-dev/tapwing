//! F8 rich failure reports: "Did you mean" selector suggestions, compact UI
//! tree dumps, automatic failure screenshots & hierarchy files, retry/optional
//! suppression, and JSON / HTML / JUnit report rendering over `FakeDriver`.

use std::path::Path;

use serde_json::Value;
use tapwing::fake_driver::{Bounds, FakeDriver, FakeEvent};
use tapwing::flow::Loader;
use tapwing::orchestra::{FlowResult, Orchestra, OrchestraOpts};
use tapwing::report::{self, TestExecutionSummary};
use tapwing::run::suite::execute_shard_on_fake_driver;
use tapwing::shard::ShardPlan;

fn make_driver() -> FakeDriver {
    let mut driver = FakeDriver::new();
    driver.add_element(|g| {
        g.text = Some("Sign in with Google".into());
        g.id = Some("google_login_btn".into());
        g.bounds = Some(Bounds::new(40, 200, 1040, 320));
        g.clickable = true;
        g.color = [66, 133, 244, 255];
    });
    driver.add_element(|s| {
        s.text = Some("Submit".into());
        s.bounds = Some(Bounds::new(40, 360, 1040, 480));
        s.clickable = true;
        s.enabled = Some(false);
    });
    driver
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("tapwing_f8_{label}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_flow(driver: &mut FakeDriver, name: &str, source: &str, out_dir: Option<&Path>) -> FlowResult {
    let flow = Loader::new().parse(Path::new(name), source).expect("flow parses");
    let opts = OrchestraOpts {
        lookup_timeout_ms: 0,
        optional_lookup_timeout_ms: 0,
        flow_path: flow.path.clone(),
        test_output_dir: out_dir.map(Path::to_path_buf),
        ..Default::default()
    };
    Orchestra::new(driver, opts).run_parsed_flow(&flow)
}

#[test]
fn failed_selector_captures_suggestions_tree_and_screenshot_without_polluting_events() {
    let mut driver = make_driver();
    let out = temp_dir("selector");
    let source = "\
appId: dev.tapwing.sample_app
---
- tapOn: Sign in
";
    let res = run_flow(&mut driver, "login.yaml", source, Some(&out));
    assert!(!res.success);
    assert!(
        !driver.events.contains(&FakeEvent::TakeScreenshot),
        "automatic failure screenshot must not pollute user TakeScreenshot events"
    );

    let failure = &res.steps[0]["failure"];
    assert!(failure.is_object(), "expected failure diagnostics on step: {}", res.steps[0]);

    let suggestions = failure["suggestions"].as_array().expect("suggestions array");
    assert_eq!(suggestions[0]["field"], "text");
    assert_eq!(suggestions[0]["value"], "Sign in with Google");
    assert!(suggestions[0]["reason"].as_str().unwrap().contains("full string"));

    let hierarchy = failure["hierarchy"].as_str().expect("compact hierarchy");
    assert!(hierarchy.contains("Sign in with Google"), "{hierarchy}");
    assert!(hierarchy.contains("[id=\"google_login_btn\"]"), "{hierarchy}");
    assert!(hierarchy.contains("(clickable, disabled)"), "{hierarchy}");
    assert_eq!(failure["hierarchySource"], "fake");
    assert_eq!(failure["totalElements"], 2);

    let shot_path = Path::new(failure["screenshot"].as_str().expect("screenshot path"));
    assert!(shot_path.exists(), "screenshot written to {}", shot_path.display());
    assert!(std::fs::read(shot_path).unwrap().starts_with(b"\x89PNG"));

    let tree_path = Path::new(failure["hierarchyFile"].as_str().expect("hierarchyFile path"));
    assert!(tree_path.exists(), "hierarchy file written to {}", tree_path.display());
    assert!(std::fs::read_to_string(tree_path).unwrap().contains("Sign in with Google"));

    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn optional_misses_and_passing_flows_capture_no_failure_artifacts() {
    let mut driver = make_driver();
    let out = temp_dir("optional");
    let source = "\
appId: dev.tapwing.sample_app
---
- tapOn:
    text: Missing Button
    optional: true
- assertVisible: Submit
";
    let res = run_flow(&mut driver, "opt.yaml", source, Some(&out));
    assert!(res.success, "{:?}", res.error);
    assert!(res.steps.iter().all(|s| s.get("failure").is_none()));
    assert!(!out.join("failures").exists(), "no failures/ directory on passing flow");

    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn retry_suppresses_intermediate_captures_and_deduplicates_wrapper_steps() {
    let mut driver = make_driver();
    let out = temp_dir("retry");
    let source = "\
appId: dev.tapwing.sample_app
---
- retry:
    maxRetries: 2
    commands:
      - tapOn: Submt
";
    let res = run_flow(&mut driver, "retry_flow.yaml", source, Some(&out));
    assert!(!res.success);

    // 3 inner failed attempts + 1 outer failed `retry` step = 4 steps;
    // ONLY the final (3rd) inner attempt should have captured diagnostics.
    let with_diag: Vec<&Value> = res.steps.iter().filter(|s| s.get("failure").is_some()).collect();
    assert_eq!(with_diag.len(), 1, "expected exactly 1 failure capture across retries + wrapper: {:?}", res.steps);
    assert_eq!(with_diag[0]["command"], "tapOn");
    assert_eq!(with_diag[0]["failure"]["suggestions"][0]["value"], "Submit");

    let files: Vec<_> = std::fs::read_dir(out.join("failures")).unwrap().flatten().collect();
    assert_eq!(files.len(), 2, "only 1 PNG + 1 hierarchy.txt written");

    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn assert_not_visible_failure_captures_tree_without_did_you_mean() {
    let mut driver = make_driver();
    let source = "\
appId: dev.tapwing.sample_app
---
- assertNotVisible: Submit
";
    let res = run_flow(&mut driver, "not_vis.yaml", source, None);
    assert!(!res.success);
    let failure = &res.steps[0]["failure"];
    assert!(failure.get("suggestions").is_none(), "no Did you mean for assertNotVisible: {failure}");
    assert!(failure["hierarchy"].as_str().unwrap().contains("Submit"));
}

#[test]
fn html_and_junit_reports_render_rich_failure_diagnostics() {
    let mut driver = make_driver();
    let out = temp_dir("reports");
    let flow = Loader::new()
        .parse(
            Path::new("checkout.yaml"),
            "\
appId: dev.tapwing.sample_app
---
- tapOn:
    text: Submit
    enabled: true
",
        )
        .unwrap();

    let shard_plan =
        ShardPlan { shard_index: None, sequence: Vec::new(), continue_on_failure: true, flows: vec![flow] };
    let opts = OrchestraOpts {
        lookup_timeout_ms: 0,
        optional_lookup_timeout_ms: 0,
        test_output_dir: Some(out.clone()),
        ..Default::default()
    };
    let suite = execute_shard_on_fake_driver(&mut driver, &shard_plan, &opts, Some("fake-1"));
    assert!(!suite.passed);

    let summary = TestExecutionSummary {
        passed: false,
        suite_name: Some("F8 Suite".into()),
        input_mode: "auto".into(),
        total_ms: 50.0,
        utc_offset_secs: 0,
        interrupted: None,
        suites: vec![suite],
    };

    // HTML report: self-contained (zero external URLs), inline JPEG thumbnail linked to full PNG, Did you mean, UI tree.
    let html = report::html::render_html(&summary, true);
    assert!(!html.contains("http://") && !html.contains("https://"));
    assert!(html.contains("Did you mean"), "{html}");
    assert!(html.contains("enabled=false, expected true"), "{html}");
    assert!(html.contains("data:image/jpeg;base64,"), "{html}");
    assert!(!html.contains("data:image/png;base64,"), "{html}");
    assert!(
        html.contains("<a href=\"")
            && html.contains("target=\"_blank\" rel=\"noopener\"><img src=\"data:image/jpeg;base64,"),
        "{html}"
    );
    assert!(html.contains("UI tree at failure (fake, 2 elements)"), "{html}");

    let report_html = report::html::render_html_for_path(&summary, true, Some(&out.join("reports/report.html")));
    assert!(report_html.contains("<a href=\"../failures/"), "{report_html}");

    // JUnit XML: clean `<failure>` + rich diagnostics in `<system-err>`.
    let xml = report::junit::render_junit(&summary).unwrap();
    assert!(xml.contains("<failure>"), "{xml}");
    assert!(xml.contains("<system-err>Failure diagnostics:"), "{xml}");
    assert!(xml.contains("enabled=false, expected true"), "{xml}");
    assert!(xml.contains("UI tree (fake, 2 elements):"), "{xml}");

    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn optional_run_flow_failure_does_not_suppress_later_real_failure_diagnostics() {
    let mut driver = make_driver();
    let out = temp_dir("optional_runflow_then_real_fail");
    let source = "\
appId: dev.tapwing.sample_app
---
- runFlow:
    optional: true
    commands:
      - tapOn: Missing Optional Button
- tapOn: Sign in
";
    let res = run_flow(&mut driver, "checkout/smoke.yaml", source, Some(&out));
    assert!(!res.success);

    // Optional runFlow was downgraded to a warning and produced no failure artifacts.
    let warned = res.steps.iter().find(|s| s["command"] == "runFlow").expect("runFlow step");
    assert_eq!(warned["status"], "warned");
    assert!(res.steps.iter().filter(|s| s["status"] == "warned").all(|s| s.get("failure").is_none()));

    // The real failing step after the optional runFlow MUST get full diagnostics.
    let real_fail = res
        .steps
        .iter()
        .find(|s| {
            s["status"] == "failed"
                && s["command"] == "tapOn"
                && s["description"].as_str().is_some_and(|d| d.contains("Sign in"))
        })
        .expect("real failing tapOn step");
    let failure = &real_fail["failure"];
    assert!(failure.is_object(), "expected failure diagnostics on real failure: {real_fail}");
    assert_eq!(failure["suggestions"][0]["value"], "Sign in with Google");
    assert!(failure["hierarchy"].as_str().unwrap().contains("Sign in with Google"));

    let shot_path = Path::new(failure["screenshot"].as_str().expect("screenshot path"));
    assert!(shot_path.exists());
    let tree_path = Path::new(failure["hierarchyFile"].as_str().expect("hierarchyFile path"));
    assert!(tree_path.exists());

    // Exactly 1 PNG + 1 hierarchy.txt written (none from the optional runFlow).
    let files: Vec<_> = std::fs::read_dir(out.join("failures")).unwrap().flatten().collect();
    assert_eq!(files.len(), 2, "expected only 1 PNG + 1 hierarchy.txt from the real failure");

    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn non_downgradable_error_inside_optional_run_flow_still_captures_diagnostics() {
    let mut driver = make_driver();
    let out = temp_dir("optional_runflow_js_err");
    let source = "\
appId: dev.tapwing.sample_app
---
- runFlow:
    optional: true
    commands:
      - evalScript: ${nope.boom()}
";
    let res = run_flow(&mut driver, "opt_js.yaml", source, Some(&out));
    assert!(!res.success);

    let with_diag: Vec<&Value> = res.steps.iter().filter(|s| s.get("failure").is_some()).collect();
    assert_eq!(with_diag.len(), 1, "expected failure diagnostics on non-downgradable JS error: {:?}", res.steps);
    assert_eq!(with_diag[0]["command"], "evalScript");

    let _ = std::fs::remove_dir_all(&out);
}
