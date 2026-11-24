//! Embedded QuickJS-ng (`rquickjs`) JavaScript engine matching Maestro 2.12.1
//! (`GraalJsEngine.kt`, `GraalJsHttp.kt`, `Env.kt`, `HttpUtils.kt`).
//!
//! Performance architecture:
//! 1. **Lazy QuickJS initialization + zero-JS fast path:** flows without JS
//!    expressions never spin up a QuickJS runtime; strings without `${` return
//!    immediately; simple `${VAR}` (in env) and `${maestro.copiedText}` /
//!    `${maestro.platform}` (before JS mutates `maestro`) resolve directly in Rust.
//! 2. **Compiled function caching:** bootstrap helpers are compiled once per
//!    context; repeated `${...}` expressions and `runScript` files are compiled
//!    into cached `Persistent<Function>` bytecode objects so loops (`repeat`/
//!    `while`) skip re-parsing on subsequent iterations.
//! 3. **Shared persistent HTTP client:** a single `Arc<ureq::Agent>` with
//!    connection pooling & keep-alive is reused across the entire run.
//! 4. **Lightweight env scope stack:** a single shared QuickJS `Context` is
//!    reused for the whole flow and all sub-flows; `enter_env_scope()` /
//!    `leave_env_scope()` save/restore only the env map delta rather than
//!    cloning JS contexts.
//! 5. **Native Rust `faker` + built-in C `JSON.parse`:** `faker` providers call
//!    directly into `crate::random` with zero JS library overhead.

mod faker;
mod http;

pub use http::default_http_agent;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use anyhow::Result;
use fancy_regex::Regex;
use rquickjs::function::Rest;
use rquickjs::object::Filter;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Function, Object, Persistent, Runtime, Type, Value};

use crate::backend::JsError;
use crate::interrupt::Watch;

/// Permanent global binding keys that env variables must never overwrite
/// (`GraalJsEngine.kt:47-50`).
const PERMANENT_BINDING_KEYS: &[&str] = &["http", "faker", "output", "maestro", "json", "relativePoint", "console"];

/// Exact `${...}` interpolation regex from `Env.kt:11`.
static INTERPOLATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?<!\\)\$\{([^$]*)\}").expect("valid interpolation regex"));

/// Exact escaped `\${...}` unescaping regex from `Env.kt:23`.
static ESCAPED_INTERPOLATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\\$\{([^$]*)\}").expect("valid escaped interpolation regex"));

/// Bootstrap JS executed once when the lazy QuickJS context is first created.
/// Sets up:
/// - global prototype Proxy so undeclared identifiers evaluate to `undefined`
///   instead of throwing `ReferenceError` (`GraalJsEngine.kt:200-206`)
/// - `json(text)` via native C `JSON.parse`
/// - `relativePoint(x, y)` matching `GraalJsEngine.kt:210-214`
/// - persistent `output` and `maestro` objects
const BOOTSTRAP_JS: &str = r#"
Object.setPrototypeOf(globalThis, new Proxy(Object.prototype, {
    has(target, key) {
        return true;
    }
}));
function json(text) {
    return JSON.parse(text);
}
function relativePoint(x, y) {
    var xPercent = Math.ceil(x * 100) + '%';
    var yPercent = Math.ceil(y * 100) + '%';
    return xPercent + ',' + yPercent;
}
globalThis.output = {};
globalThis.maestro = { platform: 'unknown' };
"#;

/// Heap cap of the QuickJS runtime. Maestro's GraalJS has no script limit of
/// its own and is bounded only by the JVM heap, so this is far above what any
/// flow script holds (HTTP bodies, JSON fixtures) and only turns a runaway
/// allocation into an error instead of an OOM kill of the whole run.
const MEMORY_LIMIT_BYTES: usize = 1024 * 1024 * 1024;

/// Native stack QuickJS may use before it throws `Maximum call stack size
/// exceeded`. This is the QuickJS-ng default, pinned here so a dependency bump
/// cannot move it; it must stay below the 2 MiB stack of a spawned Rust thread.
const MAX_STACK_BYTES: usize = 1024 * 1024;

/// Message of the [`JsError`] for a thrown bare `null`.
const THROWN_NULL: &str = "null";

/// Resource limits applied to the QuickJS runtime when it is first created.
#[derive(Clone, Copy, Debug)]
struct JsLimits {
    memory_bytes: usize,
    max_stack_bytes: usize,
}

impl Default for JsLimits {
    fn default() -> Self {
        Self { memory_bytes: MEMORY_LIMIT_BYTES, max_stack_bytes: MAX_STACK_BYTES }
    }
}

/// Counters and timing accumulated across JS and `http.*` calls.
#[derive(Clone, Debug, Default)]
pub struct JsMetrics {
    pub eval_count: u64,
    pub fast_path_hits: u64,
    pub cache_hits: u64,
    pub eval_ms: f64,
    pub http_requests: u64,
    pub http_ms: f64,
    pub runtime_initialized: bool,
}

impl JsMetrics {
    pub fn delta_since(&self, before: &Self) -> Self {
        Self {
            eval_count: self.eval_count.saturating_sub(before.eval_count),
            fast_path_hits: self.fast_path_hits.saturating_sub(before.fast_path_hits),
            cache_hits: self.cache_hits.saturating_sub(before.cache_hits),
            eval_ms: (self.eval_ms - before.eval_ms).max(0.0),
            http_requests: self.http_requests.saturating_sub(before.http_requests),
            http_ms: (self.http_ms - before.http_ms).max(0.0),
            runtime_initialized: self.runtime_initialized,
        }
    }
}

/// Shared mutable state bridged between Rust and QuickJS host callbacks.
struct BridgeState {
    http_agent: Arc<ureq::Agent>,
    interrupt: Watch,
    current_script_dir: Option<PathBuf>,
    log_callback: Option<Box<dyn FnMut(String)>>,
    http_requests: u64,
    http_ms: f64,
}

/// Lazily-created QuickJS runtime, context, and compiled bytecode caches.
///
/// Field declaration order is critical for drop safety: caches holding
/// `Persistent<Function>` MUST drop before `context`, which MUST drop before
/// `_runtime` so `JS_FreeRuntime` sees zero leaked GC objects.
struct QuickJsState {
    /// Compiled expression functions: `(function(){ "use strict"; return (\n<expr>\n); })`.
    expr_cache: HashMap<u64, CompiledEntry>,
    /// Compiled statement-only functions for `runScript` where return value is ignored.
    script_cache: HashMap<u64, Persistent<Function<'static>>>,
    /// Own property keys present on `globalThis` right after bootstrap,
    /// used to prune script-added globals without disturbing JS built-ins.
    initial_globals: HashSet<String>,
    context: Context,
    _runtime: Runtime,
}

