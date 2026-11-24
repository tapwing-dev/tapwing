//! The workspace `config.yaml` (`maestro.orchestra.WorkspaceConfig`, Maestro
//! 2.12.1) and the "is this YAML a workspace config?" probe that keeps such
//! files out of the flow list (`Filters.isWorkspaceConfigFile`).
//!
//! Tapwing reads the keys that decide what runs (`flows`, `includeTags`,
//! `excludeTags`, `executionOrder`), `testOutputDir`, and `platform`
//! (`platform.android.disableAnimations`, `platform.ios.*`). The other known
//! keys (`baselineBranch`, `notifications`, `disableRetries`) are Maestro
//! Cloud / reporting settings: accepted and ignored. Unknown keys get a
//! warning, as Maestro logs one (`validateWorkspaceConfigKeys`).

use std::path::Path;

use crate::flow::yaml::{self, Node, Pos, Typed};
use crate::flow::{FlowError, Problem};

/// `WorkspaceConfig.KNOWN_KEYS`: the declared properties of Maestro 2.12.1's
/// `WorkspaceConfig`.
pub const KNOWN_KEYS: [&str; 9] = [
    "flows",
    "includeTags",
    "excludeTags",
    "executionOrder",
    "baselineBranch",
    "notifications",
    "disableRetries",
    "platform",
    "testOutputDir",
];

/// What Tapwing uses from a workspace config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkspaceConfig {
    /// Inclusion globs relative to each input directory (`!` negates);
    /// `None` means Maestro's default `["*"]` (top-level files only).
    pub flows: Option<Vec<String>>,
    pub include_tags: Option<Vec<String>>,
    pub exclude_tags: Option<Vec<String>>,
    pub execution_order: Option<ExecutionOrder>,
    /// Run artifacts directory, relative to the working directory (Maestro's
    /// `TestCommand.resolveTestOutputDir` uses it verbatim).
    pub test_output_dir: Option<String>,
    /// `platform:` configuration (`platform.android.disableAnimations`, `platform.ios.*`).
    pub platform: PlatformConfig,
    /// Tapwing-only settings (`tapwing:`). Maestro warns about the unknown
    /// key and ignores it, so the workspace still runs in Maestro.
    pub tapwing: TapwingConfig,
}

/// `tapwing:` section of a workspace config (F7 app health defaults; CLI
/// flags win).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TapwingConfig {
    /// `appErrors: report | fail | off`.
    pub app_errors: Option<crate::app_health::AppErrorsPolicy>,
    /// `appErrorsIgnore`: regexes (single string or list).
    pub app_errors_ignore: Vec<String>,
    /// `appErrorsScreenshot`.
    pub app_errors_screenshot: Option<bool>,
    /// `nativeDriver`: deprecated and ignored (Tapwing's own driver is always used); kept so
    /// existing configs still load, see [`crate::native_lane::deprecated_native_driver_warning`].
    pub native_driver: Option<String>,
}

/// Platform-specific workspace settings (`WorkspaceConfig.PlatformConfiguration`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlatformConfig {
    pub android: AndroidPlatformConfig,
    pub ios: IosPlatformConfig,
}

/// `platform.android` settings (`WorkspaceConfig.PlatformConfiguration.AndroidConfiguration`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AndroidPlatformConfig {
    pub disable_animations: bool,
}

/// `platform.ios` settings (`WorkspaceConfig.PlatformConfiguration.IOSConfiguration`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IosPlatformConfig {
    pub disable_animations: bool,
    pub snapshot_key_honor_modal_views: Option<bool>,
}

/// `executionOrder`: flows run one after another before all others.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionOrder {
    /// `null` in YAML is kept as `None`, which Maestro treats like `false`
    /// (`continueOnFailure != true`); a missing key defaults to `true`.
    pub continue_on_failure: Option<bool>,
    pub flows_order: Vec<String>,
}

/// A parsed config plus the keys Maestro does not know.
#[derive(Debug)]
pub struct Parsed {
    pub config: WorkspaceConfig,
    pub unknown_keys: Vec<String>,
}

