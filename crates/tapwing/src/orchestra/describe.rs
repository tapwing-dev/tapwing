//! Pure command metadata, optionality, settle classification, and human-readable formatting.

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
