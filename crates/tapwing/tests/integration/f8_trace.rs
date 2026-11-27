//! F8b trace viewer over `FakeDriver`: per-step trace extras (start order,
//! depth, retry attempts, lanes, optional), HTTP attribution (current, late,
//! trailing, in flight) with redaction, `--trace-screenshots steps`, the
//! per-shard `trace.json` + offline `trace.html`, and zero overhead without
//! `--trace`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tapwing::fake_driver::{Bounds, FakeDriver};
use tapwing::flow::Loader;
use tapwing::orchestra::{FlowResult, Orchestra, OrchestraOpts};
use tapwing::run::suite::{execute_shard_on_fake_driver, write_traces};
use tapwing::shard::ShardPlan;
use tapwing::trace::{self, TraceBatch, TraceOpts, TraceScreenshots};

const FLOW: &str = "\
appId: dev.tapwing.sample_app
---
- tapOn: Login
- evalScript: ${output.n = 1}
- assertVisible: Login
";

fn make_driver() -> FakeDriver {
    let mut driver = FakeDriver::new();
    driver.add_element(|e| {
        e.text = Some("Login".into());
        e.bounds = Some(Bounds::new(40, 200, 1040, 320));
        e.clickable = true;
    });
    driver
}

use crate::common::TempDir;

fn temp_dir(label: &str) -> TempDir {
    TempDir::new(label)
}

fn opts(trace: Option<TraceOpts>, out: Option<&Path>) -> OrchestraOpts {
    OrchestraOpts {
        lookup_timeout_ms: 0,
        optional_lookup_timeout_ms: 0,
        test_output_dir: out.map(Path::to_path_buf),
        trace,
        ..Default::default()
    }
}

fn run(driver: &mut FakeDriver, source: &str, opts: OrchestraOpts) -> FlowResult {
    let flow = Loader::new().parse(Path::new("trace_flow.yaml"), source).expect("flow parses");
    let mut opts = opts;
    opts.flow_path = flow.path.clone();
    Orchestra::new(driver, opts).run_parsed_flow(&flow)
}

fn http(id: u64, method: &str, url: &str, status: u64) -> Value {
    json!({ "seq": id, "method": method, "url": url, "state": "completed", "status": status, "request": id, "durationMs": 12 })
}

fn urls(step: &Value) -> Vec<String> {
    step["http"].as_array().into_iter().flatten().map(|h| h["url"].as_str().unwrap().to_string()).collect()
}

fn trace_steps(res: &FlowResult) -> Vec<Value> {
    res.trace.as_ref().expect("trace recorded")["steps"].as_array().unwrap().clone()
}

fn list_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                out.extend(list_files(&e.path()));
            } else {
                out.push(e.path());
            }
        }
    }
    out
}

