//! Running parsed flows on a device (F0 command set; the full orchestrator
//! with JS, conditions, sub-flows and hooks is phase F3).

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use tapwing::flow::ParsedFlow;
use tapwing::flow::ir::{AssertCondition, Direction, ElementSelector, MaestroCommand};

use crate::device::Device;
use crate::driver::{Driver, DriverError};

/// Maestro's element lookup timeouts (`Orchestra`): 17 s, 7 s when optional.
pub const LOOKUP_TIMEOUT_MS: u64 = 17_000;
pub const OPTIONAL_LOOKUP_TIMEOUT_MS: u64 = 7_000;

pub struct Opts {
    pub settle_policy: String,
    pub check_settle: bool,
    /// `-e KEY=VALUE` from the command line; wins over flow `env`.
    pub cli_env: HashMap<String, String>,
}

pub struct Session<'a> {
    pub dev: &'a mut Device,
    pub drv: &'a mut Driver,
    pub opts: &'a Opts,
}

/// Run one flow; returns (passed, report entry).
pub fn run_flow(flow: &ParsedFlow, s: &mut Session) -> (bool, Value) {
    let name = flow
        .config
        .name
        .clone()
        .unwrap_or_else(|| flow.path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    println!("\n▶ {name}");
    let t0 = Instant::now();
    let mut env = s.opts.cli_env.clone();
    let mut steps = Vec::new();
    let mut ok = true;
    let mut violations = 0;
    let mut index = 0;
    for cmd in &flow.commands {
        match cmd {
            MaestroCommand::ApplyConfiguration(_) => continue,
            MaestroCommand::DefineVariables(d) => {
                for (k, v) in &d.env {
                    if !s.opts.cli_env.contains_key(k) {
                        env.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
                    }
                }
                continue;
            }
            _ => {}
        }
        let desc = describe(cmd, &env);
        let kind = kind(cmd);
        let ts = Instant::now();
        let res = exec(cmd, flow.app_id(), &env, s);
        let ms = ts.elapsed().as_secs_f64() * 1000.0;
        match &res {
            Ok(extra) => {
                println!("  ✓ {ms:>7.1} ms  {desc}");
                let mut entry = json!({ "index": index, "command": kind, "description": desc, "status": "passed", "ms": ms, "driver": extra });
                if s.opts.check_settle
                    && settles(cmd)
                    && s.drv.is_connected()
                    && let Ok(busy) = s.drv.call("busy", json!({}))
                {
                    let anims = busy["animations"].as_array().cloned().unwrap_or_default();
                    if !anims.is_empty() {
                        violations += 1;
                        let what: Vec<String> = anims
                            .iter()
                            .map(|a| {
                                format!(
                                    "{} {:.0}% {}",
                                    a["widget"].as_str().unwrap_or("?"),
                                    a["fraction"].as_f64().unwrap_or(0.0) * 100.0,
                                    a["status"].as_str().unwrap_or("")
                                )
                            })
                            .collect();
                        println!("      ⚠ returned while still animating: {}", what.join(", "));
                    }
                    entry["busyAfter"] = busy;
                }
                steps.push(entry);
            }
            Err(e) if cmd.optional() => {
                println!("  ~ {ms:>7.1} ms  {desc}  (optional: {e})");
                steps.push(json!({ "index": index, "command": kind, "description": desc, "status": "warned", "ms": ms, "error": e.to_string() }));
            }
            Err(e) => {
                println!("  ✗ {ms:>7.1} ms  {desc}\n      {}", explain(e));
                steps.push(json!({ "index": index, "command": kind, "description": desc, "status": "failed", "ms": ms, "error": e.to_string() }));
                ok = false;
                break;
            }
        }
        index += 1;
    }
    let total = t0.elapsed().as_secs_f64() * 1000.0;
    println!("{} {name} in {total:.0} ms", if ok { "PASSED" } else { "FAILED" });
    if s.opts.check_settle {
        println!("settle check ({}): {violations} violation(s)", s.opts.settle_policy);
    }
    (
        ok,
        json!({ "flow": name, "path": flow.path, "status": if ok { "passed" } else { "failed" }, "totalMs": total, "settlePolicy": s.opts.settle_policy, "settleViolations": violations, "steps": steps }),
    )
}

/// Accept the app's connection and apply the session settings. The policy
/// is always sent: an app that outlived an earlier session may still have
/// that session's policy.
pub fn attach(drv: &mut Driver, opts: &Opts, timeout: Duration) -> Result<Value> {
    attach_pid(drv, opts, timeout, None)
}

pub fn attach_pid(drv: &mut Driver, opts: &Opts, timeout: Duration, pid: Option<i64>) -> Result<Value> {
    let hello = drv.accept_pid(timeout, pid)?.clone();
    drv.call("configure", json!({ "settlePolicy": opts.settle_policy }))?;
    Ok(hello)
}

/// Commands after which the UI is expected to be settled.
fn settles(cmd: &MaestroCommand) -> bool {
    !matches!(
        cmd,
        MaestroCommand::AssertCondition(_)
            | MaestroCommand::StopApp(_)
            | MaestroCommand::KillApp(_)
            | MaestroCommand::ClearState(_)
            | MaestroCommand::TakeScreenshot(_)
    )
}

/// Commands handled by adb alone; everything else talks to the driver.
fn needs_driver(cmd: &MaestroCommand) -> bool {
    !matches!(
        cmd,
        MaestroCommand::LaunchApp(_)
            | MaestroCommand::StopApp(_)
            | MaestroCommand::KillApp(_)
            | MaestroCommand::ClearState(_)
            | MaestroCommand::TakeScreenshot(_)
    )
}

fn lookup_timeout(cmd: &MaestroCommand) -> u64 {
    if cmd.optional() { OPTIONAL_LOOKUP_TIMEOUT_MS } else { LOOKUP_TIMEOUT_MS }
}

fn exec(cmd: &MaestroCommand, flow_app: &str, env: &HashMap<String, String>, s: &mut Session) -> Result<Value> {
    let i = |t: &str| interpolate(t, env);
    if needs_driver(cmd) && !s.drv.is_connected() {
        // Like Maestro, a flow may start without launchApp and attach to the
        // running app (its driver redials every few hundred ms).
        attach(s.drv, s.opts, Duration::from_secs(5)).map_err(|e| {
            anyhow!("{flow_app} is not running with tapwing_driver (no launchApp before this step?): {e}")
        })?;
    }
    match cmd {
        MaestroCommand::LaunchApp(c) => {
            let app_id = i(&c.app_id);
            // Maestro defaults: stopApp true, clearState false.
            let clear_state = c.clear_state.unwrap_or(false);
            let stop_app = c.stop_app.unwrap_or(true);
            if stop_app || clear_state || !s.drv.is_connected() {
                if clear_state {
                    s.dev.clear_state(&app_id)?; // also kills the process
                } else if stop_app {
                    s.dev.stop_app(&app_id)?;
                }
                s.drv.disconnect();
                let pid = s.dev.launch(&app_id, s.drv.port()?)?;
                let hello = attach_pid(s.drv, s.opts, Duration::from_secs(20), pid)?;
                let idle = s.drv.call("waitIdle", json!({ "timeoutMs": 5000 }))?;
                Ok(json!({ "hello": hello, "idle": idle }))
            } else {
                s.dev.launch(&app_id, s.drv.port()?)?;
                s.drv.call("waitIdle", json!({ "timeoutMs": 5000 }))
            }
        }
        MaestroCommand::StopApp(c) | MaestroCommand::KillApp(c) => {
            s.dev.stop_app(&i(&c.app_id))?;
            s.drv.disconnect();
            Ok(Value::Null)
        }
        MaestroCommand::ClearState(c) => {
            s.dev.clear_state(&i(&c.app_id))?;
            s.drv.disconnect();
            Ok(Value::Null)
        }
        MaestroCommand::TapOnElement(c) => {
            if c.relative_point.is_some() {
                bail!("tapOn with `point` inside an element is not supported yet (F2)");
            }
            let repeat = c.repeat.as_ref();
            s.drv.call(
                "tapOn",
                json!({
                    "selector": driver_selector(&c.selector, env)?,
                    "timeoutMs": lookup_timeout(cmd),
                    "longPress": c.long_press.unwrap_or(false),
                    "repeat": repeat.map(|r| r.repeat),
                    "delayMs": repeat.map(|r| r.delay),
                    "settleTimeoutMs": c.wait_to_settle_timeout_ms,
                }),
            )
        }
        MaestroCommand::TapOnPoint(c) => s.drv.call(
            "tapPoint",
            json!({ "point": i(&c.point), "longPress": c.long_press.unwrap_or(false), "settleTimeoutMs": c.wait_to_settle_timeout_ms }),
        ),
        MaestroCommand::AssertCondition(c) => assert_condition(c, cmd, env, s),
        MaestroCommand::InputText(c) => s.drv.call("inputText", json!({ "text": i(&c.text) })),
        MaestroCommand::EraseText(c) => s.drv.call("eraseText", json!({ "count": c.characters_to_erase.unwrap_or(50) })),
        // Maestro's IOSDriver.backPress() is empty: `back` does nothing on iOS.
        MaestroCommand::BackPress(_) if s.dev.is_ios() => Ok(Value::Null),
        MaestroCommand::BackPress(_) => s.drv.call("back", json!({})),
        MaestroCommand::HideKeyboard(_) => s.drv.call("hideKeyboard", json!({})),
        MaestroCommand::ScrollUntilVisible(c) => s.drv.call(
            "scrollUntilVisible",
            json!({
                "selector": driver_selector(&c.selector, env)?,
                "direction": direction(c.direction),
                "timeoutMs": number(&i(&c.timeout), "timeout")?,
                "visibilityPercentage": c.visibility_percentage,
            }),
        ),
        MaestroCommand::Swipe(c) => {
            let (start, end) = match (&c.start_relative, &c.end_relative, &c.start_point, &c.end_point, c.direction) {
                (Some(a), Some(b), ..) => (i(a), i(b)),
                (_, _, Some(a), Some(b), _) => (format!("{}, {}", a.x, a.y), format!("{}, {}", b.x, b.y)),
                (_, _, _, _, Some(d)) if c.element_selector.is_none() => {
                    // Screen swipes by direction (relative to the screen).
                    let (a, b) = match d {
                        Direction::Up => ("50%, 50%", "50%, 10%"),
                        Direction::Down => ("50%, 20%", "50%, 90%"),
                        Direction::Left => ("90%, 50%", "10%, 50%"),
                        Direction::Right => ("10%, 50%", "90%, 50%"),
                    };
                    (a.to_string(), b.to_string())
                }
                _ => bail!("swipe on an element (`from:`) is not supported yet (F2)"),
            };
            s.drv.call("swipe", json!({ "start": start, "end": end, "durationMs": c.duration }))
        }
        MaestroCommand::WaitForAnimationToEnd(c) => {
            let timeout = match &c.timeout {
                Some(t) => number(&i(t), "timeout")?,
                None => 15_000,
            };
            s.drv.call("waitIdle", json!({ "timeoutMs": timeout }))
        }
        MaestroCommand::TakeScreenshot(c) => {
            let name = i(&c.path);
            let path = if name.ends_with(".png") { name } else { format!("{name}.png") };
            s.dev.screenshot(&path)?;
            Ok(json!({ "path": path }))
        }
        other => bail!("`{}` is parsed but not executed yet (orchestrator and host commands come in F3/F4)", kind(other)),
    }
}

fn assert_condition(
    c: &AssertCondition,
    cmd: &MaestroCommand,
    env: &HashMap<String, String>,
    s: &mut Session,
) -> Result<Value> {
    if c.condition.script_condition.is_some() {
        bail!("assertTrue / script conditions need the JS engine (F3)");
    }
    let timeout = match &c.timeout {
        Some(t) => Some(number(&interpolate(t, env), "timeout")?),
        None => None,
    };
    let mut last = Value::Null;
    if let Some(v) = &c.condition.visible {
        last = s.drv.call(
            "assertVisible",
            json!({ "selector": driver_selector(v, env)?, "timeoutMs": timeout.unwrap_or(lookup_timeout(cmd)) }),
        )?;
    }
    if let Some(v) = &c.condition.not_visible {
        last = s.drv.call(
            "assertNotVisible",
            json!({ "selector": driver_selector(v, env)?, "timeoutMs": timeout.unwrap_or(OPTIONAL_LOOKUP_TIMEOUT_MS) }),
        )?;
    }
    Ok(last)
}

fn direction(d: Direction) -> &'static str {
    match d {
        Direction::Up => "UP",
        Direction::Down => "DOWN",
        Direction::Left => "LEFT",
        Direction::Right => "RIGHT",
    }
}

