//! Pure command metadata, optionality, settle classification, and human-readable formatting.

use serde_json::Value;

use crate::flow::ir::{Condition, ElementSelector, MaestroCommand, Platform};

/// Effective optional flag matching `Orchestra.kt:378` and `MaestroCommand.elementSelector()`.
pub fn is_optional(cmd: &MaestroCommand) -> bool {
    if cmd.optional() {
        return true;
    }
    match cmd {
        MaestroCommand::TapOnElement(c) => c.selector.optional,
        MaestroCommand::CopyTextFrom(c) => c.selector.optional,
        MaestroCommand::ScrollUntilVisible(c) => c.selector.optional,
        MaestroCommand::Swipe(c) => c.element_selector.as_ref().is_some_and(|s| s.optional),
        MaestroCommand::AssertCondition(c) => {
            c.condition.visible.as_ref().is_some_and(|s| s.optional)
                || c.condition.not_visible.as_ref().is_some_and(|s| s.optional)
        }
        _ => false,
    }
}

/// One line for a step whose settle ran into its timeout: what the driver was
/// still waiting for when it gave up, read from `settleBusy` in the reply.
///
/// `None` when the settle did not time out, or when the reply names nothing
/// (an older driver, or nothing was busy). The reply is the step's own or, for
/// steps that end with a plain `waitIdle` such as `launchApp`, the one under
/// `idle`. A `source` this version does not know is printed as it is.
pub fn settle_timeout_note(driver: &Value) -> Option<String> {
    let timed_out = |reply: &&Value| reply["settle"] == "timeout" || reply["state"] == "timeout";
    let reply = [driver, &driver["idle"]].into_iter().find(timed_out)?;
    let waited_for: Vec<String> = reply["settleBusy"].as_array()?.iter().filter_map(busy_source).collect();
    if waited_for.is_empty() {
        return None;
    }
    Some(format!("settle timed out, still waiting for {}", waited_for.join(", ")))
}

fn busy_source(entry: &Value) -> Option<String> {
    let source = entry["source"].as_str()?;
    let number = |key: &str| entry[key].as_u64();
    let counted = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if let Some(more) = number("more") {
        return Some(match source {
            "channel" => counted(more, "more platform channel", "more platform channels"),
            "animation" => counted(more, "more animation", "more animations"),
            other => format!("{more} more {other}"),
        });
    }
    Some(match source {
        "channel" => {
            let mut details = Vec::new();
            if let Some(ms) = number("pendingMs") {
                details.push(format!("pending {ms} ms"));
            }
            if let Some(calls) = number("calls") {
                details.push(format!("{calls} calls"));
            }
            let name = entry["channel"].as_str().unwrap_or("?");
            if details.is_empty() {
                format!("platform channel {name}")
            } else {
                format!("platform channel {name} ({})", details.join(", "))
            }
        }
        "http" => counted(number("count").unwrap_or(1), "HTTP request", "HTTP requests"),
        "images" => counted(number("count").unwrap_or(1), "image load", "image loads"),
        "keyboard" => "the soft keyboard".to_string(),
        "animation" => format!(
            "animation {} {:.0}% {}",
            entry["widget"].as_str().unwrap_or("?"),
            entry["fraction"].as_f64().unwrap_or(0.0) * 100.0,
            entry["status"].as_str().unwrap_or("")
        )
        .trim_end()
        .to_string(),
        "frames" => "a UI that keeps changing".to_string(),
        other => other.to_string(),
    })
}

/// Commands after which the UI is expected to be settled (used by `--check-settle`).
pub fn settles(cmd: &MaestroCommand) -> bool {
    settles_for_platform(cmd, Platform::Android)
}

