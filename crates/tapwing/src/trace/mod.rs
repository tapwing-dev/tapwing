//! F8b trace viewer (`tapwing test --trace`): the per-shard trace model
//! (`trace.json`, schema [`SCHEMA`]), HTTP log redaction (defense in depth
//! over the driver's own), and the self-contained offline `trace.html`.
//!
//! The orchestrator records per-step trace extras (start offset, nesting,
//! `retry` attempt, lane, HTTP requests, optional screenshots) only when
//! [`TraceOpts`] is set; without `--trace` nothing here runs.

mod html;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use clap::ValueEnum;
use serde_json::{Value, json};

use crate::report::{FlowRunRecord, FlowStatus, SuiteResult};

pub use html::render_trace_html;

/// `trace.json` schema identifier (bumped on incompatible changes).
pub const SCHEMA: &str = "tapwing.trace/1";

/// Sub-directory of the test output dir for per-step trace screenshots.
pub const ASSETS_SUBDIR: &str = "trace";

/// Maximum width of the JPEG thumbnails inlined into `trace.html`.
pub const THUMBNAIL_MAX_WIDTH: u32 = 360;

/// Replacement for redacted values (same marker as the driver).
pub const REDACTED: &str = "<redacted>";

/// `--trace-screenshots`: when to capture a screenshot and UI tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum TraceScreenshots {
    /// Only failing steps (reuses the F8a failure capture: no extra calls).
    #[default]
    Failures,
    /// After every UI step as well.
    Steps,
}

impl TraceScreenshots {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failures => "failures",
            Self::Steps => "steps",
        }
    }
}

/// Trace settings; `None` in [`crate::orchestra::OrchestraOpts`] disables tracing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceOpts {
    pub screenshots: TraceScreenshots,
}

/// Per-step trace data from the backend: the lane that ran the step and the
/// HTTP log entries delivered since the previous step, split like F7 issues.
#[derive(Debug, Default)]
pub struct TraceBatch {
    /// `flutter`, `native`, `hybrid`, or `host` (device commands only).
    pub lane: Option<&'static str>,
    /// Requests started by the step that just ran.
    pub current: Vec<Value>,
    /// Requests started by an earlier step that finished since.
    pub late: Vec<Value>,
    /// Entries the driver dropped (ring buffer overflow) since the last batch.
    pub dropped: u64,
    /// Requests still open (only at flow end).
    pub in_flight: Vec<Value>,
}

static FLOW_ORDINAL: AtomicUsize = AtomicUsize::new(1);

/// Process-wide flow counter keeping trace asset names unique when the same
/// flow runs more than once.
pub fn next_flow_ordinal() -> usize {
    FLOW_ORDINAL.fetch_add(1, Ordering::Relaxed)
}

/// `trace` for an unsharded run, `trace-shard-N` per shard, so shards that
/// share one output dir (`--flatten-debug-output`) never collide.
pub fn file_stem(shard_index: Option<usize>) -> String {
    match shard_index {
        Some(i) => format!("trace-shard-{}", i + 1),
        None => "trace".to_string(),
    }
}

/// File name prefix for per-step assets of one flow run.
pub fn asset_prefix(shard_index: Option<usize>, flow_ordinal: usize) -> String {
    match shard_index {
        Some(i) => format!("s{}_f{flow_ordinal:02}_", i + 1),
        None => format!("f{flow_ordinal:02}_"),
    }
}

// ---------------------------------------------------------------------------
// Redaction (mirrors `HttpRedaction` in the driver)
// ---------------------------------------------------------------------------

const SENSITIVE_HEADERS: &[&str] = &["authorization", "proxy-authorization", "cookie", "set-cookie", "x-api-key"];
const SENSITIVE_HEADER_PARTS: &[&str] =
    &["token", "secret", "auth", "session", "password", "passwd", "signature", "csrf", "xsrf", "key", "credential"];
const SENSITIVE_QUERY_PARTS: &[&str] = &[
    "token",
    "secret",
    "auth",
    "session",
    "password",
    "passwd",
    "pwd",
    "pass",
    "sig",
    "key",
    "code",
    "credential",
    "otp",
    "jwt",
    "nonce",
    "ticket",
    "hash",
    "email",
    "phone",
];

/// Whether a header named `name` carries a secret.
pub fn is_sensitive_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SENSITIVE_HEADERS.contains(&n.as_str()) || SENSITIVE_HEADER_PARTS.iter().any(|p| n.contains(p))
}

/// Whether a query parameter named `name` carries a secret or PII.
pub fn is_sensitive_query_key(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SENSITIVE_QUERY_PARTS.iter().any(|p| n.contains(p))
}

