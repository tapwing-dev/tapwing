//! Self-contained HTML and HTML-Detailed test suite reporter (`--format html`
//! and `--format html-detailed`) with zero external URLs or CDN dependencies.

use std::fmt::Write;
use std::path::{Component, Path, PathBuf};

use super::{FlowRunRecord, FlowStatus, TestExecutionSummary, format_iso_local_seconds};

const DEFAULT_SUITE_NAME: &str = "Test Suite";

/// Neutral, table-first stylesheet. Light by default with a matching dark
/// palette via `prefers-color-scheme`; color is reserved for result state.
const STYLES: &str = r#"
:root{
  --bg:#ffffff;--bg-subtle:#f6f7f8;--line:#e6e8eb;--line-strong:#d3d6db;
  --fg:#1b1d21;--fg-2:#4f545c;--fg-3:#868b94;
  --pass:#22774b;--fail:#c13a2f;--warn:#a8670a;--skip:#a4a9b1;
  --sans:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;
  --mono:ui-monospace,SFMono-Regular,"SF Mono",Menlo,Consolas,"Liberation Mono",monospace;
  color-scheme:light;
}
@media (prefers-color-scheme:dark){
  :root{
    --bg:#101113;--bg-subtle:#17191c;--line:#24272c;--line-strong:#33373e;
    --fg:#e7e9ec;--fg-2:#adb2ba;--fg-3:#7b818b;
    --pass:#5db585;--fail:#e87a70;--warn:#dba24a;--skip:#5e646d;
    color-scheme:dark;
  }
}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 var(--sans);
  -webkit-font-smoothing:antialiased;font-variant-numeric:tabular-nums}
code,pre,.mono{font-family:var(--mono);font-size:12.5px}
a{color:inherit;text-decoration:none}
a:hover{text-decoration:underline;text-underline-offset:2px}
.page{max-width:960px;margin:0 auto;padding:48px 24px 64px}

h1{margin:0;font-size:20px;font-weight:600;line-height:1.3}
h2{margin:0;font-size:15px;font-weight:600}
.muted{color:var(--fg-3)}
.dot{color:var(--line-strong);margin:0 6px}

.ic{flex:none;width:16px;height:16px;display:block}
.ic circle{fill:currentColor}
.ic path{fill:none;stroke:var(--bg);stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}
.ic.skip circle{fill:none;stroke:currentColor;stroke-width:1.4}
.ic.skip path{stroke:currentColor}
.pass{color:var(--pass)}.fail{color:var(--fail)}.warn{color:var(--warn)}.skip{color:var(--skip)}
.sr{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}

.head{display:flex;flex-direction:column;gap:6px}
.result{display:inline-flex;align-items:center;gap:6px;font-weight:600}
.sub{color:var(--fg-2);display:flex;flex-wrap:wrap;align-items:center}

.facts{display:flex;flex-wrap:wrap;gap:12px 40px;margin:24px 0 40px;padding:14px 0;
  border-top:1px solid var(--line);border-bottom:1px solid var(--line)}
.facts div{display:flex;flex-direction:column}
.facts dt{font-size:12px;color:var(--fg-3)}
.facts dd{margin:0;font-size:16px;font-weight:600}

.block{margin-bottom:40px}
.block-head{display:flex;flex-wrap:wrap;justify-content:space-between;align-items:baseline;gap:4px 16px;
  padding-bottom:10px;border-bottom:1px solid var(--line-strong)}
.block-head .meta{color:var(--fg-2);font-size:13px}

.failures{list-style:none;margin:0;padding:0}
.failures li{display:grid;grid-template-columns:16px minmax(0,max-content) minmax(0,1fr);gap:4px 10px;
  align-items:center;padding:9px 12px;border-bottom:1px solid var(--line)}