/// `YamlCommandReader.readWorkspaceConfig`: a blank file is the default
/// config; unknown keys are reported on stderr.
pub fn read(path: &Path) -> Result<WorkspaceConfig, FlowError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        FlowError::at(path, Problem::new("Failed to read workspace config", e.to_string(), Pos { line: 1, col: 1 }))
    })?;
    let parsed = parse(&text).map_err(|p| FlowError::at(path, p))?;
    if !parsed.unknown_keys.is_empty() {
        eprintln!(
            "warning: config file '{}' contains unknown keys: [{}]. Valid keys are: [{}]",
            path.display(),
            parsed.unknown_keys.join(", "),
            sorted_known_keys().join(", ")
        );
    }
    Ok(parsed.config)
}

fn sorted_known_keys() -> Vec<&'static str> {
    let mut keys = KNOWN_KEYS.to_vec();
    keys.sort_unstable();
    keys
}

/// Parses the text of a workspace config.
pub fn parse(text: &str) -> Result<Parsed, Problem> {
    let mut parsed = Parsed { config: WorkspaceConfig::default(), unknown_keys: Vec::new() };
    if text.trim().is_empty() {
        return Ok(parsed);
    }
    let docs = yaml::load(text).map_err(|e| Problem::failed(e.message, e.pos))?;
    let root = match docs.first() {
        None => return Ok(parsed),
        Some(n) if n.is_null() => return Ok(parsed),
        Some(n) => n,
    };
    let Some(entries) = root.map_entries() else {
        return Err(Problem::new(
            "Incorrect Format: workspace config",
            "A workspace config must be a map of settings, e.g. `flows: [\"*\"]`",
            root.pos(),
        ));
    };
    let c = &mut parsed.config;
    for (key, value, _) in entries {
        match key {
            "flows" => c.flows = string_list(value, key)?,
            "includeTags" => c.include_tags = string_list(value, key)?,
            "excludeTags" => c.exclude_tags = string_list(value, key)?,
            "executionOrder" => c.execution_order = execution_order(value)?,
            "testOutputDir" => c.test_output_dir = string(value, key)?,
            "platform" => c.platform = platform_config(value)?,
            // Not in KNOWN_KEYS: Maestro's `isWorkspaceConfigFile` must keep
            // its exact key set.
            "tapwing" => c.tapwing = tapwing_config(value)?,
            k if KNOWN_KEYS.contains(&k) => {}
            other => parsed.unknown_keys.push(other.to_string()),
        }
    }
    Ok(parsed)
}

/// `Filters.isWorkspaceConfigFile`: YAML without a `\n---` document
/// separator whose top level is a map of known workspace keys only. Such
/// files (`config.yaml` under another name, `regression_config.yaml`, …)
/// are not flows. Anything unreadable or not a map is not a config.
pub fn looks_like_workspace_config(text: &str) -> bool {
    if text.contains("\n---") {
        return false;
    }
    let Ok(docs) = yaml::load(text) else { return false };
    match docs.first().and_then(Node::map_entries) {
        Some(entries) => entries.iter().all(|(k, _, _)| KNOWN_KEYS.contains(k)),
        None => false,
    }
}

