//! Flow commands: YAML → IR, ported from Maestro 2.12.1
//! (`MaestroFlowParser.YamlCommandDeserializer`, `YamlFluentCommand`, the
//! `Yaml*` option classes and their custom deserializers).
//!
//! Each command name maps to one decoder that mirrors the Kotlin class:
//! which YAML shapes it accepts (bare string, scalar, mapping), which
//! properties exist, their defaults, and how Maestro turns it into a
//! `MaestroCommand`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::decode::{DResult, Mode, Obj, untyped_scalar_text};
use super::error::Problem;
use super::ir::*;
use super::yaml::{Node, Pos, Typed, canonical_text, java_double};
use super::{FlowFailure, Loader};

/// What a command needs from its surroundings.
pub struct Ctx<'a> {
    /// Absolute, normalized path of the flow file being parsed.
    pub flow_path: &'a Path,
    /// The flow's `appId` (or `url`).
    pub app_id: &'a str,
    pub loader: &'a Loader,
}

/// Commands that may be written as a bare string (`- back`).
const STRING_COMMANDS: &[&str] = &[
    "launchApp",
    "stopApp",
    "killApp",
    "clearState",
    "clearKeychain",
    "eraseText",
    "inputRandomText",
    "inputRandomNumber",
    "inputRandomEmail",
    "inputRandomPersonName",
    "inputRandomCityName",
    "inputRandomCountryName",
    "inputRandomColorName",
    "back",
    "hideKeyboard",
    "hide keyboard",
    "pasteText",
    "scroll",
    "waitForAnimationToEnd",
    "stopRecording",
    "toggleAirplaneMode",
    "toggleDarkMode",
    "assertDarkMode",
    "assertLightMode",
    "assertNoDefectsWithAI",
];

/// Commands written as `- name: options` (`YamlFluentCommand` properties).
const OBJECT_COMMANDS: &[&str] = &[
    "tapOn",
    "doubleTapOn",
    "longPressOn",
    "assertVisible",
    "assertNotVisible",
    "assertTrue",
    "assertNoDefectsWithAI",
    "assertScreenshot",
    "assertWithAI",
    "extractTextWithAI",
    "back",
    "clearKeychain",
    "hideKeyboard",
    "pasteText",
    "scroll",
    "inputText",
    "inputRandomText",
    "inputRandomNumber",
    "inputRandomEmail",
    "inputRandomPersonName",
    "inputRandomCityName",
    "inputRandomCountryName",
    "inputRandomColorName",
    "launchApp",
    "setPermissions",
    "swipe",
    "openLink",
    "pressKey",
    "eraseText",
    "action",
    "takeScreenshot",
    "extendedWaitUntil",
    "stopApp",
    "killApp",
    "clearState",
    "runFlow",
    "setLocation",
    "setOrientation",
    "repeat",
    "copyTextFrom",
    "setClipboard",
    "runScript",
    "waitForAnimationToEnd",
    "evalScript",
    "scrollUntilVisible",
    "travel",
    "startRecording",
    "stopRecording",
    "addMedia",
    "setAirplaneMode",
    "toggleAirplaneMode",
    "setDarkMode",
    "toggleDarkMode",
    "assertDarkMode",
    "assertLightMode",
    "retry",
    "callHook",
];

/// Every command name (for "did you mean" suggestions).
pub fn all_command_names() -> Vec<&'static str> {
    let mut all: Vec<&str> = STRING_COMMANDS.iter().chain(OBJECT_COMMANDS).copied().collect();
    all.dedup();
    let mut seen = Vec::new();
    all.retain(|c| {
        let new = !seen.contains(c);
        seen.push(*c);
        new
    });
    all
}

const TAP_REPEAT_DELAY_MS: i64 = 100;
const MAX_WAIT_TO_SETTLE_MS: i32 = 30_000;
const SWIPE_DURATION_MS: i64 = 400;
const DOCS_COMMANDS: &str = "https://docs.maestro.dev/api-reference/commands";

/// One list item of a command list. A command may expand to several IR
/// commands (env definitions are prepended by the callers).
pub fn decode(item: &Node, ctx: &Ctx) -> Result<Vec<MaestroCommand>, FlowFailure> {
    let (name, value) = match item {
        Node::Scalar(s) => match s.typed() {
            Typed::Str(text) => return string_command(&text, s.pos, ctx).map_err(FlowFailure::here),
            _ => return Err(FlowFailure::here(invalid_command(item.pos()))),
        },
        Node::Map(entries, pos) => {
            let Some((key, value)) = entries.first() else {
                return Err(FlowFailure::here(Problem::failed("Empty command", *pos)));
            };
            let name = key.as_scalar().map(|s| s.text.as_str()).unwrap_or_default();
            if !OBJECT_COMMANDS.contains(&name) {
                return Err(FlowFailure::here(Problem::new(
                    format!("Invalid Command: {name}"),
                    format!("`{name}` is not a valid command.\n\n{}", suggest(name)).trim().to_string(),
                    key.pos(),
                )));
            }
            if value.is_null() {
                return Err(FlowFailure::here(Problem::new(
                    format!("Incorrect Command Format: {name}"),
                    format!("The command `{name}` requires additional options."),
                    value.pos(),
                )));
            }
            if let Some((extra, _)) = entries.get(1) {
                let field = extra.as_scalar().map(|s| s.text.as_str()).unwrap_or_default();
                return Err(FlowFailure::here(Problem::new(
                    format!("Invalid Command Format: {name}"),
                    format!("Found unexpected top-level field: `{field}`. Missing an indent or dash?"),
                    extra.pos(),
                )));
            }
            (name, value)
        }
        Node::Seq(..) => return Err(FlowFailure::here(invalid_command(item.pos()))),
    };
    object_command(name, value, ctx)
}

/// Command-list syntax, checked before any command is resolved: Maestro
/// binds the whole list (and nested `commands:` lists) first, so these
/// errors win over e.g. a missing sub-flow file earlier in the list.
pub fn check_syntax(item: &Node) -> DResult<()> {
    match item {
        Node::Scalar(s) => match s.typed() {
            Typed::Str(text) if STRING_COMMANDS.contains(&text.as_str()) => Ok(()),
            Typed::Str(text) if OBJECT_COMMANDS.contains(&text.as_str()) => Err(Problem::new(
                "Missing Command Options",
                format!("The command `{text}` requires additional options."),
                s.pos,
            )),
            Typed::Str(text) => Err(Problem::new(
                format!("Invalid Command: {text}"),
                format!("`{text}` is not a valid command.\n\n{}", suggest(&text)).trim().to_string(),
                s.pos,
            )),
            _ => Err(invalid_command(s.pos)),
        },
        Node::Seq(..) => Err(invalid_command(item.pos())),
        Node::Map(entries, pos) => {
            let Some((key, value)) = entries.first() else {
                return Err(Problem::failed("Empty command", *pos));
            };
            let name = key.as_scalar().map(|s| s.text.as_str()).unwrap_or_default();
            if !OBJECT_COMMANDS.contains(&name) {
                return Err(Problem::new(
                    format!("Invalid Command: {name}"),
                    format!("`{name}` is not a valid command.\n\n{}", suggest(name)).trim().to_string(),
                    key.pos(),
                ));
            }
            if value.is_null() {
                return Err(Problem::new(
                    format!("Incorrect Command Format: {name}"),
                    format!("The command `{name}` requires additional options."),
                    value.pos(),
                ));
            }
            if let Some((extra, _)) = entries.get(1) {
                let field = extra.as_scalar().map(|s| s.text.as_str()).unwrap_or_default();
                return Err(Problem::new(
                    format!("Invalid Command Format: {name}"),
                    format!("Found unexpected top-level field: `{field}`. Missing an indent or dash?"),
                    extra.pos(),
                ));
            }
            if matches!(name, "runFlow" | "repeat" | "retry")
                && let Some(Node::Seq(items, _)) = value
                    .map_entries()
                    .and_then(|e| e.into_iter().find(|(k, _, _)| *k == "commands").map(|(_, v, _)| v))
            {
                items.iter().try_for_each(check_syntax)?;
            }
            Ok(())
        }
    }
}

/// [`check_syntax`] for a hook (`onFlowStart` / `onFlowComplete`) value.
/// Only the list form exists: `YamlOnFlowStart` binds through a delegating
/// creator taking a list, so a mapping is a format error.
pub fn check_hook_syntax(n: &Node, field: &str) -> DResult<()> {
    match n {
        Node::Seq(items, _) => items.iter().try_for_each(check_syntax),
        _ => Err(Problem::format(field, n.pos())),
    }
}