enum CompiledEntry {
    /// Fast direct compiled expression function (no `eval()` overhead).
    DirectExpr(Persistent<Function<'static>>),
    /// Multi-statement fallback that preserves `eval()` completion-value semantics.
    EvalWrapper(Persistent<Function<'static>>),
}

/// Maestro-compatible JavaScript engine backed by lazy QuickJS-ng (`rquickjs`).
pub struct JsEngine {
    platform: String,
    copied_text: Option<String>,
    env_binding: HashMap<String, String>,
    /// Lightweight delta stack: each entry stores `(key, previous_value)` to
    /// restore on `leave_env_scope()` without copying the whole map.
    env_scope_stack: Vec<Vec<(String, Option<String>)>>,
    bridge: Rc<RefCell<BridgeState>>,
    qjs: Option<QuickJsState>,
    metrics: JsMetrics,
    interrupt: Watch,
    limits: JsLimits,
    closed: bool,
}

impl JsEngine {
    pub fn new(platform: impl Into<String>) -> Self {
        Self::with_http_agent(platform, default_http_agent())
    }

    pub fn with_http_agent(platform: impl Into<String>, http_agent: Arc<ureq::Agent>) -> Self {
        Self::with_interrupt(platform, http_agent, Watch::process())
    }

    fn with_interrupt(platform: impl Into<String>, http_agent: Arc<ureq::Agent>, interrupt: Watch) -> Self {
        Self {
            platform: platform.into().to_lowercase(),
            copied_text: None,
            env_binding: HashMap::new(),
            env_scope_stack: Vec::new(),
            bridge: Rc::new(RefCell::new(BridgeState {
                http_agent,
                interrupt,
                current_script_dir: None,
                log_callback: None,
                http_requests: 0,
                http_ms: 0.0,
            })),
            qjs: None,
            metrics: JsMetrics::default(),
            interrupt,
            limits: JsLimits::default(),
            closed: false,
        }
    }

    /// Registers a callback invoked on every `console.log(...)` line.
    pub fn set_log_callback<F: FnMut(String) + 'static>(&mut self, cb: F) {
        self.bridge.borrow_mut().log_callback = Some(Box::new(cb));
    }

    /// Clears the current `console.log` callback.
    pub fn clear_log_callback(&mut self) {
        self.bridge.borrow_mut().log_callback = None;
    }

    /// Releases the underlying QuickJS context & runtime (`Orchestra.kt:298`).
    pub fn close(&mut self) {
        self.qjs = None;
        self.closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether the QuickJS runtime has been lazily initialized.
    pub fn is_runtime_initialized(&self) -> bool {
        self.qjs.is_some()
    }

    /// Snapshot of accumulated JS & HTTP metrics.
    pub fn metrics(&self) -> JsMetrics {
        let bridge = self.bridge.borrow();
        JsMetrics {
            http_requests: bridge.http_requests,
            http_ms: bridge.http_ms,
            runtime_initialized: self.qjs.is_some(),
            ..self.metrics.clone()
        }
    }

    /// Saves the current env scope so subsequent `put_env` calls can be undone
    /// cheaply in `leave_env_scope()` without cloning JS contexts.
    pub fn enter_env_scope(&mut self) {
        self.env_scope_stack.push(Vec::new());
    }

    /// Restores all env keys modified since the matching `enter_env_scope()`.
    pub fn leave_env_scope(&mut self) {
        if let Some(deltas) = self.env_scope_stack.pop() {
            for (k, prev) in deltas.into_iter().rev() {
                match prev {
                    Some(v) => {
                        self.env_binding.insert(k, v);
                    }
                    None => {
                        self.env_binding.remove(&k);
                    }
                }
            }
        }
    }

    /// Sets an environment variable in the current env scope.
    pub fn put_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        let prev = self.env_binding.insert(key.clone(), value);
        if let Some(frame) = self.env_scope_stack.last_mut() {
            frame.push((key, prev));
        }
    }

    /// Looks up an env variable in the current scope.
    pub fn get_env(&self, key: &str) -> Option<&str> {
        self.env_binding.get(key).map(String::as_str)
    }

    /// Updates `maestro.copiedText` (synced to QuickJS if initialized).
    pub fn set_copied_text(&mut self, text: Option<String>) {
        self.copied_text = text.clone();
        if let Some(qjs) = &self.qjs {
            qjs.context.with(|ctx| {
                if let Ok(maestro) = ctx.globals().get::<_, Object>("maestro") {
                    match &text {
                        Some(s) => {
                            let _ = maestro.set("copiedText", s.as_str());
                        }
                        None => {
                            let _ = maestro.set("copiedText", Value::new_undefined(ctx.clone()));
                        }
                    }
                }
            });
        }
    }

    /// Evaluates Maestro `${...}` interpolation in `input` (`Env.kt:10-26`).
    ///
    /// Fast paths avoid QuickJS entirely when:
    /// - `input` does not contain `${`
    /// - expression is blank (`${}` -> `""`)
    /// - expression is a simple env variable identifier `${VAR}` present in `env_binding`
    /// - expression is `${maestro.copiedText}` or `${maestro.platform}` while QuickJS is
    ///   still uninitialized
    pub fn evaluate_scripts(&mut self, input: &str) -> Result<String> {
        if !input.contains("${") {
            return Ok(input.to_string());
        }

        // Replace unescaped `${...}` expressions (matching `(?<!\\)\$\{([^$]*)\}`).
        let mut out = String::with_capacity(input.len());
        let mut last_end = 0usize;
        let matches: Vec<(usize, usize, String)> = INTERPOLATION_RE
            .captures_iter(input)
            .filter_map(|c| c.ok())
            .filter_map(|caps| {
                let m = caps.get(0)?;
                let body = caps.get(1).map(|g| g.as_str().to_string()).unwrap_or_default();
                Some((m.start(), m.end(), body))
            })
            .collect();

        for (start, end, script) in matches {
            out.push_str(&input[last_end..start]);
            let trimmed = script.trim();
            if trimmed.is_empty() {
                self.metrics.fast_path_hits += 1;
            } else if let Some(fast) = self.try_fast_eval(trimmed) {
                self.metrics.fast_path_hits += 1;
                out.push_str(&fast);
            } else {
                let val = self.evaluate_script(&script, &HashMap::new(), "inline-script", false, None)?;
                out.push_str(&val);
            }
            last_end = end;
        }
        out.push_str(&input[last_end..]);

        // Unescape `\${...}` -> `${...}` (`Env.kt:22-25`).
        if out.contains(r"\${") {
            let unescaped = ESCAPED_INTERPOLATION_RE.replace_all(&out, |caps: &fancy_regex::Captures<'_, str>| {
                format!("${{{}}}", caps.get(1).map(|m| m.as_str()).unwrap_or(""))
            });
            Ok(unescaped.into_owned())
        } else {
            Ok(out)
        }
    }

