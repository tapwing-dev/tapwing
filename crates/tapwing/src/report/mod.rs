//! Test suite reporting: JUnit XML (`--format junit`), self-contained HTML
//! (`--format html` / `--format html-detailed`), and backward-compatible JSON
//! (`--report`).

pub mod html;
pub mod junit;

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

/// Supported `--format` values matching Maestro 2.12.1 (`ReportFormat.kt`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ReportFormat {
    #[default]
    Noop,
    Junit,
    Html,
    #[value(name = "html-detailed", alias = "html_detailed", alias = "htmldetailed")]
    HtmlDetailed,
}

impl ReportFormat {
    /// Default output filename when `--output` is omitted (`ReportFormat.fileExtension`).
    pub fn default_output_path(self) -> Option<PathBuf> {
        match self {
            Self::Noop => None,
            Self::Junit => Some(PathBuf::from("report.xml")),
            Self::Html | Self::HtmlDetailed => Some(PathBuf::from("report.html")),
        }
    }
}

impl FromStr for ReportFormat {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "noop" => Ok(Self::Noop),
            "junit" => Ok(Self::Junit),
            "html" => Ok(Self::Html),
            "html-detailed" | "html_detailed" | "htmldetailed" => Ok(Self::HtmlDetailed),
            other => Err(anyhow!("unknown report format `{other}` (expected junit, html, html-detailed, or noop)")),
        }
    }
}

/// Resolves the target path for `--format` and `--output`.
///
/// Emits a warning to stderr and returns `None` if `--output` is passed with
/// `--format noop`.
pub fn resolve_report_output(format: ReportFormat, output: Option<&Path>) -> Option<PathBuf> {
    if format == ReportFormat::Noop {
        if output.is_some() {
            eprintln!(
                "warning: --output is ignored when --format is noop (pass --format junit, html, or html-detailed)"
            );
        }
        return None;
    }
    output.map(Path::to_path_buf).or_else(|| format.default_output_path())
}

/// Flow execution status matching Maestro's `FlowStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowStatus {
    Success,
    Error,
    Warning,
    /// The flow was running when `SIGINT`/`SIGTERM` arrived (Maestro's `STOPPED`).
    Interrupted,
    /// The flow never started because the run was interrupted first
    /// (Maestro's `CANCELED`). Neither passed nor failed.
    NotRun,
}

impl FlowStatus {
    pub fn junit_str(self) -> &'static str {
        match self {
            Self::Success => "SUCCESS",
            Self::Error => "ERROR",
            Self::Warning => "WARNING",
            Self::Interrupted => "STOPPED",
            Self::NotRun => "CANCELED",
        }
    }
}

/// Summary of one command step within a flow.
#[derive(Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub index: usize,
    pub command: String,
    pub description: String,
    pub status: String,
    pub duration_ms: f64,
    pub warning: Option<String>,
    pub error: Option<String>,
    /// F7 app health issues attributed to this step, one line each
    /// (`layoutOverflow: A RenderFlex overflowed by … (×2)`).
    pub app_issues: Vec<String>,
}

/// Canonical execution record for a single flow run.
#[derive(Clone, Debug)]
pub struct FlowRunRecord {
    pub name: String,
    pub path: PathBuf,
    pub file: Option<String>,
    pub status: FlowStatus,
    pub duration_ms: f64,
    pub start_epoch_ms: u64,
    pub failure_message: Option<String>,
    pub properties: Vec<(String, String)>,
    pub tags: Vec<String>,
    pub lane: String,
    pub flutter_steps: u32,
    pub native_steps: u32,
    pub blackbox: bool,
    pub steps: Vec<StepRecord>,
    /// F8 rich failure diagnostics captured at the failing leaf step, if any.
    pub failure_diagnostics: Option<crate::failure::FailureDiagnostics>,
    pub raw_json: Value,
    /// F8b trace (`--trace` only), see [`crate::orchestra::FlowResult::trace`].
    pub trace: Option<Value>,
}

impl FlowRunRecord {
    pub fn passed(&self) -> bool {
        matches!(self.status, FlowStatus::Success | FlowStatus::Warning)
    }