fn invalid_command(pos: Pos) -> Problem {
    Problem::new(
        "Invalid Command",
        "Invalid command format. Expected: \"<commandName>: <options>\" eg. \"tapOn: submit\"",
        pos,
    )
}

/// Maestro's suggestion text for an unknown command name.
fn suggest(invalid: &str) -> String {
    let all = all_command_names();
    let mut out: Vec<&str> = Vec::new();
    let mut add = |c: &'static str| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    if invalid.len() >= 3 {
        for c in &all {
            if c.starts_with(invalid) || invalid.starts_with(c) {
                add(c);
            }
        }
    }
    let mut similar: Vec<(usize, &'static str)> =
        all.iter().map(|c| (levenshtein(invalid, c), *c)).filter(|(d, _)| *d <= 3).collect();
    similar.sort_by_key(|(d, _)| *d);
    for (_, c) in similar {
        add(c);
    }
    if invalid.len() >= 3 {
        for c in &all {
            if c.contains(invalid) || invalid.contains(c) {
                add(c);
            }
        }
    }
    match out.as_slice() {
        [] => String::new(),
        [one] => format!("Did you mean `{one}`?"),
        many => format!("Did you mean one of: {}", many.join(", ")),
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = if a[i - 1] == b[j - 1] { prev[j - 1] } else { 1 + prev[j].min(cur[j - 1]).min(prev[j - 1]) };
        }
        prev = cur;
    }
    prev[b.len()]
}

fn string_command(name: &str, pos: Pos, ctx: &Ctx) -> DResult<Vec<MaestroCommand>> {
    let basic = Basic::default;
    let app = |optional| AppCommand { app_id: ctx.app_id.to_string(), label: None, optional };
    let random = |t, length| {
        MaestroCommand::InputRandom(InputRandom { input_type: Some(t), length, label: None, optional: false })
    };
    let cmd = match name {
        "launchApp" => MaestroCommand::LaunchApp(LaunchApp {
            app_id: ctx.app_id.to_string(),
            clear_state: None,
            clear_keychain: None,
            stop_app: None,
            permissions: None,
            launch_arguments: None,
            label: None,
            optional: false,
        }),
        "stopApp" => MaestroCommand::StopApp(app(false)),
        "killApp" => MaestroCommand::KillApp(app(false)),
        "clearState" => MaestroCommand::ClearState(app(false)),
        "clearKeychain" => MaestroCommand::ClearKeychain(basic()),
        "eraseText" => MaestroCommand::EraseText(EraseText { characters_to_erase: None, label: None, optional: false }),
        "inputRandomText" => random(InputRandomType::Text, Some(8)),
        "inputRandomNumber" => random(InputRandomType::Number, Some(8)),
        "inputRandomEmail" => random(InputRandomType::TextEmailAddress, Some(8)),
        "inputRandomPersonName" => random(InputRandomType::TextPersonName, Some(8)),
        "inputRandomCityName" => random(InputRandomType::TextCityName, Some(8)),
        "inputRandomCountryName" => random(InputRandomType::TextCountryName, Some(8)),
        "inputRandomColorName" => random(InputRandomType::TextColor, Some(8)),
        "back" => MaestroCommand::BackPress(basic()),
        "hideKeyboard" | "hide keyboard" => MaestroCommand::HideKeyboard(basic()),
        "pasteText" => MaestroCommand::PasteText(basic()),
        "scroll" => MaestroCommand::Scroll(basic()),
        "waitForAnimationToEnd" => {
            MaestroCommand::WaitForAnimationToEnd(WaitForAnimationToEnd { timeout: None, label: None, optional: false })
        }
        "stopRecording" => MaestroCommand::StopRecording(basic()),
        "toggleAirplaneMode" => MaestroCommand::ToggleAirplaneMode(basic()),
        "toggleDarkMode" => MaestroCommand::ToggleDarkMode(basic()),
        "assertDarkMode" => MaestroCommand::AssertDarkMode(basic()),
        "assertLightMode" => MaestroCommand::AssertLightMode(basic()),
        "assertNoDefectsWithAI" => {
            MaestroCommand::AssertNoDefectsWithAi(AssertNoDefectsWithAi { optional: true, label: None })
        }
        _ if OBJECT_COMMANDS.contains(&name) => {
            return Err(Problem::new(
                "Missing Command Options",
                format!("The command `{name}` requires additional options."),
                pos,
            ));
        }
        _ => {
            return Err(Problem::new(
                format!("Invalid Command: {name}"),
                format!("`{name}` is not a valid command.\n\n{}\n\nSee {DOCS_COMMANDS}", suggest(name)),
                pos,
            ));
        }
    };
    Ok(vec![cmd])
}

/// The value of a command was the wrong YAML shape for its class. Jackson
/// reports these without a property path, which makes Maestro's error
/// mapper itself fail; the user sees this generic message.
fn shape(pos: Pos) -> Problem {
    Problem::new("Failed to parse file", "List is empty.", pos)
}

/// A string from a scalar for a delegating `String` creator: only real
/// string tokens qualify (numbers and booleans do not coerce there).
fn delegated_string(n: &Node) -> DResult<String> {
    match n.as_scalar().map(|s| s.typed()) {
        Some(Typed::Str(t)) => Ok(t),
        _ => Err(shape(n.pos())),
    }
}

/// Label and optional, present on almost every command class.
struct Common {
    label: Option<String>,
    optional: bool,
}

fn common(o: &Obj, optional_default: bool) -> DResult<Common> {
    Ok(Common { label: o.string("label")?, optional: o.bool("optional")?.unwrap_or(optional_default) })
}

fn basic_object(v: &Node, name: &str) -> DResult<Basic> {
    if v.as_scalar().is_some() {
        return Err(shape(v.pos()));
    }
    let o = Obj::bind(v, &["label", "optional"], name, Mode::Stream)?;
    let c = common(&o, false)?;
    Ok(Basic { label: c.label, optional: c.optional })
}

