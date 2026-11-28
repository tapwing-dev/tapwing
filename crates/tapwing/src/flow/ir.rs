//! The flow IR: the same command model Maestro 2.12.1 builds
//! (`maestro-orchestra-models`: `MaestroCommand`, `Commands.kt`,
//! `ElementSelector`, `Condition`, `MaestroConfig`).
//!
//! It serializes to exactly the JSON that Jackson writes for Maestro's
//! objects (null fields omitted), which is how `compat/` checks the parser
//! against Maestro command by command. Values that Maestro evaluates later as
//! JavaScript (`${...}`) stay unevaluated strings here, as in Maestro.

use serde::Serialize;
use serde_json::{Map, Value};

/// String-keyed map with insertion order (env, permissions, properties,
/// launch arguments, config extensions). Values keep their YAML type; a
/// YAML null stays `null`, as in Maestro.
pub type ValueMap = Map<String, Value>;

/// One step of a flow. Serialized as `{"<kind>": {...}}` like `MaestroCommand`.
#[derive(Clone, Debug, Serialize)]
pub enum MaestroCommand {
    #[serde(rename = "tapOnElement")]
    TapOnElement(TapOnElement),
    #[serde(rename = "tapOnPointV2Command")]
    TapOnPoint(TapOnPoint),
    #[serde(rename = "scrollCommand")]
    Scroll(Basic),
    #[serde(rename = "swipeCommand")]
    Swipe(Swipe),
    #[serde(rename = "backPressCommand")]
    BackPress(Basic),
    #[serde(rename = "assertConditionCommand")]
    AssertCondition(AssertCondition),
    #[serde(rename = "assertScreenshotCommand")]
    AssertScreenshot(AssertScreenshot),
    #[serde(rename = "assertNoDefectsWithAICommand")]
    AssertNoDefectsWithAi(AssertNoDefectsWithAi),
    #[serde(rename = "assertWithAICommand")]
    AssertWithAi(AssertWithAi),
    #[serde(rename = "extractTextWithAICommand")]
    ExtractTextWithAi(ExtractTextWithAi),
    #[serde(rename = "inputTextCommand")]
    InputText(InputText),
    #[serde(rename = "inputRandomTextCommand")]
    InputRandom(InputRandom),
    #[serde(rename = "launchAppCommand")]
    LaunchApp(LaunchApp),
    #[serde(rename = "setPermissionsCommand")]
    SetPermissions(SetPermissions),
    #[serde(rename = "applyConfigurationCommand")]
    ApplyConfiguration(ApplyConfiguration),
    #[serde(rename = "openLinkCommand")]
    OpenLink(OpenLink),
    #[serde(rename = "pressKeyCommand")]
    PressKey(PressKey),
    #[serde(rename = "eraseTextCommand")]
    EraseText(EraseText),
    #[serde(rename = "hideKeyboardCommand")]
    HideKeyboard(Basic),
    #[serde(rename = "takeScreenshotCommand")]
    TakeScreenshot(TakeScreenshot),
    #[serde(rename = "stopAppCommand")]
    StopApp(AppCommand),
    #[serde(rename = "killAppCommand")]
    KillApp(AppCommand),
    #[serde(rename = "clearStateCommand")]
    ClearState(AppCommand),
    #[serde(rename = "clearKeychainCommand")]
    ClearKeychain(Basic),
    #[serde(rename = "runFlowCommand")]
    RunFlow(RunFlow),
    #[serde(rename = "setLocationCommand")]
    SetLocation(SetLocation),
    #[serde(rename = "setOrientationCommand")]
    SetOrientation(SetOrientation),
    #[serde(rename = "repeatCommand")]
    Repeat(Repeat),
    #[serde(rename = "copyTextCommand")]
    CopyTextFrom(CopyTextFrom),
    #[serde(rename = "setClipboardCommand")]
    SetClipboard(SetClipboard),
    #[serde(rename = "pasteTextCommand")]
    PasteText(Basic),
    #[serde(rename = "defineVariablesCommand")]
    DefineVariables(DefineVariables),
    #[serde(rename = "runScriptCommand")]
    RunScript(RunScript),
    #[serde(rename = "waitForAnimationToEndCommand")]
    WaitForAnimationToEnd(WaitForAnimationToEnd),
    #[serde(rename = "evalScriptCommand")]
    EvalScript(EvalScript),
    #[serde(rename = "scrollUntilVisible")]
    ScrollUntilVisible(ScrollUntilVisible),
    #[serde(rename = "travelCommand")]
    Travel(Travel),
    #[serde(rename = "startRecordingCommand")]
    StartRecording(StartRecording),
    #[serde(rename = "stopRecordingCommand")]
    StopRecording(Basic),
    #[serde(rename = "addMediaCommand")]
    AddMedia(AddMedia),
    #[serde(rename = "setAirplaneModeCommand")]
    SetAirplaneMode(SetToggle),
    #[serde(rename = "toggleAirplaneModeCommand")]
    ToggleAirplaneMode(Basic),
    #[serde(rename = "setDarkModeCommand")]
    SetDarkMode(SetToggle),
    #[serde(rename = "toggleDarkModeCommand")]
    ToggleDarkMode(Basic),
    #[serde(rename = "assertDarkModeCommand")]
    AssertDarkMode(Basic),
    #[serde(rename = "assertLightModeCommand")]
    AssertLightMode(Basic),
    #[serde(rename = "retryCommand")]
    Retry(Retry),
    #[serde(rename = "callHookCommand")]
    CallHook(CallHook),
}