fn number(s: &str, what: &str) -> Result<u64> {
    s.trim().parse::<f64>().map(|v| v.max(0.0) as u64).map_err(|_| anyhow!("{what} `{s}` is not a number"))
}

/// The selector shape the Dart driver understands (`Selector.fromJson`).
fn driver_selector(sel: &ElementSelector, env: &HashMap<String, String>) -> Result<Value> {
    if sel.traits.is_some() {
        bail!("selector `traits` are not supported yet (F2)");
    }
    let sub = |s: &Option<Box<ElementSelector>>| -> Result<Option<Value>> {
        s.as_deref().map(|s| driver_selector(s, env)).transpose()
    };
    let mut out = serde_json::Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            out.insert(k.to_string(), v);
        }
    };
    put("text", sel.text_regex.as_deref().map(|t| Value::from(interpolate(t, env))));
    put("id", sel.id_regex.as_deref().map(|t| Value::from(interpolate(t, env))));
    if let Some(index) = &sel.index {
        let raw = interpolate(index, env);
        let n: i64 =
            raw.trim().parse::<f64>().map(|f| f as i64).map_err(|_| anyhow!("index `{raw}` is not a number"))?;
        put("index", Some(n.into()));
    }
    put("enabled", sel.enabled.map(Value::from));
    put("checked", sel.checked.map(Value::from));
    put("focused", sel.focused.map(Value::from));
    put("selected", sel.selected.map(Value::from));
    if let Some(size) = &sel.size {
        put("width", size.width.map(Value::from));
        put("height", size.height.map(Value::from));
        put("tolerance", size.tolerance.map(Value::from));
    }
    put("below", sub(&sel.below)?);
    put("above", sub(&sel.above)?);
    put("leftOf", sub(&sel.left_of)?);
    put("rightOf", sub(&sel.right_of)?);
    put("childOf", sub(&sel.child_of)?);
    put("containsChild", sub(&sel.contains_child)?);
    if let Some(list) = &sel.contains_descendants {
        put(
            "containsDescendants",
            Some(Value::Array(list.iter().map(|d| driver_selector(d, env)).collect::<Result<_>>()?)),
        );
    }
    Ok(Value::Object(out))
}

