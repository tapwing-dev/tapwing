//! Edge cases of the report writers (`--format junit|html|html-detailed`, `--report`) and the
//! trace viewer model: escaping, empty suites, mixed outcomes, app-health issues and output paths.

use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::{Value, json};
use tapwing::failure::{FailureDiagnostics, Suggestion};
use tapwing::report::html::{escape_html, render_html, render_html_for_path};
use tapwing::report::junit::{render_junit, sanitize_xml_text};
use tapwing::report::{
    FlowRunRecord, FlowStatus, ReportFormat, StepRecord, SuiteResult, TestExecutionSummary, render_json_report,
    resolve_report_output, write_formatted_report,
};
use tapwing::trace::{self, TraceOpts, TraceScreenshots};

const TS_MS: u64 = 1_764_167_405_000; // 2025-11-26T14:30:05Z

fn flow(name: &str, status: FlowStatus) -> FlowRunRecord {
    FlowRunRecord {
        name: name.into(),
        path: PathBuf::from(format!("{name}.yaml")),
        file: None,
        status,
        duration_ms: 1500.0,
        start_epoch_ms: TS_MS,
        failure_message: None,
        properties: Vec::new(),
        tags: Vec::new(),
        lane: "flutter".into(),
        flutter_steps: 1,
        native_steps: 0,
        blackbox: false,
        blackbox_reason: None,
        steps: Vec::new(),
        failure_diagnostics: None,
        raw_json: json!({ "name": name }),
        trace: None,
        graybox: Default::default(),
    }
}

fn failing(name: &str, message: &str) -> FlowRunRecord {
    let mut f = flow(name, FlowStatus::Error);
    f.failure_message = Some(message.into());
    f
}

fn step(index: usize, description: &str, issues: &[&str]) -> StepRecord {
    StepRecord {
        index,
        command: description.split_whitespace().next().unwrap_or("step").into(),
        description: description.into(),
        status: "passed".into(),
        duration_ms: 12.0,
        warning: None,
        error: None,
        app_issues: issues.iter().map(|s| s.to_string()).collect(),
    }
}

fn suite(shard: Option<usize>, device: Option<&str>, flows: Vec<FlowRunRecord>) -> SuiteResult {
    let passed = flows.iter().all(FlowRunRecord::passed);
    SuiteResult {
        shard_index: shard,
        device_id: device.map(String::from),
        device_description: device.map(String::from),
        platform: "android".into(),
        passed,
        setup_ms: 3.0,
        duration_ms: 2500.0,
        start_epoch_ms: TS_MS,
        skipped_count: 0,
        error: None,
        flows,
    }
}

fn summary(suites: Vec<SuiteResult>) -> TestExecutionSummary {
    TestExecutionSummary {
        passed: suites.iter().all(|s| s.passed),
        suite_name: None,
        input_mode: "auto".into(),
        total_ms: 2500.0,
        utc_offset_secs: 0,
        interrupted: None,
        suites,
    }
}