impl MaestroCommand {
    /// Command-level `optional` (a failure only warns).
    pub fn optional(&self) -> bool {
        self.common().1
    }

    /// Command-level `label` (replaces the description in reports).
    pub fn label(&self) -> Option<&str> {
        self.common().0
    }

    fn common(&self) -> (Option<&str>, bool) {
        use MaestroCommand::*;
        let (label, optional) = match self {
            TapOnElement(c) => (&c.label, c.optional),
            TapOnPoint(c) => (&c.label, c.optional),
            Swipe(c) => (&c.label, c.optional),
            AssertCondition(c) => (&c.label, c.optional),
            AssertScreenshot(c) => (&c.label, c.optional),
            AssertNoDefectsWithAi(c) => (&c.label, c.optional),
            AssertWithAi(c) => (&c.label, c.optional),
            ExtractTextWithAi(c) => (&c.label, c.optional),
            InputText(c) => (&c.label, c.optional),
            InputRandom(c) => (&c.label, c.optional),
            LaunchApp(c) => (&c.label, c.optional),
            SetPermissions(c) => (&c.label, c.optional),
            ApplyConfiguration(c) => (&c.label, c.optional),
            OpenLink(c) => (&c.label, c.optional),
            PressKey(c) => (&c.label, c.optional),
            EraseText(c) => (&c.label, c.optional),
            TakeScreenshot(c) => (&c.label, c.optional),
            StopApp(c) | KillApp(c) | ClearState(c) => (&c.label, c.optional),
            RunFlow(c) => (&c.label, c.optional),
            SetLocation(c) => (&c.label, c.optional),
            SetOrientation(c) => (&c.label, c.optional),
            Repeat(c) => (&c.label, c.optional),
            CopyTextFrom(c) => (&c.label, c.optional),
            SetClipboard(c) => (&c.label, c.optional),
            DefineVariables(c) => (&c.label, c.optional),
            RunScript(c) => (&c.label, c.optional),
            WaitForAnimationToEnd(c) => (&c.label, c.optional),
            EvalScript(c) => (&c.label, c.optional),
            ScrollUntilVisible(c) => (&c.label, c.optional),
            Travel(c) => (&c.label, c.optional),
            StartRecording(c) => (&c.label, c.optional),
            AddMedia(c) => (&c.label, c.optional),
            SetAirplaneMode(c) | SetDarkMode(c) => (&c.label, c.optional),
            Retry(c) => (&c.label, c.optional),
            CallHook(c) => (&c.label, c.optional),
            Scroll(c)
            | BackPress(c)
            | HideKeyboard(c)
            | ClearKeychain(c)
            | PasteText(c)
            | StopRecording(c)
            | ToggleAirplaneMode(c)
            | ToggleDarkMode(c)
            | AssertDarkMode(c)
            | AssertLightMode(c) => (&c.label, c.optional),
        };
        (label.as_deref(), optional)
    }
}