/// `WorkspaceConfig.StringList`: a single string or a list of strings.
/// Scalars keep their source text, as Jackson's `StringDeserializer` does.
fn string_list(node: &Node, field: &str) -> Result<Option<Vec<String>>, Problem> {
    match node {
        Node::Scalar(s) if s.typed() == Typed::Null => Ok(None),
        Node::Scalar(s) => Ok(Some(vec![s.text.clone()])),
        Node::Seq(items, _) => items
            .iter()
            .filter(|n| !n.is_null())
            .map(|n| match n {
                Node::Scalar(s) => Ok(s.text.clone()),
                other => Err(Problem::format(field, other.pos())),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Node::Map(_, pos) => Err(Problem::format(field, *pos)),
    }
}

fn string(node: &Node, field: &str) -> Result<Option<String>, Problem> {
    match node {
        Node::Scalar(s) if s.typed() == Typed::Null => Ok(None),
        Node::Scalar(s) => Ok(Some(s.text.clone())),
        other => Err(Problem::format(field, other.pos())),
    }
}

fn tapwing_config(node: &Node) -> Result<TapwingConfig, Problem> {
    let mut cfg = TapwingConfig::default();
    if node.is_null() {
        return Ok(cfg);
    }
    let Some(entries) = node.map_entries() else { return Err(Problem::format("tapwing", node.pos())) };
    for (key, value, _) in entries {
        match key {
            "appErrors" => {
                if let Some(raw) = string(value, "appErrors")? {
                    cfg.app_errors = Some(crate::app_health::AppErrorsPolicy::parse(&raw).ok_or_else(|| {
                        Problem::new(
                            "Invalid tapwing.appErrors",
                            format!("`{raw}` is not one of: report, fail, off"),
                            value.pos(),
                        )
                    })?);
                }
            }
            "appErrorsIgnore" => cfg.app_errors_ignore = string_list(value, "appErrorsIgnore")?.unwrap_or_default(),
            "appErrorsScreenshot" => cfg.app_errors_screenshot = boolean(value, "appErrorsScreenshot")?,
            "nativeDriver" => cfg.native_driver = string(value, "nativeDriver")?,
            other => {
                return Err(Problem::new(
                    "Unknown tapwing setting",
                    format!("`{other}` (valid: appErrors, appErrorsIgnore, appErrorsScreenshot, nativeDriver)"),
                    value.pos(),
                ));
            }
        }
    }
    Ok(cfg)
}

fn execution_order(node: &Node) -> Result<Option<ExecutionOrder>, Problem> {
    if node.is_null() {
        return Ok(None);
    }
    let Some(entries) = node.map_entries() else { return Err(Problem::format("executionOrder", node.pos())) };
    let mut order = ExecutionOrder { continue_on_failure: Some(true), flows_order: Vec::new() };
    for (key, value, _) in entries {
        match key {
            "continueOnFailure" => order.continue_on_failure = boolean(value, "continueOnFailure")?,
            "flowsOrder" => match value {
                Node::Seq(..) => order.flows_order = string_list(value, "flowsOrder")?.unwrap_or_default(),
                n if n.is_null() => order.flows_order = Vec::new(),
                other => return Err(Problem::format("flowsOrder", other.pos())),
            },
            // Lenient where Maestro might be strict: never fail a workspace
            // that Maestro could run.
            _ => {}
        }
    }
    Ok(Some(order))
}

fn platform_config(node: &Node) -> Result<PlatformConfig, Problem> {
    if node.is_null() {
        return Ok(PlatformConfig::default());
    }
    let Some(entries) = node.map_entries() else { return Err(Problem::format("platform", node.pos())) };
    let mut cfg = PlatformConfig::default();
    for (key, value, _) in entries {
        match key {
            "android" => cfg.android = android_platform_config(value)?,
            "ios" => cfg.ios = ios_platform_config(value)?,
            _ => {}
        }
    }
    Ok(cfg)
}

fn android_platform_config(node: &Node) -> Result<AndroidPlatformConfig, Problem> {
    if node.is_null() {
        return Ok(AndroidPlatformConfig::default());
    }
    let Some(entries) = node.map_entries() else { return Err(Problem::format("android", node.pos())) };
    let mut cfg = AndroidPlatformConfig::default();
    for (key, value, _) in entries {
        if key == "disableAnimations" {
            cfg.disable_animations = boolean(value, "disableAnimations")?.unwrap_or(false);
        }
    }
    Ok(cfg)
}

fn ios_platform_config(node: &Node) -> Result<IosPlatformConfig, Problem> {
    if node.is_null() {
        return Ok(IosPlatformConfig::default());
    }
    let Some(entries) = node.map_entries() else { return Err(Problem::format("ios", node.pos())) };
    let mut cfg = IosPlatformConfig::default();
    for (key, value, _) in entries {
        match key {
            "disableAnimations" => cfg.disable_animations = boolean(value, "disableAnimations")?.unwrap_or(false),
            "snapshotKeyHonorModalViews" => {
                cfg.snapshot_key_honor_modal_views = boolean(value, "snapshotKeyHonorModalViews")?;
            }
            _ => {}
        }
    }
    Ok(cfg)
}

/// A Jackson `Boolean` with default scalar coercion (`"true"`, `1`).
fn boolean(node: &Node, field: &str) -> Result<Option<bool>, Problem> {
    let Node::Scalar(s) = node else { return Err(Problem::format(field, node.pos())) };
    match s.typed() {
        Typed::Null => Ok(None),
        Typed::Bool(b) => Ok(Some(b)),
        Typed::Int(i) => Ok(Some(i != 0)),
        Typed::Str(t) if t.eq_ignore_ascii_case("true") => Ok(Some(true)),
        Typed::Str(t) if t.eq_ignore_ascii_case("false") => Ok(Some(false)),
        _ => Err(Problem::format(field, s.pos)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tapwing_section_is_parsed_and_not_reported_as_unknown() {
        let p = parse(
            "flows: ['*']\ntapwing:\n  appErrors: FAIL\n  appErrorsIgnore: 'overflowed'\n  appErrorsScreenshot: true\n  nativeDriver: Tapwing\n",
        )
        .unwrap();
        assert!(p.unknown_keys.is_empty());
        assert_eq!(
            p.config.tapwing,
            TapwingConfig {
                app_errors: Some(crate::app_health::AppErrorsPolicy::Fail),
                app_errors_ignore: vec!["overflowed".into()],
                app_errors_screenshot: Some(true),
                native_driver: Some("Tapwing".into()),
            }
        );
        assert!(parse("tapwing:\n  appErrors: loud\n").is_err());
        // Deprecated and ignored: any value loads (the CLI warns about it).
        let old = parse("tapwing:\n  nativeDriver: maestro\n").unwrap();
        assert_eq!(old.config.tapwing.native_driver.as_deref(), Some("maestro"));
        assert!(parse("tapwing:\n  nope: 1\n").is_err());
        // Maestro's workspace-config file detection keeps its exact key set.
        assert!(!looks_like_workspace_config("tapwing:\n  appErrors: fail\n"));
    }

    #[test]
    fn parses_string_lists_from_scalar_or_list() {
        let p = parse("flows: featureA/*\nincludeTags:\n  - a\n  - b\nexcludeTags: c\n").unwrap();
        assert_eq!(p.config.flows, Some(vec!["featureA/*".to_string()]));
        assert_eq!(p.config.include_tags, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(p.config.exclude_tags, Some(vec!["c".to_string()]));
        assert!(p.unknown_keys.is_empty());
    }

    #[test]
    fn execution_order_defaults_and_unknown_keys() {
        let p = parse("executionOrder:\n  flowsOrder:\n    - flowB\n    - flowC\nappId: x\ncustomField: 1\n").unwrap();
        assert_eq!(
            p.config.execution_order,
            Some(ExecutionOrder { continue_on_failure: Some(true), flows_order: vec!["flowB".into(), "flowC".into()] })
        );
        assert_eq!(p.unknown_keys, vec!["appId".to_string(), "customField".to_string()]);
        let p = parse("executionOrder:\n  continueOnFailure: false\n").unwrap();
        assert_eq!(p.config.execution_order.unwrap().continue_on_failure, Some(false));
    }

    #[test]
    fn blank_and_comment_only_configs_are_default() {
        assert_eq!(parse("").unwrap().config, WorkspaceConfig::default());
        assert_eq!(parse("\n# nothing here\n").unwrap().config, WorkspaceConfig::default());
    }

    #[test]
    fn rejects_wrong_shapes() {
        assert_eq!(parse("flows:\n  a: b\n").unwrap_err().title, "Incorrect Format: flows");
        assert_eq!(parse("- a\n").unwrap_err().title, "Incorrect Format: workspace config");
        assert_eq!(parse("executionOrder: x\n").unwrap_err().title, "Incorrect Format: executionOrder");
    }

    #[test]
    fn workspace_config_probe_matches_filters_kt() {
        assert!(looks_like_workspace_config("includeTags:\n  - included\n"));
        assert!(looks_like_workspace_config("platform:\n  android:\n    disableAnimations: true\n"));
        assert!(!looks_like_workspace_config("appId: com.example\n---\n- launchApp\n"));
        assert!(!looks_like_workspace_config("appId: com.example\n"));
        assert!(!looks_like_workspace_config("- launchApp\n"));
        assert!(!looks_like_workspace_config(""));
    }

    #[test]
    fn parses_platform_android_and_ios_settings_and_rejects_non_maps() {
        let p = parse(
            "platform:\n  android:\n    disableAnimations: true\n    extraKey: 1\n  ios:\n    disableAnimations: true\n    snapshotKeyHonorModalViews: false\n",
        )
        .unwrap();
        assert!(p.config.platform.android.disable_animations);
        assert!(p.config.platform.ios.disable_animations);
        assert_eq!(p.config.platform.ios.snapshot_key_honor_modal_views, Some(false));

        assert_eq!(parse("platform: null\n").unwrap().config.platform, PlatformConfig::default());
        assert_eq!(parse("platform: 123\n").unwrap_err().title, "Incorrect Format: platform");
        assert_eq!(parse("platform:\n  android: true\n").unwrap_err().title, "Incorrect Format: android");
        assert_eq!(parse("platform:\n  ios: [1]\n").unwrap_err().title, "Incorrect Format: ios");
    }
}