.failures .name{font-weight:500;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.failures .name{margin-right:14px}
.failures .msg{color:var(--fg-2);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}

details.flow-card{border-bottom:1px solid var(--line)}
details.flow-card>summary{list-style:none;cursor:pointer;display:grid;
  grid-template-columns:10px 16px minmax(0,1fr) auto;gap:10px;align-items:center;padding:10px 12px}
details.flow-card>summary::-webkit-details-marker{display:none}
details.flow-card>summary:hover{background:var(--bg-subtle)}
details.flow-card>summary:focus-visible{outline:2px solid var(--line-strong);outline-offset:-2px}
.chev{width:6px;height:6px;border-right:1.5px solid var(--fg-3);border-bottom:1.5px solid var(--fg-3);
  transform:rotate(-45deg);justify-self:center}
details[open]>summary .chev{transform:rotate(45deg)}
.flow-name,.flow-sub{display:block}
.flow-name{font-weight:500;overflow-wrap:anywhere}
.flow-sub{font-size:12.5px;color:var(--fg-3);overflow-wrap:anywhere}
.dur{color:var(--fg-2);text-align:right;white-space:nowrap}

.flow-body{padding:2px 12px 18px 60px;display:flex;flex-direction:column;gap:14px}
pre.failure-box{margin:0;padding:12px 14px;background:var(--bg-subtle);border:1px solid var(--line);
  border-radius:4px;color:var(--fg);white-space:pre-wrap;overflow-wrap:anywhere;line-height:1.55}
.failure-diag{display:flex;flex-direction:column;gap:10px}
.did-you-mean{margin:0;padding:10px 14px;background:var(--bg-subtle);border:1px solid var(--line);border-radius:4px;font-size:13px}
.did-you-mean-title{font-weight:600;margin-bottom:4px;color:var(--fg)}
.did-you-mean ul{margin:0;padding-left:18px}
.did-you-mean li{margin:2px 0;color:var(--fg-2);overflow-wrap:anywhere}
.did-you-mean code{color:var(--fg)}
.failure-shot{margin:0}
.failure-shot a{display:inline-block}
.failure-shot img{display:block;max-width:100%;max-height:420px;border:1px solid var(--line);border-radius:4px;background:var(--bg-subtle)}
.failure-shot figcaption{margin-top:4px;font-size:12px;color:var(--fg-3);overflow-wrap:anywhere}
details.failure-tree{border:1px solid var(--line);border-radius:4px;background:var(--bg-subtle)}
details.failure-tree>summary{cursor:pointer;padding:8px 12px;font-size:13px;font-weight:500;color:var(--fg-2)}
details.failure-tree>pre{margin:0;padding:10px 12px;border-top:1px solid var(--line);overflow-x:auto;
  max-height:360px;line-height:1.45;color:var(--fg)}
.props{display:grid;grid-template-columns:max-content minmax(0,1fr);gap:3px 20px;margin:0;font-size:13px}
.props dt{color:var(--fg-3)}
.props dd{margin:0;color:var(--fg-2);overflow-wrap:anywhere}

.table-wrap{overflow-x:auto}
table.steps{width:100%;border-collapse:collapse;font-size:13px}
table.steps th{text-align:left;font-size:12px;font-weight:500;color:var(--fg-3);padding:0 10px 6px;
  border-bottom:1px solid var(--line)}
table.steps td{padding:6px 10px;border-bottom:1px solid var(--line);vertical-align:top}
table.steps tr:last-child td{border-bottom:0}
table.steps .n{width:1%;color:var(--fg-3);text-align:right}
table.steps .s{width:1%;padding-top:7px}
table.steps .s .ic{width:13px;height:13px}
table.steps .t{width:1%;text-align:right;color:var(--fg-2);white-space:nowrap}
table.steps .r{text-align:right}
table.steps .c{overflow-wrap:anywhere}
tr.is-failed .c .mono{color:var(--fail)}
tr.is-skipped td{color:var(--fg-3)}
.step-err{margin-top:3px;color:var(--fg-2);font-family:var(--mono);font-size:12px;white-space:pre-wrap;overflow-wrap:anywhere}
.step-warn{margin-top:3px;color:var(--warn);font-size:12.5px}

@media (max-width:640px){
  .page{padding:32px 16px 48px}
  .facts{gap:12px 28px}
  .flow-body{padding-left:12px}
  .failures li{grid-template-columns:16px minmax(0,1fr)}
  .failures .msg{grid-column:2}
}
@media (prefers-reduced-motion:no-preference){.chev{transition:transform .12s ease}}
@media print{details.flow-card>summary:hover{background:none}}
"#;

#[derive(Clone, Copy)]
enum Mark {
    Pass,
    Fail,
    Warn,
    Skip,
}

impl Mark {
    fn class(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Warn => "warn",
            Self::Skip => "skip",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pass => "Passed",
            Self::Fail => "Failed",
            Self::Warn => "Passed with warnings",
            Self::Skip => "Skipped",
        }
    }

    /// Inline SVG status glyph (no `xmlns`, so the report stays free of URLs).
    fn icon(self) -> String {
        let path = match self {
            Self::Pass => "M5 8.3l2 2 4-4.5",
            Self::Fail => "M5.6 5.6l4.8 4.8M10.4 5.6l-4.8 4.8",
            Self::Warn => "M8 4.6v4M8 11.3v.1",
            Self::Skip => "M5.5 8h5",
        };
        format!(
            "<svg class=\"ic {cls}\" viewBox=\"0 0 16 16\" aria-hidden=\"true\"><circle cx=\"8\" cy=\"8\" r=\"7\"/><path d=\"{path}\"/></svg>",
            cls = self.class()
        )
    }

    fn for_flow(flow: &FlowRunRecord) -> Self {
        match flow.status {
            FlowStatus::Success => Self::Pass,
            FlowStatus::Error | FlowStatus::Interrupted => Self::Fail,
            FlowStatus::Warning => Self::Warn,
            FlowStatus::NotRun => Self::Skip,
        }
    }

    fn for_step(status: &str) -> Self {
        match status {
            "passed" => Self::Pass,
            "warned" => Self::Warn,
            "skipped" => Self::Skip,
            _ => Self::Fail,
        }
    }
}