/// Whether `value` looks like a token (JWT, long mixed alphanumeric run) or
/// contains an e-mail address.
pub fn looks_like_token(value: &str) -> bool {
    if value.contains('@') {
        return true;
    }
    let jwt_part = |p: &str| p.len() >= 8 && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() == 3 && parts.iter().all(|p| jwt_part(p)) {
        return true;
    }
    value.len() >= 24
        && value.chars().all(|c| c.is_ascii_alphanumeric() || "+/=_-.~%:".contains(c))
        && value.chars().any(|c| c.is_ascii_digit())
        && value.chars().any(|c| c.is_ascii_alphabetic())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Drops user info and the fragment, redacts token-like path segments and
/// sensitive or token-like query values. Idempotent on driver-redacted URLs.
pub fn sanitize_url(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or("");
    let (scheme, rest) = match without_fragment.find("://") {
        Some(i) => (&without_fragment[..i + 3], &without_fragment[i + 3..]),
        None => ("", without_fragment),
    };
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let tail = &rest[authority_end..];
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    let path: Vec<String> = path
        .split('/')
        .map(|seg| if looks_like_token(&percent_decode(seg)) { REDACTED.to_string() } else { seg.to_string() })
        .collect();
    let mut out = format!("{scheme}{host}{}", path.join("/"));
    if let Some(q) = query {
        let parts: Vec<String> = q
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((k, v))
                    if v != REDACTED
                        && (is_sensitive_query_key(&percent_decode(k)) || looks_like_token(&percent_decode(v))) =>
                {
                    format!("{k}={REDACTED}")
                }
                _ => pair.to_string(),
            })
            .collect();
        if !parts.is_empty() {
            out.push('?');
            out.push_str(&parts.join("&"));
        }
    }
    out
}

