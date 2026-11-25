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

/// First issue that is not ignored.
pub fn first_blocking(issues: &[Value]) -> Option<&Value> {
    issues.iter().find(|i| i["ignored"] != true)
}

/// Console line printed under a step.
pub fn console_line(issue: &Value) -> String {
    let kind = issue["kind"].as_str().unwrap_or("issue");
    let message = issue["message"].as_str().unwrap_or("");
    let count = issue["count"].as_u64().unwrap_or(1);
    let times = if count > 1 { format!(" (×{count})") } else { String::new() };
    let ignored = if issue["ignored"] == true { " [ignored]" } else { "" };
    format!("⚠ {kind}: {message}{times}{ignored}")
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

/// Device wall-clock date/time (`YYYY MM DD HH MM SS` from `date`) used to
/// resolve `MM-DD` years and filter `logcat -v threadtime` entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceClock {
    pub year: i32,
    pub epoch_ms: i64,
}

/// Parses `date '+%Y %m %d %H %M %S'` output from an Android device.
pub fn parse_device_datetime(raw: &str) -> Option<DeviceClock> {
    let mut parts = raw.split_whitespace();
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    let hour: u32 = parts.next()?.parse().ok()?;
    let min: u32 = parts.next()?.parse().ok()?;
    let sec: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let epoch_ms = civil_to_ms(year, month, day, hour, min, sec, 0)?;
    Some(DeviceClock { year, epoch_ms })
}

/// Parses the `MM-DD HH:MM:SS.mmm` timestamp of a `logcat -v threadtime` line
/// into epoch milliseconds using `ref_clock` to resolve the year across Dec/Jan
/// boundaries.
pub fn parse_logcat_threadtime_ms(line: &str, ref_clock: DeviceClock) -> Option<i64> {
    let mut parts = line.split_whitespace();
    let date_part = parts.next()?;
    let time_part = parts.next()?;
    let (mm, dd) = date_part.split_once('-')?;
    if mm.len() != 2 || dd.len() != 2 {
        return None;
    }
    let month: u32 = mm.parse().ok()?;
    let day: u32 = dd.parse().ok()?;

    let (hh_mm_ss, frac) = match time_part.split_once('.') {
        Some((t, f)) => (t, Some(f)),
        None => (time_part, None),
    };
    let mut hms = hh_mm_ss.split(':');
    let hour: u32 = hms.next()?.parse().ok()?;
    let min: u32 = hms.next()?.parse().ok()?;
    let sec: u32 = hms.next()?.parse().ok()?;
    if hms.next().is_some() {
        return None;
    }
    let ms = match frac {
        Some(f) if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) => {
            let mut val = 0u32;
            let mut weight = 100u32;
            for b in f.bytes().take(3) {
                val += u32::from(b - b'0') * weight;
                weight /= 10;
            }
            val
        }
        Some(_) => return None,
        None => 0,
    };

    // Pick the candidate year closest to `ref_clock` without placing the log
    // entry more than 1 day in the future (handles Dec 31 -> Jan 1 rollover and
    // rejects old entries from later months of the previous year).
    const ONE_DAY_MS: i64 = 86_400_000;
    [ref_clock.year, ref_clock.year - 1, ref_clock.year + 1]
        .into_iter()
        .filter_map(|y| civil_to_ms(y, month, day, hour, min, sec, ms))
        .filter(|&ts| ts <= ref_clock.epoch_ms.saturating_add(ONE_DAY_MS))
        .max()
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn max_day_in_month(year: i32, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if is_leap_year(year) { 29 } else { 28 }),
        _ => None,
    }
}

/// Days since 1970-01-01 (proleptic Gregorian calendar, Howard Hinnant).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_to_ms(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32, ms: u32) -> Option<i64> {
    let max_day = max_day_in_month(year, month)?;
    if !(1..=max_day).contains(&day) || hour > 23 || min > 59 || sec > 60 || ms > 999 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
    Some(secs * 1000 + i64::from(ms))
}