/// `${NAME}` from the env. Real JS evaluation of `${...}` is F3.
pub fn interpolate(s: &str, env: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find('}') {
            Some(end) => {
                let key = rest[start + 2..start + 2 + end].trim();
                out.push_str(env.get(key).map(String::as_str).unwrap_or(""));
                rest = &rest[start + 3 + end..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Short command name for reports.
pub fn kind(cmd: &MaestroCommand) -> &'static str {
    use MaestroCommand::*;
    match cmd {
        TapOnElement(c) if c.long_press == Some(true) => "longPressOn",
        TapOnElement(c) if c.repeat.as_ref().is_some_and(|r| r.repeat == 2) => "doubleTapOn",
        TapOnElement(_) | TapOnPoint(_) => "tapOn",
        AssertCondition(c) if c.condition.script_condition.is_some() => "assertTrue",
        AssertCondition(c) if c.timeout.is_some() => "extendedWaitUntil",
        AssertCondition(c) if c.condition.not_visible.is_some() => "assertNotVisible",
        AssertCondition(_) => "assertVisible",
        Scroll(_) => "scroll",
        Swipe(_) => "swipe",
        BackPress(_) => "back",
        AssertScreenshot(_) => "assertScreenshot",
        AssertNoDefectsWithAi(_) => "assertNoDefectsWithAI",
        AssertWithAi(_) => "assertWithAI",
        ExtractTextWithAi(_) => "extractTextWithAI",
        InputText(_) => "inputText",
        InputRandom(_) => "inputRandom",
        LaunchApp(_) => "launchApp",
        SetPermissions(_) => "setPermissions",
        ApplyConfiguration(_) => "config",
        OpenLink(_) => "openLink",
        PressKey(_) => "pressKey",
        EraseText(_) => "eraseText",
        HideKeyboard(_) => "hideKeyboard",
        TakeScreenshot(_) => "takeScreenshot",
        StopApp(_) => "stopApp",
        KillApp(_) => "killApp",
        ClearState(_) => "clearState",
        ClearKeychain(_) => "clearKeychain",
        RunFlow(_) => "runFlow",
        SetLocation(_) => "setLocation",
        SetOrientation(_) => "setOrientation",
        Repeat(_) => "repeat",
        CopyTextFrom(_) => "copyTextFrom",
        SetClipboard(_) => "setClipboard",
        PasteText(_) => "pasteText",
        DefineVariables(_) => "env",
        RunScript(_) => "runScript",
        WaitForAnimationToEnd(_) => "waitForAnimationToEnd",
        EvalScript(_) => "evalScript",
        ScrollUntilVisible(_) => "scrollUntilVisible",
        Travel(_) => "travel",
        StartRecording(_) => "startRecording",
        StopRecording(_) => "stopRecording",
        AddMedia(_) => "addMedia",
        SetAirplaneMode(_) => "setAirplaneMode",
        ToggleAirplaneMode(_) => "toggleAirplaneMode",
        SetDarkMode(_) => "setDarkMode",
        ToggleDarkMode(_) => "toggleDarkMode",
        AssertDarkMode(_) => "assertDarkMode",
        AssertLightMode(_) => "assertLightMode",
        Retry(_) => "retry",
    }
}

fn describe_selector(s: &ElementSelector, env: &HashMap<String, String>) -> String {
    match (&s.text_regex, &s.id_regex) {
        (Some(t), _) => format!("\"{}\"", interpolate(t, env)),
        (_, Some(i)) => format!("id: {}", interpolate(i, env)),
        _ => serde_json::to_string(s).unwrap_or_default(),
    }
}

/// One-line description (the label, if the flow gives one).
pub fn describe(cmd: &MaestroCommand, env: &HashMap<String, String>) -> String {
    if let Some(label) = cmd.label() {
        return label.to_string();
    }
    let k = kind(cmd);
    match cmd {
        MaestroCommand::LaunchApp(c) => {
            format!("launchApp{}", if c.clear_state == Some(true) { " (clearState)" } else { "" })
        }
        MaestroCommand::TapOnElement(c) => format!("{k} {}", describe_selector(&c.selector, env)),
        MaestroCommand::TapOnPoint(c) => format!("{k} point {}", interpolate(&c.point, env)),
        MaestroCommand::AssertCondition(c) => match (&c.condition.visible, &c.condition.not_visible) {
            (Some(v), _) => format!("{k} {}", describe_selector(v, env)),
            (_, Some(v)) => format!("{k} {}", describe_selector(v, env)),
            _ => format!("{k} {}", c.condition.script_condition.as_deref().unwrap_or_default()),
        },
        MaestroCommand::InputText(c) => format!("inputText \"{}\"", interpolate(&c.text, env)),
        MaestroCommand::ScrollUntilVisible(c) => format!("{k} {}", describe_selector(&c.selector, env)),
        _ => k.to_string(),
    }
}

pub fn explain(e: &anyhow::Error) -> String {
    if let Some(d) = e.downcast_ref::<DriverError>()
        && d.code == "ELEMENT_NOT_FOUND"
    {
        let seen: Vec<String> = d.details["visibleTexts"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| format!("\"{s}\""))).take(12).collect())
            .unwrap_or_default();
        return format!("{}\n      on screen: {}", d.message, seen.join(", "));
    }
    format!("{e:#}")
}