/// Commands that only carry `label` and `optional`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Basic {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementSelector {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_regex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_regex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<SizeSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub below: Option<Box<ElementSelector>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub above: Option<Box<ElementSelector>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_of: Option<Box<ElementSelector>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_of: Option<Box<ElementSelector>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains_child: Option<Box<ElementSelector>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains_descendants: Option<Vec<ElementSelector>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<ElementTrait>>,
    /// A string because it may be a `${...}` expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Deprecated in Maestro (commands have their own `optional`), kept for parity.
    pub optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_of: Option<Box<ElementSelector>>,
    /// Web only; ignored on Android.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SizeSelector {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ElementTrait {
    Text,
    Square,
    LongText,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<ElementSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_visible: Option<ElementSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Platform {
    Android,
    Ios,
    Web,
}

#[derive(Clone, Debug, Serialize)]
pub struct TapRepeat {
    pub repeat: i32,
    pub delay: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TapOnElement {
    pub selector: ElementSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_if_no_change: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_until_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_press: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat: Option<TapRepeat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_to_settle_timeout_ms: Option<i32>,
    /// Point inside the element ("50%, 90%" or "25, 75").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_point: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TapOnPoint {
    pub point: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_if_no_change: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_press: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat: Option<TapRepeat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_to_settle_timeout_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Direction {
    Up,
    Down,
    Right,
    Left,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Swipe {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_point: Option<Point>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_point: Option<Point>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_selector: Option<ElementSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_relative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_relative: Option<String>,
    pub duration: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_to_settle_timeout_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_point: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

/// `assertVisible`, `assertNotVisible`, `assertTrue` and `extendedWaitUntil`.
#[derive(Clone, Debug, Serialize)]
pub struct AssertCondition {
    pub condition: Condition,
    /// Milliseconds, possibly a `${...}` expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertScreenshot {
    pub path: String,
    pub threshold_percentage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crop_on: Option<ElementSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssertNoDefectsWithAi {
    pub optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssertWithAi {
    pub assertion: String,
    pub optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractTextWithAi {
    pub query: String,
    pub output_variable: String,
    pub optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputText {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InputRandomType {
    Number,
    Text,
    TextEmailAddress,
    TextPersonName,
    TextCityName,
    TextCountryName,
    TextColor,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRandom {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<InputRandomType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchApp {
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_state: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_keychain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_app: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ValueMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_arguments: Option<ValueMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPermissions {
    pub app_id: String,
    pub permissions: ValueMap,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApplyConfiguration {
    pub config: MaestroConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

/// The flow header (before `---`).
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaestroConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Keys Maestro does not know, kept verbatim (`@JsonAnySetter`).
    pub ext: ValueMap,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_flow_start: Option<Hook>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_flow_complete: Option<Hook>,
    pub properties: ValueMap,
}

/// `onFlowStart` / `onFlowComplete`.
#[derive(Clone, Debug, Serialize)]
pub struct Hook {
    pub commands: Vec<MaestroCommand>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenLink {
    pub link: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_verify: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PressKey {
    pub code: KeyCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EraseText {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub characters_to_erase: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeScreenshot {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crop_on: Option<ElementSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

/// `stopApp`, `killApp`, `clearState`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppCommand {
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFlow {
    pub commands: Vec<MaestroCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    /// The `file` as written (inline `commands` have none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<MaestroConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetLocation {
    pub latitude: String,
    pub longitude: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetOrientation {
    /// `DeviceOrientation` name, or the raw `${...}` expression.
    pub orientation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Repeat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub times: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    pub commands: Vec<MaestroCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CopyTextFrom {
    pub selector: ElementSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetClipboard {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

/// Env defined by the flow header, `runFlow`/`retry` `env`.
#[derive(Clone, Debug, Serialize)]
pub struct DefineVariables {
    pub env: ValueMap,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunScript {
    /// The script source (read at parse time, like Maestro).
    pub script: String,
    pub env: ValueMap,
    pub source_description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct WaitForAnimationToEnd {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalScript {
    pub script_string: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollUntilVisible {
    pub selector: ElementSelector,
    pub direction: Direction,
    /// Maestro calls the YAML `speed` this (0–100, may be `${...}`).
    pub scroll_duration: String,
    pub visibility_percentage: i32,
    pub timeout: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_to_settle_timeout_ms: Option<i32>,
    pub center_element: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_speed_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
    /// Maestro serializes this derived value too: `(visibilityPercentage / 100)`
    /// in integer arithmetic, so it is 1.0 for 100 and 0.0 below.
    pub visibility_percentage_normalized: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct GeoPoint {
    pub latitude: String,
    pub longitude: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Travel {
    pub points: Vec<GeoPoint>,
    #[serde(rename = "speedMPS", skip_serializing_if = "Option::is_none")]
    pub speed_mps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct StartRecording {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMedia {
    pub media_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum OnOff {
    Enable,
    Disable,
}

/// `setAirplaneMode` / `setDarkMode`.
#[derive(Clone, Debug, Serialize)]
pub struct SetToggle {
    pub value: OnOff,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<String>,
    pub commands: Vec<MaestroCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<MaestroConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

/// Tapwing-only gray-box command invoking an in-app registered Dart test hook.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHook {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<ValueMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settle: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_to_settle_timeout_ms: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_variable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub optional: bool,
}

/// Keys `pressKey` accepts (`maestro.KeyCode`): IR name and YAML spelling.
macro_rules! key_codes {
    ($($variant:ident, $wire:literal => $yaml:literal,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
        pub enum KeyCode { $(#[serde(rename = $wire)] $variant,)* }

        impl KeyCode {
            /// Case-insensitive match on the YAML spelling (`KeyCode.getByName`).
            pub fn by_name(name: &str) -> Option<KeyCode> {
                let lower = name.to_lowercase();
                [$((KeyCode::$variant, $yaml),)*]
                    .into_iter()
                    .find(|(_, spelled)| spelled.to_lowercase() == lower)
                    .map(|(code, _)| code)
            }
        }
    };
}

key_codes! {
    Enter, "ENTER" => "Enter",
    Backspace, "BACKSPACE" => "Backspace",
    Back, "BACK" => "Back",
    Home, "HOME" => "Home",
    Lock, "LOCK" => "Lock",
    VolumeUp, "VOLUME_UP" => "Volume Up",
    VolumeDown, "VOLUME_DOWN" => "Volume Down",
    RemoteUp, "REMOTE_UP" => "Remote Dpad Up",
    RemoteDown, "REMOTE_DOWN" => "Remote Dpad Down",
    RemoteLeft, "REMOTE_LEFT" => "Remote Dpad Left",
    RemoteRight, "REMOTE_RIGHT" => "Remote Dpad Right",
    RemoteCenter, "REMOTE_CENTER" => "Remote Dpad Center",
    RemotePlayPause, "REMOTE_PLAY_PAUSE" => "Remote Media Play Pause",
    RemoteStop, "REMOTE_STOP" => "Remote Media Stop",
    RemoteNext, "REMOTE_NEXT" => "Remote Media Next",
    RemotePrevious, "REMOTE_PREVIOUS" => "Remote Media Previous",
    RemoteRewind, "REMOTE_REWIND" => "Remote Media Rewind",
    RemoteFastForward, "REMOTE_FAST_FORWARD" => "Remote Media Fast Forward",
    Escape, "ESCAPE" => "Escape",
    Power, "POWER" => "Power",
    Tab, "TAB" => "Tab",
    RemoteSystemNavigationUp, "REMOTE_SYSTEM_NAVIGATION_UP" => "Remote System Navigation Up",
    RemoteSystemNavigationDown, "REMOTE_SYSTEM_NAVIGATION_DOWN" => "Remote System Navigation Down",
    RemoteButtonA, "REMOTE_BUTTON_A" => "Remote Button A",
    RemoteButtonB, "REMOTE_BUTTON_B" => "Remote Button B",
    RemoteMenu, "REMOTE_MENU" => "Remote Menu",
    TvInput, "TV_INPUT" => "TV Input",
    TvInputHdmi1, "TV_INPUT_HDMI_1" => "TV Input HDMI 1",
    TvInputHdmi2, "TV_INPUT_HDMI_2" => "TV Input HDMI 2",
    TvInputHdmi3, "TV_INPUT_HDMI_3" => "TV Input HDMI 3",
}

/// `DeviceOrientation` names; matching ignores case and underscores.
pub const ORIENTATIONS: [&str; 4] = ["PORTRAIT", "LANDSCAPE_LEFT", "LANDSCAPE_RIGHT", "UPSIDE_DOWN"];

pub fn orientation_by_name(name: &str) -> Option<&'static str> {
    let comparable = |s: &str| s.to_lowercase().replace('_', "");
    ORIENTATIONS.into_iter().find(|o| comparable(o) == comparable(name))
}