fn line_matches_package_process(line: &str, package: &str) -> bool {
    let needle = format!("Process: {package}");
    let Some(idx) = line.find(&needle) else {
        return false;
    };
    let after = &line[idx + needle.len()..];
    after.is_empty() || after.starts_with(',') || after.starts_with(char::is_whitespace)
}

fn block_names_other_package(lines: &[&str], idx: usize, package: &str) -> bool {
    let end = (idx + 4).min(lines.len());
    lines[idx..end].iter().any(|l| {
        (l.contains("Process: ") && !line_matches_package_process(l, package))
            || (l.contains(">>> ") && l.contains(" <<<") && !l.contains(&format!(">>> {package} <<<")))
    })
}

fn line_matches_package_pid(lines: &[&str], idx: usize, package: &str, pid_str: &str) -> bool {
    let line = lines[idx];
    let java_needle = format!("Process: {package}, PID: {pid_str}");
    if let Some(pos) = line.find(&java_needle) {
        let after = &line[pos + java_needle.len()..];
        if after.as_bytes().first().is_none_or(|b| !b.is_ascii_digit()) {
            return true;
        }
    }
    if line.contains(&format!("pid: {pid_str},")) && line.contains(&format!(">>> {package} <<<")) {
        return true;
    }
    log_pid(line) == Some(pid_str)
        && (line.contains("FATAL EXCEPTION") || line.contains("Fatal signal"))
        && !block_names_other_package(lines, idx, package)
}

