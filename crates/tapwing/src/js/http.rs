//! QuickJS `http.*` client bindings and RFC 7578 multipart form encoding (`GraalJsHttp.kt`, `HttpUtils.kt`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, Object, Value};

use crate::random;

use super::{BridgeState, catch_js_error, js_value_to_maestro_string, throw_js_error};

/// Builds a shared `ureq::Agent` configured for Maestro parity:
/// - HTTP/1.1 connection pooling & keep-alive enabled
/// - `http_status_as_error(false)` so 4xx/5xx return `{ ok: false, status, ... }`
/// - 5-minute timeouts matching `GraalJsEngine.kt:31-33`
pub fn default_http_agent() -> Arc<ureq::Agent> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(300)))
        .build();
    Arc::new(config.into())
}

pub(super) fn install_http<'js>(ctx: &Ctx<'js>, bridge: Rc<RefCell<BridgeState>>) -> Result<()> {
    let http_obj = Object::new(ctx.clone()).map_err(|e| catch_js_error(ctx, e))?;

    for (js_name, fixed_method) in [
        ("get", Some("GET")),
        ("post", Some("POST")),
        ("put", Some("PUT")),
        ("delete", Some("DELETE")),
        ("request", None),
    ] {
        let b = Rc::clone(&bridge);
        let f = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, url: String, params: Opt<Value<'js>>| -> rquickjs::Result<Object<'js>> {
                let params_obj = params.0.and_then(|v| v.into_object());
                let method = match fixed_method {
                    Some(m) => m.to_string(),
                    None => params_obj
                        .as_ref()
                        .and_then(|o| o.get::<_, Option<String>>("method").ok().flatten())
                        .unwrap_or_else(|| "GET".to_string()),
                };
                execute_js_http(&ctx, &b, &url, &method, params_obj.as_ref())
            },
        )
        .map_err(|e| catch_js_error(ctx, e))?;
        http_obj.set(js_name, f).map_err(|e| catch_js_error(ctx, e))?;
    }

    ctx.globals().set("http", http_obj).map_err(|e| catch_js_error(ctx, e))?;
    Ok(())
}

fn execute_js_http<'js>(
    ctx: &Ctx<'js>,
    bridge: &Rc<RefCell<BridgeState>>,
    url: &str,
    method: &str,
    params: Option<&Object<'js>>,
) -> rquickjs::Result<Object<'js>> {
    let t0 = Instant::now();
    let (agent, script_dir) = {
        let b = bridge.borrow();
        (Arc::clone(&b.http_agent), b.current_script_dir.clone())
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut body_str: Option<String> = None;
    let mut multipart_payload: Option<(String, Vec<u8>)> = None;

    if let Some(p) = params {
        if let Ok(Some(hdr_val)) = p.get::<_, Option<Value<'js>>>("headers")
            && let Some(hdr_obj) = hdr_val.into_object()
        {
            for (k, v) in hdr_obj.props::<String, Value<'js>>().flatten() {
                let s = js_value_to_maestro_string(ctx, v).unwrap_or_default();
                headers.push((k, s));
            }
        }

        if let Ok(Some(mp_val)) = p.get::<_, Option<Value<'js>>>("multipartForm")
            && let Some(mp_obj) = mp_val.into_object()
        {
            let mp =
                build_multipart_body(ctx, &mp_obj, script_dir.as_deref()).map_err(|msg| throw_js_error(ctx, &msg))?;
            multipart_payload = Some(mp);
        } else if let Ok(Some(b_val)) = p.get::<_, Option<Value<'js>>>("body")
            && !b_val.is_undefined()
            && !b_val.is_null()
        {
            body_str = Some(js_value_to_maestro_string(ctx, b_val).unwrap_or_default());
        }
    }

    let method_upper = method.to_ascii_uppercase();
    let mut builder = ureq::http::Request::builder().method(method_upper.as_str()).uri(url);
    for (k, v) in &headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    let response = if let Some((content_type, bytes)) = multipart_payload {
        builder = builder.header("Content-Type", content_type);
        let req = builder.body(bytes).map_err(|e| throw_js_error(ctx, &format!("invalid HTTP request: {e}")))?;
        agent.run(req)
    } else if let Some(b) = body_str {
        let req = builder.body(b).map_err(|e| throw_js_error(ctx, &format!("invalid HTTP request: {e}")))?;
        agent.run(req)
    } else {
        let req = builder.body(()).map_err(|e| throw_js_error(ctx, &format!("invalid HTTP request: {e}")))?;
        agent.run(req)
    };

    let mut resp = response.map_err(|e| throw_js_error(ctx, &format!("HTTP {method_upper} {url} failed: {e}")))?;
    let status = resp.status().as_u16();
    let ok = (200..300).contains(&status);

    // Join repeated response headers with `,` and lowercase header names (`GraalJsHttp.kt:111-116`).
    let mut header_map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, val) in resp.headers() {
        let key = name.as_str().to_ascii_lowercase();
        let v = val.to_str().unwrap_or_default().to_string();
        header_map.entry(key).or_default().push(v);
    }

    let resp_body =
        resp.body_mut().read_to_string().map_err(|e| throw_js_error(ctx, &format!("reading HTTP body failed: {e}")))?;

    {
        let mut b = bridge.borrow_mut();
        b.http_requests += 1;
        b.http_ms += t0.elapsed().as_secs_f64() * 1000.0;
    }

    let js_headers = Object::new(ctx.clone())?;
    for (k, vals) in header_map {
        js_headers.set(k.as_str(), vals.join(","))?;
    }

    let js_resp = Object::new(ctx.clone())?;
    js_resp.set("ok", ok)?;
    js_resp.set("status", i32::from(status))?;
    js_resp.set("body", resp_body)?;
    js_resp.set("headers", js_headers)?;
    Ok(js_resp)
}