fn object_command(name: &str, v: &Node, ctx: &Ctx) -> Result<Vec<MaestroCommand>, FlowFailure> {
    let one = |c: MaestroCommand| Ok(vec![c]);
    let here = FlowFailure::here;
    match name {
        "tapOn" => one(tap(v, false, None).map_err(here)?),
        "longPressOn" => one(tap(v, true, None).map_err(here)?),
        "doubleTapOn" => {
            let sel = union(v, name).map_err(here)?;
            let delay = match &sel {
                Union::Object(y) => y.delay.map(i64::from).filter(|d| *d >= 0).unwrap_or(TAP_REPEAT_DELAY_MS),
                Union::Text(_) => TAP_REPEAT_DELAY_MS,
            };
            one(tap_command(sel, false, Some(TapRepeat { repeat: 2, delay })).map_err(here)?)
        }
        "assertVisible" | "assertNotVisible" => {
            let sel = union(v, name).map_err(here)?;
            let (label, optional) = match &sel {
                Union::Object(y) => (y.label.clone(), y.optional.unwrap_or(false)),
                Union::Text(_) => (None, false),
            };
            let selector = Some(sel.into_selector().map_err(here)?);
            let condition = if name == "assertVisible" {
                Condition { visible: selector, ..Default::default() }
            } else {
                Condition { not_visible: selector, ..Default::default() }
            };
            one(MaestroCommand::AssertCondition(AssertCondition { condition, timeout: None, label, optional }))
        }
        "copyTextFrom" => {
            let sel = union(v, name).map_err(here)?;
            let (label, optional) = match &sel {
                Union::Object(y) => (y.label.clone(), y.optional.unwrap_or(false)),
                Union::Text(_) => (None, false),
            };
            let selector = sel.into_selector().map_err(here)?;
            one(MaestroCommand::CopyTextFrom(CopyTextFrom { selector, label, optional }))
        }
        "assertTrue" => {
            let (condition, label, optional) = match v {
                Node::Map(..) => {
                    let o = Obj::open(v, name, Mode::Tree).map_err(here)?;
                    let condition = map_string(&o, "condition", "").map_err(here)?;
                    let label = map_opt_string(&o, "label").map_err(here)?;
                    let optional = match o.get("optional") {
                        None => false,
                        Some(n) => match n.as_scalar().map(|s| s.typed()) {
                            Some(Typed::Bool(b)) => b,
                            _ => {
                                return Err(here(Problem::failed(
                                    "class cannot be cast to class java.lang.Boolean",
                                    n.pos(),
                                )));
                            }
                        },
                    };
                    (condition, label, optional)
                }
                Node::Scalar(s) => (any_scalar_text(s, "assert true")?, None, false),
                Node::Seq(..) => return Err(here(unsupported("assert true", v.pos()))),
            };
            let condition = Condition { script_condition: Some(condition), ..Default::default() };
            one(MaestroCommand::AssertCondition(AssertCondition { condition, timeout: None, label, optional }))
        }
        "assertNoDefectsWithAI" => {
            if v.as_scalar().is_some() {
                return Err(here(shape(v.pos())));
            }
            let o = Obj::bind(v, &["optional", "label"], name, Mode::Stream).map_err(here)?;
            let c = common(&o, true).map_err(here)?;
            one(MaestroCommand::AssertNoDefectsWithAi(AssertNoDefectsWithAi { optional: c.optional, label: c.label }))
        }
        "assertWithAI" => {
            let (assertion, c) = match v {
                Node::Scalar(_) => (delegated_string(v).map_err(here)?, Common { label: None, optional: true }),
                _ => {
                    let o = Obj::bind(v, &["assertion", "optional", "label"], name, Mode::Stream).map_err(here)?;
                    (o.req_string("assertion").map_err(here)?, common(&o, true).map_err(here)?)
                }
            };
            one(MaestroCommand::AssertWithAi(AssertWithAi { assertion, optional: c.optional, label: c.label }))
        }
        "extractTextWithAI" => {
            let (query, output, c) = match v {
                Node::Scalar(_) => {
                    (delegated_string(v).map_err(here)?, "aiOutput".to_string(), Common { label: None, optional: true })
                }
                _ => {
                    let o = Obj::bind(v, &["query", "outputVariable", "optional", "label"], name, Mode::Stream)
                        .map_err(here)?;
                    let out = o.string("outputVariable").map_err(here)?.unwrap_or_else(|| "aiOutput".into());
                    (o.req_string("query").map_err(here)?, out, common(&o, true).map_err(here)?)
                }
            };
            one(MaestroCommand::ExtractTextWithAi(ExtractTextWithAi {
                query,
                output_variable: output,
                optional: c.optional,
                label: c.label,
            }))
        }
        "assertScreenshot" => {
            let (path, threshold, crop_on, c) = match v {
                Node::Scalar(_) => {
                    (delegated_string(v).map_err(here)?, None, None, Common { label: None, optional: false })
                }
                _ => {
                    let o = Obj::bind(
                        v,
                        &["path", "thresholdPercentage", "cropOn", "label", "optional"],
                        name,
                        Mode::Stream,
                    )
                    .map_err(here)?;
                    let crop = o.get("cropOn").map(|n| union(n, "cropOn")).transpose().map_err(here)?;
                    (
                        o.req_string("path").map_err(here)?,
                        o.string("thresholdPercentage").map_err(here)?,
                        crop,
                        common(&o, false).map_err(here)?,
                    )
                }
            };
            one(MaestroCommand::AssertScreenshot(AssertScreenshot {
                path,
                threshold_percentage: threshold.unwrap_or_else(|| "95".into()),
                crop_on: crop_on.map(Union::into_selector).transpose().map_err(here)?,
                label: c.label,
                optional: c.optional,
            }))
        }
        "addMedia" => one(MaestroCommand::AddMedia(add_media(v, ctx)?)),
        "inputText" => {
            let (text, label) = match v {
                Node::Map(..) => {
                    let o = Obj::open(v, name, Mode::Tree).map_err(here)?;
                    (map_string(&o, "text", "").map_err(here)?, map_opt_string(&o, "label").map_err(here)?)
                }
                Node::Scalar(s) => (any_scalar_text(s, "input text")?, None),
                Node::Seq(..) => return Err(here(unsupported("input text", v.pos()))),
            };
            // The mapping form ignores `optional` (YamlInputText.parse).
            one(MaestroCommand::InputText(InputText { text, label, optional: false }))
        }
        "inputRandomText" | "inputRandomNumber" => {
            if v.as_scalar().is_some() {
                return Err(here(shape(v.pos())));
            }
            let o = Obj::bind(v, &["length", "label", "optional"], name, Mode::Stream).map_err(here)?;
            let c = common(&o, false).map_err(here)?;
            let t = if name == "inputRandomText" { InputRandomType::Text } else { InputRandomType::Number };
            one(MaestroCommand::InputRandom(InputRandom {
                input_type: Some(t),
                length: o.int("length").map_err(here)?,
                label: c.label,
                optional: c.optional,
            }))
        }
        "inputRandomEmail"
        | "inputRandomPersonName"
        | "inputRandomCityName"
        | "inputRandomCountryName"
        | "inputRandomColorName" => {
            let b = basic_object(v, name).map_err(here)?;
            let t = match name {
                "inputRandomEmail" => InputRandomType::TextEmailAddress,
                "inputRandomPersonName" => InputRandomType::TextPersonName,
                "inputRandomCityName" => InputRandomType::TextCityName,
                "inputRandomCountryName" => InputRandomType::TextCountryName,
                _ => InputRandomType::TextColor,
            };
            one(MaestroCommand::InputRandom(InputRandom {
                input_type: Some(t),
                length: Some(8),
                label: b.label,
                optional: b.optional,
            }))
        }
        "launchApp" => {
            let cmd = match v {
                Node::Scalar(_) => LaunchApp {
                    app_id: delegated_string(v).map_err(here)?,
                    clear_state: None,
                    clear_keychain: None,
                    stop_app: None,
                    permissions: None,
                    launch_arguments: None,
                    label: None,
                    optional: false,
                },
                _ => {
                    let o = Obj::bind(
                        v,
                        &[
                            "appId",
                            "url",
                            "clearState",
                            "clearKeychain",
                            "stopApp",
                            "permissions",
                            "arguments",
                            "label",
                            "optional",
                        ],
                        name,
                        Mode::Stream,
                    )
                    .map_err(here)?;
                    let c = common(&o, false).map_err(here)?;
                    LaunchApp {
                        app_id: app_id(&o).map_err(here)?.unwrap_or_else(|| ctx.app_id.to_string()),
                        clear_state: o.bool("clearState").map_err(here)?,
                        clear_keychain: o.bool("clearKeychain").map_err(here)?,
                        stop_app: o.bool("stopApp").map_err(here)?,
                        permissions: o.str_map("permissions").map_err(here)?,
                        launch_arguments: o.any_map("arguments").map_err(here)?,
                        label: c.label,
                        optional: c.optional,
                    }
                }
            };
            one(MaestroCommand::LaunchApp(cmd))
        }
        "setPermissions" => {
            if v.as_scalar().is_some() {
                return Err(here(shape(v.pos())));
            }
            let o = Obj::bind(v, &["appId", "url", "permissions", "label", "optional"], name, Mode::Stream)
                .map_err(here)?;
            let c = common(&o, false).map_err(here)?;
            let permissions =
                o.str_map("permissions").map_err(here)?.ok_or_else(|| here(Problem::missing("permissions", o.pos)))?;
            one(MaestroCommand::SetPermissions(SetPermissions {
                app_id: app_id(&o).map_err(here)?.unwrap_or_else(|| ctx.app_id.to_string()),
                permissions,
                label: c.label,
                optional: c.optional,
            }))
        }
        "swipe" => one(MaestroCommand::Swipe(swipe(v).map_err(here)?)),
        "openLink" => {
            let (link, browser, auto_verify, c) = match v {
                Node::Scalar(_) => {
                    (delegated_string(v).map_err(here)?, false, false, Common { label: None, optional: false })
                }
                _ => {
                    let o = Obj::bind(v, &["link", "browser", "autoVerify", "label", "optional"], name, Mode::Stream)
                        .map_err(here)?;
                    (
                        o.req_string("link").map_err(here)?,
                        o.bool("browser").map_err(here)?.unwrap_or(false),
                        o.bool("autoVerify").map_err(here)?.unwrap_or(false),
                        common(&o, false).map_err(here)?,
                    )
                }
            };
            one(MaestroCommand::OpenLink(OpenLink {
                link,
                auto_verify: Some(auto_verify),
                browser: Some(browser),
                label: c.label,
                optional: c.optional,
            }))
        }
        "pressKey" => {
            let (key, c) = match v {
                Node::Scalar(_) => (delegated_string(v).map_err(here)?, Common { label: None, optional: false }),
                _ => {
                    let o = Obj::bind(v, &["key", "label", "optional"], name, Mode::Stream).map_err(here)?;
                    (o.req_string("key").map_err(here)?, common(&o, false).map_err(here)?)
                }
            };
            let code = KeyCode::by_name(&key)
                .ok_or_else(|| here(Problem::failed(format!("Unknown key name: {key}"), v.pos())))?;
            one(MaestroCommand::PressKey(PressKey { code, label: c.label, optional: c.optional }))
        }
        "eraseText" => {
            let cmd = match v {
                Node::Scalar(_) => EraseText {
                    characters_to_erase: Some(super::decode::int(v, name).map_err(|_| here(shape(v.pos())))?),
                    label: None,
                    optional: false,
                },
                _ => {
                    let o =
                        Obj::bind(v, &["charactersToErase", "label", "optional"], name, Mode::Stream).map_err(here)?;
                    let c = common(&o, false).map_err(here)?;
                    EraseText {
                        characters_to_erase: o.int("charactersToErase").map_err(here)?,
                        label: c.label,
                        optional: c.optional,
                    }
                }
            };
            one(MaestroCommand::EraseText(cmd))
        }
        "action" => {
            let action = super::decode::string(v, name, Mode::Stream).map_err(here)?;
            let b = Basic::default();
            one(match action.as_str() {
                "back" => MaestroCommand::BackPress(b),
                "hideKeyboard" => MaestroCommand::HideKeyboard(b),
                "scroll" => MaestroCommand::Scroll(b),
                "clearKeychain" => MaestroCommand::ClearKeychain(b),
                "pasteText" => MaestroCommand::PasteText(b),
                _ => return Err(here(Problem::failed(format!("Unknown navigation target: {action}"), v.pos()))),
            })
        }
        "back" => one(MaestroCommand::BackPress(basic_object(v, name).map_err(here)?)),
        "clearKeychain" => one(MaestroCommand::ClearKeychain(basic_object(v, name).map_err(here)?)),
        "hideKeyboard" => one(MaestroCommand::HideKeyboard(basic_object(v, name).map_err(here)?)),
        "pasteText" => one(MaestroCommand::PasteText(basic_object(v, name).map_err(here)?)),
        "scroll" => one(MaestroCommand::Scroll(basic_object(v, name).map_err(here)?)),
        "stopRecording" => one(MaestroCommand::StopRecording(basic_object(v, name).map_err(here)?)),
        "toggleAirplaneMode" => one(MaestroCommand::ToggleAirplaneMode(basic_object(v, name).map_err(here)?)),
        "toggleDarkMode" => one(MaestroCommand::ToggleDarkMode(basic_object(v, name).map_err(here)?)),
        "assertDarkMode" => one(MaestroCommand::AssertDarkMode(basic_object(v, name).map_err(here)?)),
        "assertLightMode" => one(MaestroCommand::AssertLightMode(basic_object(v, name).map_err(here)?)),
        "takeScreenshot" => {
            let (path, crop, c) = match v {
                Node::Scalar(_) => (delegated_string(v).map_err(here)?, None, Common { label: None, optional: false }),
                _ => {
                    let o = Obj::bind(v, &["path", "label", "cropOn", "optional"], name, Mode::Stream).map_err(here)?;
                    let crop = o.get("cropOn").map(|n| union(n, "cropOn")).transpose().map_err(here)?;
                    (o.req_string("path").map_err(here)?, crop, common(&o, false).map_err(here)?)
                }
            };
            one(MaestroCommand::TakeScreenshot(TakeScreenshot {
                path,
                crop_on: crop.map(Union::into_selector).transpose().map_err(here)?,
                label: c.label,
                optional: c.optional,
            }))
        }
        "extendedWaitUntil" => {
            if v.as_scalar().is_some() {
                return Err(here(shape(v.pos())));
            }
            let o = Obj::bind(v, &["visible", "notVisible", "timeout", "label", "optional"], name, Mode::Stream)
                .map_err(here)?;
            let visible = o.get("visible").map(|n| union(n, "visible")).transpose().map_err(here)?;
            let not_visible = o.get("notVisible").map(|n| union(n, "notVisible")).transpose().map_err(here)?;
            let timeout = o.string("timeout").map_err(here)?;
            let c = common(&o, false).map_err(here)?;
            if visible.is_none() && not_visible.is_none() {
                return Err(here(Problem::failed(
                    "extendedWaitUntil expects either `visible` or `notVisible` to be provided",
                    v.pos(),
                )));
            }
            let condition = Condition {
                visible: visible.map(Union::into_selector).transpose().map_err(here)?,
                not_visible: not_visible.map(Union::into_selector).transpose().map_err(here)?,
                ..Default::default()
            };
            one(MaestroCommand::AssertCondition(AssertCondition {
                condition,
                timeout,
                label: c.label,
                optional: c.optional,
            }))
        }
        "stopApp" | "killApp" | "clearState" => {
            let cmd = match v {
                Node::Scalar(_) => {
                    AppCommand { app_id: delegated_string(v).map_err(here)?, label: None, optional: false }
                }
                _ => {
                    let o = Obj::bind(v, &["appId", "label", "optional"], name, Mode::Stream).map_err(here)?;
                    let c = common(&o, false).map_err(here)?;
                    AppCommand {
                        app_id: o.string("appId").map_err(here)?.unwrap_or_else(|| ctx.app_id.to_string()),
                        label: c.label,
                        optional: c.optional,
                    }
                }
            };
            one(match name {
                "stopApp" => MaestroCommand::StopApp(cmd),
                "killApp" => MaestroCommand::KillApp(cmd),
                _ => MaestroCommand::ClearState(cmd),
            })
        }
        "runFlow" => one(run_flow(v, ctx)?),
        "retry" => one(retry(v, ctx)?),
        "repeat" => {
            if v.as_scalar().is_some() {
                return Err(here(shape(v.pos())));
            }
            let o =
                Obj::bind(v, &["times", "while", "commands", "label", "optional"], name, Mode::Stream).map_err(here)?;
            let times = o.string("times").map_err(here)?;
            let condition = o.get("while").map(|n| condition(n, "while")).transpose().map_err(here)?;
            let c = common(&o, false).map_err(here)?;
            let list = o.get("commands").ok_or_else(|| here(Problem::missing("commands", o.pos)))?;
            let commands = command_list(list, "commands", ctx, None)?;
            one(MaestroCommand::Repeat(Repeat { times, condition, commands, label: c.label, optional: c.optional }))
        }
        "setLocation" => {
            if v.as_scalar().is_some() {
                return Err(here(shape(v.pos())));
            }
            let o = Obj::bind(v, &["latitude", "longitude", "label", "optional"], name, Mode::Stream).map_err(here)?;
            let c = common(&o, false).map_err(here)?;
            one(MaestroCommand::SetLocation(SetLocation {
                latitude: o.req_string("latitude").map_err(here)?,
                longitude: o.req_string("longitude").map_err(here)?,
                label: c.label,
                optional: c.optional,
            }))
        }
        "setOrientation" => one(MaestroCommand::SetOrientation(set_orientation(v).map_err(here)?)),
        "setClipboard" => {
            let (text, c) = match v {
                Node::Scalar(_) => (delegated_string(v).map_err(here)?, Common { label: None, optional: false }),
                _ => {
                    let o = Obj::bind(v, &["text", "label", "optional"], name, Mode::Stream).map_err(here)?;
                    (o.req_string("text").map_err(here)?, common(&o, false).map_err(here)?)
                }
            };
            one(MaestroCommand::SetClipboard(SetClipboard { text, label: c.label, optional: c.optional }))
        }
        "runScript" => one(run_script(v, ctx)?),
        "waitForAnimationToEnd" => {
            if v.as_scalar().is_some() {
                return Err(here(shape(v.pos())));
            }
            let o = Obj::bind(v, &["timeout", "label", "optional"], name, Mode::Stream).map_err(here)?;
            let c = common(&o, false).map_err(here)?;
            one(MaestroCommand::WaitForAnimationToEnd(WaitForAnimationToEnd {
                timeout: o.string("timeout").map_err(here)?,
                label: c.label,
                optional: c.optional,
            }))
        }
        "evalScript" => {
            let (script, label) = match v {
                Node::Map(..) => {
                    let o = Obj::open(v, name, Mode::Tree).map_err(here)?;
                    (map_string(&o, "script", "").map_err(here)?, map_opt_string(&o, "label").map_err(here)?)
                }
                Node::Scalar(s) => (any_scalar_text(s, "evaluate script")?, None),
                Node::Seq(..) => return Err(here(unsupported("evaluate script", v.pos()))),
            };
            one(MaestroCommand::EvalScript(EvalScript { script_string: script, label, optional: false }))
        }
        "scrollUntilVisible" => one(MaestroCommand::ScrollUntilVisible(scroll_until_visible(v).map_err(here)?)),
        "travel" => one(MaestroCommand::Travel(travel(v).map_err(here)?)),
        "startRecording" => {
            let (path, c) = match v {
                Node::Scalar(_) => (delegated_string(v).map_err(here)?, Common { label: None, optional: false }),
                _ => {
                    let o = Obj::bind(v, &["path", "label", "optional"], name, Mode::Stream).map_err(here)?;
                    (o.req_string("path").map_err(here)?, common(&o, false).map_err(here)?)
                }
            };
            one(MaestroCommand::StartRecording(StartRecording { path, label: c.label, optional: c.optional }))
        }
        "setAirplaneMode" => {
            one(MaestroCommand::SetAirplaneMode(set_toggle(v, "setAirplaneMode", "airplane mode").map_err(here)?))
        }
        "setDarkMode" => one(MaestroCommand::SetDarkMode(set_toggle(v, "setDarkMode", "dark mode").map_err(here)?)),
        "callHook" => one(MaestroCommand::CallHook(call_hook(v).map_err(here)?)),
        _ => Err(here(Problem::failed(format!("Invalid command: No mapping provided for {name}"), v.pos()))),
    }
}