/// Platform-aware variant of [`settles`] (accounts for Maestro iOS no-op `back`
/// and non-in-app `pressKey`).
pub fn settles_for_platform(cmd: &MaestroCommand, platform: Platform) -> bool {
    match cmd {
        MaestroCommand::BackPress(_) if platform == Platform::Ios => false,
        MaestroCommand::PressKey(c) if platform == Platform::Ios => crate::run::is_in_app_key_ios(c.code),
        MaestroCommand::AssertCondition(_)
        | MaestroCommand::AssertScreenshot(_)
        | MaestroCommand::AssertDarkMode(_)
        | MaestroCommand::AssertLightMode(_)
        | MaestroCommand::StopApp(_)
        | MaestroCommand::KillApp(_)
        | MaestroCommand::ClearState(_)
        | MaestroCommand::ClearKeychain(_)
        | MaestroCommand::TakeScreenshot(_)
        | MaestroCommand::CopyTextFrom(_)
        | MaestroCommand::SetClipboard(_)
        | MaestroCommand::SetPermissions(_)
        | MaestroCommand::SetAirplaneMode(_)
        | MaestroCommand::ToggleAirplaneMode(_)
        | MaestroCommand::SetLocation(_)
        | MaestroCommand::ApplyConfiguration(_)
        | MaestroCommand::DefineVariables(_)
        | MaestroCommand::EvalScript(_)
        | MaestroCommand::RunScript(_)
        | MaestroCommand::RunFlow(_)
        | MaestroCommand::Repeat(_)
        | MaestroCommand::Retry(_) => false,
        MaestroCommand::PressKey(c) => crate::run::is_in_app_key(c.code),
        _ => true,
    }
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

pub(super) fn describe_selector(s: &ElementSelector) -> String {
    match (&s.text_regex, &s.id_regex) {
        (Some(t), _) => format!("\"{t}\""),
        (_, Some(i)) => format!("id: {i}"),
        _ => serde_json::to_string(s).unwrap_or_default(),
    }
}

pub(super) fn describe_condition(c: &Condition) -> String {
    if let Some(l) = &c.label {
        return l.clone();
    }
    let mut parts = Vec::new();
    if let Some(p) = c.platform {
        parts.push(format!("Platform is {p:?}"));
    }
    if let Some(v) = &c.visible {
        parts.push(format!("{} is visible", describe_selector(v)));
    }
    if let Some(nv) = &c.not_visible {
        parts.push(format!("{} is not visible", describe_selector(nv)));
    }
    if let Some(sc) = &c.script_condition {
        parts.push(format!("{sc} is true"));
    }
    if parts.is_empty() { "true".to_string() } else { parts.join(" and ") }
}

pub fn describe_command(cmd: &MaestroCommand) -> String {
    if let Some(label) = cmd.label() {
        return label.to_string();
    }
    let k = kind(cmd);
    match cmd {
        MaestroCommand::LaunchApp(c) => {
            format!("launchApp{}", if c.clear_state == Some(true) { " (clearState)" } else { "" })
        }
        MaestroCommand::TapOnElement(c) => format!("{k} {}", describe_selector(&c.selector)),
        MaestroCommand::TapOnPoint(c) => format!("{k} point {}", c.point),
        MaestroCommand::AssertCondition(c) => format!("{k} {}", describe_condition(&c.condition)),
        MaestroCommand::InputText(c) => format!("inputText \"{}\"", c.text),
        MaestroCommand::ScrollUntilVisible(c) => format!("{k} {}", describe_selector(&c.selector)),
        MaestroCommand::RunFlow(c) => match &c.source_description {
            Some(src) => format!("runFlow {src}"),
            None => "runFlow".to_string(),
        },
        MaestroCommand::RunScript(c) => format!("runScript {}", c.source_description),
        MaestroCommand::EvalScript(c) => format!("evalScript {}", c.script_string),
        _ => k.to_string(),
    }
}

#[cfg(test)]
mod settle_timeout_note_tests {
    use serde_json::json;

    use super::settle_timeout_note;

    #[test]
    fn names_the_pending_channel_and_how_long_it_has_waited() {
        let reply = json!({
            "settle": "timeout",
            "t": { "settleUs": 3_003_000 },
            "settleBusy": [
                { "source": "channel", "channel": "dev.tapwing.sample_app/permissions", "pendingMs": 3012 }
            ]
        });
        assert_eq!(
            settle_timeout_note(&reply).as_deref(),
            Some(
                "settle timed out, still waiting for platform channel dev.tapwing.sample_app/permissions \
                 (pending 3012 ms)"
            )
        );
    }

    #[test]
    fn lists_every_source_and_prints_an_unknown_one_as_it_is() {
        let reply = json!({
            "settle": "timeout",
            "settleBusy": [
                { "source": "channel", "channel": "a/b", "pendingMs": 40, "calls": 2 },
                { "source": "channel", "more": 3 },
                { "source": "http", "count": 2 },
                { "source": "images", "count": 1 },
                { "source": "keyboard" },
                { "source": "animation", "widget": "FadeTransition", "fraction": 0.354, "status": "forward" },
                { "source": "frames", "transientCallbacks": 1 },
                { "source": "nativeView" },
                { "noSource": true }
            ]
        });
        assert_eq!(
            settle_timeout_note(&reply).as_deref(),
            Some(
                "settle timed out, still waiting for platform channel a/b (pending 40 ms, 2 calls), \
                 3 more platform channels, 2 HTTP requests, 1 image load, the soft keyboard, \
                 animation FadeTransition 35% forward, a UI that keeps changing, nativeView"
            )
        );
    }

    #[test]
    fn reads_the_wait_idle_reply_of_a_launch() {
        let reply = json!({
            "pid": 7,
            "idle": { "state": "timeout", "settleBusy": [{ "source": "http", "count": 1 }] }
        });
        assert_eq!(settle_timeout_note(&reply).as_deref(), Some("settle timed out, still waiting for 1 HTTP request"));
    }

    #[test]
    fn says_nothing_without_a_timeout_or_without_named_sources() {
        // A settle that ended normally, even if a driver sent the field.
        assert_eq!(settle_timeout_note(&json!({ "settle": "idle", "settleBusy": [{ "source": "keyboard" }] })), None);
        // An older driver: a timeout without the field.
        assert_eq!(settle_timeout_note(&json!({ "settle": "timeout" })), None);
        assert_eq!(settle_timeout_note(&json!({ "settle": "timeout", "settleBusy": [] })), None);
        assert_eq!(settle_timeout_note(&json!({ "settle": "timeout", "settleBusy": "soon" })), None);
        // Replies that are not objects (`ping`, `assertNotVisible`).
        assert_eq!(settle_timeout_note(&json!(true)), None);
        assert_eq!(settle_timeout_note(&json!(null)), None);
    }
}