    /// Failed, errored with its shard, or interrupted while running.
    pub fn failed(&self) -> bool {
        matches!(self.status, FlowStatus::Error | FlowStatus::Interrupted)
    }

    /// Never started because the run was interrupted first.
    pub fn not_run(&self) -> bool {
        self.status == FlowStatus::NotRun
    }
}

/// Result of running a flow suite on one shard / device.
#[derive(Clone, Debug)]
pub struct SuiteResult {
    pub shard_index: Option<usize>,
    pub device_id: Option<String>,
    pub device_description: Option<String>,
    pub platform: String,
    pub passed: bool,
    pub setup_ms: f64,
    pub duration_ms: f64,
    pub start_epoch_ms: u64,
    /// Number of sequential flows skipped after a failure with `continueOnFailure: false`.
    pub skipped_count: usize,
    /// Why the shard could not run all of its flows (device or driver setup
    /// error, or a panic). The flows it did not finish are recorded in `flows`
    /// as errored with this message.
    pub error: Option<String>,
    pub flows: Vec<FlowRunRecord>,
}

impl SuiteResult {
    pub fn passed_count(&self) -> usize {
        self.flows.iter().filter(|f| f.passed()).count()
    }

    pub fn failed_count(&self) -> usize {
        self.flows.iter().filter(|f| f.failed()).count()
    }

    /// Flows recorded but never started because the run was interrupted.
    pub fn not_run_count(&self) -> usize {
        self.flows.iter().filter(|f| f.not_run()).count()
    }

    pub fn total_executed(&self) -> usize {
        self.flows.len() - self.not_run_count()
    }
}

/// Aggregated summary across all shards in a `tapwing test` invocation.
#[derive(Clone, Debug)]
pub struct TestExecutionSummary {
    pub passed: bool,
    pub suite_name: Option<String>,
    pub input_mode: String,
    pub total_ms: f64,
    pub utc_offset_secs: i32,
    /// The signal (`SIGINT` = 2, `SIGTERM` = 15) that interrupted the run, if
    /// any. The report is then partial: see [`FlowStatus::Interrupted`] and
    /// [`FlowStatus::NotRun`].
    pub interrupted: Option<i32>,
    pub suites: Vec<SuiteResult>,
}

impl TestExecutionSummary {
    pub fn passed_count(&self) -> usize {
        self.suites.iter().map(SuiteResult::passed_count).sum()
    }

    pub fn failed_count(&self) -> usize {
        self.suites.iter().map(SuiteResult::failed_count).sum()
    }

    pub fn skipped_count(&self) -> usize {
        self.suites.iter().map(|s| s.skipped_count).sum()
    }

    pub fn not_run_count(&self) -> usize {
        self.suites.iter().map(SuiteResult::not_run_count).sum()
    }

    pub fn total_executed(&self) -> usize {
        self.suites.iter().map(SuiteResult::total_executed).sum()
    }

    /// One message per shard that errored, prefixed with `[shard N] ` on sharded runs.
    pub fn shard_errors(&self) -> Vec<String> {
        self.suites
            .iter()
            .enumerate()
            .filter_map(|(idx, suite)| {
                let error = suite.error.as_deref()?;
                Some(match suite.shard_index {
                    Some(shard) => format!("[shard {}] {error}", shard + 1),
                    None if self.suites.len() > 1 => format!("[shard {}] {error}", idx + 1),
                    None => error.to_string(),
                })
            })
            .collect()
    }
}

/// Parses a POSIX `date +%z` offset (`+0100`, `-0500`, `+0530`) into seconds east of UTC.
pub fn parse_utc_offset_hhmm(raw: &str) -> Option<i32> {
    let s = raw.trim();
    let (sign, digits) = match s.as_bytes() {
        [b'+', rest @ ..] => (1i32, rest),
        [b'-', rest @ ..] => (-1i32, rest),
        _ => return None,
    };
    if digits.len() != 4 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digits_str = std::str::from_utf8(digits).ok()?;
    let hours: i32 = digits_str.get(0..2)?.parse().ok()?;
    let mins: i32 = digits_str.get(2..4)?.parse().ok()?;
    if hours > 23 || mins > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + mins * 60))
}