/// Sanitizes the URLs a header value carries (`Location`, `Referer`,
/// `Content-Location`, `Link`, `Refresh`): a value that is itself a relative
/// reference, and every absolute `http(s)://` URL inside the value. An OAuth
/// redirect's `code` or a reset link's `token` would otherwise reach the trace
/// verbatim. Idempotent on driver-redacted values.
pub fn sanitize_header_value(value: &str) -> String {
    if value.starts_with('/') && !value.contains(char::is_whitespace) {
        return sanitize_url(value);
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = [rest.find("http://"), rest.find("https://")].into_iter().flatten().min() {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = url_end(tail);
        out.push_str(&sanitize_url(&tail[..end]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Byte length of the URL at the start of `text`: up to whitespace or a
/// delimiter no URL contains unescaped. An earlier pass's [`REDACTED`] marker
/// (which contains `<` and `>`) is part of the URL, not its end.
fn url_end(text: &str) -> usize {
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with(REDACTED) {
            i += REDACTED.len();
            continue;
        }
        let Some(c) = text[i..].chars().next() else { break };
        if c.is_whitespace() || matches!(c, '<' | '>' | '"') {
            break;
        }
        i += c.len_utf8();
    }
    i
}

fn strip_queries(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '?' {
            out.push_str("?…");
            while let Some(&n) = chars.peek() {
                if n.is_whitespace() || n == ',' || n == ')' || n == ']' {
                    break;
                }
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Re-applies URL, header, and error redaction to one HTTP log entry.
pub fn sanitize_http_entry(entry: &mut Value) {
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        entry["url"] = Value::from(sanitize_url(url));
    }
    for key in ["requestHeaders", "responseHeaders"] {
        if let Some(headers) = entry.get_mut(key).and_then(Value::as_object_mut) {
            for (name, value) in headers.iter_mut() {
                if is_sensitive_header(name) {
                    *value = Value::from(REDACTED);
                } else if let Some(text) = value.as_str() {
                    let sanitized = sanitize_header_value(text);
                    if sanitized != text {
                        *value = Value::from(sanitized);
                    }
                }
            }
        }
    }
    if let Some(err) = entry.get("error").and_then(Value::as_str) {
        entry["error"] = Value::from(strip_queries(err));
    }
    // Bodies are never part of the wire format; drop anything that looks like one.
    if let Some(obj) = entry.as_object_mut() {
        obj.remove("body");
        obj.remove("requestBody");
        obj.remove("responseBody");
    }
}

/// Whether an HTTP entry is an error for the viewer filter (status ≥ 400
/// or any state other than `completed`).
pub fn is_http_error(entry: &Value) -> bool {
    entry["status"].as_u64().is_some_and(|s| s >= 400) || entry["state"].as_str().is_some_and(|s| s != "completed")
}

// ---------------------------------------------------------------------------
// Trace model
// ---------------------------------------------------------------------------

fn now_epoch_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64).unwrap_or(0)
}

fn flow_trace(record: &FlowRunRecord) -> Value {
    let t = record.trace.clone().unwrap_or_else(|| json!({}));
    let steps = t.get("steps").cloned().unwrap_or_else(|| record.raw_json["steps"].clone());
    let mut flow = json!({
        "name": record.name,
        "path": record.path,
        "status": if record.passed() {
            "passed"
        } else if record.not_run() {
            "skipped"
        } else {
            "failed"
        },
        "startEpochMs": record.start_epoch_ms,
        "totalMs": record.duration_ms,
        "lane": record.lane,
        "http": t.get("http").cloned().unwrap_or_else(|| Value::from("off")),
        "httpDropped": t.get("httpDropped").cloned().unwrap_or_else(|| Value::from(0)),
        "steps": steps,
    });
    if let Some(file) = &record.file {
        flow["file"] = Value::from(file.as_str());
    }
    if record.blackbox {
        flow["blackbox"] = Value::Bool(true);
    }
    if let Some(err) = &record.failure_message {
        flow["error"] = Value::from(err.as_str());
    }
    if let Some(summary) = record.raw_json.get("appIssueSummary") {
        flow["appIssueSummary"] = summary.clone();
    }
    flow
}

/// Builds the `tapwing.trace/1` document for one shard.
pub fn build_shard_trace(suite: &SuiteResult, opts: &TraceOpts) -> Value {
    let mut trace = json!({
        "schema": SCHEMA,
        "tapwingVersion": env!("CARGO_PKG_VERSION"),
        "createdEpochMs": now_epoch_ms(),
        "shard": {
            "index": suite.shard_index,
            "device": suite.device_description.as_deref().or(suite.device_id.as_deref()),
            "platform": suite.platform,
        },
        "options": { "screenshots": opts.screenshots.as_str() },
        "flows": suite.flows.iter().map(flow_trace).collect::<Vec<_>>(),
    });
    // Only on abnormal runs, so a normal trace is unchanged.
    if let Some(error) = &suite.error {
        trace["shard"]["error"] = Value::from(error.as_str());
    }
    if suite.flows.iter().any(|f| matches!(f.status, FlowStatus::Interrupted | FlowStatus::NotRun)) {
        trace["interrupted"] = Value::Bool(true);
    }
    trace
}

/// Paths written by [`write_shard_trace`].
#[derive(Clone, Debug)]
pub struct TraceFiles {
    pub json: PathBuf,
    pub html: PathBuf,
}

/// Writes `<stem>.json` and the self-contained `<stem>.html` into `dir`.
pub fn write_shard_trace(suite: &SuiteResult, dir: &Path, opts: &TraceOpts) -> Result<TraceFiles> {
    let trace = build_shard_trace(suite, opts);
    let stem = file_stem(suite.shard_index);
    let json_path = dir.join(format!("{stem}.json"));
    let html_path = dir.join(format!("{stem}.html"));
    crate::report::write_file_creating_parents(&json_path, &serde_json::to_string_pretty(&trace)?)?;
    crate::report::write_file_creating_parents(&html_path, &render_trace_html(&trace))?;
    Ok(TraceFiles { json: json_path, html: html_path })
}

/// Reads a screenshot and returns a downscaled (≤ [`THUMBNAIL_MAX_WIDTH`]
/// px wide) JPEG `data:` URI for inlining into the viewer.
pub fn thumbnail_data_uri(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let img = if img.width() > THUMBNAIL_MAX_WIDTH {
        img.resize(THUMBNAIL_MAX_WIDTH, u32::MAX, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 72)
        .encode_image(&image::DynamicImage::ImageRgb8(rgb))
        .ok()?;
    Some(format!("data:image/jpeg;base64,{}", crate::report::html::encode_base64(&out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_url_drops_userinfo_fragment_and_redacts_secrets() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXZhbHVl";
        let url = format!(
            "https://user:pw@api.example.test:8443/v1/users/{jwt}/orders?page=2&access_token=s3cr3t&Api-Key=k&q=shoes&mail=a%40b.cz#frag"
        );
        assert_eq!(
            sanitize_url(&url),
            "https://api.example.test:8443/v1/users/<redacted>/orders?page=2&access_token=<redacted>&Api-Key=<redacted>&q=shoes&mail=<redacted>"
        );
        // Idempotent on already-redacted URLs.
        let once = sanitize_url(&url);
        assert_eq!(sanitize_url(&once), once);
        assert_eq!(sanitize_url("http://127.0.0.1:9/x"), "http://127.0.0.1:9/x");
        assert_eq!(sanitize_url("/relative?code=1"), "/relative?code=<redacted>");
    }

    #[test]
    fn urls_inside_header_values_are_sanitized() {
        // An absolute redirect target, a relative one, and a referer.
        assert_eq!(
            sanitize_header_value("https://app.example/cb?code=AbC123&state=xyz&page=2"),
            "https://app.example/cb?code=<redacted>&state=xyz&page=2"
        );
        assert_eq!(sanitize_header_value("/cb?code=AbC123&next=home"), "/cb?code=<redacted>&next=home");
        assert_eq!(
            sanitize_header_value("https://user:pw@x.example/reset?token=t0k#frag"),
            "https://x.example/reset?token=<redacted>"
        );
        // URLs embedded in a larger value keep their surroundings.
        assert_eq!(
            sanitize_header_value("<https://api.example/items?access_token=s3cret&page=2>; rel=\"next\""),
            "<https://api.example/items?access_token=<redacted>&page=2>; rel=\"next\""
        );
        assert_eq!(
            sanitize_header_value("0; url=http://h.example/p?password=hunter2"),
            "0; url=http://h.example/p?password=<redacted>"
        );
        // Values without a URL are untouched, request ids included.
        for plain in ["*/*", "application/json; charset=utf-8", "9f1c2a7e-0b1d-4c55-9d8e-6a1f0c2b3d4e", ""] {
            assert_eq!(sanitize_header_value(plain), plain);
        }
        // A second pass changes nothing, and still sanitizes what follows a marker.
        let once = sanitize_header_value("https://h.example/p?code=abc&token=def");
        assert_eq!(sanitize_header_value(&once), once);
        assert_eq!(
            sanitize_header_value("https://h.example/p?code=<redacted>&token=def"),
            "https://h.example/p?code=<redacted>&token=<redacted>"
        );

        let mut entry = json!({
            "requestHeaders": { "referer": "https://x.example/reset?token=t0k", "accept": "*/*" },
            "responseHeaders": { "location": "/cb?code=AbC123", "content-length": "0" },
        });
        sanitize_http_entry(&mut entry);
        assert_eq!(entry["requestHeaders"]["referer"], "https://x.example/reset?token=<redacted>");
        assert_eq!(entry["requestHeaders"]["accept"], "*/*");
        assert_eq!(entry["responseHeaders"]["location"], "/cb?code=<redacted>");
        assert_eq!(entry["responseHeaders"]["content-length"], "0");
    }

    #[test]
    fn sanitize_http_entry_redacts_headers_errors_and_bodies() {
        let mut entry = json!({
            "url": "http://h/p?token=abc",
            "requestHeaders": { "authorization": "Bearer x", "x-custom-token": "t", "accept": "*/*" },
            "responseHeaders": { "set-cookie": "sid=1", "content-type": "application/json" },
            "error": "HttpException: Connection closed, uri = http://h/p?token=abc",
            "responseBody": "{\"secret\":1}",
        });
        sanitize_http_entry(&mut entry);
        assert_eq!(entry["url"], "http://h/p?token=<redacted>");
        assert_eq!(entry["requestHeaders"]["authorization"], REDACTED);
        assert_eq!(entry["requestHeaders"]["x-custom-token"], REDACTED);
        assert_eq!(entry["requestHeaders"]["accept"], "*/*");
        assert_eq!(entry["responseHeaders"]["set-cookie"], REDACTED);
        assert_eq!(entry["responseHeaders"]["content-type"], "application/json");
        assert_eq!(entry["error"], "HttpException: Connection closed, uri = http://h/p?…");
        assert!(entry.get("responseBody").is_none());
    }

    #[test]
    fn token_detection_and_sensitive_names() {
        assert!(!looks_like_token("shoes"));
        assert!(!looks_like_token("products"));
        assert!(!looks_like_token(REDACTED));
        assert!(looks_like_token("9f86d081884c7d659a2feaa0c55ad015"));
        assert!(looks_like_token("a@b.cz"));
        for h in ["Authorization", "Cookie", "Set-Cookie", "X-Api-Key", "X-Auth-Token", "x-xsrf-token"] {
            assert!(is_sensitive_header(h), "{h}");
        }
        assert!(!is_sensitive_header("content-type"));
        assert!(is_sensitive_query_key("refresh_token"));
        assert!(!is_sensitive_query_key("page"));
    }

    #[test]
    fn file_stems_and_asset_prefixes_never_collide_across_shards() {
        assert_eq!(file_stem(None), "trace");
        assert_eq!(file_stem(Some(0)), "trace-shard-1");
        assert_eq!(file_stem(Some(1)), "trace-shard-2");
        assert_eq!(asset_prefix(None, 3), "f03_");
        assert_eq!(asset_prefix(Some(1), 3), "s2_f03_");
        let a = next_flow_ordinal();
        assert!(next_flow_ordinal() > a);
    }

    #[test]
    fn http_error_classification() {
        assert!(!is_http_error(&json!({"status": 200, "state": "completed"})));
        assert!(is_http_error(&json!({"status": 404, "state": "completed"})));
        assert!(is_http_error(&json!({"state": "failed"})));
        assert!(is_http_error(&json!({"state": "inFlight"})));
    }
}