    /// Zero-JS fast path for simple expressions whose result is guaranteed
    /// identical to GraalJS / QuickJS without invoking the JS VM.
    fn try_fast_eval(&self, trimmed: &str) -> Option<String> {
        // Simple env variable lookup: because Maestro resets non-permanent globals
        // to `env_binding` before every evaluation, an identifier present in
        // `env_binding` (and not a permanent binding or JS keyword) always
        // evaluates to its exact string value.
        if is_simple_env_ident(trimmed)
            && !PERMANENT_BINDING_KEYS.contains(&trimmed)
            && let Some(val) = self.env_binding.get(trimmed)
        {
            return Some(val.clone());
        }

        // Before QuickJS has been initialized, JS cannot have mutated `maestro`.
        if self.qjs.is_none() {
            match trimmed {
                "maestro.platform" => return Some(self.platform.clone()),
                "maestro.copiedText" => {
                    return Some(self.copied_text.clone().unwrap_or_else(|| "undefined".to_string()));
                }
                _ => {}
            }
        }
        None
    }

    /// Evaluates a JavaScript snippet or script file, returning its stringified
    /// completion value matching `GraalJsEngine.evaluateScript`.
    pub fn evaluate_script(
        &mut self,
        script: &str,
        env: &HashMap<String, String>,
        source_name: &str,
        run_in_sub_scope: bool,
        script_dir: Option<&Path>,
    ) -> Result<String> {
        self.evaluate_script_impl(script, env, source_name, run_in_sub_scope, script_dir, true)
    }

    /// Runs a `runScript` file where the return value is discarded (matching
    /// `Orchestra.kt:787`), enabling direct bytecode caching without `eval()`.
    pub fn run_script_file(
        &mut self,
        script: &str,
        env: &HashMap<String, String>,
        source_name: &str,
        script_dir: Option<&Path>,
    ) -> Result<()> {
        let _ = self.evaluate_script_impl(script, env, source_name, true, script_dir, false)?;
        Ok(())
    }

    fn evaluate_script_impl(
        &mut self,
        script: &str,
        env: &HashMap<String, String>,
        source_name: &str,
        run_in_sub_scope: bool,
        script_dir: Option<&Path>,
        need_completion_value: bool,
    ) -> Result<String> {
        let t0 = Instant::now();
        self.bridge.borrow_mut().current_script_dir = script_dir.map(Path::to_path_buf);

        if run_in_sub_scope {
            self.enter_env_scope();
        }
        for (k, v) in env {
            self.put_env(k.clone(), v.clone());
        }

        let res = self.eval_in_qjs(script, source_name, need_completion_value);

        if run_in_sub_scope {
            self.leave_env_scope();
        }
        self.metrics.eval_count += 1;
        self.metrics.eval_ms += t0.elapsed().as_secs_f64() * 1000.0;
        res
    }

    fn eval_in_qjs(&mut self, script: &str, source_name: &str, need_completion_value: bool) -> Result<String> {
        let env_snapshot = self.env_binding.clone();
        let qjs = self.ensure_qjs()?;

        let mut cache_hit = false;
        let out = qjs.context.with(|ctx| -> Result<String> {
            sync_env_bindings(&ctx, &qjs.initial_globals, &env_snapshot)?;

            if !need_completion_value {
                let key = hash_key(source_name, script);
                let func: Function<'_> = if let Some(cached) = qjs.script_cache.get(&key) {
                    cache_hit = true;
                    cached.clone().restore(&ctx).map_err(|e| catch_js_error(&ctx, e))?
                } else {
                    let wrapped = format!("(function(){{\n\"use strict\";\n{script}\n}})");
                    let compiled: Function<'_> = ctx.eval(wrapped).catch(&ctx).map_err(caught_to_anyhow)?;
                    let persistent = Persistent::save(&ctx, compiled.clone());
                    qjs.script_cache.insert(key, persistent);
                    compiled
                };
                let _: Value<'_> = func.call(()).catch(&ctx).map_err(caught_to_anyhow)?;
                return Ok("undefined".to_string());
            }

            let key = hash_key(source_name, script);
            if let Some(entry) = qjs.expr_cache.get(&key) {
                cache_hit = true;
                let func: Function<'_> = match entry {
                    CompiledEntry::DirectExpr(p) | CompiledEntry::EvalWrapper(p) => {
                        p.clone().restore(&ctx).map_err(|e| catch_js_error(&ctx, e))?
                    }
                };
                let val: Value<'_> = func.call(()).catch(&ctx).map_err(caught_to_anyhow)?;
                return js_value_to_maestro_string(&ctx, val);
            }

            // Fast compiled expression path: compile `(function(){ "use strict"; return (\n<script>\n); })`
            // once so loop expressions (`repeat`/`while`, `assertTrue`, `${...}`) run at native
            // bytecode speed with zero `eval()` parsing overhead.
            let expr_wrapped = format!("(function(){{ \"use strict\"; return (\n{script}\n); }})");
            let compiled_entry = match ctx.eval::<Function<'_>, _>(expr_wrapped) {
                Ok(func) => {
                    let persistent = Persistent::save(&ctx, func.clone());
                    qjs.expr_cache.insert(key, CompiledEntry::DirectExpr(persistent));
                    func
                }
                Err(_) => {
                    // Clear the syntax-error exception left by trying expression compilation
                    // before compiling the general IIFE+eval wrapper (which preserves exact
                    // completion-value semantics for multi-statement scripts).
                    let _ = ctx.catch();
                    let json_src = serde_json::to_string(script)?;
                    let eval_wrapped = format!("(function(){{ \"use strict\"; return eval({json_src}); }})");
                    let func: Function<'_> = ctx.eval(eval_wrapped).catch(&ctx).map_err(caught_to_anyhow)?;
                    let persistent = Persistent::save(&ctx, func.clone());
                    qjs.expr_cache.insert(key, CompiledEntry::EvalWrapper(persistent));
                    func
                }
            };