#[test]
fn without_trace_no_trace_calls_no_files_and_unchanged_run_json() {
    let out = temp_dir("off");
    let mut off_driver = make_driver();
    let off = run(&mut off_driver, FLOW, opts(None, Some(&out)));
    assert!(off.success, "{:?}", off.error);
    assert_eq!(off_driver.trace_calls, 0, "no trace calls without --trace");
    assert!(off.trace.is_none());
    assert!(list_files(&out).is_empty(), "no files written: {:?}", list_files(&out));

    let mut on_driver = make_driver();
    let on = run(&mut on_driver, FLOW, opts(Some(TraceOpts::default()), None));
    assert!(on.success);
    assert_eq!(on_driver.trace_calls, 4, "one per step plus the flow-end drain");

    // `run.json` steps carry exactly the same keys with and without --trace:
    // trace extras live only in the trace.
    let keys = |r: &FlowResult| -> Vec<BTreeSet<String>> {
        r.steps.iter().map(|s| s.as_object().unwrap().keys().cloned().collect()).collect()
    };
    assert_eq!(keys(&off), keys(&on));
    let report = on.to_report_json("f", Path::new("trace_flow.yaml"), "v2");
    assert!(report.get("trace").is_none());
    assert!(report["steps"][0].get("startMs").is_none() && report["steps"][0].get("http").is_none());
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn http_entries_attach_to_the_step_that_started_them_including_late_trailing_and_in_flight() {
    let mut driver = make_driver();
    driver.trace_batches = [
        TraceBatch { current: vec![http(1, "GET", "http://127.0.0.1:8080/api/a", 200)], ..Default::default() },
        // Started by step 1, finished while step 2 ran.
        TraceBatch {
            lane: Some("host"),
            late: vec![http(2, "GET", "http://127.0.0.1:8080/api/b?page=1", 200)],
            dropped: 3,
            ..Default::default()
        },
        TraceBatch {
            lane: Some("native"),
            current: vec![json!({
                "seq": 3, "method": "POST",
                "url": "http://user:pw@127.0.0.1:8080/api/login?access_token=abc#frag",
                "state": "completed", "status": 500, "request": 5,
                "requestHeaders": { "authorization": "Bearer secret", "accept": "application/json" },
            })],
            ..Default::default()
        },
    ]
    .into();
    driver.trailing_trace = TraceBatch {
        current: vec![http(4, "GET", "http://127.0.0.1:8080/api/d", 404)],
        in_flight: vec![
            json!({ "method": "GET", "url": "http://127.0.0.1:8080/api/slow", "state": "inFlight", "startMs": 9 }),
        ],
        ..Default::default()
    };
    let res = run(&mut driver, FLOW, opts(Some(TraceOpts::default()), None));
    assert!(res.success, "{:?}", res.error);
    let t = res.trace.as_ref().unwrap();
    assert_eq!(t["http"], "enabled");
    assert_eq!(t["httpDropped"], 3);
    let steps = trace_steps(&res);
    assert_eq!(steps.len(), 3);
    assert_eq!(urls(&steps[0]), ["http://127.0.0.1:8080/api/a", "http://127.0.0.1:8080/api/b?page=1"]);
    assert_eq!(steps[0]["lane"], "flutter");
    assert_eq!(steps[1]["lane"], "host");
    assert!(urls(&steps[1]).is_empty());
    assert_eq!(steps[2]["lane"], "native");
    // Redacted again on the CLI side, whatever the driver sent.
    assert_eq!(
        urls(&steps[2]),
        [
            "http://127.0.0.1:8080/api/login?access_token=<redacted>",
            "http://127.0.0.1:8080/api/d",
            "http://127.0.0.1:8080/api/slow"
        ]
    );
    assert_eq!(steps[2]["http"][0]["requestHeaders"]["authorization"], trace::REDACTED);
    assert_eq!(steps[2]["http"][0]["requestHeaders"]["accept"], "application/json");
    assert_eq!(steps[2]["http"][2]["state"], "inFlight");
    let encoded = serde_json::to_string(t).unwrap();
    for secret in ["Bearer secret", "user:pw", "abc#frag", "frag"] {
        assert!(!encoded.contains(secret), "{secret} leaked: {encoded}");
    }
    // Start offsets are monotonic in the trace.
    let starts: Vec<f64> = steps.iter().map(|s| s["startMs"].as_f64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
}

#[test]
fn nested_steps_follow_their_wrapper_with_depth_attempts_and_optional() {
    let mut driver = make_driver();
    let source = "\
appId: dev.tapwing.sample_app
---
- runFlow:
    commands:
      - tapOn: Login
- retry:
    maxRetries: 1
    commands:
      - evalScript: ${output.n = (output.n || 0) + 1}
      - assertTrue: ${output.n > 1}
- tapOn:
    text: Missing
    optional: true
";
    let res = run(&mut driver, source, opts(Some(TraceOpts::default()), None));
    assert!(res.success, "{:?}", res.error);
    let steps = trace_steps(&res);
    let summary: Vec<(String, u64, Option<u64>, String)> = steps
        .iter()
        .map(|s| {
            (
                s["command"].as_str().unwrap().to_string(),
                s["depth"].as_u64().unwrap(),
                s["attempt"].as_u64(),
                s["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expect = |c: &str, d: u64, a: Option<u64>, st: &str| (c.to_string(), d, a, st.to_string());
    assert_eq!(
        summary,
        vec![
            expect("runFlow", 0, None, "passed"),
            expect("tapOn", 1, None, "passed"),
            expect("retry", 0, None, "passed"),
            expect("evalScript", 1, Some(1), "passed"),
            expect("assertTrue", 1, Some(1), "failed"),
            expect("evalScript", 1, Some(2), "passed"),
            expect("assertTrue", 1, Some(2), "passed"),
            expect("tapOn", 0, None, "warned"),
        ],
        "{steps:#?}"
    );
    // Wrappers have no lane of their own; the optional miss is flagged.
    assert!(steps[0].get("lane").is_none());
    assert_eq!(steps[1]["lane"], "flutter");
    assert_eq!(steps[7]["optional"], true);
}

#[test]
fn every_step_capture_writes_screenshots_and_trees_for_ui_steps_only() {
    let out = temp_dir("steps");
    let mut driver = make_driver();
    let source = "\
appId: dev.tapwing.sample_app
---
- tapOn: Login
- evalScript: ${output.n = 1}
- tapOn: Nope
";
    let res = run(&mut driver, source, opts(Some(TraceOpts { screenshots: TraceScreenshots::Steps }), Some(&out)));
    assert!(!res.success);
    let steps = trace_steps(&res);
    let shot = Path::new(steps[0]["screenshot"]["path"].as_str().expect("per-step screenshot"));
    assert!(shot.starts_with(out.join("trace")), "{}", shot.display());
    assert!(std::fs::read(shot).unwrap().starts_with(b"\x89PNG"));
    assert!(steps[0]["tree"].as_str().unwrap().contains("Login"));
    assert_eq!(steps[0]["treeSource"], "fake");
    assert!(steps[1].get("screenshot").is_none() && steps[1].get("tree").is_none(), "no capture for evalScript");
    // The failing step relies on the F8a failure capture.
    assert!(steps[2].get("screenshot").is_none());
    assert!(Path::new(steps[2]["failure"]["screenshot"].as_str().unwrap()).exists());
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn shard_traces_write_json_and_offline_html_without_collisions() {
    let out = temp_dir("files");
    let flow = Loader::new().parse(Path::new("checkout.yaml"), FLOW).unwrap();
    let trace_opts = TraceOpts { screenshots: TraceScreenshots::Steps };
    let base = opts(Some(trace_opts), Some(&out));

    let mut suites = Vec::new();
    for shard in [Some(0), Some(1)] {
        let mut driver = make_driver();
        driver.trace_batches =
            [TraceBatch { current: vec![http(1, "GET", "http://127.0.0.1:8080/x", 503)], ..Default::default() }].into();
        let plan = ShardPlan {
            shard_index: shard,
            sequence: Vec::new(),
            continue_on_failure: true,
            flows: vec![flow.clone()],
        };
        suites.push(execute_shard_on_fake_driver(&mut driver, &plan, &base, Some("fake-1")));
    }
    // `--flatten-debug-output`: both shards share one directory.
    write_traces(&suites, &[out.to_path_buf(), out.to_path_buf()], &trace_opts);
    for n in [1, 2] {
        let json_path = out.join(format!("trace-shard-{n}.json"));
        let html_path = out.join(format!("trace-shard-{n}.html"));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(doc["schema"], trace::SCHEMA);
        assert_eq!(doc["shard"]["index"], n - 1);
        assert_eq!(doc["options"]["screenshots"], "steps");
        let flow = &doc["flows"][0];
        assert_eq!(flow["status"], "passed");
        assert_eq!(flow["http"], "enabled");
        assert_eq!(flow["steps"].as_array().unwrap().len(), 3);
        assert_eq!(flow["steps"][0]["http"][0]["status"], 503);

        let html = std::fs::read_to_string(&html_path).unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("data:image/jpeg;base64,"), "inline thumbnails");
        let data_start = html.find("<script id=\"trace-data\"").unwrap();
        let data_end = data_start + html[data_start..].find("</script>").unwrap();
        let outside = format!("{}{}", &html[..data_start], &html[data_end..]);
        assert!(!outside.contains("http://") && !outside.contains("https://"), "no external URLs");
    }
    // Per-step assets of the two shards do not overwrite each other.
    let shots: Vec<PathBuf> = list_files(&out.join("trace"));
    let names: BTreeSet<String> = shots.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
    assert_eq!(names.len(), shots.len());
    assert!(names.iter().any(|n| n.starts_with("s1_")) && names.iter().any(|n| n.starts_with("s2_")), "{names:?}");

    // Unsharded runs write plain `trace.json` / `trace.html`.
    let single = temp_dir("single");
    let mut driver = make_driver();
    let plan = ShardPlan { shard_index: None, sequence: Vec::new(), continue_on_failure: true, flows: vec![flow] };
    let suite = execute_shard_on_fake_driver(&mut driver, &plan, &opts(Some(TraceOpts::default()), None), None);
    let files = trace::write_shard_trace(&suite, &single, &TraceOpts::default()).unwrap();
    assert_eq!(files.html, single.join("trace.html"));
    assert!(files.json.exists());

    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(&single);
}
