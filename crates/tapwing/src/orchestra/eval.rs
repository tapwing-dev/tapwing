//! Pure `${...}` script-evaluation AST walker matching `MaestroCommand.evaluateScripts` (`Commands.kt`).

use anyhow::Result;
use serde_json::Value;

use crate::backend::{MaestroError, scroll_timeout_ms, speed_to_duration};
use crate::flow::ir::{
    AssertCondition, CallHook, Condition, DefineVariables, ElementSelector, EvalScript, GeoPoint, LaunchApp,
    MaestroCommand, MaestroConfig, OpenLink, Repeat, Retry, RunFlow, RunScript, ScrollUntilVisible, SetLocation,
    SetOrientation, SetPermissions, StartRecording, Swipe, TakeScreenshot, TapOnElement, TapOnPoint, Travel, ValueMap,
    WaitForAnimationToEnd, orientation_by_name,
};
use crate::js::JsEngine;

/// Evaluates `${...}` script expressions across every interpolatable field of `cmd`.
pub fn evaluate_command_scripts(cmd: &MaestroCommand, js: &mut JsEngine) -> Result<MaestroCommand> {
    let eval_opt = |s: &Option<String>, js: &mut JsEngine| -> Result<Option<String>> {
        s.as_deref().map(|v| js.evaluate_scripts(v)).transpose()
    };

    Ok(match cmd {
        MaestroCommand::TapOnElement(c) => MaestroCommand::TapOnElement(TapOnElement {
            selector: evaluate_selector_scripts(&c.selector, js)?,
            relative_point: eval_opt(&c.relative_point, js)?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::TapOnPoint(c) => MaestroCommand::TapOnPoint(TapOnPoint {
            point: js.evaluate_scripts(&c.point)?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::Swipe(c) => MaestroCommand::Swipe(Swipe {
            element_selector: c.element_selector.as_ref().map(|s| evaluate_selector_scripts(s, js)).transpose()?,
            start_relative: eval_opt(&c.start_relative, js)?,
            end_relative: eval_opt(&c.end_relative, js)?,
            relative_point: eval_opt(&c.relative_point, js)?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::ScrollUntilVisible(c) => {
            let eval_speed = js.evaluate_scripts(&c.scroll_duration)?;
            let duration = speed_to_duration(&eval_speed)?;
            let eval_timeout = js.evaluate_scripts(&c.timeout)?;
            let timeout_ms = scroll_timeout_ms(&eval_timeout)?;
            MaestroCommand::ScrollUntilVisible(ScrollUntilVisible {
                original_speed_value: Some(c.scroll_duration.clone()),
                selector: evaluate_selector_scripts(&c.selector, js)?,
                scroll_duration: duration.to_string(),
                timeout: timeout_ms.to_string(),
                label: eval_opt(&c.label, js)?,
                ..c.clone()
            })
        }
        MaestroCommand::AssertCondition(c) => MaestroCommand::AssertCondition(AssertCondition {
            condition: evaluate_condition_scripts(&c.condition, js)?,
            timeout: eval_opt(&c.timeout, js)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::AssertScreenshot(c) => MaestroCommand::AssertScreenshot(crate::flow::ir::AssertScreenshot {
            path: js.evaluate_scripts(&c.path)?,
            threshold_percentage: js.evaluate_scripts(&c.threshold_percentage)?,
            crop_on: c.crop_on.as_ref().map(|s| evaluate_selector_scripts(s, js)).transpose()?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::InputText(c) => MaestroCommand::InputText(crate::flow::ir::InputText {
            text: js.evaluate_scripts(&c.text)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::CopyTextFrom(c) => MaestroCommand::CopyTextFrom(crate::flow::ir::CopyTextFrom {
            selector: evaluate_selector_scripts(&c.selector, js)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::SetClipboard(c) => MaestroCommand::SetClipboard(crate::flow::ir::SetClipboard {
            text: js.evaluate_scripts(&c.text)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::LaunchApp(c) => MaestroCommand::LaunchApp(LaunchApp {
            app_id: js.evaluate_scripts(&c.app_id)?,
            permissions: c.permissions.as_ref().map(|m| evaluate_value_map(m, js, false)).transpose()?,
            launch_arguments: c.launch_arguments.as_ref().map(|m| evaluate_value_map(m, js, true)).transpose()?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::SetPermissions(c) => MaestroCommand::SetPermissions(SetPermissions {
            app_id: js.evaluate_scripts(&c.app_id)?,
            permissions: evaluate_value_map(&c.permissions, js, false)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::ApplyConfiguration(c) => {
            MaestroCommand::ApplyConfiguration(crate::flow::ir::ApplyConfiguration {
                config: evaluate_config_scripts(&c.config, js)?,
                label: eval_opt(&c.label, js)?,
                optional: c.optional,
            })
        }
        MaestroCommand::OpenLink(c) => MaestroCommand::OpenLink(OpenLink {
            link: js.evaluate_scripts(&c.link)?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::TakeScreenshot(c) => MaestroCommand::TakeScreenshot(TakeScreenshot {
            path: js.evaluate_scripts(&c.path)?,
            crop_on: c.crop_on.as_ref().map(|s| evaluate_selector_scripts(s, js)).transpose()?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::StopApp(c) => MaestroCommand::StopApp(crate::flow::ir::AppCommand {
            app_id: js.evaluate_scripts(&c.app_id)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::KillApp(c) => MaestroCommand::KillApp(crate::flow::ir::AppCommand {
            app_id: js.evaluate_scripts(&c.app_id)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::ClearState(c) => MaestroCommand::ClearState(crate::flow::ir::AppCommand {
            app_id: js.evaluate_scripts(&c.app_id)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::RunFlow(c) => MaestroCommand::RunFlow(RunFlow {
            condition: c.condition.as_ref().map(|cond| evaluate_condition_scripts(cond, js)).transpose()?,
            config: c.config.as_ref().map(|cfg| evaluate_config_scripts(cfg, js)).transpose()?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::SetLocation(c) => MaestroCommand::SetLocation(SetLocation {
            latitude: js.evaluate_scripts(&c.latitude)?,
            longitude: js.evaluate_scripts(&c.longitude)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::SetOrientation(c) => {
            let evaluated = js.evaluate_scripts(&c.orientation)?;
            let resolved = orientation_by_name(&evaluated)
                .ok_or_else(|| MaestroError::invalid_command(format!("Unknown orientation: {evaluated}")))?;
            MaestroCommand::SetOrientation(SetOrientation {
                orientation: resolved.to_string(),
                label: eval_opt(&c.label, js)?,
                optional: c.optional,
            })
        }
        MaestroCommand::Repeat(c) => MaestroCommand::Repeat(Repeat {
            times: eval_opt(&c.times, js)?,
            label: eval_opt(&c.label, js)?,
            // Condition is intentionally NOT evaluated here; it is re-evaluated
            // fresh on every loop iteration (`Commands.kt:955-960`).
            ..c.clone()
        }),
        MaestroCommand::Retry(c) => MaestroCommand::Retry(Retry {
            max_retries: eval_opt(&c.max_retries, js)?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::CallHook(c) => MaestroCommand::CallHook(CallHook {
            name: js.evaluate_scripts(&c.name)?,
            args: c.args.as_ref().map(|m| evaluate_json_value_map(m, js)).transpose()?,
            timeout: eval_opt(&c.timeout, js)?,
            settle: c.settle,
            wait_to_settle_timeout_ms: eval_opt(&c.wait_to_settle_timeout_ms, js)?,
            output_variable: eval_opt(&c.output_variable, js)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::DefineVariables(c) => MaestroCommand::DefineVariables(evaluate_define_variables(c, js)?),
        MaestroCommand::RunScript(c) => MaestroCommand::RunScript(RunScript {
            env: evaluate_value_map(&c.env, js, false)?,
            condition: c.condition.as_ref().map(|cond| evaluate_condition_scripts(cond, js)).transpose()?,
            label: eval_opt(&c.label, js)?,
            ..c.clone()
        }),
        MaestroCommand::WaitForAnimationToEnd(c) => MaestroCommand::WaitForAnimationToEnd(WaitForAnimationToEnd {
            timeout: eval_opt(&c.timeout, js)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        // `Commands.kt:1082`: `EvalScriptCommand.evaluateScripts` returns `this` unchanged.
        MaestroCommand::EvalScript(c) => MaestroCommand::EvalScript(EvalScript { ..c.clone() }),
        MaestroCommand::Travel(c) => {
            let mut pts = Vec::with_capacity(c.points.len());
            for p in &c.points {
                pts.push(GeoPoint {
                    latitude: js.evaluate_scripts(&p.latitude)?,
                    longitude: js.evaluate_scripts(&p.longitude)?,
                });
            }
            MaestroCommand::Travel(Travel { points: pts, label: eval_opt(&c.label, js)?, ..c.clone() })
        }
        MaestroCommand::StartRecording(c) => MaestroCommand::StartRecording(StartRecording {
            path: js.evaluate_scripts(&c.path)?,
            label: eval_opt(&c.label, js)?,
            optional: c.optional,
        }),
        MaestroCommand::AddMedia(c) => {
            let mut paths = Vec::with_capacity(c.media_paths.len());
            for p in &c.media_paths {
                paths.push(js.evaluate_scripts(p)?);
            }
            MaestroCommand::AddMedia(crate::flow::ir::AddMedia {
                media_paths: paths,
                label: eval_opt(&c.label, js)?,
                optional: c.optional,
            })
        }
        other => other.clone(),
    })
}

pub(super) fn evaluate_define_variables(d: &DefineVariables, js: &mut JsEngine) -> Result<DefineVariables> {
    Ok(DefineVariables {
        env: evaluate_value_map(&d.env, js, false)?,
        label: d.label.as_deref().map(|l| js.evaluate_scripts(l)).transpose()?,
        optional: d.optional,
    })
}

fn evaluate_config_scripts(cfg: &MaestroConfig, js: &mut JsEngine) -> Result<MaestroConfig> {
    Ok(MaestroConfig {
        app_id: cfg.app_id.as_deref().map(|s| js.evaluate_scripts(s)).transpose()?,
        name: cfg.name.as_deref().map(|s| js.evaluate_scripts(s)).transpose()?,
        properties: evaluate_value_map(&cfg.properties, js, false)?,
        ..cfg.clone()
    })
}

pub(super) fn evaluate_condition_scripts(cond: &Condition, js: &mut JsEngine) -> Result<Condition> {
    Ok(Condition {
        platform: cond.platform,
        visible: cond.visible.as_ref().map(|s| evaluate_selector_scripts(s, js)).transpose()?,
        not_visible: cond.not_visible.as_ref().map(|s| evaluate_selector_scripts(s, js)).transpose()?,
        script_condition: cond.script_condition.as_deref().map(|s| js.evaluate_scripts(s)).transpose()?,
        label: cond.label.clone(),
    })
}

fn evaluate_selector_scripts(sel: &ElementSelector, js: &mut JsEngine) -> Result<ElementSelector> {
    let eval_opt = |s: &Option<String>, js: &mut JsEngine| -> Result<Option<String>> {
        s.as_deref().map(|v| js.evaluate_scripts(v)).transpose()
    };
    let eval_box = |b: &Option<Box<ElementSelector>>, js: &mut JsEngine| -> Result<Option<Box<ElementSelector>>> {
        b.as_deref().map(|s| evaluate_selector_scripts(s, js).map(Box::new)).transpose()
    };
    Ok(ElementSelector {
        text_regex: eval_opt(&sel.text_regex, js)?,
        id_regex: eval_opt(&sel.id_regex, js)?,
        size: sel.size.clone(),
        below: eval_box(&sel.below, js)?,
        above: eval_box(&sel.above, js)?,
        left_of: eval_box(&sel.left_of, js)?,
        right_of: eval_box(&sel.right_of, js)?,
        contains_child: eval_box(&sel.contains_child, js)?,
        contains_descendants: sel
            .contains_descendants
            .as_ref()
            .map(|list| list.iter().map(|d| evaluate_selector_scripts(d, js)).collect::<Result<Vec<_>>>())
            .transpose()?,
        traits: sel.traits.clone(),
        index: eval_opt(&sel.index, js)?,
        enabled: sel.enabled,
        optional: sel.optional,
        selected: sel.selected,
        checked: sel.checked,
        focused: sel.focused,
        child_of: eval_box(&sel.child_of, js)?,
        css: eval_opt(&sel.css, js)?,
    })
}

fn evaluate_value_map(map: &ValueMap, js: &mut JsEngine, eval_keys: bool) -> Result<ValueMap> {
    let mut out = ValueMap::new();
    for (k, v) in map {
        if v.is_null() {
            return Err(anyhow::anyhow!("entry \"{k}\" has no value"));
        }
        let new_key = if eval_keys { js.evaluate_scripts(k)? } else { k.clone() };
        if eval_keys && out.contains_key(&new_key) {
            return Err(anyhow::anyhow!("duplicate key \"{new_key}\" after interpolation"));
        }
        let new_val = match v {
            Value::String(s) => Value::String(js.evaluate_scripts(s)?),
            other => other.clone(),
        };
        out.insert(new_key, new_val);
    }
    Ok(out)
}

fn evaluate_json_value(v: &Value, js: &mut JsEngine) -> Result<Value> {
    match v {
        Value::String(s) => Ok(Value::String(js.evaluate_scripts(s)?)),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(evaluate_json_value(item, js)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => Ok(Value::Object(evaluate_json_value_map(map, js)?)),
        other => Ok(other.clone()),
    }
}

fn evaluate_json_value_map(map: &ValueMap, js: &mut JsEngine) -> Result<ValueMap> {
    let mut out = ValueMap::new();
    for (k, v) in map {
        let new_key = js.evaluate_scripts(k)?;
        if out.contains_key(&new_key) {
            return Err(anyhow::anyhow!("duplicate key \"{new_key}\" after interpolation"));
        }
        out.insert(new_key, evaluate_json_value(v, js)?);
    }
    Ok(out)
}