/// `appId` or its alias `url`.
fn app_id(o: &Obj) -> DResult<Option<String>> {
    Ok(o.string("appId")?.or(o.string("url")?))
}

/// `Map.getOrDefault(key, default) as String` in a delegating creator.
fn map_string(o: &Obj, key: &str, default: &str) -> DResult<String> {
    Ok(map_opt_string(o, key)?.unwrap_or_else(|| default.to_string()))
}

fn map_opt_string(o: &Obj, key: &str) -> DResult<Option<String>> {
    match o.get(key) {
        None => Ok(None),
        Some(n) => match n.as_scalar().map(|s| s.typed()) {
            Some(Typed::Str(t)) => Ok(Some(t)),
            _ => Err(Problem::failed("class cannot be cast to class java.lang.String", n.pos())),
        },
    }
}

/// Text of a scalar handed to a delegating creator taking `Any`.
fn any_scalar_text(s: &super::yaml::Scalar, what: &str) -> Result<String, FlowFailure> {
    untyped_scalar_text(s).ok_or_else(|| FlowFailure::here(unsupported(what, s.pos)))
}

fn unsupported(what: &str, pos: Pos) -> Problem {
    Problem::failed(format!("Cannot deserialize {what} with this data type"), pos)
}

// ---------------------------------------------------------------- selectors

