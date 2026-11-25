//! F7 app health diagnostics: what the app reported while a flow ran (Flutter
//! errors, uncaught Dart errors, layout overflows, crashes), even when every
//! assertion passed.
//!
//! The in-app driver attaches issue records to RPC responses (see
//! `packages/tapwing_driver/lib/src/diagnostics/app_issues.dart`). This module
//! holds the host-side pieces: policy, ignore rules, per-step attribution
//! batches, console lines, report summaries, and crash excerpts.

use std::collections::BTreeMap;

use clap::ValueEnum;
use fancy_regex::Regex;
use serde_json::{Value, json};

/// `--app-errors`: what to do with captured issues.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum AppErrorsPolicy {
    /// Print and report issues; pass/fail stays exactly as in Maestro.
    #[default]
    Report,
    /// Fail the flow at the first step that produced a non-ignored issue.
    Fail,
    /// Do not capture (the driver uninstalls its hooks).
    Off,
}

impl AppErrorsPolicy {
    /// Whether the driver should capture and deliver issues.
    pub fn enabled(self) -> bool {
        self != AppErrorsPolicy::Off
    }

    /// Parses a `config.yaml` `tapwing.appErrors` value.
    pub fn parse(raw: &str) -> Option<Self> {
        <Self as ValueEnum>::from_str(raw, true).ok()
    }
}

/// App health settings for one run.
#[derive(Clone, Debug, Default)]
pub struct AppHealthOpts {
    pub policy: AppErrorsPolicy,
    /// `--app-errors-ignore`: issues whose message matches are reported as
    /// ignored and never fail a flow.
    pub ignore: Vec<Regex>,
    /// `--app-errors-screenshot`: screenshot the first occurrence of each issue.
    pub screenshot: bool,
}

impl AppHealthOpts {
    /// Compiles ignore patterns; errors name the offending pattern.
    pub fn new(policy: AppErrorsPolicy, ignore: &[String], screenshot: bool) -> anyhow::Result<Self> {
        let ignore = ignore
            .iter()
            .map(|p| Regex::new(p).map_err(|e| anyhow::anyhow!("invalid --app-errors-ignore pattern `{p}`: {e}")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self { policy, ignore, screenshot })
    }

    fn is_ignored(&self, issue: &Value) -> bool {
        if issue["kind"] == "appCrash" {
            return false;
        }
        let message = issue["message"].as_str().unwrap_or("");
        let summary = issue["summary"].as_str().unwrap_or("");
        self.ignore.iter().any(|re| re.is_match(message).unwrap_or(false) || re.is_match(summary).unwrap_or(false))
    }

    /// Marks ignored issues (`"ignored": true`) in place.
    pub fn mark_ignored(&self, issues: &mut [Value]) {
        for issue in issues.iter_mut() {
            if self.is_ignored(issue) {
                issue["ignored"] = Value::Bool(true);
            }
        }
    }
}

/// Issues delivered while one step ran, split by the step that caused them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IssueBatch {
    /// Caused by the step that just ran.
    pub current: Vec<Value>,
    /// Caused by the previous step (they fired after its last response).
    pub late: Vec<Value>,
    /// Undelivered issues evicted from the driver's ring buffer since the
    /// previous batch.
    pub dropped: u64,
}

impl IssueBatch {
    pub fn is_empty(&self) -> bool {
        self.current.is_empty() && self.late.is_empty() && self.dropped == 0
    }
}

/// Merges `incoming` into `list`, replacing records with the same driver
/// `id` (a repeat carries an updated `count`) and keeping first-seen order.
pub fn merge_issues(list: &mut Vec<Value>, incoming: Vec<Value>) {
    for issue in incoming {
        match issue.get("id").and_then(Value::as_u64).and_then(|id| list.iter().position(|v| v["id"] == id)) {
            Some(pos) => list[pos] = issue,
            None => list.push(issue),
        }
    }
}

/// Returns `true` when `issue` is a non-blocking setup warning (such as
/// Flutter's `Zone mismatch.` warning) that should be surfaced visibly in
/// console/reports without failing `--app-errors fail`.
pub fn is_setup_warning(issue: &Value) -> bool {
    issue["kind"] == "setupWarning" || is_zone_mismatch_issue(issue)
}