            let val: Value<'_> = compiled_entry.call(()).catch(&ctx).map_err(caught_to_anyhow)?;
            js_value_to_maestro_string(&ctx, val)
        });

        if cache_hit {
            self.metrics.cache_hits += 1;
        }
        out.map_err(|err| self.classify_eval_error(err))
    }

    /// Replaces the engine's raw failure with what actually ended the script.
    ///
    /// QuickJS reports an abort by the interrupt handler as an uncatchable
    /// `interrupted` exception and a hit memory limit as `out of memory`. At
    /// the limit it usually cannot allocate that exception either and throws a
    /// bare `null` (`JS_ThrowError2`), which nothing tells apart from a
    /// script's own `throw null`, so that message names both.
    fn classify_eval_error(&self, err: anyhow::Error) -> anyhow::Error {
        if let Err(interrupted) = self.interrupt.check() {
            return interrupted;
        }
        let limit_mib = self.limits.memory_bytes / (1024 * 1024);
        let message = match err.downcast_ref::<JsError>().map(|js| js.message.as_str()) {
            Some("out of memory" | "Allocation failed while creating object") => {
                format!("JavaScript ran out of memory (limit: {limit_mib} MiB)")
            }
            Some(THROWN_NULL) => {
                format!("JavaScript ran out of memory (limit: {limit_mib} MiB) or the script threw null")
            }
            _ => return err,
        };
        JsError::new(message, vec![]).into()
    }

    fn ensure_qjs(&mut self) -> Result<&mut QuickJsState> {
        if self.qjs.is_none() {
            let runtime =
                Runtime::new().map_err(|e| JsError::new(format!("QuickJS runtime init failed: {e}"), vec![]))?;
            runtime.set_memory_limit(self.limits.memory_bytes);
            runtime.set_max_stack_size(self.limits.max_stack_bytes);
            // Polled by QuickJS every few thousand bytecode jumps and calls, so a
            // `while (true) {}` ends on the first SIGINT/SIGTERM and the device
            // cleanup guards still run.
            let interrupt = self.interrupt;
            runtime.set_interrupt_handler(Some(Box::new(move || interrupt.is_interrupted())));
            let context = Context::full(&runtime)
                .map_err(|e| JsError::new(format!("QuickJS context init failed: {e}"), vec![]))?;

            let platform = self.platform.clone();
            let copied_text = self.copied_text.clone();
            let bridge = Rc::clone(&self.bridge);

            let initial_globals = context.with(|ctx| -> Result<HashSet<String>> {
                ctx.eval::<(), _>(BOOTSTRAP_JS).catch(&ctx).map_err(caught_to_anyhow)?;

                let globals = ctx.globals();
                if let Ok(maestro) = globals.get::<_, Object>("maestro") {
                    maestro.set("platform", platform.as_str()).map_err(|e| catch_js_error(&ctx, e))?;
                    if let Some(ct) = &copied_text {
                        maestro.set("copiedText", ct.as_str()).map_err(|e| catch_js_error(&ctx, e))?;
                    }
                }

                install_console(&ctx, Rc::clone(&bridge))?;
                http::install_http(&ctx, Rc::clone(&bridge))?;
                faker::install_faker(&ctx)?;

                // Record every own global key present after bootstrap so env syncing
                // never touches JS built-ins or permanent Maestro bindings.
                let mut initial = HashSet::new();
                for k in globals.own_keys::<String>(Filter::new().string()).flatten() {
                    initial.insert(k);
                }
                for &perm in PERMANENT_BINDING_KEYS {
                    initial.insert(perm.to_string());
                }
                Ok(initial)
            })?;

            self.qjs = Some(QuickJsState {
                _runtime: runtime,
                context,
                initial_globals,
                expr_cache: HashMap::new(),
                script_cache: HashMap::new(),
            });
            self.metrics.runtime_initialized = true;
        }
        self.qjs.as_mut().ok_or_else(|| anyhow::anyhow!("QuickJS runtime unavailable after initialization"))
    }
}

/// Syncs `env_binding` onto `globalThis`, clearing any previously-injected or
/// script-mutated non-permanent global properties (`GraalJsEngine.kt:161-166`).
fn sync_env_bindings<'js>(
    ctx: &Ctx<'js>,
    initial_globals: &HashSet<String>,
    env_binding: &HashMap<String, String>,
) -> Result<()> {
    let globals = ctx.globals();
    let existing_keys: Vec<String> = globals.own_keys::<String>(Filter::new().string()).flatten().collect();
    for key in existing_keys {
        if !initial_globals.contains(&key) && !PERMANENT_BINDING_KEYS.contains(&key.as_str()) {
            globals.remove(key.as_str()).map_err(|e| catch_js_error(ctx, e))?;
        }
    }
    for (k, v) in env_binding {
        if !PERMANENT_BINDING_KEYS.contains(&k.as_str()) {
            globals.set(k.as_str(), v.as_str()).map_err(|e| catch_js_error(ctx, e))?;
        }
    }
    Ok(())
}