/// Escapes `&`, `<`, `>`, `"`, and `'` for safe interpolation into HTML text
/// nodes and double/single-quoted attribute values.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Zero-dependency RFC 4648 base64 encoder for embedding failure thumbnails
/// as self-contained `data:image/jpeg;base64,…` URIs with no external URLs.
pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map(u32::from).unwrap_or(0);
        let b2 = chunk.get(2).copied().map(u32::from).unwrap_or(0);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 0x3f) as usize] as char } else { '=' });
    }
    out
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if can_pop {
                    out.pop();
                } else if !out.has_root() {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn diff_paths(target: &Path, base: &Path) -> Option<PathBuf> {
    let t_comps: Vec<Component<'_>> = target.components().collect();
    let b_comps: Vec<Component<'_>> = base.components().collect();
    if t_comps.first() != b_comps.first()
        && (matches!(t_comps.first(), Some(Component::RootDir | Component::Prefix(_)))
            || matches!(b_comps.first(), Some(Component::RootDir | Component::Prefix(_))))
    {
        return None;
    }
    let common = t_comps.iter().zip(b_comps.iter()).take_while(|(a, b)| a == b).count();
    let mut rel = PathBuf::new();
    for comp in &b_comps[common..] {
        if matches!(comp, Component::Normal(_)) {
            rel.push("..");
        }
    }
    for comp in &t_comps[common..] {
        rel.push(comp);
    }
    if rel.as_os_str().is_empty() {
        rel.push(".");
    }
    Some(rel)
}

fn format_slash_path(path: &Path) -> String {
    let mut parts = Vec::new();
    let mut leading_slash = false;
    for comp in path.components() {
        match comp {
            Component::RootDir => leading_slash = true,
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => parts.push("..".to_string()),
            Component::CurDir => parts.push(".".to_string()),
            Component::Prefix(p) => parts.push(p.as_os_str().to_string_lossy().into_owned()),
        }
    }
    if parts.is_empty() {
        if leading_slash { "/".into() } else { path.to_string_lossy().replace('\\', "/") }
    } else if leading_slash {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

/// Percent-encodes everything except unreserved URI characters and `/` so local
/// relative paths containing `#`, `?`, `%`, spaces, or `:` never break `href`.
fn percent_encode_href(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

fn relative_screenshot_href(shot_path: &Path, report_base: Option<&Path>) -> String {
    let rel_str = if let Some(base) = report_base {
        let base = if base.as_os_str().is_empty() { Path::new(".") } else { base };
        let resolved = if !base.is_absolute() && !shot_path.is_absolute() {
            diff_paths(&normalize_lexical(shot_path), &normalize_lexical(base))
        } else {
            let cwd = std::env::current_dir().ok();
            let norm_base = if base.is_absolute() {
                Some(normalize_lexical(base))
            } else {
                cwd.as_ref().map(|c| normalize_lexical(&c.join(base)))
            };
            let norm_shot = if shot_path.is_absolute() {
                Some(normalize_lexical(shot_path))
            } else {
                cwd.as_ref().map(|c| normalize_lexical(&c.join(shot_path)))
            };
            match (norm_shot, norm_base) {
                (Some(s), Some(b)) => diff_paths(&s, &b).or(Some(s)),
                _ => None,
            }
        };
        resolved.map(|p| format_slash_path(&p)).unwrap_or_else(|| shot_path.to_string_lossy().replace('\\', "/"))
    } else {
        super::cwd_relative_path(shot_path).unwrap_or_else(|| shot_path.to_string_lossy().replace('\\', "/"))
    };
    percent_encode_href(&rel_str)
}

/// Renders F8 rich failure diagnostics (suggestions, inline JPEG thumbnail
/// linked to the full-res PNG on disk, and collapsible UI hierarchy dump).
fn render_failure_diagnostics(out: &mut String, diag: &crate::failure::FailureDiagnostics, report_base: Option<&Path>) {
    if diag.is_empty() {
        return;
    }
    out.push_str("<div class=\"failure-diag\">\n");
    if !diag.suggestions.is_empty() {
        out.push_str("<div class=\"did-you-mean\"><div class=\"did-you-mean-title\">Did you mean</div><ul>\n");
        for s in &diag.suggestions {
            let _ = writeln!(
                out,
                "<li><code>{}=\"{}\"</code> — {}</li>",
                escape_html(&s.field),
                escape_html(&s.value),
                escape_html(&s.reason)
            );
        }
        out.push_str("</ul></div>\n");
    }
    if let Some(path) = &diag.screenshot {
        let shot_path = Path::new(path);
        if let Some(uri) = crate::trace::thumbnail_data_uri(shot_path) {
            let href = relative_screenshot_href(shot_path, report_base);
            let _ = writeln!(
                out,
                "<figure class=\"failure-shot\"><a href=\"{}\" target=\"_blank\" rel=\"noopener\"><img src=\"{uri}\" alt=\"Failure screenshot\"></a><figcaption class=\"mono\">{}</figcaption></figure>",
                escape_html(&href),
                escape_html(path)
            );
        } else if shot_path.is_file() {
            let href = relative_screenshot_href(shot_path, report_base);
            let _ = writeln!(
                out,
                "<div class=\"did-you-mean\">Screenshot: <a href=\"{}\" target=\"_blank\" rel=\"noopener\"><code>{}</code></a></div>",
                escape_html(&href),
                escape_html(path)
            );
        } else {
            let _ = writeln!(out, "<div class=\"did-you-mean\">Screenshot: <code>{}</code></div>", escape_html(path));
        }
    }
    if let Some(tree) = &diag.hierarchy {
        let mut label_parts = Vec::new();
        if let Some(src) = &diag.hierarchy_source {
            label_parts.push(escape_html(src));
        }
        if let Some(n) = diag.total_elements {
            label_parts.push(format!("{n} element{}", if n == 1 { "" } else { "s" }));
        }
        let summary = if label_parts.is_empty() {
            "UI tree at failure".to_string()
        } else {
            format!("UI tree at failure ({})", label_parts.join(", "))
        };
        let _ = writeln!(
            out,
            "<details class=\"failure-tree\"><summary>{summary}</summary><pre>{}</pre></details>",
            escape_html(tree)
        );
    }
    out.push_str("</div>\n");
}

/// `850 ms`, `14.25 s`, `3 min 05 s`.
fn format_duration(ms: f64) -> String {
    let ms = if ms.is_finite() && ms > 0.0 { ms } else { 0.0 };
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 60_000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        let secs = (ms / 1000.0).round() as u64;
        format!("{} min {:02} s", secs / 60, secs % 60)
    }
}

fn platform_label(platform: &str) -> String {
    match platform.to_ascii_lowercase().as_str() {
        "android" => "Android".into(),
        "ios" => "iOS".into(),
        _ => escape_html(platform),
    }
}

fn flow_anchor(shard_num: usize, flow_idx: usize) -> String {
    format!("flow-{shard_num}-{flow_idx}")
}

/// Renders a self-contained HTML report (`detailed == true` includes per-flow
/// command step tables).
pub fn render_html(summary: &TestExecutionSummary, detailed: bool) -> String {
    render_html_for_path(summary, detailed, None)
}

/// Renders a self-contained HTML report whose failure screenshot links are
/// resolved relative to `report_path`'s parent directory when provided.
pub fn render_html_for_path(summary: &TestExecutionSummary, detailed: bool, report_path: Option<&Path>) -> String {
    let report_base = report_path.and_then(Path::parent);
    let mut out = String::with_capacity(16384);
    let suite_title = escape_html(summary.suite_name.as_deref().unwrap_or(DEFAULT_SUITE_NAME));
    let shard_count = summary.suites.len();
    let failed_flows = summary.failed_count();
    let overall = if summary.passed { Mark::Pass } else { Mark::Fail };
    let dot = "<span class=\"dot\">·</span>";

    let mut sub = vec!["Tapwing".to_string()];
    sub.push(if shard_count == 1 { "1 device".into() } else { format!("{shard_count} devices") });
    if let Some(start) = summary.suites.iter().map(|s| s.start_epoch_ms).min() {
        sub.push(format!("started {}", format_iso_local_seconds(start, summary.utc_offset_secs).replacen('T', " ", 1)));
    }
    if let Some(signal) = summary.interrupted {
        sub.push(format!(
            "<span class=\"warn\">interrupted by {}, partial results</span>",
            escape_html(&crate::interrupt::signal_name(signal))
        ));
    }
    let sub = sub.join(dot);

    let _ = write!(
        out,
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{suite_title} — Tapwing</title>\n\
         <style>{STYLES}</style>\n\
         </head>\n\
         <body>\n\
         <main class=\"page\">\n\
         <header class=\"head\">\n\
           <h1>{suite_title}</h1>\n\
           <div class=\"sub\"><span class=\"result {cls}\">{icon}{label}</span>{dot}{sub}</div>\n\
         </header>\n\
         <dl class=\"facts\">\n\
           <div><dt>Flows</dt><dd>{total}</dd></div>\n\
           <div><dt>Passed</dt><dd>{passed}</dd></div>\n\
           <div><dt>Failed</dt><dd{fail_cls}>{failed_flows}</dd></div>\n\
           <div><dt>Skipped</dt><dd>{skipped}</dd></div>\n\
           <div><dt>Duration</dt><dd>{duration}</dd></div>\n\
         </dl>\n",
        cls = overall.class(),
        icon = overall.icon(),
        label = overall.label(),
        total = summary.total_executed(),
        passed = summary.passed_count(),
        fail_cls = if failed_flows > 0 { " class=\"fail\"" } else { "" },
        skipped = summary.skipped_count() + summary.not_run_count(),
        duration = format_duration(summary.total_ms),
    );

    if failed_flows > 0 {
        let _ = write!(
            out,
            "<section class=\"block\">\n\
               <div class=\"block-head\"><h2>Failures</h2><span class=\"meta\">{failed_flows}</span></div>\n\
               <ul class=\"failures\">\n"
        );
        for (idx, suite) in summary.suites.iter().enumerate() {
            let shard_num = suite.shard_index.unwrap_or(idx) + 1;
            for (flow_idx, flow) in suite.flows.iter().enumerate().filter(|(_, f)| f.failed()) {
                let first_line = flow.failure_message.as_deref().and_then(|m| m.lines().next()).unwrap_or("");
                let device = if shard_count > 1 {
                    format!(
                        " <span class=\"muted\">on {}</span>",
                        escape_html(suite.device_description.as_deref().or(suite.device_id.as_deref()).unwrap_or(""))
                    )
                } else {
                    String::new()
                };
                let _ = writeln!(
                    out,
                    "<li>{icon}<a class=\"name\" href=\"#{anchor}\">{name}{device}</a><span class=\"msg mono\">{msg}</span></li>",
                    icon = Mark::Fail.icon(),
                    anchor = flow_anchor(shard_num, flow_idx),
                    name = escape_html(&flow.name),
                    msg = escape_html(first_line),
                );
            }
        }
        out.push_str("</ul>\n</section>\n");
    }

    for (idx, suite) in summary.suites.iter().enumerate() {
        let shard_num = suite.shard_index.unwrap_or(idx) + 1;
        let dev_label =
            escape_html(suite.device_description.as_deref().or(suite.device_id.as_deref()).unwrap_or("Default device"));
        let mut meta = vec![platform_label(&suite.platform)];
        if shard_count > 1 {
            meta.push(format!("Shard {shard_num}"));
        }
        let mut counts = format!("{} of {} passed", suite.passed_count(), suite.total_executed());
        if suite.skipped_count > 0 {
            let _ = write!(counts, ", {} skipped", suite.skipped_count);
        }
        if suite.not_run_count() > 0 {
            let _ = write!(counts, ", {} not run", suite.not_run_count());
        }
        meta.push(counts);
        meta.push(format_duration(suite.duration_ms));
        let meta = meta.join(dot);

        let _ = write!(
            out,
            "<section class=\"block\">\n\
               <div class=\"block-head\"><h2>{dev_label}</h2><span class=\"meta\">{meta}</span></div>\n"
        );

        for (flow_idx, flow) in suite.flows.iter().enumerate() {
            let mark = Mark::for_flow(flow);
            // F7: a passing flow with app issues stays open, so it never looks clean.
            let issue_count: usize = flow.steps.iter().map(|s| s.app_issues.len()).sum();
            let open_attr = if flow.failed() || issue_count > 0 { " open" } else { "" };
            let mut flow_sub = vec![escape_html(&flow.lane)];
            if flow.blackbox {
                flow_sub.push("blackbox".into());
                if let Some(reason) = &flow.blackbox_reason {
                    flow_sub.push(format!("<span class=\"muted\">{}</span>", escape_html(reason)));
                }
            }
            if issue_count > 0 {
                flow_sub.push(format!(
                    "<span class=\"warn\">⚠ {issue_count} app issue{}</span>",
                    if issue_count == 1 { "" } else { "s" }
                ));
            }
            if let Some(file) = &flow.file {
                flow_sub.push(format!("<span class=\"mono\">{}</span>", escape_html(file)));
            }

            let _ = write!(
                out,
                "<details class=\"flow-card\"{open_attr} id=\"{anchor}\">\n\
                   <summary>\
                     <span class=\"chev\" aria-hidden=\"true\"></span>\
                     <span class=\"{cls}\" title=\"{label}\">{icon}<span class=\"sr\">{label}</span></span>\
                     <span><span class=\"flow-name\">{name}</span><span class=\"flow-sub\">{flow_sub}</span></span>\
                     <span class=\"dur\">{dur}</span>\
                   </summary>\n\
                   <div class=\"flow-body\">\n",
                anchor = flow_anchor(shard_num, flow_idx),
                cls = mark.class(),
                label = mark.label(),
                icon = mark.icon(),
                name = escape_html(&flow.name),
                flow_sub = flow_sub.join(dot),
                dur = format_duration(flow.duration_ms),
            );

            if let Some(err) = &flow.failure_message {
                let _ = writeln!(out, "<pre class=\"failure-box\">{}</pre>", escape_html(err));
            }
            if let Some(diag) = &flow.failure_diagnostics {
                render_failure_diagnostics(&mut out, diag, report_base);
            }

            let _ = write!(
                out,
                "<dl class=\"props\"><dt>Steps</dt><dd>{} ({} Flutter, {} native)</dd>",
                flow.steps.len(),
                flow.flutter_steps,
                flow.native_steps
            );
            if !flow.tags.is_empty() {
                let tags_str = flow.tags.iter().map(|t| escape_html(t)).collect::<Vec<_>>().join(", ");
                let _ = write!(out, "<dt>Tags</dt><dd>{tags_str}</dd>");
            }
            if !flow.properties.is_empty() {
                let props_str = flow
                    .properties
                    .iter()
                    .map(|(k, v)| format!("{}={}", escape_html(k), escape_html(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = write!(out, "<dt>Properties</dt><dd><code>{props_str}</code></dd>");
            }
            out.push_str("</dl>\n");

            if detailed && !flow.steps.is_empty() {
                out.push_str(
                    "<div class=\"table-wrap\"><table class=\"steps\">\n\
                       <thead><tr><th class=\"r\">#</th><th></th><th>Command</th><th class=\"r\">Time</th></tr></thead>\n\
                       <tbody>\n",
                );
                for step in &flow.steps {
                    let step_mark = Mark::for_step(&step.status);
                    let row_cls = match step_mark {
                        Mark::Fail => " class=\"is-failed\"",
                        Mark::Skip => " class=\"is-skipped\"",
                        _ => "",
                    };
                    let mut detail_html = String::new();
                    if let Some(w) = &step.warning {
                        let _ = write!(detail_html, "<div class=\"step-warn\">{}</div>", escape_html(w));
                    }
                    if let Some(e) = &step.error {
                        let _ = write!(detail_html, "<div class=\"step-err\">{}</div>", escape_html(e));
                    }
                    for issue in &step.app_issues {
                        let _ = write!(detail_html, "<div class=\"step-warn\">⚠ {}</div>", escape_html(issue));
                    }
                    let _ = writeln!(
                        out,
                        "<tr{row_cls}><td class=\"n\">{num}</td>\
                         <td class=\"s {cls}\" title=\"{label}\">{icon}<span class=\"sr\">{label}</span></td>\
                         <td class=\"c\"><span class=\"mono\">{desc}</span>{detail_html}</td>\
                         <td class=\"t\">{dur}</td></tr>",
                        num = step.index + 1,
                        cls = step_mark.class(),
                        label = step_mark.label(),
                        icon = step_mark.icon(),
                        desc = escape_html(&step.description),
                        dur = format_duration(step.duration_ms),
                    );
                }
                out.push_str("</tbody>\n</table></div>\n");
            }

            out.push_str("</div>\n</details>\n");
        }

        out.push_str("</section>\n");
    }

    out.push_str("</main>\n</body>\n</html>\n");
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::path::PathBuf;

    use super::*;
    use crate::report::{FlowRunRecord, FlowStatus, StepRecord, SuiteResult, TestExecutionSummary};

    #[test]
    fn html_report_escapes_xss_has_zero_external_urls_and_renders_detailed_steps() {
        let flow = FlowRunRecord {
            name: "<script>alert('xss')</script>".into(),
            path: PathBuf::from("xss.yaml"),
            file: Some("flows/<xss>.yaml".into()),
            status: FlowStatus::Error,
            duration_ms: 1250.0,
            start_epoch_ms: 1_764_167_405_000,
            failure_message: Some("Failed on <img src=x onerror=alert(1)> & \"quote\"".into()),
            properties: vec![("owner".into(), "<admin>".into())],
            tags: vec!["<tag>".into()],
            lane: "hybrid".into(),
            flutter_steps: 1,
            native_steps: 1,
            blackbox: true,
            blackbox_reason: Some("no tapwing_driver in the installed build: read <app> completely".into()),
            steps: vec![
                StepRecord {
                    index: 0,
                    command: "launchApp".into(),
                    description: "launchApp".into(),
                    status: "passed".into(),
                    duration_ms: 250.0,
                    warning: Some("minor <warn>".into()),
                    error: None,
                    app_issues: vec!["layoutOverflow: A <RenderFlex> overflowed".into()],
                },
                StepRecord {
                    index: 1,
                    command: "tapOn".into(),
                    description: "tapOn \"<b>Btn</b>\"".into(),
                    status: "failed".into(),
                    duration_ms: 1000.0,
                    warning: None,
                    error: Some("Element <b>Btn</b> not found".into()),
                    app_issues: Vec::new(),
                },
            ],
            failure_diagnostics: Some(crate::failure::FailureDiagnostics {
                suggestions: vec![crate::failure::Suggestion {
                    field: "text".into(),
                    value: "<Button>".into(),
                    reason: "similar (edit distance 1)".into(),
                }],
                hierarchy: Some("- text=\"<Button>\" [0,0][100,40]".into()),
                hierarchy_source: Some("flutter".into()),
                total_elements: Some(1),
                screenshot: None,
                hierarchy_file: None,
            }),
            raw_json: json!({}),
            trace: None,
        };

        let summary = TestExecutionSummary {
            passed: false,
            suite_name: Some("Suite & <Friends>".into()),
            input_mode: "auto".into(),
            total_ms: 1250.0,
            utc_offset_secs: 0,
            interrupted: None,
            suites: vec![SuiteResult {
                shard_index: Some(0),
                device_id: Some("emulator-5554".into()),
                device_description: Some("emulator-5554".into()),
                platform: "android".into(),
                passed: false,
                setup_ms: 5.0,
                duration_ms: 1250.0,
                start_epoch_ms: 1_764_167_405_000,
                skipped_count: 1,
                error: None,
                flows: vec![flow],
            }],
        };

        let basic = render_html(&summary, false);
        assert!(!basic.contains("http://") && !basic.contains("https://"));
        assert!(!basic.contains("<script>alert"));
        assert!(basic.contains("&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;"));
        assert!(basic.contains("details class=\"flow-card\" open"));
        assert!(basic.contains("blackbox"));
        assert!(
            basic.contains(
                "<span class=\"muted\">no tapwing_driver in the installed build: read &lt;app&gt; completely</span>"
            ),
            "the reason is shown, escaped"
        );
        assert!(!basic.contains("<table class=\"steps\">"));

        let detailed = render_html(&summary, true);
        assert!(!detailed.contains("http://") && !detailed.contains("https://"));
        assert!(detailed.contains("<table class=\"steps\">"));
        assert!(detailed.contains("tapOn &quot;&lt;b&gt;Btn&lt;/b&gt;&quot;"));
        assert!(detailed.contains("minor &lt;warn&gt;"));
        assert!(detailed.contains("⚠ layoutOverflow: A &lt;RenderFlex&gt; overflowed"));
        assert!(basic.contains("⚠ 1 app issue</span>"));
    }

    #[test]
    fn relative_screenshot_href_resolves_across_dirs_and_escapes_special_chars() {
        let shot = Path::new("/tmp/run-1/failures/my shot #1?v=2%20:test.png");
        let report_dir = Path::new("/tmp/run-1/reports");
        assert_eq!(
            relative_screenshot_href(shot, Some(report_dir)),
            "../failures/my%20shot%20%231%3Fv%3D2%2520%3Atest.png"
        );
        // Report in CWD (`Path::new("report.html").parent() == Some(Path::new(""))`).
        assert_eq!(
            relative_screenshot_href(Path::new("failures/fail #1.png"), Some(Path::new(""))),
            "failures/fail%20%231.png"
        );
        // None base dir preserves relative paths and percent-encodes special characters.
        assert_eq!(relative_screenshot_href(Path::new("failures/a b%c.png"), None), "failures/a%20b%25c.png");
    }
}