fn is_zone_mismatch_issue(issue: &Value) -> bool {
    let message = issue["message"].as_str().unwrap_or("");
    let summary = issue["summary"].as_str().unwrap_or("");
    message.starts_with("Zone mismatch.")
        && (issue["kind"] == "setupWarning"
            || summary.contains("The Flutter bindings were initialized in a different zone than is now being used.")
            || message.contains("The Flutter bindings were initialized in a different zone than is now being used."))
}

fn setup_warning_hint(issue: &Value) -> Option<&'static str> {
    if is_zone_mismatch_issue(issue) {
        Some("call TapwingBinding.ensureInitialized() inside your zone/appRunner; see docs/setup.md")
    } else {
        None
    }
}

/// First issue that is not ignored and not a non-blocking setup warning.
pub fn first_blocking(issues: &[Value]) -> Option<&Value> {
    issues.iter().find(|i| i["ignored"] != true && !is_setup_warning(i))
}

/// Console line printed under a step.
pub fn console_line(issue: &Value) -> String {
    let kind = issue["kind"].as_str().unwrap_or("issue");
    let message = issue["message"].as_str().unwrap_or("");
    let hint = setup_warning_hint(issue).map(|h| format!(" ({h})")).unwrap_or_default();
    let count = issue["count"].as_u64().unwrap_or(1);
    let times = if count > 1 { format!(" (×{count})") } else { String::new() };
    let ignored = if issue["ignored"] == true { " [ignored]" } else { "" };
    format!("⚠ {kind}: {message}{hint}{times}{ignored}")
}

/// Error failing a step under `--app-errors fail`. Never downgraded by
/// `optional` and never retried as a transport error.
#[derive(Debug)]
pub struct AppHealthError(pub String);

impl std::fmt::Display for AppHealthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AppHealthError {}

/// Failure message for `--app-errors fail`.
pub fn failure_message(issue: &Value, step_description: &str) -> String {
    format!(
        "App issue during `{step_description}`: {}: {} (--app-errors fail)",
        issue["kind"].as_str().unwrap_or("issue"),
        issue["message"].as_str().unwrap_or("")
    )
}

/// Console warning when the driver ring buffer evicted undelivered issues.
pub fn dropped_line(dropped: u64) -> String {
    format!("⚠ {dropped} app issue(s) dropped (driver buffer limit reached)")
}

/// Flow-level summary over all steps' `appIssues`, or `None` when the flow
/// ran clean.
pub fn summarize(steps: &[Value]) -> Option<Value> {
    summarize_with_dropped(steps, 0)
}

/// Flow-level summary over all steps' `appIssues` plus any driver-dropped count.
pub fn summarize_with_dropped(steps: &[Value], dropped: u64) -> Option<Value> {
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    let (mut total, mut ignored) = (0u64, 0u64);
    for issue in steps.iter().filter_map(|s| s["appIssues"].as_array()).flatten() {
        total += 1;
        if issue["ignored"] == true {
            ignored += 1;
        }
        *by_kind.entry(issue["kind"].as_str().unwrap_or("issue").to_string()).or_default() += 1;
    }
    if total == 0 && dropped == 0 {
        return None;
    }
    let mut out = json!({ "total": total, "ignored": ignored, "byKind": by_kind });
    if dropped > 0 {
        out["dropped"] = Value::from(dropped);
    }
    Some(out)
}

/// Short lines describing a step's issues for JUnit `<system-err>` and HTML.
pub fn issue_lines(step_description: &str, issues: &[Value]) -> Vec<String> {
    issues.iter().map(|i| format!("{step_description}: {}", console_line(i).trim_start_matches("⚠ "))).collect()
}

/// Host-detected crash record (`kind: appCrash`).
pub fn crash_issue(message: &str, excerpt: Option<&str>) -> Value {
    let mut issue = json!({ "kind": "appCrash", "message": message, "count": 1 });
    issue["summary"] = Value::from(match excerpt {
        Some(e) => format!("{message}\n{e}"),
        None => message.to_string(),
    });
    issue
}