/// Builds an RFC 7578 `multipart/form-data` payload matching `HttpUtils.kt:10-39`.
fn build_multipart_body<'js>(
    ctx: &Ctx<'js>,
    mp_obj: &Object<'js>,
    script_dir: Option<&Path>,
) -> std::result::Result<(String, Vec<u8>), String> {
    let boundary = format!("----TapwingBoundary{}", random::faker_number_digits(16));
    let mut out = Vec::new();

    for entry in mp_obj.props::<String, Value<'js>>() {
        let (field_name, value) = entry.map_err(|e| format!("reading multipartForm entry: {e}"))?;
        if let Some(sub_obj) = value.as_object()
            && let Ok(Some(fp_val)) = sub_obj.get::<_, Option<Value<'js>>>("filePath")
            && !fp_val.is_undefined()
            && !fp_val.is_null()
        {
            let file_path_str = js_value_to_maestro_string(ctx, fp_val).map_err(|e| e.to_string())?;
            let resolved = resolve_multipart_path(&file_path_str, script_dir);
            let file_bytes = std::fs::read(&resolved)
                .map_err(|e| format!("Failed to read multipart file {}: {e}", resolved.display()))?;
            let file_name = resolved.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let media_type = sub_obj
                .get::<_, Option<Value<'js>>>("mediaType")
                .ok()
                .flatten()
                .filter(|v| !v.is_undefined() && !v.is_null())
                .and_then(|v| js_value_to_maestro_string(ctx, v).ok())
                .unwrap_or_else(|| "application/octet-stream".to_string());

            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            out.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{file_name}\"\r\n")
                    .as_bytes(),
            );
            out.extend_from_slice(format!("Content-Type: {media_type}\r\n\r\n").as_bytes());
            out.extend_from_slice(&file_bytes);
            out.extend_from_slice(b"\r\n");
        } else {
            let text = js_value_to_maestro_string(ctx, value).map_err(|e| e.to_string())?;
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            out.extend_from_slice(format!("Content-Disposition: form-data; name=\"{field_name}\"\r\n\r\n").as_bytes());
            out.extend_from_slice(text.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), out))
}

fn resolve_multipart_path(file_path: &str, script_dir: Option<&Path>) -> PathBuf {
    let p = Path::new(file_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(dir) = script_dir {
        dir.join(p)
    } else {
        p.to_path_buf()
    }
}