/// `YamlElementSelectorUnion`: a bare value is a text selector.
pub enum Union {
    Text(String),
    Object(Box<YamlSelector>),
}

/// `YamlElementSelector` (bound from a tree, so strings are canonical).
#[derive(Default)]
pub struct YamlSelector {
    text: Option<String>,
    id: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    tolerance: Option<i32>,
    optional: Option<bool>,
    retry_tap_if_no_change: Option<bool>,
    wait_until_visible: Option<bool>,
    point: Option<String>,
    below: Option<Union>,
    above: Option<Union>,
    left_of: Option<Union>,
    right_of: Option<Union>,
    contains_child: Option<Union>,
    contains_descendants: Option<Vec<Union>>,
    traits: Option<String>,
    index: Option<String>,
    enabled: Option<bool>,
    selected: Option<bool>,
    checked: Option<bool>,
    focused: Option<bool>,
    repeat: Option<i32>,
    delay: Option<i32>,
    wait_to_settle_timeout_ms: Option<i32>,
    child_of: Option<Union>,
    label: Option<String>,
    css: Option<String>,
}

const SELECTOR_FIELDS: &[&str] = &[
    "text",
    "id",
    "width",
    "height",
    "tolerance",
    "optional",
    "retryTapIfNoChange",
    "waitUntilVisible",
    "point",
    "below",
    "above",
    "leftOf",
    "rightOf",
    "containsChild",
    "containsDescendants",
    "traits",
    "index",
    "enabled",
    "selected",
    "checked",
    "focused",
    "repeat",
    "delay",
    "waitToSettleTimeoutMs",
    "childOf",
    "label",
    "css",
];

pub fn union(n: &Node, field: &str) -> DResult<Union> {
    match n {
        Node::Scalar(s) => Ok(Union::Text(canonical_text(s).unwrap_or_else(|| "null".into()))),
        Node::Map(..) => Ok(Union::Object(Box::new(yaml_selector(n, field)?))),
        Node::Seq(..) => Err(Problem::format(field, n.pos())),
    }
}

fn opt_union(o: &Obj, key: &str) -> DResult<Option<Union>> {
    o.get(key).map(|n| union(n, key)).transpose()
}

fn yaml_selector(n: &Node, field: &str) -> DResult<YamlSelector> {
    let o = Obj::bind(n, SELECTOR_FIELDS, field, Mode::Tree)?;
    let contains_descendants = match o.get("containsDescendants") {
        None => None,
        Some(Node::Seq(items, _)) => {
            Some(items.iter().map(|i| union(i, "containsDescendants")).collect::<DResult<_>>()?)
        }
        Some(other) => return Err(Problem::format("containsDescendants", other.pos())),
    };
    Ok(YamlSelector {
        text: o.string("text")?,
        id: o.string("id")?,
        width: o.int("width")?,
        height: o.int("height")?,
        tolerance: o.int("tolerance")?,
        optional: o.bool("optional")?,
        retry_tap_if_no_change: o.bool("retryTapIfNoChange")?,
        wait_until_visible: o.bool("waitUntilVisible")?,
        point: o.string("point")?,
        below: opt_union(&o, "below")?,
        above: opt_union(&o, "above")?,
        left_of: opt_union(&o, "leftOf")?,
        right_of: opt_union(&o, "rightOf")?,
        contains_child: opt_union(&o, "containsChild")?,
        contains_descendants,
        traits: o.string("traits")?,
        index: o.string("index")?,
        enabled: o.bool("enabled")?,
        selected: o.bool("selected")?,
        checked: o.bool("checked")?,
        focused: o.bool("focused")?,
        repeat: o.int("repeat")?,
        delay: o.int("delay")?,
        wait_to_settle_timeout_ms: o.int("waitToSettleTimeoutMs")?,
        child_of: opt_union(&o, "childOf")?,
        label: o.string("label")?,
        css: o.string("css")?,
    })
}

impl Union {
    /// `toElementSelector` (traits are validated here, as in Maestro).
    pub fn into_selector(self) -> DResult<ElementSelector> {
        let y = match self {
            Union::Text(t) => return Ok(ElementSelector { text_regex: Some(t), ..Default::default() }),
            Union::Object(y) => *y,
        };
        let sub = |u: Option<Union>| -> DResult<Option<Box<ElementSelector>>> {
            u.map(|u| u.into_selector().map(Box::new)).transpose()
        };
        let size = (y.width.is_some() || y.height.is_some()).then_some(SizeSelector {
            width: y.width,
            height: y.height,
            tolerance: y.tolerance,
        });
        let traits = y
            .traits
            .map(|t| {
                t.split(' ')
                    .map(|name| match name.replace('-', "_").to_uppercase().as_str() {
                        "TEXT" => Ok(ElementTrait::Text),
                        "SQUARE" => Ok(ElementTrait::Square),
                        "LONG_TEXT" => Ok(ElementTrait::LongText),
                        other => Err(Problem::failed(
                            format!("No enum constant maestro.orchestra.ElementTrait.{other}"),
                            Pos::default(),
                        )),
                    })
                    .collect::<DResult<Vec<_>>>()
            })
            .transpose()?;
        Ok(ElementSelector {
            text_regex: y.text,
            id_regex: y.id,
            size,
            below: sub(y.below)?,
            above: sub(y.above)?,
            left_of: sub(y.left_of)?,
            right_of: sub(y.right_of)?,
            contains_child: sub(y.contains_child)?,
            contains_descendants: y
                .contains_descendants
                .map(|v| v.into_iter().map(Union::into_selector).collect::<DResult<Vec<_>>>())
                .transpose()?,
            traits,
            index: y.index,
            enabled: y.enabled,
            optional: y.optional.unwrap_or(false),
            selected: y.selected,
            checked: y.checked,
            focused: y.focused,
            child_of: sub(y.child_of)?,
            css: y.css,
        })
    }
}

fn has_any_selector(s: &ElementSelector) -> bool {
    s.text_regex.is_some()
        || s.id_regex.is_some()
        || s.size.is_some()
        || s.below.is_some()
        || s.above.is_some()
        || s.left_of.is_some()
        || s.right_of.is_some()
        || s.contains_child.is_some()
        || s.contains_descendants.is_some()
        || s.traits.is_some()
        || s.index.is_some()
        || s.enabled.is_some()
        || s.selected.is_some()
        || s.checked.is_some()
        || s.focused.is_some()
        || s.child_of.is_some()
        || s.css.is_some()
}

fn tap(v: &Node, long_press: bool, repeat: Option<TapRepeat>) -> DResult<MaestroCommand> {
    tap_command(union(v, "tapOn")?, long_press, repeat)
}