/// The most telling line of a crash excerpt (the exception or signal, not
/// the block header), for one-line failure messages.
pub fn crash_headline(crash: &Value) -> Option<String> {
    let summary = crash["summary"].as_str()?;
    summary
        .lines()
        .skip(1)
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.contains("FATAL EXCEPTION")
                && !l.contains("Process: ")
                && !l.contains("*** *** ***")
                && !l.contains(">>> ")
        })
        .map(str::to_string)
}

/// Extracts the crash block for `pid` from `adb logcat -b crash -d` output
/// (Java `FATAL EXCEPTION` or native `Fatal signal` + tombstone), without
/// timestamps, capped at 30 lines.
pub fn android_crash_excerpt(logcat: &str, package: &str, pid: Option<i64>) -> Option<String> {
    let lines: Vec<&str> = logcat.lines().collect();
    let pid_text = pid.map(|p| p.to_string());
    // With a known PID, only that process's crash counts (an older crash of
    // the same package stays in the crash buffer).
    let belongs = |line: &str| match pid_text.as_deref() {
        Some(p) => {
            line.contains(&format!("Process: {package}, PID: {p}"))
                || line.contains(&format!("pid: {p},"))
                || (log_pid(line) == Some(p) && (line.contains("FATAL EXCEPTION") || line.contains("Fatal signal")))
        }
        None => line.contains(&format!("Process: {package}")) || line.contains(&format!(">>> {package} <<<")),
    };
    let hit = lines.iter().rposition(|l| belongs(l))?;
    // Start at the block header just before the match (`FATAL EXCEPTION`
    // precedes `Process:`; `*** ***` precedes the tombstone `pid:` line).
    let start = (hit.saturating_sub(3)..=hit)
        .rev()
        .find(|&i| {
            let l = lines[i];
            l.contains("FATAL EXCEPTION") || l.contains("*** *** ***") || l.contains("Fatal signal")
        })
        .unwrap_or(hit);
    let excerpt: Vec<String> = lines[start..].iter().take(30).map(|l| strip_logcat_prefix(l).to_string()).collect();
    Some(excerpt.join("\n"))
}

/// The pid column of a `threadtime` logcat line
/// (`11-21 15:33:24.242  1234  1250 E AndroidRuntime: ...`).
fn log_pid(line: &str) -> Option<&str> {
    let mut parts = line.split_whitespace();
    parts.next()?; // date
    parts.next()?; // time
    parts.next()
}

fn strip_logcat_prefix(line: &str) -> &str {
    // `threadtime`: date time pid tid level tag: message
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start();
        match rest.find(char::is_whitespace) {
            Some(i) => rest = &rest[i..],
            None => return line,
        }
    }
    rest.trim_start()
}

