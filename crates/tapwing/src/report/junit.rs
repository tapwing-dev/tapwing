//! JUnit XML test suite reporter matching Maestro 2.12.1
//! (`maestro.cli.report.JUnitTestSuiteReporter`).

use anyhow::Result;
use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};

use super::{FlowStatus, TestExecutionSummary, format_iso_local_seconds, format_junit_seconds};

const DEFAULT_SUITE_NAME: &str = "Test Suite";

/// Strips characters forbidden in XML 1.0 documents (`0x00..=0x08`, `0x0B`,
/// `0x0C`, `0x0E..=0x1F`, `0xFFFE`, `0xFFFF`) while preserving `\t` (`0x09`),
/// `\n` (`0x0A`), and `\r` (`0x0D`).
pub fn sanitize_xml_text(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            let u = c as u32;
            !matches!(u, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0xFFFE | 0xFFFF)
        })
        .collect()
}

/// Renders `summary` as a Maestro-compatible JUnit XML document.
pub fn render_junit(summary: &TestExecutionSummary) -> Result<String> {
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
    let suite_name = sanitize_xml_text(summary.suite_name.as_deref().unwrap_or(DEFAULT_SUITE_NAME));

    if summary.suites.is_empty() {
        writer.write_event(Event::Empty(BytesStart::new("testsuites")))?;
    } else {
        writer.write_event(Event::Start(BytesStart::new("testsuites")))?;
        for suite in &summary.suites {
            let mut ts = BytesStart::new("testsuite");
            ts.push_attribute(("name", suite_name.as_str()));
            if let Some(dev) = suite.device_id.as_deref().filter(|d| !d.is_empty()) {
                let dev_clean = sanitize_xml_text(dev);
                ts.push_attribute(("device", dev_clean.as_str()));
            }
            let tests_str = suite.flows.len().to_string();
            let failures_str = suite.failed_count().to_string();
            let time_str = format_junit_seconds(suite.duration_ms);
            let timestamp_str = format_iso_local_seconds(suite.start_epoch_ms, summary.utc_offset_secs);
            ts.push_attribute(("tests", tests_str.as_str()));
            ts.push_attribute(("failures", failures_str.as_str()));
            // Only on an interrupted run, so a normal run keeps Maestro's exact XML.
            let skipped_str = suite.not_run_count().to_string();
            if suite.not_run_count() > 0 {
                ts.push_attribute(("skipped", skipped_str.as_str()));
            }
            ts.push_attribute(("time", time_str.as_str()));
            ts.push_attribute(("timestamp", timestamp_str.as_str()));

            if suite.flows.is_empty() {
                writer.write_event(Event::Empty(ts))?;
                continue;
            }

            writer.write_event(Event::Start(ts))?;
            for flow in &suite.flows {
                let flow_name = sanitize_xml_text(&flow.name);
                let junit_id = flow
                    .properties
                    .iter()
                    .find(|(k, _)| k == "junitId")
                    .map(|(_, v)| sanitize_xml_text(v))
                    .unwrap_or_else(|| flow_name.clone());
                let junit_classname = flow
                    .properties
                    .iter()
                    .find(|(k, _)| k == "junitClassname")
                    .map(|(_, v)| sanitize_xml_text(v))
                    .unwrap_or_else(|| flow_name.clone());

                let mut tc = BytesStart::new("testcase");
                tc.push_attribute(("id", junit_id.as_str()));
                tc.push_attribute(("name", flow_name.as_str()));
                tc.push_attribute(("classname", junit_classname.as_str()));
                if let Some(file) = flow.file.as_deref().filter(|f| !f.is_empty()) {
                    let file_clean = sanitize_xml_text(file);
                    tc.push_attribute(("file", file_clean.as_str()));
                }
                let flow_time = format_junit_seconds(flow.duration_ms);
                let flow_ts = format_iso_local_seconds(flow.start_epoch_ms, summary.utc_offset_secs);
                tc.push_attribute(("time", flow_time.as_str()));
                tc.push_attribute(("timestamp", flow_ts.as_str()));
                tc.push_attribute(("status", flow.status.junit_str()));

                let has_tags = !flow.tags.is_empty();
                let mut props: Vec<(String, String)> = flow
                    .properties
                    .iter()
                    .filter(|(k, _)| k != "junitId" && k != "junitClassname" && !(has_tags && k == "tags"))
                    .map(|(k, v)| (sanitize_xml_text(k), sanitize_xml_text(v)))
                    .collect();
                if has_tags {
                    let joined = flow.tags.iter().map(|t| sanitize_xml_text(t)).collect::<Vec<_>>().join(", ");
                    props.push(("tags".to_string(), joined));
                }

                // F7 app health issues and F8 failure diagnostics go into
                // `<system-err>` (Maestro has neither, so clean flows keep
                // Maestro's exact XML).
                let issue_lines: Vec<String> = flow
                    .steps
                    .iter()
                    .flat_map(|st| {
                        st.app_issues.iter().map(move |i| format!("step {} `{}`: {i}", st.index + 1, st.description))
                    })
                    .map(|l| sanitize_xml_text(&l))
                    .collect();
                let mut system_err_sections = Vec::new();
                if let Some(diag) = flow.failure_diagnostics.as_ref().filter(|d| !d.is_empty()) {
                    system_err_sections.push(sanitize_xml_text(&crate::failure::format_system_err(diag)));
                }
                if !issue_lines.is_empty() {
                    system_err_sections.push(format!("App issues:\n{}", issue_lines.join("\n")));
                }
                // A flow the interrupt kept from starting is `<skipped>`, the
                // JUnit way to say "neither passed nor failed".
                let skipped_message = (flow.status == FlowStatus::NotRun).then(|| {
                    let cause = summary.interrupted.map(|sig| crate::interrupt::InterruptedError { signal: sig });
                    cause.map_or_else(|| "not run".to_string(), |c| format!("not run: {c}"))
                });
                let has_children = !props.is_empty()
                    || flow.failure_message.is_some()
                    || skipped_message.is_some()
                    || !system_err_sections.is_empty();
                if !has_children {
                    writer.write_event(Event::Empty(tc))?;
                } else {
                    writer.write_event(Event::Start(tc))?;
                    if !props.is_empty() {
                        writer.write_event(Event::Start(BytesStart::new("properties")))?;
                        for (k, v) in &props {
                            let mut prop = BytesStart::new("property");
                            prop.push_attribute(("name", k.as_str()));
                            prop.push_attribute(("value", v.as_str()));
                            writer.write_event(Event::Empty(prop))?;
                        }
                        writer.write_event(Event::End(BytesEnd::new("properties")))?;
                    }
                    if let Some(raw_failure) = &flow.failure_message {
                        let prefixed = match suite.shard_index {
                            Some(idx) => {
                                let p = format!("[shard {}] ", idx + 1);
                                if raw_failure.starts_with(&p) {
                                    raw_failure.clone()
                                } else {
                                    format!("{p}{raw_failure}")
                                }
                            }
                            None => raw_failure.clone(),
                        };
                        let clean_failure = sanitize_xml_text(&prefixed);
                        writer.create_element("failure").write_text_content(BytesText::new(&clean_failure))?;
                    }
                    if let Some(message) = &skipped_message {
                        let mut skipped = BytesStart::new("skipped");
                        skipped.push_attribute(("message", message.as_str()));
                        writer.write_event(Event::Empty(skipped))?;
                    }
                    if !system_err_sections.is_empty() {
                        let text = system_err_sections.join("\n\n");
                        writer.create_element("system-err").write_text_content(BytesText::new(&text))?;
                    }
                    writer.write_event(Event::End(BytesEnd::new("testcase")))?;
                }
            }
            writer.write_event(Event::End(BytesEnd::new("testsuite")))?;
        }
        writer.write_event(Event::End(BytesEnd::new("testsuites")))?;
    }

    let body = String::from_utf8(writer.into_inner())?;
    Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{body}\n"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use quick_xml::Reader;
    use quick_xml::events::Event;
    use serde_json::json;

    use super::*;
    use crate::report::{FlowRunRecord, FlowStatus, StepRecord, SuiteResult, TestExecutionSummary};

    // Fixed timestamp: 2025-11-26T14:30:05Z (1_764_167_405_000 ms)
    const TS_MS: u64 = 1_764_167_405_000;

    fn sample_flow(name: &str, status: FlowStatus, duration_ms: f64) -> FlowRunRecord {
        FlowRunRecord {
            name: name.into(),
            path: PathBuf::from(format!("{name}.yaml")),
            file: None,
            status,
            duration_ms,
            start_epoch_ms: TS_MS,
            failure_message: None,
            properties: Vec::new(),
            tags: Vec::new(),
            lane: "flutter".into(),
            flutter_steps: 1,
            native_steps: 0,
            blackbox: false,
            steps: Vec::new(),
            failure_diagnostics: None,
            raw_json: json!({}),
            trace: None,
        }
    }

    fn assert_valid_xml(xml: &str) {
        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        let mut depth = 0usize;
        loop {
            match reader.read_event_into(&mut buf).expect("valid XML event") {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    assert!(depth > 0, "unbalanced closing tag");
                    depth -= 1;
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        assert_eq!(depth, 0, "unclosed XML elements remain");
    }

    #[test]
    fn junit_xml_all_passed_matches_maestro_snapshot() {
        let mut flow_a = sample_flow("Flow A", FlowStatus::Success, 42125.0);
        flow_a.file = Some("workspace/flow_a.yaml".into());
        let flow_b = sample_flow("Flow B", FlowStatus::Warning, 1915947.0);

        let summary = TestExecutionSummary {
            passed: true,
            suite_name: None,
            input_mode: "auto".into(),
            total_ms: 1958072.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![SuiteResult {
                shard_index: None,
                device_id: Some("Pixel 6".into()),
                device_description: Some("Pixel 6".into()),
                platform: "android".into(),
                passed: true,
                setup_ms: 10.0,
                duration_ms: 1958072.0,
                start_epoch_ms: TS_MS,
                skipped_count: 0,
                error: None,
                flows: vec![flow_a, flow_b],
            }],
        };

        let xml = render_junit(&summary).unwrap();
        assert_valid_xml(&xml);
        assert_eq!(
            xml,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <testsuites>\n  \
               <testsuite name=\"Test Suite\" device=\"Pixel 6\" tests=\"2\" failures=\"0\" time=\"1958.072\" timestamp=\"2025-11-26T14:30:05\">\n    \
                 <testcase id=\"Flow A\" name=\"Flow A\" classname=\"Flow A\" file=\"workspace/flow_a.yaml\" time=\"42.125\" timestamp=\"2025-11-26T14:30:05\" status=\"SUCCESS\"/>\n    \
                 <testcase id=\"Flow B\" name=\"Flow B\" classname=\"Flow B\" time=\"1915.947\" timestamp=\"2025-11-26T14:30:05\" status=\"WARNING\"/>\n  \
               </testsuite>\n\
             </testsuites>\n"
        );
    }

    #[test]
    fn junit_xml_lists_app_issues_in_system_err() {
        let mut flow = sample_flow("Flow A", FlowStatus::Success, 10.0);
        flow.steps = vec![StepRecord {
            index: 2,
            command: "tapOn".into(),
            description: "tapOn \"Pay\"".into(),
            status: "passed".into(),
            duration_ms: 5.0,
            warning: None,
            error: None,
            app_issues: vec!["layoutOverflow: A RenderFlex overflowed by 12 pixels on the right. (×2)".into()],
        }];
        let summary = TestExecutionSummary {
            passed: true,
            suite_name: None,
            input_mode: "auto".into(),
            total_ms: 10.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![SuiteResult {
                shard_index: None,
                device_id: None,
                device_description: None,
                platform: "android".into(),
                passed: true,
                setup_ms: 1.0,
                duration_ms: 10.0,
                start_epoch_ms: TS_MS,
                skipped_count: 0,
                error: None,
                flows: vec![flow],
            }],
        };
        let xml = render_junit(&summary).unwrap();
        assert_valid_xml(&xml);
        assert!(xml.contains(
            "<system-err>App issues:\nstep 3 `tapOn &quot;Pay&quot;`: layoutOverflow: A RenderFlex overflowed by 12 pixels on the right. (×2)</system-err>"
        ), "{xml}");
    }

    #[test]
    fn junit_xml_with_failure_custom_suite_name_properties_and_tags() {
        let mut flow_a = sample_flow("Flow A", FlowStatus::Success, 6000.0);
        flow_a.properties = vec![
            ("junitId".into(), "custom-id-1".into()),
            ("junitClassname".into(), "com.example.CustomClass".into()),
            ("customProp1".into(), "value1".into()),
            ("customProp2".into(), "value2".into()),
        ];
        flow_a.tags = vec!["smoke".into(), "regression".into()];

        let mut flow_b = sample_flow("Flow B", FlowStatus::Error, 2500.0);
        flow_b.failure_message = Some("Failure message".into());

        let summary = TestExecutionSummary {
            passed: false,
            suite_name: Some("Custom test suite name".into()),
            input_mode: "auto".into(),
            total_ms: 8500.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![SuiteResult {
                shard_index: None,
                device_id: None,
                device_description: None,
                platform: "ios".into(),
                passed: false,
                setup_ms: 5.0,
                duration_ms: 8500.0,
                start_epoch_ms: TS_MS,
                skipped_count: 0,
                error: None,
                flows: vec![flow_a, flow_b],
            }],
        };

        let xml = render_junit(&summary).unwrap();
        assert_valid_xml(&xml);
        assert_eq!(
            xml,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <testsuites>\n  \
               <testsuite name=\"Custom test suite name\" tests=\"2\" failures=\"1\" time=\"8.5\" timestamp=\"2025-11-26T14:30:05\">\n    \
                 <testcase id=\"custom-id-1\" name=\"Flow A\" classname=\"com.example.CustomClass\" time=\"6.0\" timestamp=\"2025-11-26T14:30:05\" status=\"SUCCESS\">\n      \
                   <properties>\n        \
                     <property name=\"customProp1\" value=\"value1\"/>\n        \
                     <property name=\"customProp2\" value=\"value2\"/>\n        \
                     <property name=\"tags\" value=\"smoke, regression\"/>\n      \
                   </properties>\n    \
                 </testcase>\n    \
                 <testcase id=\"Flow B\" name=\"Flow B\" classname=\"Flow B\" time=\"2.5\" timestamp=\"2025-11-26T14:30:05\" status=\"ERROR\">\n      \
                   <failure>Failure message</failure>\n    \
                 </testcase>\n  \
               </testsuite>\n\
             </testsuites>\n"
        );
    }

    #[test]
    fn junit_xml_multi_shard_prefixes_failure_and_strips_illegal_control_chars() {
        let flow_s1 = sample_flow("Flow 1", FlowStatus::Success, 1000.0);
        let mut flow_s2 = sample_flow("Flow 2\u{0007}", FlowStatus::Error, 2000.0);
        flow_s2.failure_message = Some("Element <\"Login\" & 'Btn'> not found\u{0000}\u{001b}".into());

        let summary = TestExecutionSummary {
            passed: false,
            suite_name: Some("Sharded Suite".into()),
            input_mode: "auto".into(),
            total_ms: 2000.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![
                SuiteResult {
                    shard_index: Some(0),
                    device_id: Some("emulator-5554".into()),
                    device_description: Some("emulator-5554".into()),
                    platform: "android".into(),
                    passed: true,
                    setup_ms: 1.0,
                    duration_ms: 1000.0,
                    start_epoch_ms: TS_MS,
                    skipped_count: 0,
                    error: None,
                    flows: vec![flow_s1],
                },
                SuiteResult {
                    shard_index: Some(1),
                    device_id: Some("emulator-5556".into()),
                    device_description: Some("emulator-5556".into()),
                    platform: "android".into(),
                    passed: false,
                    setup_ms: 1.0,
                    duration_ms: 2000.0,
                    start_epoch_ms: TS_MS,
                    skipped_count: 0,
                    error: None,
                    flows: vec![flow_s2],
                },
            ],
        };

        let xml = render_junit(&summary).unwrap();
        assert_valid_xml(&xml);
        assert!(xml.contains("device=\"emulator-5554\""));
        assert!(xml.contains("device=\"emulator-5556\""));
        assert!(xml.contains("name=\"Flow 2\""));
        assert!(xml.contains("<failure>[shard 2] Element &lt;"));
        assert!(xml.contains("&amp;"));
        assert!(!xml.contains("Element <\"Login\""));
        assert!(!xml.contains('\u{0007}') && !xml.contains('\u{0000}') && !xml.contains('\u{001b}'));
    }

    #[test]
    fn junit_xml_strips_xml10_noncharacters_and_deduplicates_tags_property() {
        let mut flow = sample_flow("Flow\u{FFFE}\u{FFFF}", FlowStatus::Success, 1000.0);
        flow.properties = vec![("tags".into(), "stale".into()), ("team".into(), "mobile".into())];
        flow.tags = vec!["smoke".into(), "e2e".into()];

        let summary = TestExecutionSummary {
            passed: true,
            suite_name: None,
            input_mode: "auto".into(),
            total_ms: 1000.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![SuiteResult {
                shard_index: None,
                device_id: None,
                device_description: None,
                platform: "android".into(),
                passed: true,
                setup_ms: 0.0,
                duration_ms: 1000.0,
                start_epoch_ms: TS_MS,
                skipped_count: 0,
                error: None,
                flows: vec![flow],
            }],
        };

        let xml = render_junit(&summary).unwrap();
        assert_valid_xml(&xml);
        assert!(!xml.contains('\u{FFFE}') && !xml.contains('\u{FFFF}'));
        assert!(xml.contains("name=\"Flow\""));
        assert_eq!(xml.matches("name=\"tags\"").count(), 1, "{xml}");
        assert!(xml.contains("<property name=\"tags\" value=\"smoke, e2e\"/>"), "{xml}");
    }
}