fn tap_command(sel: Union, long_press: bool, tap_repeat: Option<TapRepeat>) -> DResult<MaestroCommand> {
    let y = match &sel {
        Union::Object(y) => Some(y.as_ref()),
        Union::Text(_) => None,
    };
    let retry = y.and_then(|y| y.retry_tap_if_no_change).unwrap_or(false);
    let wait_until_visible = y.and_then(|y| y.wait_until_visible).unwrap_or(false);
    let point = y.and_then(|y| y.point.clone());
    let label = y.and_then(|y| y.label.clone());
    let optional = y.and_then(|y| y.optional).unwrap_or(false);
    let delay = y.and_then(|y| y.delay).map(i64::from);
    let repeat = tap_repeat.or_else(|| {
        y.and_then(|y| y.repeat).map(|r| TapRepeat {
            repeat: if r <= 0 { 1 } else { r },
            delay: delay.filter(|d| *d >= 0).unwrap_or(TAP_REPEAT_DELAY_MS),
        })
    });
    let settle = y.and_then(|y| y.wait_to_settle_timeout_ms).map(|t| t.min(MAX_WAIT_TO_SETTLE_MS));
    let selector = sel.into_selector()?;
    Ok(match point {
        Some(point) if !has_any_selector(&selector) => MaestroCommand::TapOnPoint(TapOnPoint {
            point,
            retry_if_no_change: Some(retry),
            long_press: Some(long_press),
            repeat,
            wait_to_settle_timeout_ms: settle,
            label,
            optional,
        }),
        relative_point => MaestroCommand::TapOnElement(TapOnElement {
            selector,
            retry_if_no_change: Some(retry),
            wait_until_visible: Some(wait_until_visible),
            long_press: Some(long_press),
            repeat,
            wait_to_settle_timeout_ms: settle,
            relative_point,
            label,
            optional,
        }),
    })
}

// --------------------------------------------------------------- conditions

/// `YamlCondition` (`when:` / `while:`).
fn condition(n: &Node, field: &str) -> DResult<Condition> {
    let o = Obj::bind(n, &["platform", "visible", "notVisible", "true", "label", "optional"], field, Mode::Stream)?;
    let platform = o
        .string("platform")?
        .map(|p| match p.to_uppercase().as_str() {
            "ANDROID" => Ok(Platform::Android),
            "IOS" => Ok(Platform::Ios),
            "WEB" => Ok(Platform::Web),
            _ => Err(Problem::format("platform", o.pos)),
        })
        .transpose()?;
    let visible = opt_union(&o, "visible")?.map(Union::into_selector).transpose()?;
    let not_visible = opt_union(&o, "notVisible")?.map(Union::into_selector).transpose()?;
    o.bool("optional")?;
    Ok(Condition {
        platform,
        visible,
        not_visible,
        script_condition: o.string("true")?.map(|s| s.trim().to_string()),
        label: o.string("label")?,
    })
}

// -------------------------------------------------------------------- swipe

/// `YamlSwipeDeserializer`: reads a tree and picks the swipe kind by keys.
fn swipe(v: &Node) -> DResult<Swipe> {
    let o = Obj::open(v, "swipe", Mode::Tree).map_err(|_| shape(v.pos()))?;
    let text = |key: &str| o.get(key).and_then(|n| n.as_scalar()).and_then(canonical_text);
    let fail = |m: &str| Problem::failed(m, v.pos());
    let duration = match text("duration") {
        None => SWIPE_DURATION_MS,
        Some(d) => d.parse::<i64>().map_err(|_| fail(&format!("For input string: \"{d}\"")))?,
    };
    let label = text("label");
    let optional = text("optional").is_some_and(|s| s.eq_ignore_ascii_case("true"));
    let wait = text("waitToSettleTimeoutMs").and_then(|s| s.parse::<i32>().ok());
    let base = Swipe { duration, label, optional, wait_to_settle_timeout_ms: wait, ..Default::default() };
    let keys: Vec<&str> = o.keys().collect();
    if o.has("start") || o.has("end") {
        if o.has("direction") {
            return Err(fail("You cannot provide direction with start/end swipe."));
        }
        let (Some(start), Some(end)) = (text("start"), text("end")) else {
            return Err(fail("You need to provide both start and end coordinates, to swipe with coordinates"));
        };
        if start.contains('%') || end.contains('%') {
            if !(start.contains('%') && end.contains('%')) {
                return Err(fail(&format!(
                    "You need to provide start and end coordinates with %, Found: ({start}, {end})"
                )));
            }
            for p in [&start, &end] {
                let pts = ints(&p.replace('%', "")).ok_or_else(|| fail(&format!("Invalid point: {p}")))?;
                if pts.len() < 2 || !(0..=100).contains(&pts[0]) || !(0..=100).contains(&pts[1]) {
                    return Err(fail(&format!("Invalid start point: {p} should be between 0 to 100")));
                }
            }
            return Ok(Swipe { start_relative: Some(start), end_relative: Some(end), ..base });
        }
        let point = |p: &str| -> DResult<Point> {
            let pts = ints(p).ok_or_else(|| fail(&format!("For input string: \"{p}\"")))?;
            match pts.as_slice() {
                [x, y, ..] => Ok(Point { x: *x, y: *y }),
                _ => Err(fail("Index 1 out of bounds for length 1")),
            }
        };
        return Ok(Swipe { start_point: Some(point(&start)?), end_point: Some(point(&end)?), ..base });
    }
    if o.has("direction") {
        let raw = text("direction").unwrap_or_default();
        let direction = match raw.as_str() {
            "UP" | "up" => Direction::Up,
            "DOWN" | "down" => Direction::Down,
            "LEFT" | "left" => Direction::Left,
            "RIGHT" | "right" => Direction::Right,
            _ => {
                return Err(fail(&format!(
                    "Invalid direction provided to directional swipe: {raw}. Direction can be either:\n1. RIGHT or right\n2. LEFT or left\n3. UP or up\n4. DOWN or down"
                )));
            }
        };
        if !keys.contains(&"from") {
            return Ok(Swipe { direction: Some(direction), ..base });
        }
        // YamlSwipeElement via convertValue: strict properties.
        let e = Obj::bind(
            v,
            &["direction", "from", "duration", "label", "optional", "waitToSettleTimeoutMs"],
            "swipe",
            Mode::Tree,
        )?;
        let from = e.get("from").ok_or_else(|| Problem::missing("from", e.pos))?;
        let from = union(from, "from")?;
        let relative_point = match &from {
            Union::Object(y) => y.point.clone(),
            Union::Text(_) => None,
        };
        return Ok(Swipe {
            direction: Some(direction),
            element_selector: Some(from.into_selector()?),
            relative_point,
            ..base
        });
    }
    Err(fail(&format!(
        "Swipe command takes either: \n\t1. direction: Direction based swipe with: \"RIGHT\", \"LEFT\", \"UP\", or \"DOWN\" or \n\t2. start and end: Coordinates based swipe with: \"start\" and \"end\" coordinates \n\t3. direction and element to swipe directionally on element\nIt seems you provided invalid input with: [{}]",
        keys.join(", ")
    )))
}

/// `"10, 20".split(",").map { it.trim().toInt() }`.
fn ints(s: &str) -> Option<Vec<i32>> {
    s.split(',').map(|p| p.trim().parse::<i32>().ok()).collect()
}

// ------------------------------------------------------ orientation, toggles

fn set_orientation(v: &Node) -> DResult<SetOrientation> {
    let text_node = |n: &Node| match n.as_scalar().map(|s| s.typed()) {
        Some(Typed::Str(t)) => Ok(t),
        _ => {
            Err(Problem::failed("class cannot be cast to class com.fasterxml.jackson.databind.node.TextNode", n.pos()))
        }
    };
    let (orientation, label, optional) = match v {
        Node::Scalar(_) => (text_node(v)?, None, false),
        _ => {
            let o = Obj::open(v, "setOrientation", Mode::Tree)?;
            let node = o.get("orientation").ok_or_else(|| {
                Problem::failed("Missing required field 'orientation' in SetOrientation action", v.pos())
            })?;
            let label = o.get("label").and_then(|n| match n.as_scalar().map(|s| s.typed()) {
                Some(Typed::Str(t)) => Some(t),
                _ => None,
            });
            // `root.get("optional").toString().toBoolean()`: a text node prints
            // with its quotes, so only a YAML boolean true counts.
            let optional =
                o.get("optional").and_then(|n| n.as_scalar()).is_some_and(|s| matches!(s.typed(), Typed::Bool(true)));
            (text_node(node)?, label, optional)
        }
    };
    let resolved = orientation_by_name(&orientation);
    if resolved.is_none() && !orientation.contains("${") {
        return Err(Problem::failed(
            format!(
                "Unknown orientation: {orientation}. Valid orientations are: [PORTRAIT, LANDSCAPE_LEFT, LANDSCAPE_RIGHT, UPSIDE_DOWN]"
            ),
            v.pos(),
        ));
    }
    Ok(SetOrientation { orientation: resolved.map(str::to_string).unwrap_or(orientation), label, optional })
}