/// Converts a QuickJS value to Maestro/GraalJS string representation.
///
/// Crucially, arrays return a non-empty `[...]` string (e.g. `[]` -> `"[]"`)
/// rather than JS `String([]) == ""`, matching GraalJS `Value.toString()` so
/// `when: { true: ${[]} }` evaluates as truthy (`IntegrationTest` Case 065).
fn js_value_to_maestro_string<'js>(ctx: &Ctx<'js>, val: Value<'js>) -> Result<String> {
    match val.type_of() {
        Type::Undefined | Type::Uninitialized => Ok("undefined".to_string()),
        Type::Null => Ok("null".to_string()),
        Type::Bool => Ok(if val.as_bool().unwrap_or(false) { "true" } else { "false" }.to_string()),
        Type::Int => Ok(val.as_int().unwrap_or(0).to_string()),
        Type::Float => {
            let f = val.as_float().unwrap_or(0.0);
            if f.is_nan() {
                Ok("NaN".to_string())
            } else if f.is_infinite() {
                Ok(if f.is_sign_positive() { "Infinity" } else { "-Infinity" }.to_string())
            } else if f.fract() == 0.0 && f.abs() < 1e15 {
                Ok(format!("{}", f as i64))
            } else {
                Ok(f.to_string())
            }
        }
        Type::String => Ok(val.as_string().and_then(|s| s.to_string().ok()).unwrap_or_default()),
        Type::Array => {
            if let Ok(Some(json)) = ctx.json_stringify(&val)
                && let Ok(s) = json.to_string()
            {
                return Ok(s);
            }
            Ok("[]".to_string())
        }
        _ => {
            // Invoke JS `String(val)` for objects, functions, BigInt, Symbols, etc.
            let string_fn: Function<'js> = ctx.globals().get("String").map_err(|e| catch_js_error(ctx, e))?;
            let s: String = string_fn.call((val,)).catch(ctx).map_err(caught_to_anyhow)?;
            Ok(s)
        }
    }
}

fn format_console_arg<'js>(ctx: &Ctx<'js>, val: Value<'js>) -> String {
    match val.type_of() {
        Type::Undefined | Type::Uninitialized => "undefined".to_string(),
        Type::Null => "null".to_string(),
        Type::String => val.as_string().and_then(|s| s.to_string().ok()).unwrap_or_default(),
        Type::Object | Type::Array => ctx
            .json_stringify(&val)
            .ok()
            .flatten()
            .and_then(|s| s.to_string().ok())
            .unwrap_or_else(|| js_value_to_maestro_string(ctx, val).unwrap_or_default()),
        _ => js_value_to_maestro_string(ctx, val).unwrap_or_default(),
    }
}

fn install_console<'js>(ctx: &Ctx<'js>, bridge: Rc<RefCell<BridgeState>>) -> Result<()> {
    let console = Object::new(ctx.clone()).map_err(|e| catch_js_error(ctx, e))?;
    for method in ["log", "info", "warn", "error", "debug"] {
        let b = Rc::clone(&bridge);
        let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
            let line = args.0.into_iter().map(|v| format_console_arg(&ctx, v)).collect::<Vec<_>>().join(" ");
            if let Some(cb) = b.borrow_mut().log_callback.as_mut() {
                cb(line);
            }
            Ok(())
        })
        .map_err(|e| catch_js_error(ctx, e))?;
        console.set(method, f).map_err(|e| catch_js_error(ctx, e))?;
    }
    ctx.globals().set("console", console).map_err(|e| catch_js_error(ctx, e))?;
    Ok(())
}

fn throw_js_error<'js>(ctx: &Ctx<'js>, message: &str) -> rquickjs::Error {
    rquickjs::Exception::throw_message(ctx, message)
}

fn catch_js_error<'js>(ctx: &Ctx<'js>, err: rquickjs::Error) -> anyhow::Error {
    caught_to_anyhow(CaughtError::from_error(ctx, err))
}

fn caught_to_anyhow<'js>(caught: CaughtError<'js>) -> anyhow::Error {
    match caught {
        CaughtError::Exception(ex) => {
            let msg = ex.message().unwrap_or_else(|| "(no message)".to_string());
            let stack = ex
                .stack()
                .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
                .unwrap_or_default();
            JsError::new(msg, stack).into()
        }
        CaughtError::Value(v) if v.is_null() => JsError::new(THROWN_NULL, vec![]).into(),
        CaughtError::Value(v) => JsError::new(format!("{v:?}"), vec![]).into(),
        CaughtError::Error(e) => JsError::new(e.to_string(), vec![]).into(),
    }
}

fn hash_key(source_name: &str, script: &str) -> u64 {
    let mut h = DefaultHasher::new();
    source_name.hash(&mut h);
    0xffu8.hash(&mut h);
    script.hash(&mut h);
    h.finish()
}