/// Summarizes an iOS `.ips` crash report: header line + JSON body with
/// `exception`, `termination`, and the crashed thread's top frames.
pub fn ios_crash_excerpt(ips: &str) -> Option<String> {
    let (header, body) = ips.split_once('\n')?;
    let header: Value = serde_json::from_str(header).ok()?;
    let body: Value = serde_json::from_str(body).ok()?;
    let mut out = vec![format!(
        "{} {} ({})",
        header["app_name"].as_str().unwrap_or("app"),
        header["timestamp"].as_str().unwrap_or(""),
        header["bug_type"].as_str().unwrap_or("crash")
    )];
    if let Some(ex) = body.get("exception") {
        out.push(format!("Exception: {} {}", ex["type"].as_str().unwrap_or(""), ex["signal"].as_str().unwrap_or("")));
    }
    if let Some(reason) = body.pointer("/termination/indicator").and_then(Value::as_str) {
        out.push(format!("Termination: {reason}"));
    }
    if let Some(threads) = body["threads"].as_array()
        && let Some(crashed) = threads.iter().find(|t| t["triggered"] == true)
    {
        let images = body["usedImages"].as_array();
        for frame in crashed["frames"].as_array().into_iter().flatten().take(10) {
            let image = frame["imageIndex"]
                .as_u64()
                .and_then(|i| images.and_then(|imgs| imgs.get(i as usize)))
                .and_then(|img| img["name"].as_str())
                .unwrap_or("?");
            out.push(format!("  {image} {}", frame["symbol"].as_str().unwrap_or("")));
        }
    }
    Some(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(id: u64, kind: &str, message: &str, count: u64) -> Value {
        json!({ "id": id, "kind": kind, "message": message, "count": count })
    }

    #[test]
    fn merge_replaces_repeats_by_id() {
        let mut list = vec![issue(1, "imageLoadError", "404", 1)];
        merge_issues(&mut list, vec![issue(1, "imageLoadError", "404", 3), issue(2, "uncaughtError", "x", 1)]);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["count"], 3);
    }

    #[test]
    fn ignore_rules_never_ignore_crashes() {
        let opts = AppHealthOpts::new(AppErrorsPolicy::Fail, &["overflowed".into(), ".*".into()], false).unwrap();
        let mut issues = vec![
            issue(1, "layoutOverflow", "A RenderFlex overflowed by 12 pixels on the right.", 1),
            crash_issue("App crashed", None),
        ];
        opts.mark_ignored(&mut issues);
        assert_eq!(issues[0]["ignored"], true);
        assert!(issues[1].get("ignored").is_none());
        assert_eq!(first_blocking(&issues).unwrap()["kind"], "appCrash");
        assert!(AppHealthOpts::new(AppErrorsPolicy::Fail, &["(".into()], false).is_err());
    }

    #[test]
    fn console_and_failure_lines() {
        let mut i = issue(1, "layoutOverflow", "A RenderFlex overflowed by 12 pixels on the right.", 3);
        assert_eq!(console_line(&i), "⚠ layoutOverflow: A RenderFlex overflowed by 12 pixels on the right. (×3)");
        i["ignored"] = Value::Bool(true);
        assert!(console_line(&i).ends_with("[ignored]"));
        assert_eq!(
            failure_message(&issue(2, "uncaughtError", "StateError: boom", 1), "tapOn \"Pay\""),
            "App issue during `tapOn \"Pay\"`: uncaughtError: StateError: boom (--app-errors fail)"
        );
    }

    #[test]
    fn summary_counts_kinds_and_ignored() {
        let steps = vec![
            json!({ "appIssues": [issue(1, "layoutOverflow", "a", 1), json!({"id": 2, "kind": "layoutOverflow", "message": "b", "ignored": true})] }),
            json!({}),
            json!({ "appIssues": [issue(3, "uncaughtError", "c", 2)] }),
        ];
        assert_eq!(
            summarize(&steps).unwrap(),
            json!({ "total": 3, "ignored": 1, "byKind": { "layoutOverflow": 2, "uncaughtError": 1 } })
        );
        assert!(summarize(&[json!({})]).is_none());
    }

    #[test]
    fn policy_parsing() {
        assert_eq!(AppErrorsPolicy::parse("FAIL"), Some(AppErrorsPolicy::Fail));
        assert_eq!(AppErrorsPolicy::parse("nope"), None);
        assert!(!AppErrorsPolicy::Off.enabled());
    }

    #[test]
    fn android_java_crash_excerpt() {
        let log = "\
--------- beginning of crash
11-21 15:00:00.000  999  999 E AndroidRuntime: FATAL EXCEPTION: main
11-21 15:00:00.000  999  999 E AndroidRuntime: Process: other.app, PID: 999
11-21 15:40:01.100  4321  4321 E AndroidRuntime: FATAL EXCEPTION: main
11-21 15:40:01.100  4321  4321 E AndroidRuntime: Process: dev.tapwing.sample_app, PID: 4321
11-21 15:40:01.100  4321  4321 E AndroidRuntime: java.lang.IllegalStateException: boom
11-21 15:40:01.100  4321  4321 E AndroidRuntime: \tat dev.tapwing.sample_app.MainActivity.crash(MainActivity.kt:12)
";
        let excerpt = android_crash_excerpt(log, "dev.tapwing.sample_app", Some(4321)).unwrap();
        assert_eq!(
            excerpt,
            "AndroidRuntime: FATAL EXCEPTION: main\nAndroidRuntime: Process: dev.tapwing.sample_app, PID: 4321\n\
             AndroidRuntime: java.lang.IllegalStateException: boom\n\
             AndroidRuntime: \tat dev.tapwing.sample_app.MainActivity.crash(MainActivity.kt:12)"
        );
        assert!(android_crash_excerpt(log, "dev.tapwing.missing", Some(1)).is_none());
        // An older crash of the same package (another PID) is not this crash.
        assert!(android_crash_excerpt(log, "dev.tapwing.sample_app", Some(5555)).is_none());
        assert!(android_crash_excerpt(log, "dev.tapwing.sample_app", None).is_some());
    }

    #[test]
    fn android_native_crash_excerpt() {
        let log = "\
11-21 15:40:01.000  4321  4400 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4400 (1.ui), pid 4321 (ibri.sample_app)
11-21 15:40:01.200  5000  5000 F DEBUG   : *** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
11-21 15:40:01.200  5000  5000 F DEBUG   : pid: 4321, tid: 4400, name: 1.ui  >>> dev.tapwing.sample_app <<<
11-21 15:40:01.200  5000  5000 F DEBUG   : signal 6 (SIGABRT), code -1 (SI_QUEUE), fault addr --------
";
        let excerpt = android_crash_excerpt(log, "dev.tapwing.sample_app", Some(4321)).unwrap();
        assert!(excerpt.starts_with("DEBUG   : *** ***"), "{excerpt}");
        assert!(excerpt.contains("signal 6 (SIGABRT)"));
    }

    #[test]
    fn ios_ips_excerpt() {
        let ips = concat!(
            r#"{"app_name":"Runner","timestamp":"2026-11-21 15:40:01.00 +0100","bug_type":"309"}"#,
            "\n",
            r#"{"exception":{"type":"EXC_CRASH","signal":"SIGABRT"},"termination":{"indicator":"Abort trap: 6"},"usedImages":[{"name":"Flutter"}],"threads":[{"frames":[]},{"triggered":true,"frames":[{"imageIndex":0,"symbol":"abort"}]}]}"#
        );
        assert_eq!(
            ios_crash_excerpt(ips).unwrap(),
            "Runner 2026-11-21 15:40:01.00 +0100 (309)\nException: EXC_CRASH SIGABRT\nTermination: Abort trap: 6\n  Flutter abort"
        );
    }

    #[test]
    fn setup_warning_is_non_blocking_and_includes_setup_hint() {
        let zone_warning = json!({
            "id": 1,
            "kind": "setupWarning",
            "message": "Zone mismatch.",
            "summary": "Zone mismatch.\nThe Flutter bindings were initialized in a different zone than is now being used.",
            "count": 1
        });
        assert!(is_setup_warning(&zone_warning));
        assert!(first_blocking(std::slice::from_ref(&zone_warning)).is_none());
        let line = console_line(&zone_warning);
        assert!(line.starts_with("⚠ setupWarning: Zone mismatch."));
        assert!(line.contains("docs/setup.md"), "{line}");

        // Legacy driver reporting Zone mismatch as frameworkAssertion is also non-blocking.
        let legacy_zone = json!({
            "id": 2,
            "kind": "frameworkAssertion",
            "message": "Zone mismatch.",
            "summary": "Zone mismatch.\nThe Flutter bindings were initialized in a different zone than is now being used.",
            "count": 1
        });
        assert!(is_setup_warning(&legacy_zone));
        assert!(first_blocking(std::slice::from_ref(&legacy_zone)).is_none());
        assert!(console_line(&legacy_zone).contains("docs/setup.md"));

        // Unrelated frameworkAssertion with a similar title is still blocking.
        let other = issue(3, "frameworkAssertion", "Zone mismatch.", 1);
        assert!(!is_setup_warning(&other));
        assert!(first_blocking(&[zone_warning, other]).is_some());
    }
}