fn set_toggle(v: &Node, command: &str, what: &str) -> DResult<SetToggle> {
    let invalid = |input: String| {
        Problem::failed(
            format!(
                "{command} command takes either: \n\t1. enabled: To enable {what}\n\t2. disabled: To disable {what}\n\t3. value: To set {what} to a specific value (enabled or disabled) \nIt seems you provided invalid input with: [{input}]"
            ),
            v.pos(),
        )
    };
    let value_of = |n: &Node| match n.as_scalar().map(|s| s.typed()) {
        Some(Typed::Str(t)) if t == "enabled" => Ok(OnOff::Enable),
        Some(Typed::Str(t)) if t == "disabled" => Ok(OnOff::Disable),
        _ => Err(invalid(n.as_scalar().and_then(canonical_text).unwrap_or_default())),
    };
    match v {
        Node::Scalar(_) => Ok(SetToggle { value: value_of(v)?, label: None, optional: false }),
        _ => {
            let o = Obj::open(v, command, Mode::Tree)?;
            let Some(node) = o.get("value") else { return Err(invalid(o.keys().collect::<Vec<_>>().join(", "))) };
            Ok(SetToggle {
                value: value_of(node)?,
                label: o.string("label")?,
                optional: o.bool("optional")?.unwrap_or(false),
            })
        }
    }
}

// -------------------------------------------------------- scroll and travel

fn scroll_until_visible(v: &Node) -> DResult<ScrollUntilVisible> {
    if v.as_scalar().is_some() {
        return Err(shape(v.pos()));
    }
    let o = Obj::bind(
        v,
        &[
            "direction",
            "element",
            "timeout",
            "speed",
            "visibilityPercentage",
            "centerElement",
            "waitToSettleTimeoutMs",
            "label",
            "optional",
        ],
        "scrollUntilVisible",
        Mode::Stream,
    )?;
    let direction = match o.string("direction")?.map(|d| d.to_uppercase()) {
        None => Direction::Down,
        Some(d) => match d.as_str() {
            "UP" => Direction::Up,
            "DOWN" => Direction::Down,
            "LEFT" => Direction::Left,
            "RIGHT" => Direction::Right,
            _ => return Err(Problem::format("direction", o.pos)),
        },
    };
    let element = o.get("element").ok_or_else(|| Problem::missing("element", o.pos))?;
    let selector = union(element, "element")?.into_selector()?;
    let speed = o.string("speed")?.unwrap_or_else(|| "40".into());
    let visibility = o.int("visibilityPercentage")?.unwrap_or(100).clamp(0, 100);
    let c = common(&o, false)?;
    Ok(ScrollUntilVisible {
        selector,
        direction,
        scroll_duration: speed.clone(),
        visibility_percentage: visibility,
        timeout: o.string("timeout")?.unwrap_or_else(|| "20000".into()),
        wait_to_settle_timeout_ms: o.int("waitToSettleTimeoutMs")?,
        center_element: o.bool("centerElement")?.unwrap_or(false),
        original_speed_value: Some(speed),
        label: c.label,
        optional: c.optional,
        visibility_percentage_normalized: f64::from(visibility / 100),
    })
}

fn travel(v: &Node) -> DResult<Travel> {
    if v.as_scalar().is_some() {
        return Err(shape(v.pos()));
    }
    let o = Obj::bind(v, &["points", "speed", "label", "optional"], "travel", Mode::Stream)?;
    let points = o.str_list("points")?.ok_or_else(|| Problem::missing("points", o.pos))?;
    let c = common(&o, false)?;
    let parse = |p: &str, part: &str, what: &str| -> DResult<String> {
        part.trim()
            .parse::<f64>()
            .map(java_double)
            .map_err(|_| Problem::failed(format!("Invalid travel point {what}: {p}"), o.pos))
    };
    let points = points
        .iter()
        .map(|p| {
            let parts: Vec<&str> = p.split(',').collect();
            if parts.len() != 2 {
                return Err(Problem::failed(format!("Invalid travel point: {p}"), o.pos));
            }
            Ok(GeoPoint { latitude: parse(p, parts[0], "latitude")?, longitude: parse(p, parts[1], "longitude")? })
        })
        .collect::<DResult<Vec<_>>>()?;
    Ok(Travel { points, speed_mps: o.double("speed")?, label: c.label, optional: c.optional })
}

// ------------------------------------------- files: subflows, scripts, media

/// `FileAccessScope.everything.resolve(flowPath.parent, requested)`.
fn resolve(ctx: &Ctx, requested: &str) -> PathBuf {
    let anchor = ctx.flow_path.parent().unwrap_or(Path::new(""));
    normalize(&anchor.join(requested))
}