/// Extracts the crash block for `package` from `adb logcat -b crash -d -v threadtime`
/// output (Java `FATAL EXCEPTION` or native `Fatal signal` + tombstone), without
/// timestamps, capped at 30 lines.
///
/// Requires either a known `pid` or a `since` time window `(min_epoch_ms, device_now)`
/// so crashes lingering in the ring buffer from before the flow started cannot
/// be misattributed.
pub fn android_crash_excerpt(
    logcat: &str,
    package: &str,
    pid: Option<i64>,
    since: Option<(i64, DeviceClock)>,
) -> Option<String> {
    if pid.is_none() && since.is_none() {
        return None;
    }
    let lines: Vec<&str> = logcat.lines().collect();
    let pid_text = pid.map(|p| p.to_string());
    let is_recent = |line: &str| match since {
        None => true,
        Some((min_ms, ref_clock)) => parse_logcat_threadtime_ms(line, ref_clock).is_some_and(|ts| ts >= min_ms),
    };
    let belongs = |idx: usize| match pid_text.as_deref() {
        Some(p) => line_matches_package_pid(&lines, idx, package, p),
        None => line_matches_package_process(lines[idx], package) || lines[idx].contains(&format!(">>> {package} <<<")),
    };
    let hit = (0..lines.len()).rev().find(|&i| belongs(i) && is_recent(lines[i]))?;
    // Start at the block header just before the match (`FATAL EXCEPTION`
    // precedes `Process:`; `*** ***` precedes the tombstone `pid:` line).
    let start = (hit.saturating_sub(3)..=hit)
        .rev()
        .find(|&i| {
            let l = lines[i];
            is_recent(l) && (l.contains("FATAL EXCEPTION") || l.contains("*** *** ***") || l.contains("Fatal signal"))
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
        let clock = parse_device_datetime("2026 11 21 15 40 02").unwrap();
        let after_old = parse_device_datetime("2026 11 21 15 40 01").unwrap();
        let excerpt = android_crash_excerpt(log, "dev.tapwing.sample_app", Some(4321), None).unwrap();
        assert_eq!(
            excerpt,
            "AndroidRuntime: FATAL EXCEPTION: main\nAndroidRuntime: Process: dev.tapwing.sample_app, PID: 4321\n\
             AndroidRuntime: java.lang.IllegalStateException: boom\n\
             AndroidRuntime: \tat dev.tapwing.sample_app.MainActivity.crash(MainActivity.kt:12)"
        );
        assert!(android_crash_excerpt(log, "dev.tapwing.missing", Some(1), None).is_none());
        // An older crash of the same package (another PID) is not this crash.
        assert!(android_crash_excerpt(log, "dev.tapwing.sample_app", Some(5555), None).is_none());
        // Prefix of PID or package name must not match.
        assert!(android_crash_excerpt(log, "dev.tapwing.sample_app", Some(43), None).is_none());
        assert!(android_crash_excerpt(log, "dev.tapwing", None, Some((after_old.epoch_ms, clock))).is_none());
        // Unknown PID without timestamp boundary refuses to match lingering logcat crashes.
        assert!(android_crash_excerpt(log, "dev.tapwing.sample_app", None, None).is_none());
        // Unknown PID with timestamp boundary matches only crashes at/after flow start.
        assert!(
            android_crash_excerpt(log, "dev.tapwing.sample_app", None, Some((after_old.epoch_ms, clock))).is_some()
        );
        assert!(android_crash_excerpt(log, "dev.tapwing.sample_app", None, Some((clock.epoch_ms, clock))).is_none());
        // Known PID is also rejected if older than the flow-start window.
        assert!(
            android_crash_excerpt(log, "dev.tapwing.sample_app", Some(4321), Some((clock.epoch_ms, clock))).is_none()
        );
    }

    #[test]
    fn android_native_crash_excerpt() {
        let log = "\
11-21 15:40:01.000  4321  4400 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 4400 (1.ui), pid 4321 (ibri.sample_app)
11-21 15:40:01.200  5000  5000 F DEBUG   : *** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
11-21 15:40:01.200  5000  5000 F DEBUG   : pid: 4321, tid: 4400, name: 1.ui  >>> dev.tapwing.sample_app <<<
11-21 15:40:01.200  5000  5000 F DEBUG   : signal 6 (SIGABRT), code -1 (SI_QUEUE), fault addr --------
";
        let excerpt = android_crash_excerpt(log, "dev.tapwing.sample_app", Some(4321), None).unwrap();
        assert!(excerpt.starts_with("DEBUG   : *** ***"), "{excerpt}");
        assert!(excerpt.contains("signal 6 (SIGABRT)"));
        // Tombstone with same PID but different package does not match.
        assert!(android_crash_excerpt(log, "dev.tapwing.other_app", Some(4321), None).is_none());
    }

    #[test]
    fn threadtime_timestamp_year_rollover_and_old_crashes() {
        // Flow started Dec 31 23:59:55, crashed Dec 31 23:59:58, diagnosed Jan 1 00:00:05.
        let jan1 = parse_device_datetime("2026 01 01 00 00 05").unwrap();
        let dec31_start = parse_device_datetime("2025 12 31 23 59 55").unwrap();
        let rollover_log = "\
12-31 23:59:58.500  777  777 E AndroidRuntime: FATAL EXCEPTION: main
12-31 23:59:58.500  777  777 E AndroidRuntime: Process: dev.tapwing.app, PID: 777
";
        assert!(
            android_crash_excerpt(rollover_log, "dev.tapwing.app", None, Some((dec31_start.epoch_ms, jan1))).is_some()
        );

        // Old crash from November still in logcat when device clock is in June must NOT resolve to future November.
        let june = parse_device_datetime("2026 06 01 12 00 00").unwrap();
        let june_start = parse_device_datetime("2026 06 01 11 59 50").unwrap();
        let stale_nov_log = "\
11-21 15:40:01.100  4321  4321 E AndroidRuntime: FATAL EXCEPTION: main
11-21 15:40:01.100  4321  4321 E AndroidRuntime: Process: dev.tapwing.app, PID: 4321
";
        assert!(
            android_crash_excerpt(stale_nov_log, "dev.tapwing.app", None, Some((june_start.epoch_ms, june))).is_none()
        );
        assert!(
            android_crash_excerpt(stale_nov_log, "dev.tapwing.app", Some(4321), Some((june_start.epoch_ms, june)))
                .is_none()
        );

        // Leap day parsing across non-leap year.
        let mar_non_leap = parse_device_datetime("2025 03 01 10 00 00").unwrap();
        let leap_ts = parse_logcat_threadtime_ms("02-29 12:00:00.000  1  1 E Tag: msg", mar_non_leap).unwrap();
        assert!(leap_ts < mar_non_leap.epoch_ms);
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
}