fn is_simple_env_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return false;
    }
    !matches!(s, "true" | "false" | "null" | "undefined" | "NaN" | "Infinity" | "this" | "globalThis")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    /// Minimal ephemeral HTTP/1.1 test server (with keep-alive support) for unit tests.
    fn spawn_test_server<F>(handler: F, max_requests: usize) -> (u16, thread::JoinHandle<()>)
    where
        F: Fn(&str, &str, &HashMap<String, String>, &[u8]) -> (u16, Vec<(String, String)>, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let mut handled = 0;
            while handled < max_requests {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                while handled < max_requests {
                    let mut req_line = String::new();
                    match reader.read_line(&mut req_line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let mut parts = req_line.split_whitespace();
                    let method = parts.next().unwrap_or("GET").to_string();
                    let path = parts.next().unwrap_or("/").to_string();

                    let mut headers = HashMap::new();
                    let mut content_len: usize = 0;
                    let mut close_conn = false;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
                            break;
                        }
                        if let Some((k, v)) = line.split_once(':') {
                            let kl = k.trim().to_ascii_lowercase();
                            let vt = v.trim().to_string();
                            if kl == "content-length" {
                                content_len = vt.parse().unwrap_or(0);
                            } else if kl == "connection" && vt.eq_ignore_ascii_case("close") {
                                close_conn = true;
                            }
                            headers.insert(kl, vt);
                        }
                    }

                    let mut body = vec![0u8; content_len];
                    if content_len > 0 {
                        let _ = reader.read_exact(&mut body);
                    }

                    let (status, resp_headers, resp_body) = handler(&method, &path, &headers, &body);
                    let status_text = match status {
                        200 => "OK",
                        201 => "Created",
                        404 => "Not Found",
                        _ => "Status",
                    };
                    let mut resp = format!("HTTP/1.1 {status} {status_text}\r\n");
                    for (k, v) in resp_headers {
                        resp.push_str(&format!("{k}: {v}\r\n"));
                    }
                    resp.push_str(&format!("Content-Length: {}\r\n", resp_body.len()));
                    resp.push_str("Connection: keep-alive\r\n\r\n");
                    resp.push_str(&resp_body);
                    let _ = writer.write_all(resp.as_bytes());
                    let _ = writer.flush();
                    handled += 1;
                    if close_conn {
                        break;
                    }
                }
            }
        });
        (port, handle)
    }

    #[test]
    fn lazy_init_and_zero_js_fast_paths() {
        let mut js = JsEngine::new("android");
        js.put_env("USER", "alice");
        js.set_copied_text(Some("secret".into()));

        // Plain string, simple env var, maestro.copiedText, maestro.platform, escaped \${...}
        assert_eq!(js.evaluate_scripts("plain text").unwrap(), "plain text");
        assert_eq!(js.evaluate_scripts("hi ${USER}!").unwrap(), "hi alice!");
        assert_eq!(js.evaluate_scripts("${maestro.copiedText}").unwrap(), "secret");
        assert_eq!(js.evaluate_scripts("${maestro.platform}").unwrap(), "android");
        assert_eq!(js.evaluate_scripts(r"\${USER} is ${USER}").unwrap(), "${USER} is alice");
        assert_eq!(js.evaluate_scripts("${}").unwrap(), "");
        assert!(!js.is_runtime_initialized(), "QuickJS must remain uninitialized on fast paths");

        // Now trigger actual JS evaluation
        assert_eq!(js.evaluate_scripts("${1 + 2}").unwrap(), "3");
        assert!(js.is_runtime_initialized());
    }

    #[test]
    fn graal_js_engine_parity_tests() {
        let mut js = JsEngine::new("ios");

        // Allow redefinitions of const across evaluations
        js.evaluate_script("const foo = null", &HashMap::new(), "test", false, None).unwrap();
        js.evaluate_script("const foo = null", &HashMap::new(), "test", false, None).unwrap();

        // Variables cannot leak between evaluations
        js.evaluate_script("const bar = 'bar'", &HashMap::new(), "test", false, None).unwrap();
        assert_eq!(js.evaluate_script("bar", &HashMap::new(), "test", false, None).unwrap(), "undefined");

        // Undeclared variables are falsy & support default values
        assert_eq!(js.evaluate_script("!!undeclaredVar", &HashMap::new(), "test", false, None).unwrap(), "false");
        assert_eq!(js.evaluate_scripts("${UNDECLARED || 'fallback'}").unwrap(), "fallback");

        // Backslash and newline preserved
        js.set_copied_text(Some("\\\n".into()));
        js.put_env("FOO", "\\\n");
        assert_eq!(
            js.evaluate_script("maestro.copiedText + FOO", &HashMap::new(), "test", false, None).unwrap(),
            "\\\n\\\n"
        );

        // parseInt
        assert_eq!(js.evaluate_script("parseInt('1')", &HashMap::new(), "test", false, None).unwrap(), "1");

        // Sandboxing: require('fs') throws JsError
        let err = js.evaluate_script("require('fs')", &HashMap::new(), "test", false, None).unwrap_err();
        assert!(err.is::<JsError>());

        // Output persistence for primitives, arrays, objects
        js.evaluate_script("output.myValue = 'test'", &HashMap::new(), "test", false, None).unwrap();
        assert_eq!(js.evaluate_script("output.myValue", &HashMap::new(), "test", false, None).unwrap(), "test");

        js.evaluate_script("output.list = [1, 2, 3]", &HashMap::new(), "test", false, None).unwrap();
        assert_eq!(js.evaluate_script("output.list.length", &HashMap::new(), "test", false, None).unwrap(), "3");
        assert_eq!(js.evaluate_script("output.list[0]", &HashMap::new(), "test", false, None).unwrap(), "1");

        js.evaluate_script("output.user = { name: 'Alice', age: 30 }", &HashMap::new(), "test", false, None).unwrap();
        assert_eq!(js.evaluate_script("output.user.name", &HashMap::new(), "test", false, None).unwrap(), "Alice");
        assert_eq!(js.evaluate_script("output.user.age", &HashMap::new(), "test", false, None).unwrap(), "30");

        // Empty array stringifies to non-blank "[]" (Case 065)
        assert_eq!(js.evaluate_scripts("${[]}").unwrap(), "[]");

        // relativePoint & json helpers
        assert_eq!(js.evaluate_scripts("${relativePoint(0.25, 0.25)}").unwrap(), "25%,25%");
        assert_eq!(js.evaluate_scripts(r#"${json('{"a": 42}').a}"#).unwrap(), "42");

        // Static bindings cannot be overridden by env vars
        js.put_env("output", "should_not_override");
        js.put_env("maestro", "should_not_override");
        assert_eq!(js.evaluate_script("output.myValue", &HashMap::new(), "test", false, None).unwrap(), "test");
        assert_eq!(js.evaluate_script("maestro.platform", &HashMap::new(), "test", false, None).unwrap(), "ios");
    }

    #[test]
    fn env_scope_isolation_and_subscope_mutation_guard() {
        let mut js = JsEngine::new("android");
        js.put_env("ROOT_VAR", "root_value");
        js.put_env("MY_VAR", "original");

        js.enter_env_scope();
        js.put_env("SCOPED_VAR", "scoped_value");
        assert_eq!(js.evaluate_script("ROOT_VAR", &HashMap::new(), "t", false, None).unwrap(), "root_value");
        assert_eq!(js.evaluate_script("SCOPED_VAR", &HashMap::new(), "t", false, None).unwrap(), "scoped_value");

        // runInSubScope with overridden env var + script mutating global MY_VAR
        let sub_env = HashMap::from([("MY_VAR".to_string(), "scoped".to_string())]);
        js.evaluate_script("MY_VAR = 'mutated'", &sub_env, "sub.js", true, None).unwrap();
        assert_eq!(js.evaluate_script("MY_VAR", &HashMap::new(), "t", false, None).unwrap(), "original");

        js.leave_env_scope();
        assert_eq!(js.evaluate_script("ROOT_VAR", &HashMap::new(), "t", false, None).unwrap(), "root_value");
        assert_eq!(js.evaluate_script("SCOPED_VAR", &HashMap::new(), "t", false, None).unwrap(), "undefined");
    }

    #[test]
    fn native_rust_faker_providers_and_expressions() {
        let mut js = JsEngine::new("android");
        let first = js.evaluate_script("faker.name().firstName()", &HashMap::new(), "t", false, None).unwrap();
        assert!(!first.is_empty() && first.chars().all(|c| c.is_ascii_alphabetic()), "got {first}");

        let expr = js
            .evaluate_script(
                "faker.expression('#{name.firstName} #{name.lastName}')",
                &HashMap::new(),
                "t",
                false,
                None,
            )
            .unwrap();
        assert_eq!(expr.split(' ').count(), 2, "got {expr}");

        let email = js.evaluate_script("faker.internet().emailAddress()", &HashMap::new(), "t", false, None).unwrap();
        assert!(email.contains('@'));

        // Clamping protects against runaway allocations from huge JS counts.
        let huge_text =
            js.evaluate_script("faker.text().text(10000000).length", &HashMap::new(), "t", false, None).unwrap();
        assert_eq!(huge_text, faker::MAX_FAKER_LEN.to_string());
        let huge_para = js
            .evaluate_script(
                "faker.lorem().paragraph(10000000).split('.').length - 1",
                &HashMap::new(),
                "t",
                false,
                None,
            )
            .unwrap();
        assert_eq!(huge_para, faker::MAX_FAKER_PARAGRAPH_SENTENCES.to_string());
    }

    #[test]
    fn compiled_expression_cache_hits_on_repeated_evals() {
        let mut js = JsEngine::new("android");
        js.evaluate_script("output.n = 0", &HashMap::new(), "t", false, None).unwrap();
        for _ in 0..20 {
            js.evaluate_scripts("${output.n = output.n + 1}").unwrap();
        }
        assert_eq!(js.evaluate_scripts("${output.n}").unwrap(), "20");
        assert!(js.metrics().cache_hits >= 19, "expected >=19 cache hits, got {}", js.metrics().cache_hits);
    }

    #[test]
    fn http_get_post_put_delete_headers_status_and_multipart() {
        let tmp = std::env::temp_dir().join(format!("tapwing_js_http_{}", std::process::id()));
        let scripts_dir = tmp.join("scripts");
        let media_dir = tmp.join("media");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::create_dir_all(&media_dir).unwrap();
        std::fs::write(media_dir.join("sample.txt"), "hello-file-bytes").unwrap();

        let (port, handle) = spawn_test_server(
            |method, path, headers, body| match (method, path) {
                ("GET", "/json") => {
                    let auth = headers.get("authorization").cloned().unwrap_or_default();
                    let body = serde_json::json!({ "message": "GET Endpoint", "auth": auth }).to_string();
                    (
                        200,
                        vec![
                            ("Content-Type".into(), "application/json".into()),
                            ("testHeader".into(), "first".into()),
                            ("testHeader".into(), "second".into()),
                        ],
                        body,
                    )
                }
                ("POST", "/json") => {
                    let text = String::from_utf8_lossy(body);
                    let resp = serde_json::json!({ "echo": text }).to_string();
                    (201, vec![("Content-Type".into(), "application/json".into())], resp)
                }
                ("PUT", "/item") => (200, vec![], r#"{"updated":true}"#.into()),
                ("DELETE", "/item") => (200, vec![], r#"{"deleted":true}"#.into()),
                ("GET", "/missing") => (404, vec![], "not found".into()),
                ("POST", "/upload") => {
                    let raw = String::from_utf8_lossy(body);
                    let has_type = raw.contains("import");
                    let has_file = raw.contains("hello-file-bytes");
                    (200, vec![], format!(r#"{{"ok":{}}}"#, has_type && has_file))
                }
                _ => (404, vec![], "unknown".into()),
            },
            6,
        );

        let mut js = JsEngine::new("android");

        // 1. GET with auth header + multi-value response headers
        let res = js
            .evaluate_script(
                &format!(
                    r#"
                    var r = http.get('http://127.0.0.1:{port}/json', {{
                        headers: {{ Authorization: 'Bearer Token' }}
                    }});
                    var d = json(r.body);
                    d.message + '|' + d.auth + '|' + r.headers['content-type'] + '|' + r.headers['testheader'] + '|' + r.ok + '|' + r.status;
                    "#
                ),
                &HashMap::new(),
                "t",
                false,
                None,
            )
            .unwrap();
        assert_eq!(res, "GET Endpoint|Bearer Token|application/json|first,second|true|200");

        // 2. POST with JSON body
        let res = js
            .evaluate_script(
                &format!(
                    r#"
                    var r = http.post('http://127.0.0.1:{port}/json', {{
                        body: JSON.stringify({{ payload: 'Value' }})
                    }});
                    json(r.body).echo + '|' + r.status;
                    "#
                ),
                &HashMap::new(),
                "t",
                false,
                None,
            )
            .unwrap();
        assert_eq!(res, r#"{"payload":"Value"}|201"#);

        // 3. PUT
        let res = js
            .evaluate_script(
                &format!("json(http.put('http://127.0.0.1:{port}/item', {{ body: 'x' }}).body).updated"),
                &HashMap::new(),
                "t",
                false,
                None,
            )
            .unwrap();
        assert_eq!(res, "true");

        // 4. DELETE via http.request
        let res = js
            .evaluate_script(
                &format!("json(http.request('http://127.0.0.1:{port}/item', {{ method: 'DELETE' }}).body).deleted"),
                &HashMap::new(),
                "t",
                false,
                None,
            )
            .unwrap();
        assert_eq!(res, "true");

        // 5. 404 returns ok=false without throwing
        let res = js
            .evaluate_script(
                &format!("var r = http.get('http://127.0.0.1:{port}/missing'); r.ok + ':' + r.status + ':' + r.body"),
                &HashMap::new(),
                "t",
                false,
                None,
            )
            .unwrap();
        assert_eq!(res, "false:404:not found");

        // 6. Multipart form resolving relative filePath against script_dir
        let res = js
            .evaluate_script(
                &format!(
                    r#"
                    var r = http.post('http://127.0.0.1:{port}/upload', {{
                        multipartForm: {{
                            uploadType: 'import',
                            file: {{ filePath: '../media/sample.txt', mediaType: 'text/plain' }}
                        }}
                    }});
                    json(r.body).ok
                    "#
                ),
                &HashMap::new(),
                "upload.js",
                true,
                Some(&scripts_dir),
            )
            .unwrap();
        assert_eq!(res, "true");

        assert_eq!(js.metrics().http_requests, 6);
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn http_agent_reuses_keep_alive_connection_and_run_script_hits_cache() {
        let accepts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepts_srv = Arc::clone(&accepts);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let srv = thread::spawn(move || {
            let mut handled = 0usize;
            while handled < 5 {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                accepts_srv.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                while handled < 5 {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" || h == "\n" {
                            break;
                        }
                    }
                    let body = format!(r#"{{"n":{handled}}}"#);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = writer.write_all(resp.as_bytes());
                    let _ = writer.flush();
                    handled += 1;
                }
            }
        });

        let mut js = JsEngine::new("android");
        let script = format!("var r = http.get('http://127.0.0.1:{port}/ping'); output.last = json(r.body).n;");
        for expected in 0..5 {
            js.run_script_file(&script, &HashMap::new(), "ping.js", None).unwrap();
            assert_eq!(js.evaluate_scripts("${output.last}").unwrap(), expected.to_string());
        }
        assert_eq!(js.metrics().http_requests, 5);
        assert!(js.metrics().cache_hits >= 4, "expected runScript cache hits >= 4, got {}", js.metrics().cache_hits);
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "expected all 5 HTTP requests to reuse 1 keep-alive TCP connection"
        );
        srv.join().unwrap();
    }

    #[test]
    fn microbench_cold_vs_warm_js_and_fast_path_interpolation() {
        // 1. Zero-JS fast-path interpolation (no QuickJS runtime ever initialized)
        let mut fast_js = JsEngine::new("android");
        fast_js.put_env("USER", "alice");
        fast_js.put_env("ITEM", "widget-42");
        let t_fast = Instant::now();
        for _ in 0..1000 {
            let s = fast_js.evaluate_scripts("Hello ${USER}, item ${ITEM}!").unwrap();
            assert_eq!(s, "Hello alice, item widget-42!");
        }
        let fast_us = t_fast.elapsed().as_secs_f64() * 1_000_000.0;
        assert!(!fast_js.is_runtime_initialized());

        // 2. Cold QuickJS runtime + context + bootstrap initialization + first eval
        let mut js = JsEngine::new("android");
        let t_cold = Instant::now();
        assert_eq!(js.evaluate_scripts("${output.n = 0; output.n}").unwrap(), "0");
        let cold_ms = t_cold.elapsed().as_secs_f64() * 1000.0;
        assert!(js.is_runtime_initialized());

        // 3. Warm compiled expression evaluations (1,000 iterations)
        let t_warm = Instant::now();
        for _ in 0..1000 {
            let _ = js.evaluate_scripts("${output.n = output.n + 1}").unwrap();
        }
        let warm_ms = t_warm.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(js.evaluate_scripts("${output.n}").unwrap(), "1000");

        // 4. Lightweight env scope stack push/pop (1,000 sub-flow frames)
        js.put_env("X", "outer");
        let t_scope = Instant::now();
        for _ in 0..1000 {
            js.enter_env_scope();
            js.put_env("X", "inner");
            let _ = js.evaluate_scripts("${X}").unwrap();
            js.leave_env_scope();
        }
        let scope_ms = t_scope.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(js.evaluate_scripts("${X}").unwrap(), "outer");

        println!(
            "JS microbench: fast-path 1000x={:.2} us ({:.2} us/op), cold-init={:.2} ms, warm-eval 1000x={:.2} ms ({:.2} us/op), env-scope 1000x={:.2} ms ({:.2} us/op), cache_hits={}",
            fast_us,
            fast_us / 1000.0,
            cold_ms,
            warm_ms,
            warm_ms,
            scope_ms,
            scope_ms,
            js.metrics().cache_hits
        );
    }

    #[test]
    fn interrupt_ends_an_infinite_loop_even_inside_try_catch() {
        let (watch, trigger) = Watch::manual();
        let mut engine = JsEngine::with_interrupt("android", default_http_agent(), watch);
        // The "signal" arrives from inside the script, right before it starts spinning.
        engine.set_log_callback(move |_| trigger(2));
        let script = "console.log('spinning'); try { while (true) {} } catch (e) {} while (true) {}";
        let err = engine.run_script_file(script, &HashMap::new(), "loop.js", None).unwrap_err();
        assert!(crate::interrupt::is_interrupted_error(&err), "unexpected error: {err:#}");
        assert_eq!(err.to_string(), "interrupted by SIGINT");
        assert!(!crate::backend::is_optional_downgradable(&err));
    }

    #[test]
    fn runaway_allocation_fails_with_a_memory_limit_error() {
        let mut engine = JsEngine::new("android");
        engine.limits.memory_bytes = 16 * 1024 * 1024;
        let bomb = "var chunks = []; while (true) { chunks.push(new Array(4096).fill(chunks.length)); }";
        let err = engine.run_script_file(bomb, &HashMap::new(), "bomb.js", None).unwrap_err();
        assert!(err.is::<JsError>(), "unexpected error: {err:#}");
        assert!(err.to_string().starts_with("JavaScript ran out of memory (limit: 16 MiB)"), "unexpected: {err}");

        // The limit ends the script, not the engine: the next evaluation still works.
        assert_eq!(engine.evaluate_script("1 + 1", &HashMap::new(), "inline-script", false, None).unwrap(), "2");
    }

    #[test]
    fn unbounded_recursion_fails_with_a_stack_error() {
        let mut engine = JsEngine::new("android");
        let err = engine
            .evaluate_script("(function f() { return f() + 1; })()", &HashMap::new(), "inline-script", false, None)
            .unwrap_err();
        assert!(err.is::<JsError>(), "unexpected error: {err:#}");
        assert!(err.to_string().contains("stack"), "unexpected message: {err}");
    }

    #[test]
    fn interrupt_ends_an_http_request_the_server_never_answers() {
        let (watch, trigger) = Watch::manual();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let server = thread::spawn(move || {
            // Accept, read nothing, answer nothing; signal only once the request is in flight.
            let (stream, _) = listener.accept().unwrap();
            trigger(15);
            let _ = held.recv();
            drop(stream);
        });

        let mut engine = JsEngine::with_interrupt("android", default_http_agent(), watch);
        let script = format!("output.status = http.get('http://127.0.0.1:{port}/hang').status");
        let t0 = Instant::now();
        let err = engine.run_script_file(&script, &HashMap::new(), "hang.js", None).unwrap_err();
        let waited = t0.elapsed();
        release.send(()).unwrap();
        server.join().unwrap();

        assert!(crate::interrupt::is_interrupted_error(&err), "unexpected error: {err:#}");
        assert_eq!(err.to_string(), "terminated by SIGTERM");
        assert!(waited < Duration::from_secs(5), "request held the run for {waited:?} (agent timeout is 300 s)");
        assert_eq!(engine.metrics().http_requests, 0, "an abandoned request must not be counted as completed");
    }
}