/// Parses `xml` strictly and returns its element names in document order.
fn xml_elements(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    let mut names = Vec::new();
    let mut depth = 0usize;
    loop {
        match reader.read_event().expect("well-formed XML") {
            Event::Start(e) => {
                depth += 1;
                names.push(e.name().as_ref().to_string());
            }
            Event::Empty(e) => names.push(e.name().as_ref().to_string()),
            Event::End(_) => {
                assert!(depth > 0, "unbalanced end tag");
                depth -= 1;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!(depth, 0, "unclosed elements");
    names
}

use crate::common::TempDir;

fn temp_dir(label: &str) -> TempDir {
    TempDir::new(label)
}

// ---------------------------------------------------------------- JUnit

#[test]
fn junit_with_no_suites_is_an_empty_testsuites_element() {
    let xml = render_junit(&summary(vec![])).unwrap();
    assert_eq!(xml, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites/>\n");
    assert_eq!(xml_elements(&xml), ["testsuites"]);
}

#[test]
fn junit_with_an_empty_suite_reports_zero_tests() {
    let xml = render_junit(&summary(vec![suite(None, Some("Pixel 8"), vec![])])).unwrap();
    assert_eq!(xml_elements(&xml), ["testsuites", "testsuite"]);
    assert!(xml.contains("tests=\"0\" failures=\"0\""), "{xml}");
    assert!(!xml.contains("<testcase"), "{xml}");
}

#[test]
fn junit_mixed_pass_warning_and_fail_counts_only_errors_as_failures() {
    let flows = vec![
        flow("passes", FlowStatus::Success),
        flow("warns", FlowStatus::Warning),
        failing("fails", "boom"),
        failing("fails too", "bang"),
    ];
    let xml = render_junit(&summary(vec![suite(None, None, flows)])).unwrap();
    assert!(xml.contains("tests=\"4\" failures=\"2\""), "{xml}");
    assert!(
        xml.contains("status=\"SUCCESS\"") && xml.contains("status=\"WARNING\"") && xml.contains("status=\"ERROR\"")
    );
    assert_eq!(xml.matches("<failure>").count(), 2, "{xml}");
    // A warning is not a failure: its testcase carries no <failure>.
    let warns = xml.lines().find(|l| l.contains("name=\"warns\"")).unwrap();
    assert!(warns.trim_end().ends_with("/>"), "{warns}");
}

#[test]
fn junit_escapes_every_xml_special_character_in_attributes_and_text() {
    let mut f = failing("a<b>&\"c\"'d'", "x < y && y > z \"quoted\" 'single'");
    f.file = Some("flows/<odd> & \"name\".yaml".into());
    f.tags = vec!["<t1>".into(), "a&b".into()];
    f.properties = vec![("k\"ey".into(), "v<al>&".into())];
    let xml = render_junit(&summary(vec![suite(None, Some("dev<1>&"), vec![f])])).unwrap();
    // Round-trips through a strict XML parser and never leaks raw markup.
    xml_elements(&xml);
    assert!(!xml.contains("<b>") && !xml.contains("<odd>") && !xml.contains("<t1>"), "{xml}");
    assert!(
        xml.contains("name=\"a&lt;b&gt;&amp;&quot;c&quot;&apos;d&apos;\"")
            || xml.contains("name=\"a&lt;b&gt;&amp;&quot;c&quot;'d'\""),
        "{xml}"
    );
    assert!(xml.contains("device=\"dev&lt;1&gt;&amp;\""), "{xml}");
    assert!(xml.contains("x &lt; y &amp;&amp; y &gt; z"), "{xml}");
}

#[test]
fn junit_keeps_unicode_and_strips_only_illegal_control_characters() {
    let f = failing(
        "Přihlášení 🚀 – čeština",
        "Chyba: „pole“ je prázdné\t(tab) ok\u{0008}\u{000B}\u{000C}\u{FFFE}\u{FFFF}",
    );
    let xml = render_junit(&summary(vec![suite(None, None, vec![f])])).unwrap();
    xml_elements(&xml);
    assert!(xml.contains("Přihlášení 🚀 – čeština"), "{xml}");
    assert!(xml.contains("Chyba: „pole“ je prázdné\t(tab) ok"), "{xml}");
    for bad in ['\u{0008}', '\u{000B}', '\u{000C}', '\u{FFFE}', '\u{FFFF}'] {
        assert!(!xml.contains(bad), "U+{:04X} leaked", bad as u32);
    }
}

#[test]
fn sanitize_xml_text_boundaries() {
    assert_eq!(sanitize_xml_text("a\tb\nc\rd"), "a\tb\nc\rd", "tab, newline and carriage return survive");
    assert_eq!(sanitize_xml_text("\u{0000}\u{0001}\u{0008}\u{000B}\u{000C}\u{000E}\u{001F}"), "");
    assert_eq!(
        sanitize_xml_text("\u{0020}\u{007F}\u{0080}\u{D7FF}\u{E000}\u{FFFD}\u{10000}"),
        "\u{0020}\u{007F}\u{0080}\u{D7FF}\u{E000}\u{FFFD}\u{10000}"
    );
    assert_eq!(sanitize_xml_text(""), "");
}

#[test]
fn junit_app_health_issues_pass_failures_and_have_one_system_err_per_flow() {
    let mut clean = flow("clean", FlowStatus::Success);
    clean.steps = vec![step(0, "launchApp", &[])];
    let mut noisy = flow("noisy", FlowStatus::Success);
    noisy.steps = vec![
        step(0, "launchApp", &[]),
        step(1, "tapOn \"Pay\"", &["layoutOverflow: overflowed by 12 pixels (×2)", "uncaughtError: <Bad> & worse"]),
        step(2, "assertVisible Done", &["flutterError: late"]),
    ];
    let xml = render_junit(&summary(vec![suite(None, None, vec![clean, noisy])])).unwrap();
    xml_elements(&xml);
    assert_eq!(xml.matches("<system-err>").count(), 1, "only the noisy flow has app issues:\n{xml}");
    assert!(
        xml.contains("App issues:\nstep 2 `tapOn &quot;Pay&quot;`: layoutOverflow: overflowed by 12 pixels (×2)\n"),
        "{xml}"
    );
    assert!(xml.contains("uncaughtError: &lt;Bad&gt; &amp; worse"), "{xml}");
    assert!(xml.contains("step 3 `assertVisible Done`: flutterError: late"), "{xml}");
}

#[test]
fn junit_failure_diagnostics_and_app_issues_share_one_system_err() {
    let mut f = failing("diag", "Element not found");
    f.steps = vec![step(0, "tapOn Login", &["layoutOverflow: x"])];
    f.failure_diagnostics = Some(FailureDiagnostics {
        suggestions: vec![Suggestion { field: "text".into(), value: "Log in".into(), reason: "similar".into() }],
        hierarchy: Some("- text=\"Log in\"".into()),
        hierarchy_source: Some("flutter".into()),
        total_elements: Some(1),
        screenshot: None,
        hierarchy_file: None,
    });
    let xml = render_junit(&summary(vec![suite(None, None, vec![f])])).unwrap();
    xml_elements(&xml);
    assert_eq!(xml.matches("<system-err>").count(), 1, "{xml}");
    assert!(xml.contains("Log in") && xml.contains("App issues:"), "{xml}");
}

#[test]
fn junit_single_shard_failure_has_no_shard_prefix_and_multi_shard_failures_do() {
    let single = render_junit(&summary(vec![suite(None, None, vec![failing("f", "boom")])])).unwrap();
    assert!(single.contains("<failure>boom</failure>"), "{single}");

    let multi = render_junit(&summary(vec![
        suite(Some(0), Some("emu-1"), vec![failing("a", "boom")]),
        suite(Some(2), Some("emu-3"), vec![failing("b", "[shard 3] already prefixed")]),
    ]))
    .unwrap();
    assert!(multi.contains("<failure>[shard 1] boom</failure>"), "{multi}");
    // Never prefixed twice.
    assert!(multi.contains("<failure>[shard 3] already prefixed</failure>"), "{multi}");
    assert!(!multi.contains("[shard 3] [shard 3]"), "{multi}");
}

#[test]
fn junit_tag_and_junit_properties_override_ids_and_do_not_duplicate() {
    let mut f = flow("Flow", FlowStatus::Success);
    f.tags = vec!["smoke".into(), "regression".into()];
    f.properties = vec![
        ("junitId".into(), "id-7".into()),
        ("junitClassname".into(), "com.example.Suite".into()),
        ("tags".into(), "stale".into()),
        ("owner".into(), "qa".into()),
    ];
    let xml = render_junit(&summary(vec![suite(None, None, vec![f])])).unwrap();
    assert!(xml.contains("id=\"id-7\" name=\"Flow\" classname=\"com.example.Suite\""), "{xml}");
    assert_eq!(xml.matches("name=\"tags\"").count(), 1, "{xml}");
    assert!(xml.contains("<property name=\"tags\" value=\"smoke, regression\"/>"), "{xml}");
    assert!(xml.contains("<property name=\"owner\" value=\"qa\"/>"), "{xml}");
    assert!(!xml.contains("junitId") && !xml.contains("stale"), "{xml}");
}

#[test]
fn junit_timestamps_follow_the_local_utc_offset() {
    let mut s = summary(vec![suite(None, None, vec![flow("f", FlowStatus::Success)])]);
    s.utc_offset_secs = 2 * 3600;
    let xml = render_junit(&s).unwrap();
    assert!(xml.contains("timestamp=\"2025-11-26T16:30:05\""), "{xml}");
    s.utc_offset_secs = -(5 * 3600 + 30 * 60);
    let xml = render_junit(&s).unwrap();
    assert!(xml.contains("timestamp=\"2025-11-26T09:00:05\""), "{xml}");
}

// ---------------------------------------------------------------- HTML

#[test]
fn html_with_no_suites_and_with_empty_suites_still_renders_a_complete_document() {
    for s in [summary(vec![]), summary(vec![suite(None, None, vec![])])] {
        for detailed in [false, true] {
            let html = render_html(&s, detailed);
            assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
            assert!(html.trim_end().ends_with("</html>"), "{html}");
            assert!(html.contains("<dt>Flows</dt><dd>0</dd>"), "{html}");
            assert!(!html.contains("<h2>Failures</h2>"), "no failures block for an empty run");
        }
    }
}

#[test]
fn html_failure_list_links_every_failed_flow_and_names_devices_only_when_sharded() {
    let single =
        render_html(&summary(vec![suite(None, Some("emu-1"), vec![failing("f1", "bad\nsecond line")])]), false);
    assert!(single.contains("<h2>Failures</h2>"), "{single}");
    assert!(single.contains("href=\"#flow-1-0\""), "{single}");
    assert!(single.contains("<span class=\"msg mono\">bad</span>"), "only the first message line is listed: {single}");
    assert!(!single.contains("on emu-1"), "{single}");

    let sharded = render_html(
        &summary(vec![
            suite(Some(0), Some("emu-1"), vec![flow("ok", FlowStatus::Success), failing("f1", "x")]),
            suite(Some(1), Some("emu-2"), vec![failing("f2", "y")]),
        ]),
        false,
    );
    assert!(sharded.contains("href=\"#flow-1-1\"") && sharded.contains("href=\"#flow-2-0\""), "{sharded}");
    assert!(sharded.contains("on emu-1") && sharded.contains("on emu-2"), "{sharded}");
    assert!(sharded.contains("2 devices"), "{sharded}");
}

#[test]
fn html_passing_flow_with_app_issues_stays_open_and_pluralizes() {
    let mut f = flow("noisy", FlowStatus::Success);
    f.steps = vec![step(0, "tapOn A", &["one", "two"]), step(1, "tapOn B", &["three"])];
    let html = render_html(&summary(vec![suite(None, None, vec![f])]), true);
    assert!(html.contains("<details class=\"flow-card\" open"), "{html}");
    assert!(html.contains("⚠ 3 app issues"), "{html}");

    let mut one = flow("one", FlowStatus::Success);
    one.steps = vec![step(0, "tapOn A", &["only"])];
    let html = render_html(&summary(vec![suite(None, None, vec![one])]), false);
    assert!(html.contains("⚠ 1 app issue</span>"), "{html}");

    let clean = render_html(&summary(vec![suite(None, None, vec![flow("clean", FlowStatus::Success)])]), true);
    assert!(!clean.contains("<details class=\"flow-card\" open"), "clean passing flows are collapsed");
    assert!(!clean.contains("app issue"), "{clean}");
}

#[test]
fn html_escapes_suite_names_tags_properties_and_failure_screenshot_paths() {
    let mut f = failing("flow", "<boom>");
    f.tags = vec!["<tag>".into()];
    f.properties = vec![("<k>".into(), "<v>".into())];
    f.failure_diagnostics = Some(FailureDiagnostics {
        screenshot: Some("/no/such/<shot>.png".into()),
        hierarchy: Some("<Text/>".into()),
        hierarchy_source: Some("<src>".into()),
        total_elements: Some(2),
        ..Default::default()
    });
    let mut s = summary(vec![suite(None, None, vec![f])]);
    s.suite_name = Some("Suite <\"x\"> & 'y'".into());
    let html = render_html(&s, true);
    for raw in ["<boom>", "<tag>", "<k>", "<v>", "<shot>", "<Text/>", "<src>", "Suite <\"x\">"] {
        assert!(!html.contains(raw), "{raw} must be escaped");
    }
    assert!(html.contains("<title>Suite &lt;&quot;x&quot;&gt; &amp; &#39;y&#39; — Tapwing</title>"), "{html}");
    // A screenshot file that is gone degrades to its escaped path, never an <img> with a bad URI.
    assert!(html.contains("Screenshot: <code>/no/such/&lt;shot&gt;.png</code>"), "{html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(html.contains("UI tree at failure (&lt;src&gt;, 2 elements)"), "{html}");
}

#[test]
fn html_inlines_an_existing_failure_screenshot_as_jpeg_thumbnail_linked_to_full_png() {
    let dir = temp_dir("shot");
    let failures = dir.join("failures");
    std::fs::create_dir_all(&failures).unwrap();
    let png = failures.join("my fail #1?v=2%20.png");
    image::RgbaImage::from_pixel(4, 4, image::Rgba([66, 133, 244, 255])).save(&png).unwrap();

    let mut f = failing("flow", "boom");
    f.failure_diagnostics = Some(FailureDiagnostics {
        screenshot: Some(png.to_string_lossy().into_owned()),
        total_elements: Some(1),
        hierarchy: Some("- a".into()),
        ..Default::default()
    });
    let s = summary(vec![suite(None, None, vec![f])]);

    let html = render_html(&s, false);
    assert!(html.contains("data:image/jpeg;base64,"), "{html}");
    assert!(!html.contains("data:image/png;base64,"), "{html}");
    assert!(html.contains("target=\"_blank\" rel=\"noopener\"><img src=\"data:image/jpeg;base64,"), "{html}");
    assert!(!html.contains("http://") && !html.contains("https://"), "self-contained: no external URLs");
    assert!(html.contains("UI tree at failure (1 element)"), "singular element: {html}");

    // Relative link resolution across directories & URL-escaping of #, ?, %, spaces.
    let nested_report = dir.join("reports/report.html");
    let cross_dir = render_html_for_path(&s, false, Some(&nested_report));
    assert!(cross_dir.contains("href=\"../failures/my%20fail%20%231%3Fv%3D2%2520.png\""), "{cross_dir}");
    assert!(!cross_dir.contains("http://") && !cross_dir.contains("https://"));

    // Non-decodable bytes on disk degrade cleanly without panicking or emitting <img>.
    let bad_png = failures.join("corrupt.png");
    std::fs::write(&bad_png, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
    let mut f_bad = failing("bad_img", "boom");
    f_bad.failure_diagnostics =
        Some(FailureDiagnostics { screenshot: Some(bad_png.to_string_lossy().into_owned()), ..Default::default() });
    let bad_html = render_html_for_path(&summary(vec![suite(None, None, vec![f_bad])]), false, Some(&nested_report));
    assert!(!bad_html.contains("<img"), "{bad_html}");
    assert!(bad_html.contains("href=\"../failures/corrupt.png\""), "{bad_html}");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn html_formats_durations_across_the_ms_s_and_minute_ranges() {
    for (ms, expected) in [
        (0.0, "0 ms"),
        (850.0, "850 ms"),
        (999.4, "999 ms"),
        (1000.0, "1.00 s"),
        (14_250.0, "14.25 s"),
        (59_990.0, "59.99 s"),
        (60_000.0, "1 min 00 s"),
        (185_000.0, "3 min 05 s"),
        (f64::NAN, "0 ms"),
        (-5.0, "0 ms"),
    ] {
        let mut s = summary(vec![]);
        s.total_ms = ms;
        let html = render_html(&s, false);
        assert!(html.contains(&format!("<dt>Duration</dt><dd>{expected}</dd>")), "{ms} -> {expected}");
    }
}

#[test]
fn html_platform_labels_and_skipped_counts() {
    let mut ios = suite(None, Some("iPhone 16"), vec![flow("f", FlowStatus::Success)]);
    ios.platform = "ios".into();
    ios.skipped_count = 2;
    let mut other = suite(Some(1), Some("x"), vec![flow("g", FlowStatus::Success)]);
    other.platform = "<web>".into();
    let html = render_html(&summary(vec![ios, other]), false);
    assert!(html.contains("iOS"), "{html}");
    assert!(html.contains("1 of 1 passed, 2 skipped"), "{html}");
    assert!(html.contains("&lt;web&gt;") && !html.contains("<web>"), "{html}");
    assert!(html.contains("<dt>Skipped</dt><dd>2</dd>"), "{html}");
}

#[test]
fn escape_html_handles_every_special_character_once() {
    assert_eq!(escape_html("<a href=\"x\">&'</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
    assert_eq!(escape_html("&amp;"), "&amp;amp;", "already-escaped input is escaped again, never trusted");
    assert_eq!(escape_html("Čeština 🚀"), "Čeština 🚀");
    assert_eq!(escape_html(""), "");
}

// ---------------------------------------------------------------- JSON + output paths

#[test]
fn json_report_single_shard_keeps_the_legacy_shape() {
    let report =
        render_json_report(&summary(vec![suite(None, Some("emu"), vec![flow("a", FlowStatus::Success)])])).unwrap();
    let v: Value = serde_json::from_str(&report).unwrap();
    assert_eq!(v["platform"], "android");
    assert_eq!(v["inputMode"], "auto");
    assert_eq!(v["flows"], json!([{ "name": "a" }]));
    assert!(v.get("shards").is_none() && v.get("summary").is_none(), "{v}");
}

#[test]
fn json_report_multi_shard_flattens_flows_and_summarizes() {
    let mut ios = suite(Some(1), Some("sim"), vec![failing("b", "x")]);
    ios.platform = "ios".into();
    ios.skipped_count = 1;
    let s = summary(vec![suite(Some(0), Some("emu"), vec![flow("a", FlowStatus::Success)]), ios]);
    let v: Value = serde_json::from_str(&render_json_report(&s).unwrap()).unwrap();
    assert_eq!(v["platform"], "mixed");
    assert_eq!(v["flows"].as_array().unwrap().len(), 2);
    assert_eq!(v["flows"][1]["shardId"], 2);
    assert_eq!(v["flows"][1]["device"], "sim");
    assert_eq!(v["flows"][1]["platform"], "ios");
    assert_eq!(v["summary"]["passed"], false);
    assert_eq!(v["summary"]["passedCount"], 1);
    assert_eq!(v["summary"]["failedCount"], 1);
    assert_eq!(v["summary"]["skippedCount"], 1);
    assert_eq!(v["summary"]["totalCount"], 2);
    assert_eq!(v["shards"][0]["passedCount"], 1);
    assert_eq!(v["shards"][1]["failedCount"], 1);
}

#[test]
fn report_output_defaults_and_noop_handling() {
    assert_eq!(resolve_report_output(ReportFormat::Noop, None), None);
    assert_eq!(
        resolve_report_output(ReportFormat::Noop, Some(Path::new("x.xml"))),
        None,
        "--output is ignored for noop"
    );
    assert_eq!(resolve_report_output(ReportFormat::Junit, None), Some(PathBuf::from("report.xml")));
    assert_eq!(resolve_report_output(ReportFormat::Html, None), Some(PathBuf::from("report.html")));
    assert_eq!(resolve_report_output(ReportFormat::HtmlDetailed, None), Some(PathBuf::from("report.html")));
    assert_eq!(
        resolve_report_output(ReportFormat::Junit, Some(Path::new("out/r.xml"))),
        Some(PathBuf::from("out/r.xml"))
    );
}

#[test]
fn write_formatted_report_creates_parent_dirs_and_skips_noop() {
    let dir = temp_dir("write");
    let s = summary(vec![suite(None, None, vec![flow("f", FlowStatus::Success)])]);

    assert_eq!(write_formatted_report(ReportFormat::Noop, None, &s).unwrap(), None);

    let target = dir.join("nested/deeper/report.xml");
    let written = write_formatted_report(ReportFormat::Junit, Some(&target), &s).unwrap();
    assert_eq!(written.as_deref(), Some(target.as_path()));
    xml_elements(&std::fs::read_to_string(&target).unwrap());

    let html_target = dir.join("r.html");
    write_formatted_report(ReportFormat::HtmlDetailed, Some(&html_target), &s).unwrap();
    assert!(std::fs::read_to_string(&html_target).unwrap().contains("<!DOCTYPE html>"));

    // A directory where the file should go is an error that names the path.
    let err = write_formatted_report(ReportFormat::Junit, Some(&dir), &s).unwrap_err();
    assert!(format!("{err:#}").contains("writing report to"), "{err:#}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn report_format_parsing_rejects_unknown_formats_with_the_valid_list() {
    let err = "pdf".parse::<ReportFormat>().unwrap_err().to_string();
    assert_eq!(err, "unknown report format `pdf` (expected junit, html, html-detailed, or noop)");
    assert_eq!("  JUnit ".parse::<ReportFormat>().unwrap(), ReportFormat::Junit);
    assert_eq!("HTML_DETAILED".parse::<ReportFormat>().unwrap(), ReportFormat::HtmlDetailed);
}

#[test]
fn summary_counters_aggregate_across_suites() {
    let mut a = suite(Some(0), None, vec![flow("a", FlowStatus::Success), flow("b", FlowStatus::Warning)]);
    a.skipped_count = 3;
    let b = suite(Some(1), None, vec![failing("c", "x")]);
    let s = summary(vec![a, b]);
    assert_eq!((s.passed_count(), s.failed_count(), s.skipped_count(), s.total_executed()), (2, 1, 3, 3));
    assert!(flow("w", FlowStatus::Warning).passed(), "a warning still passes");
    assert!(!failing("e", "x").passed());
}

// ---------------------------------------------------------------- trace model + viewer

#[test]
fn trace_document_describes_shard_options_and_flow_outcomes() {
    let mut ok = flow("ok", FlowStatus::Success);
    ok.raw_json = json!({ "steps": [{ "command": "launchApp" }] });
    let mut bad = failing("bad", "Element not found");
    bad.blackbox = true;
    bad.file = Some("flows/bad.yaml".into());
    bad.raw_json = json!({ "steps": [], "appIssueSummary": { "total": 2 } });
    bad.trace = Some(json!({ "http": "on", "httpDropped": 4, "steps": [{ "command": "tapOn" }] }));
    let s = suite(Some(1), Some("Pixel 8"), vec![ok, bad]);
    let doc = trace::build_shard_trace(&s, &TraceOpts { screenshots: TraceScreenshots::Steps });

    assert_eq!(doc["schema"], trace::SCHEMA);
    assert_eq!(doc["tapwingVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["shard"], json!({ "index": 1, "device": "Pixel 8", "platform": "android" }));
    assert_eq!(doc["options"]["screenshots"], "steps");
    let flows = doc["flows"].as_array().unwrap();
    assert_eq!(flows[0]["status"], "passed");
    assert_eq!(flows[0]["http"], "off", "no HTTP log without a trace payload");
    assert_eq!(flows[0]["httpDropped"], 0);
    assert_eq!(flows[0]["steps"], json!([{ "command": "launchApp" }]), "falls back to the raw steps");
    assert!(flows[0].get("error").is_none() && flows[0].get("blackbox").is_none());
    assert_eq!(flows[1]["status"], "failed");
    assert_eq!(flows[1]["error"], "Element not found");
    assert_eq!(flows[1]["blackbox"], true);
    assert_eq!(flows[1]["file"], "flows/bad.yaml");
    assert_eq!(flows[1]["http"], "on");
    assert_eq!(flows[1]["httpDropped"], 4);
    assert_eq!(flows[1]["steps"], json!([{ "command": "tapOn" }]), "trace steps win over raw steps");
    assert_eq!(flows[1]["appIssueSummary"], json!({ "total": 2 }));
}

#[test]
fn trace_files_never_collide_across_shards_and_the_viewer_is_self_contained() {
    let dir = temp_dir("trace");
    let opts = TraceOpts::default();
    assert_eq!(opts.screenshots, TraceScreenshots::Failures);
    let a = trace::write_shard_trace(&suite(None, None, vec![flow("a", FlowStatus::Success)]), &dir, &opts).unwrap();
    let b = trace::write_shard_trace(&suite(Some(0), None, vec![flow("b", FlowStatus::Success)]), &dir, &opts).unwrap();
    let c = trace::write_shard_trace(&suite(Some(1), None, vec![failing("c", "x")]), &dir, &opts).unwrap();
    let names: Vec<_> =
        [&a, &b, &c].iter().map(|t| t.json.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["trace.json", "trace-shard-1.json", "trace-shard-2.json"]);
    for t in [&a, &b, &c] {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&t.json).unwrap()).unwrap();
        assert_eq!(doc["schema"], trace::SCHEMA);
        let html = std::fs::read_to_string(&t.html).unwrap();
        assert!(html.contains("<html"), "{}", t.html.display());
        assert!(!html.contains("src=\"http") && !html.contains("href=\"http"), "viewer must work offline");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn trace_viewer_cannot_be_broken_out_of_by_hostile_flow_data() {
    let mut hostile = failing("</script><script>alert(1)</script>", "<img src=x onerror=alert(1)>\u{2028}\u{2029}");
    hostile.raw_json = json!({ "steps": [] });
    let doc = trace::build_shard_trace(&suite(None, None, vec![hostile]), &TraceOpts::default());
    let html = trace::render_trace_html(&doc);
    assert!(!html.contains("</script><script>alert(1)"), "script block was terminated by data");
    assert!(!html.contains('\u{2028}') && !html.contains('\u{2029}'), "line separators must be escaped inside JSON");
}

#[test]
fn trace_naming_helpers_are_stable_and_unique_per_shard() {
    assert_eq!(trace::file_stem(None), "trace");
    assert_eq!(trace::file_stem(Some(0)), "trace-shard-1");
    assert_eq!(trace::file_stem(Some(11)), "trace-shard-12");
    assert_eq!(trace::asset_prefix(None, 3), "f03_");
    assert_eq!(trace::asset_prefix(Some(1), 12), "s2_f12_");
    let a = trace::next_flow_ordinal();
    let b = trace::next_flow_ordinal();
    assert!(b > a, "ordinals only grow so asset names never repeat");
}

#[test]
fn trace_redaction_edge_cases() {
    // Credentials in the userinfo and fragments never reach the viewer.
    let url = trace::sanitize_url("https://user:pw@example.com/path?token=abc&page=2&email=a%40b.cz#frag");
    assert!(!url.contains("user") && !url.contains("pw@") && !url.contains("abc") && !url.contains("frag"), "{url}");
    assert!(url.contains("page=2"), "harmless query parameters stay readable: {url}");
    assert!(url.contains(trace::REDACTED), "{url}");
    assert!(trace::is_sensitive_header("Authorization") && trace::is_sensitive_header("X-CSRF-Token"));
    assert!(!trace::is_sensitive_header("Content-Type") && !trace::is_sensitive_header("Accept"));
    assert!(trace::is_sensitive_query_key("access_token") && trace::is_sensitive_query_key("EMAIL"));
    assert!(!trace::is_sensitive_query_key("page") && !trace::is_sensitive_query_key("limit"));
    assert!(trace::is_http_error(&json!({ "status": 404 })));
    assert!(trace::is_http_error(&json!({ "status": 200, "state": "failed" })));
    assert!(!trace::is_http_error(&json!({ "status": 204, "state": "completed" })));
    assert!(!trace::is_http_error(&json!({ "status": 399 })));
    assert!(trace::is_http_error(&json!({ "status": 400 })));
}