/// Returns the host's current local UTC offset in seconds (e.g. `+3600` for CET),
/// falling back to `0` (UTC) if unavailable. Called at most once per report write.
pub fn local_utc_offset_secs() -> i32 {
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_utc_offset_hhmm(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(0)
}

/// Formats `epoch_ms` shifted by `utc_offset_secs` as `YYYY-MM-DDTHH:MM:SS`
/// using Howard Hinnant's `civil_from_days` algorithm.
pub fn format_iso_local_seconds(epoch_ms: u64, utc_offset_secs: i32) -> String {
    let utc_secs = (epoch_ms / 1000) as i64;
    let local_secs = utc_secs.saturating_add(i64::from(utc_offset_secs));
    let days = local_secs.div_euclid(86_400);
    let secs_of_day = local_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// Formats a millisecond duration into JUnit seconds matching Kotlin's
/// `(duration.inWholeMilliseconds / 1000.0).toString()` (`6000.0 -> "6.0"`,
/// `2500.0 -> "2.5"`, `1915947.0 -> "1915.947"`, `0.0 -> "0.0"`).
pub fn format_junit_seconds(duration_ms: f64) -> String {
    let ms = if duration_ms.is_finite() && duration_ms > 0.0 { duration_ms.round() as u64 } else { 0 };
    let whole = ms / 1000;
    let rem = ms % 1000;
    if rem == 0 {
        format!("{whole}.0")
    } else if rem.is_multiple_of(100) {
        format!("{whole}.{}", rem / 100)
    } else if rem.is_multiple_of(10) {
        format!("{whole}.{:02}", rem / 10)
    } else {
        format!("{whole}.{rem:03}")
    }
}

/// Formats `path` relative to the current working directory with `/` separators.
pub fn cwd_relative_path(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let rel = if path.is_relative() {
        path
    } else if let Ok(cwd) = std::env::current_dir()
        && let Ok(stripped) = path.strip_prefix(&cwd)
    {
        stripped
    } else {
        path
    };
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => Some("..".to_string()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        Some(path.to_string_lossy().replace('\\', "/"))
    } else if rel.is_absolute() {
        Some(format!("/{}", parts.join("/")))
    } else {
        Some(parts.join("/"))
    }
}

/// Renders the JSON report for `--report <PATH>`.
///
/// - Single-shard (`suites.len() == 1`): preserves the exact legacy shape
///   `{ "platform", "inputMode", "setupMs", "flows" }`.
/// - Multi-shard (`suites.len() > 1`): keeps top-level `platform` (homogeneous
///   or `"mixed"`), `inputMode`, `setupMs` (max), flattened `flows` (enriched
///   with `shardIndex`, `shardId`, `device`, `platform`), plus `"shards"` and
///   `"summary"`.
///
/// Abnormal runs add fields and never change the ones above: `interrupted` +
/// `interruptSignal` (partial report after `SIGINT`/`SIGTERM`), `shardErrors`
/// and per-shard `error` (a shard that could not run its flows), and
/// `notRunCount` (flows not started before an interrupt).
pub fn render_json_report(summary: &TestExecutionSummary) -> Result<String> {
    if let [single] = summary.suites.as_slice() {
        let flows: Vec<&Value> = single.flows.iter().map(|f| &f.raw_json).collect();
        let mut doc = json!({
            "platform": single.platform,
            "inputMode": summary.input_mode,
            "setupMs": single.setup_ms,
            "flows": flows,
        });
        add_abnormal_run_fields(&mut doc, summary);
        return Ok(serde_json::to_string_pretty(&doc)?);
    }

    let first_platform = summary.suites.first().map(|s| s.platform.as_str()).unwrap_or("android");
    let homogeneous = summary.suites.iter().all(|s| s.platform == first_platform);
    let platform = if homogeneous { first_platform } else { "mixed" };
    let max_setup_ms = summary.suites.iter().map(|s| s.setup_ms).fold(0.0_f64, f64::max);

    let mut flat_flows = Vec::new();
    let mut shards_json = Vec::new();
    for (idx, suite) in summary.suites.iter().enumerate() {
        let shard_idx = suite.shard_index.unwrap_or(idx);
        let shard_id = shard_idx + 1;
        let mut shard_flows = Vec::with_capacity(suite.flows.len());
        for flow in &suite.flows {
            let mut obj = flow.raw_json.clone();
            obj["shardIndex"] = json!(shard_idx);
            obj["shardId"] = json!(shard_id);
            if let Some(dev) = &suite.device_id {
                obj["device"] = json!(dev);
            }
            obj["platform"] = json!(suite.platform);
            flat_flows.push(obj.clone());
            shard_flows.push(obj);
        }
        let mut shard_json = json!({
            "shardIndex": shard_idx,
            "shardId": shard_id,
            "device": suite.device_id,
            "platform": suite.platform,
            "passed": suite.passed,
            "setupMs": suite.setup_ms,
            "durationMs": suite.duration_ms,
            "passedCount": suite.passed_count(),
            "failedCount": suite.failed_count(),
            "skippedCount": suite.skipped_count,
            "flows": shard_flows,
        });
        if let Some(error) = &suite.error {
            shard_json["error"] = json!(error);
        }
        if suite.not_run_count() > 0 {
            shard_json["notRunCount"] = json!(suite.not_run_count());
        }
        shards_json.push(shard_json);
    }

    let mut doc = json!({
        "platform": platform,
        "inputMode": summary.input_mode,
        "setupMs": max_setup_ms,
        "flows": flat_flows,
        "shards": shards_json,
        "summary": {
            "passed": summary.passed,
            "shardCount": summary.suites.len(),
            "passedCount": summary.passed_count(),
            "failedCount": summary.failed_count(),
            "skippedCount": summary.skipped_count(),
            "totalCount": summary.total_executed(),
            "totalMs": summary.total_ms,
        }
    });
    if summary.not_run_count() > 0 {
        doc["summary"]["notRunCount"] = json!(summary.not_run_count());
    }
    add_abnormal_run_fields(&mut doc, summary);
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// Top-level markers of a run that did not finish normally. Absent on a
/// normal run, so its JSON is unchanged.
fn add_abnormal_run_fields(doc: &mut Value, summary: &TestExecutionSummary) {
    if let Some(signal) = summary.interrupted {
        doc["interrupted"] = json!(true);
        doc["interruptSignal"] = json!(crate::interrupt::signal_name(signal));
    }
    let shard_errors: Vec<Value> = summary
        .suites
        .iter()
        .enumerate()
        .filter_map(|(idx, suite)| {
            let error = suite.error.as_deref()?;
            let shard_idx = suite.shard_index.unwrap_or(idx);
            Some(json!({
                "shardIndex": shard_idx,
                "shardId": shard_idx + 1,
                "device": suite.device_id,
                "error": error,
            }))
        })
        .collect();
    if !shard_errors.is_empty() {
        doc["shardErrors"] = Value::Array(shard_errors);
    }
}

/// Writes `contents` to `path` atomically (see [`crate::atomic_write`]),
/// creating parent directories if needed.
pub fn write_file_creating_parents(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("creating directory {}", parent.display()))?;
    }
    crate::atomic_write::write(path, contents).with_context(|| format!("writing report to {}", path.display()))
}

/// Writes the requested `--format` report (`junit`, `html`, `html-detailed`) if enabled.
pub fn write_formatted_report(
    format: ReportFormat,
    output: Option<&Path>,
    summary: &TestExecutionSummary,
) -> Result<Option<PathBuf>> {
    let Some(target) = resolve_report_output(format, output) else {
        return Ok(None);
    };
    let rendered = match format {
        ReportFormat::Noop => return Ok(None),
        ReportFormat::Junit => junit::render_junit(summary)?,
        ReportFormat::Html => html::render_html(summary, false),
        ReportFormat::HtmlDetailed => html::render_html(summary, true),
    };
    write_file_creating_parents(&target, &rendered)?;
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_format_parsing_is_case_insensitive_and_supports_aliases() {
        assert_eq!("noop".parse::<ReportFormat>().unwrap(), ReportFormat::Noop);
        assert_eq!("JUNIT".parse::<ReportFormat>().unwrap(), ReportFormat::Junit);
        assert_eq!("Html".parse::<ReportFormat>().unwrap(), ReportFormat::Html);
        assert_eq!("html-detailed".parse::<ReportFormat>().unwrap(), ReportFormat::HtmlDetailed);
        assert_eq!("HTML_DETAILED".parse::<ReportFormat>().unwrap(), ReportFormat::HtmlDetailed);
        assert!("xml".parse::<ReportFormat>().is_err());

        assert_eq!(ReportFormat::Noop.default_output_path(), None);
        assert_eq!(ReportFormat::Junit.default_output_path(), Some(PathBuf::from("report.xml")));
        assert_eq!(ReportFormat::Html.default_output_path(), Some(PathBuf::from("report.html")));
        assert_eq!(ReportFormat::HtmlDetailed.default_output_path(), Some(PathBuf::from("report.html")));
    }

    #[test]
    fn format_junit_seconds_matches_kotlin_double_to_string() {
        assert_eq!(format_junit_seconds(0.0), "0.0");
        assert_eq!(format_junit_seconds(-5.0), "0.0");
        assert_eq!(format_junit_seconds(6000.0), "6.0");
        assert_eq!(format_junit_seconds(2500.0), "2.5");
        assert_eq!(format_junit_seconds(2550.0), "2.55");
        assert_eq!(format_junit_seconds(1915947.0), "1915.947");
        assert_eq!(format_junit_seconds(42125.0), "42.125");
    }

    #[test]
    fn format_iso_local_seconds_handles_utc_and_offsets() {
        // 2025-11-26T14:30:05Z -> 1764167405000 ms
        assert_eq!(format_iso_local_seconds(1_764_167_405_000, 0), "2025-11-26T14:30:05");
        // +01:00 (+3600s)
        assert_eq!(format_iso_local_seconds(1_764_167_405_000, 3600), "2025-11-26T15:30:05");
        // +05:30 (+19800s)
        assert_eq!(format_iso_local_seconds(1_764_167_405_000, 19_800), "2025-11-26T20:00:05");
        // Negative offset crossing epoch year boundary (1970-01-01T00:00:00Z at UTC-05:00)
        assert_eq!(format_iso_local_seconds(0, -18_000), "1969-12-31T19:00:00");
        // Leap day 2024-02-29T12:00:00Z -> 1709208000000 ms
        assert_eq!(format_iso_local_seconds(1_709_208_000_000, 0), "2024-02-29T12:00:00");
        // Unix epoch
        assert_eq!(format_iso_local_seconds(0, 0), "1970-01-01T00:00:00");
    }

    #[test]
    fn parse_utc_offset_hhmm_parses_valid_and_rejects_invalid() {
        assert_eq!(parse_utc_offset_hhmm("+0000\n"), Some(0));
        assert_eq!(parse_utc_offset_hhmm("+0100"), Some(3600));
        assert_eq!(parse_utc_offset_hhmm("-0500"), Some(-18_000));
        assert_eq!(parse_utc_offset_hhmm("+0530"), Some(19_800));
        assert_eq!(parse_utc_offset_hhmm("-0330"), Some(-12_600));
        assert_eq!(parse_utc_offset_hhmm("+2400"), None);
        assert_eq!(parse_utc_offset_hhmm("+0160"), None);
        assert_eq!(parse_utc_offset_hhmm("UTC"), None);
        assert_eq!(parse_utc_offset_hhmm(""), None);
    }

    #[test]
    fn cwd_relative_path_normalizes_separators() {
        assert_eq!(cwd_relative_path(Path::new("")), None);
        assert_eq!(cwd_relative_path(Path::new("flows/sub/login.yaml")), Some("flows/sub/login.yaml".to_string()));
        if let Ok(cwd) = std::env::current_dir() {
            let inside = cwd.join("examples/sample_app/flow.yaml");
            assert_eq!(cwd_relative_path(&inside), Some("examples/sample_app/flow.yaml".to_string()));
        }
    }
}