/// Lexical normalization (`Path.normalize()`), absolute.
pub fn normalize(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

/// `resolvePath`: a sibling flow file that must exist and differ from this one.
fn flow_file(ctx: &Ctx, requested: &str, pos: Pos) -> Result<PathBuf, FlowFailure> {
    let resolved = resolve(ctx, requested);
    let invalid = |m: String| FlowFailure::here(Problem::new("Invalid File Path", m, pos));
    if resolved == ctx.flow_path {
        return Err(invalid(format!(
            "Referenced Flow file can't be the same as the main Flow file: {}",
            uri(&resolved)
        )));
    }
    if !resolved.exists() {
        return Err(invalid(format!("Flow file does not exist: {}", uri(&resolved))));
    }
    if resolved.is_dir() {
        return Err(invalid(format!("Flow file can't be a directory: {}", uri(&resolved))));
    }
    Ok(resolved)
}

/// Inline `commands:` of runFlow / retry / repeat. With `env`, Maestro
/// prepends the env definition to *each* expanded command.
fn command_list(
    list: &Node,
    field: &str,
    ctx: &Ctx,
    env: Option<&ValueMap>,
) -> Result<Vec<MaestroCommand>, FlowFailure> {
    let Node::Seq(items, _) = list else { return Err(FlowFailure::here(Problem::format(field, list.pos()))) };
    let mut out = Vec::new();
    for item in items {
        let cmds = decode(item, ctx)?;
        match env {
            Some(env) => out.extend(with_env(cmds, env, item.pos()).map_err(FlowFailure::here)?),
            None => out.extend(cmds),
        }
    }
    Ok(out)
}

/// `List<MaestroCommand>.withEnv`: prepend `DefineVariables` unless empty.
pub fn with_env(cmds: Vec<MaestroCommand>, env: &ValueMap, pos: Pos) -> DResult<Vec<MaestroCommand>> {
    if env.is_empty() {
        return Ok(cmds);
    }
    let missing: Vec<&str> = env.iter().filter(|(_, v)| v.is_null()).map(|(k, _)| k.as_str()).collect();
    if !missing.is_empty() {
        return Err(Problem::new(
            format!("Environment variable has no value: {}", missing.join(", ")),
            format!("Environment variable(s) {} have no value.", missing.join(", ")),
            pos,
        ));
    }
    let mut out =
        vec![MaestroCommand::DefineVariables(DefineVariables { env: env.clone(), label: None, optional: false })];
    out.extend(cmds);
    Ok(out)
}

fn run_flow(v: &Node, ctx: &Ctx) -> Result<MaestroCommand, FlowFailure> {
    let here = FlowFailure::here;
    let (file, when, env, commands, c) = match v {
        Node::Scalar(_) => (
            Some(delegated_string(v).map_err(here)?),
            None,
            ValueMap::new(),
            None,
            Common { label: None, optional: false },
        ),
        _ => {
            let o = Obj::bind(v, &["file", "when", "env", "commands", "label", "optional"], "runFlow", Mode::Stream)
                .map_err(here)?;
            let when = o.get("when").map(|n| condition(n, "when")).transpose().map_err(here)?;
            let env = o.str_map("env").map_err(here)?.unwrap_or_default();
            (o.string("file").map_err(here)?, when, env, o.get("commands"), common(&o, false).map_err(here)?)
        }
    };
    let (commands, source, config) = sub_commands("runFlow", file, commands, &env, v.pos(), ctx)?;
    Ok(MaestroCommand::RunFlow(RunFlow {
        commands,
        condition: when,
        source_description: source,
        config,
        label: c.label,
        optional: c.optional,
    }))
}

fn retry(v: &Node, ctx: &Ctx) -> Result<MaestroCommand, FlowFailure> {
    let here = FlowFailure::here;
    if v.as_scalar().is_some() {
        return Err(here(shape(v.pos())));
    }
    let o = Obj::bind(v, &["maxRetries", "file", "commands", "env", "label", "optional"], "retry", Mode::Stream)
        .map_err(here)?;
    let max_retries = o.string("maxRetries").map_err(here)?.unwrap_or_else(|| "1".into());
    let env = o.str_map("env").map_err(here)?.unwrap_or_default();
    let c = common(&o, false).map_err(here)?;
    let (commands, source, config) =
        sub_commands("retry", o.string("file").map_err(here)?, o.get("commands"), &env, v.pos(), ctx)?;
    Ok(MaestroCommand::Retry(Retry {
        max_retries: Some(max_retries),
        commands,
        source_description: source,
        config,
        label: c.label,
        optional: c.optional,
    }))
}

/// Commands, `sourceDescription` and sub-flow config of a runFlow / retry.
type SubCommands = (Vec<MaestroCommand>, Option<String>, Option<MaestroConfig>);

/// Shared by runFlow and retry: either a sub-flow file or inline commands.
fn sub_commands(
    what: &str,
    file: Option<String>,
    inline: Option<&Node>,
    env: &ValueMap,
    pos: Pos,
    ctx: &Ctx,
) -> Result<SubCommands, FlowFailure> {
    match (file, inline) {
        (None, None) => Err(FlowFailure::here(Problem::failed(
            format!("Invalid {what} command: No file or commands provided"),
            pos,
        ))),
        (Some(_), Some(_)) => Err(FlowFailure::here(Problem::failed(
            format!("Invalid {what} command: Can't provide both file and commands at the same time"),
            pos,
        ))),
        (None, Some(list)) => Ok((command_list(list, "commands", ctx, Some(env))?, None, None)),
        (Some(file), None) => {
            let path = flow_file(ctx, &file, pos)?;
            let parsed = ctx.loader.include(&path, pos)?;
            let commands = with_env(parsed.commands, env, pos).map_err(FlowFailure::here)?;
            Ok((commands, Some(file), Some(parsed.config)))
        }
    }
}

fn run_script(v: &Node, ctx: &Ctx) -> Result<MaestroCommand, FlowFailure> {
    let here = FlowFailure::here;
    let (file, env, when, c) = match v {
        Node::Scalar(_) => {
            (delegated_string(v).map_err(here)?, ValueMap::new(), None, Common { label: None, optional: false })
        }
        _ => {
            let o =
                Obj::bind(v, &["file", "env", "when", "label", "optional"], "runScript", Mode::Stream).map_err(here)?;
            let when = o.get("when").map(|n| condition(n, "when")).transpose().map_err(here)?;
            (
                o.req_string("file").map_err(here)?,
                o.str_map("env").map_err(here)?.unwrap_or_default(),
                when,
                common(&o, false).map_err(here)?,
            )
        }
    };
    let path = flow_file(ctx, &file, v.pos())?;
    let script = std::fs::read_to_string(&path)
        .map_err(|e| here(Problem::failed(format!("Cannot read {}: {e}", path.display()), v.pos())))?;
    Ok(MaestroCommand::RunScript(RunScript {
        script,
        env,
        source_description: file,
        script_dir: path.parent().map(|p| p.display().to_string()),
        condition: when,
        label: c.label,
        optional: c.optional,
    }))
}

fn add_media(v: &Node, ctx: &Ctx) -> Result<AddMedia, FlowFailure> {
    let here = FlowFailure::here;
    let (files, c): (Option<Vec<Option<String>>>, Common) = match v {
        Node::Seq(items, _) => (
            Some(items.iter().map(|i| delegated_string(i).map(Some)).collect::<DResult<_>>().map_err(here)?),
            Common { label: None, optional: false },
        ),
        Node::Scalar(_) => return Err(here(shape(v.pos()))),
        Node::Map(..) => {
            let o = Obj::bind(v, &["files", "label", "optional"], "addMedia", Mode::Stream).map_err(here)?;
            let files = match o.get("files") {
                None => None,
                Some(Node::Seq(items, _)) => Some(
                    items
                        .iter()
                        .map(|i| {
                            if i.is_null() {
                                Ok(None)
                            } else {
                                super::decode::string(i, "files", Mode::Stream).map(Some)
                            }
                        })
                        .collect::<DResult<_>>()
                        .map_err(here)?,
                ),
                Some(other) => return Err(here(Problem::format("files", other.pos()))),
            };
            (files, common(&o, false).map_err(here)?)
        }
    };
    let Some(files) = files.filter(|f| f.iter().all(Option::is_some)) else {
        return Err(here(Problem::failed("Invalid addMedia command: media files cannot be empty", v.pos())));
    };
    let mut media_paths = Vec::new();
    for f in files.into_iter().flatten() {
        let resolved = resolve(ctx, &f);
        if !resolved.exists() {
            return Err(here(Problem::new(
                "Media File Not Found",
                format!("Media file at {f} in flow file: {} not found", ctx.flow_path.display()),
                v.pos(),
            )));
        }
        media_paths.push(resolved.display().to_string());
    }
    Ok(AddMedia { media_paths, label: c.label, optional: c.optional })
}

/// Hook command lists (`onFlowStart` / `onFlowComplete`).
pub fn hook(n: &Node, field: &str, ctx: &Ctx) -> Result<Hook, FlowFailure> {
    Ok(Hook { commands: command_list(n, field, ctx, None)? })
}

const MAX_HOOK_TIMEOUT_MS: i64 = 86_400_000;

/// Checks whether `name` is a valid JS identifier on `output.<name>`.
pub fn is_valid_js_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn validate_ms_literal(raw: &str, field: &str, min: i64, max: i64, pos: Pos) -> DResult<()> {
    if raw.contains("${") {
        return Ok(());
    }
    let clean = raw.trim().replace('_', "");
    let parsed = clean.parse::<i64>().ok().filter(|v| (min..=max).contains(v));
    if parsed.is_none() {
        return Err(Problem::new(
            "Invalid Command: callHook",
            format!("`{field}` must be an integer in {min}..={max} ms, got `{raw}`"),
            pos,
        ));
    }
    Ok(())
}

fn call_hook(v: &Node) -> DResult<CallHook> {
    match v {
        Node::Scalar(_) => {
            let name = delegated_string(v)?;
            if !name.contains("${") && name.trim().is_empty() {
                return Err(Problem::new("Invalid Command: callHook", "Hook `name` must not be empty.", v.pos()));
            }
            Ok(CallHook {
                name,
                args: None,
                timeout: None,
                settle: None,
                wait_to_settle_timeout_ms: None,
                output_variable: None,
                label: None,
                optional: false,
            })
        }
        Node::Seq(..) => Err(shape(v.pos())),
        Node::Map(..) => {
            let o = Obj::bind(
                v,
                &["name", "args", "timeout", "settle", "waitToSettleTimeoutMs", "outputVariable", "label", "optional"],
                "callHook",
                Mode::Stream,
            )?;
            let name = o.req_string("name")?;
            if !name.contains("${") && name.trim().is_empty() {
                return Err(Problem::new("Invalid Command: callHook", "Hook `name` must not be empty.", o.pos));
            }
            let args = o.any_map("args")?;
            let timeout = o.string("timeout")?;
            if let Some(t) = &timeout {
                let pos = o.get("timeout").map_or(o.pos, Node::pos);
                validate_ms_literal(t, "timeout", 1, MAX_HOOK_TIMEOUT_MS, pos)?;
            }
            let settle = o.bool("settle")?;
            let wait_to_settle_timeout_ms = o.string("waitToSettleTimeoutMs")?;
            if let Some(t) = &wait_to_settle_timeout_ms {
                let pos = o.get("waitToSettleTimeoutMs").map_or(o.pos, Node::pos);
                validate_ms_literal(t, "waitToSettleTimeoutMs", 0, MAX_HOOK_TIMEOUT_MS, pos)?;
            }
            let output_variable = o.string("outputVariable")?;
            if let Some(var) = &output_variable
                && !var.contains("${")
                && !is_valid_js_identifier(var)
            {
                let pos = o.get("outputVariable").map_or(o.pos, Node::pos);
                return Err(Problem::new(
                    "Invalid Command: callHook",
                    format!("`outputVariable` must be a valid JS identifier, got `{var}`"),
                    pos,
                ));
            }
            let c = common(&o, false)?;
            Ok(CallHook {
                name,
                args,
                timeout,
                settle,
                wait_to_settle_timeout_ms,
                output_variable,
                label: c.label,
                optional: c.optional,
            })
        }
    }
}

/// Bind-time check used for config values that must be strings.
pub fn config_string(v: &Node, field: &str) -> DResult<String> {
    super::decode::string(v, field, Mode::Stream)
}

/// Untyped config extension value.
pub fn ext_value(v: &Node) -> Value {
    super::decode::untyped(v)
}
