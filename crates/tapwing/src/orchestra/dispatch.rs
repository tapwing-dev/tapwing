//! Per-command execution dispatch, condition evaluation, and timeout math.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::backend::{
    AssertionError, FlowDriver, InvalidNumericError, LaunchRequest, MAX_RETRIES_ALLOWED, MaestroError,
    ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest, is_maestro_error, parse_timeout_ms,
    parse_wait_animation_timeout, scroll_timeout_ms,
};
use crate::flow::ir::{Condition, GeoPoint, MaestroCommand, MaestroConfig, OnOff, Travel};
use crate::{random, screenshot};

use super::describe::{describe_condition, is_optional};
use super::eval::evaluate_condition_scripts;
use super::{CommandMetadata, CommandSkipped, Orchestra};

impl<'a, D: FlowDriver + ?Sized> Orchestra<'a, D> {
    pub(super) fn execute_single_command(
        &mut self,
        cmd: &MaestroCommand,
        raw_cmd: &MaestroCommand,
        config: Option<&MaestroConfig>,
    ) -> Result<(bool, Value)> {
        match cmd {
            MaestroCommand::ApplyConfiguration(_) => Ok((false, Value::Null)),
            MaestroCommand::DefineVariables(d) => {
                for (k, v) in &d.env {
                    if let Some(s) = v.as_str() {
                        self.js.put_env(k.clone(), s.to_string());
                    }
                }
                Ok((false, Value::Null))
            }
            MaestroCommand::LaunchApp(c) => {
                self.flow_app_id = Some(c.app_id.clone());
                self.app_intentionally_stopped = false;
                let permissions: BTreeMap<String, String> = match &c.permissions {
                    Some(map) => map
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                        .collect(),
                    None => BTreeMap::from([("all".to_string(), "allow".to_string())]),
                };
                let req = LaunchRequest {
                    app_id: c.app_id.clone(),
                    clear_state: c.clear_state.unwrap_or(false),
                    clear_keychain: c.clear_keychain.unwrap_or(false),
                    stop_app: c.stop_app.unwrap_or(true),
                    permissions,
                    launch_arguments: c.launch_arguments.clone().unwrap_or_default(),
                };
                let extra = self.driver.launch_app(&req)?;
                Ok((true, extra))
            }
            MaestroCommand::StopApp(c) => {
                let res = self.driver.stop_app(&c.app_id)?;
                if self.flow_app_id.as_deref().is_none_or(|id| id == c.app_id) {
                    self.app_intentionally_stopped = true;
                }
                Ok((true, res))
            }
            MaestroCommand::KillApp(c) => {
                let res = self.driver.kill_app(&c.app_id)?;
                if self.flow_app_id.as_deref().is_none_or(|id| id == c.app_id) {
                    self.app_intentionally_stopped = true;
                }
                Ok((true, res))
            }
            MaestroCommand::ClearState(c) => {
                let res = self.driver.clear_state(&c.app_id)?;
                if self.flow_app_id.as_deref().is_none_or(|id| id == c.app_id) {
                    self.app_intentionally_stopped = true;
                }
                Ok((true, res))
            }
            MaestroCommand::ClearKeychain(_) => Ok((false, self.driver.clear_keychain()?)),
            MaestroCommand::SetPermissions(c) => {
                let perms: BTreeMap<String, String> = c
                    .permissions
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                    .collect();
                Ok((false, self.driver.set_permissions(&c.app_id, &perms)?))
            }
            MaestroCommand::TapOnElement(c) => {
                let app_id = self.evaluated_app_id(config);
                let req = TapElementRequest {
                    selector: c.selector.clone(),
                    timeout_ms: self.lookup_timeout(is_optional(cmd)),
                    optional: is_optional(cmd),
                    long_press: c.long_press.unwrap_or(false),
                    repeat: c.repeat.clone(),
                    relative_point: c.relative_point.clone(),
                    retry_if_no_change: c.retry_if_no_change.unwrap_or(false),
                    wait_until_visible: c.wait_until_visible.unwrap_or(false),
                    wait_to_settle_timeout_ms: c.wait_to_settle_timeout_ms,
                    app_id,
                };
                Ok((true, self.driver.tap_on_element(&req)?))
            }
            MaestroCommand::TapOnPoint(c) => {
                let req = TapPointRequest {
                    point: c.point.clone(),
                    long_press: c.long_press.unwrap_or(false),
                    repeat: c.repeat.clone(),
                    retry_if_no_change: c.retry_if_no_change.unwrap_or(false),
                    wait_to_settle_timeout_ms: c.wait_to_settle_timeout_ms,
                };
                Ok((true, self.driver.tap_on_point(&req)?))
            }
            MaestroCommand::AssertCondition(c) => {
                let timeout_ms = match &c.timeout {
                    Some(t) => parse_timeout_ms(t, "timeout")?,
                    None => self.opts.lookup_timeout_ms,
                };
                let ok = self.evaluate_condition(Some(&c.condition), c.optional, Some(timeout_ms))?;
                if !ok {
                    return Err(AssertionError(format!("Assertion is false: {}", describe_condition(&c.condition))).into());
                }
                Ok((false, Value::Null))
            }
            MaestroCommand::InputText(c) => Ok((true, self.driver.input_text(&c.text)?)),
            MaestroCommand::InputRandom(c) => {
                let text = random::generate(c.input_type, c.length);
                Ok((true, self.driver.input_text(&text)?))
            }
            MaestroCommand::EraseText(c) => Ok((true, self.driver.erase_text(c.characters_to_erase.unwrap_or(50))?)),
            MaestroCommand::BackPress(_) => Ok((true, self.driver.back_press()?)),
            MaestroCommand::HideKeyboard(_) => Ok((true, self.driver.hide_keyboard()?)),
            MaestroCommand::PressKey(c) => Ok((true, self.driver.press_key(c.code)?)),
            MaestroCommand::CopyTextFrom(c) => {
                let timeout = self.lookup_timeout(is_optional(cmd));
                let (text, extra) = self.driver.copy_text_from(&c.selector, timeout, is_optional(cmd))?;
                self.copied_text = Some(text.clone());
                self.js.set_copied_text(Some(text));
                Ok((false, extra))
            }
            MaestroCommand::SetClipboard(c) => {
                self.copied_text = Some(c.text.clone());
                self.js.set_copied_text(Some(c.text.clone()));
                Ok((false, json!({ "text": c.text })))
            }
            MaestroCommand::PasteText(_) => match self.copied_text.clone() {
                Some(text) => Ok((true, self.driver.input_text(&text)?)),
                None => Ok((true, Value::Null)),
            },
            MaestroCommand::Scroll(_) => Ok((true, self.driver.scroll()?)),
            MaestroCommand::Swipe(c) => {
                let req = SwipeRequest {
                    element_selector: c.element_selector.clone(),
                    direction: c.direction,
                    start_relative: c.start_relative.clone(),
                    end_relative: c.end_relative.clone(),
                    start_point: c.start_point.clone(),
                    end_point: c.end_point.clone(),
                    relative_point: c.relative_point.clone(),
                    duration_ms: c.duration,
                    timeout_ms: self.lookup_timeout(is_optional(cmd)),
                    optional: is_optional(cmd),
                    wait_to_settle_timeout_ms: c.wait_to_settle_timeout_ms,
                };
                Ok((true, self.driver.swipe(&req)?))
            }
            MaestroCommand::ScrollUntilVisible(c) => {
                // `evaluate_command_scripts` already converted speed -> duration_ms string
                // and validated timeout (`Commands.kt:171-179`).
                let duration_ms = c.scroll_duration.trim().parse::<u64>().unwrap_or(601);
                let timeout_ms = scroll_timeout_ms(&c.timeout)?;
                let req = ScrollUntilVisibleRequest {
                    selector: c.selector.clone(),
                    direction: c.direction,
                    duration_ms,
                    timeout_ms,
                    visibility_percentage: c.visibility_percentage,
                    center_element: c.center_element,
                    optional: c.optional,
                    wait_to_settle_timeout_ms: c.wait_to_settle_timeout_ms,
                };
                Ok((true, self.driver.scroll_until_visible(&req)?))
            }
            MaestroCommand::WaitForAnimationToEnd(c) => {
                let timeout = match &c.timeout {
                    Some(t) => parse_wait_animation_timeout(t)?,
                    None => Some(15_000),
                };
                Ok((true, self.driver.wait_for_animation_to_end(timeout)?))
            }
            MaestroCommand::TakeScreenshot(c) => {
                let png = if let Some(crop_on) = &c.crop_on {
                    let bounds = self.driver.find_element_bounds(crop_on, self.lookup_timeout(c.optional), c.optional)?;
                    let full = self.driver.screenshot_image()?;
                    let cropped = screenshot::crop_image(&full, bounds).map_err(|e| AssertionError(e.to_string()))?;
                    screenshot::encode_png(&cropped)?
                } else {
                    self.driver.screenshot_bytes()?
                };
                let out_path = match &self.opts.test_output_dir {
                    Some(dir) => screenshot::resolve_take_path(dir, &c.path),
                    None => PathBuf::from(screenshot::normalize_take_path(&c.path)),
                };
                if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| MaestroError::not_writable(format!("Cannot create {}: {e}", parent.display())))?;
                }
                std::fs::write(&out_path, &png)
                    .map_err(|e| MaestroError::not_writable(format!("Cannot write {}: {e}", out_path.display())))?;
                Ok((false, json!({ "path": out_path })))
            }
            MaestroCommand::AssertScreenshot(c) => {
                let threshold = c.threshold_percentage.trim().parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(
                    || {
                        AssertionError(format!(
                            "Invalid thresholdPercentage for assertScreenshot: \"{}\". Expected a number.",
                            c.threshold_percentage
                        ))
                    },
                )?;
                let out_dir = self.opts.test_output_dir.clone().unwrap_or_else(|| PathBuf::from("."));
                let expected_path = screenshot::find_expected_file(&self.opts.flow_path, &out_dir, &c.path)
                    .map_err(|e| AssertionError(e.to_string()))?;
                let crop_bounds = if let Some(crop_on) = &c.crop_on {
                    Some(self.driver.find_element_bounds(crop_on, self.lookup_timeout(c.optional), c.optional)?)
                } else {
                    None
                };
                let actual_full = self.driver.screenshot_image()?;
                let actual_img = match crop_bounds {
                    Some(bounds) => {
                        screenshot::crop_image(&actual_full, bounds).map_err(|e| AssertionError(e.to_string()))?
                    }
                    None => actual_full,
                };
                let expected_img = image::open(&expected_path).map_err(|_| {
                    AssertionError(format!(
                        "Failed to read image file: {}. Unsupported image format or file could not be read.",
                        expected_path.display()
                    ))
                })?;
                let diff_path = screenshot::diff_output_path(&out_dir, &expected_path);
                match screenshot::compare(
                    &expected_img,
                    &actual_img,
                    threshold,
                    &diff_path,
                    screenshot::DEFAULT_PIXEL_TOLERANCE,
                )? {
                    screenshot::CompareResult::Match { match_percent } => {
                        Ok((false, json!({ "expected": expected_path, "matchPercent": match_percent })))
                    }
                    screenshot::CompareResult::SizeMismatch { expected_w, expected_h, actual_w, actual_h } => {
                        Err(AssertionError(format!(
                            "Screenshot size mismatch: expected {expected_w}x{expected_h}, actual {actual_w}x{actual_h}."
                        ))
                        .into())
                    }
                    screenshot::CompareResult::Mismatch { match_percent } => Err(AssertionError(format!(
                        "Comparison error: threshold {threshold}% not met, current: {match_percent:.4}% (diff written to {})",
                        diff_path.display()
                    ))
                    .into()),
                }
            }
            MaestroCommand::OpenLink(c) => {
                let app_id = self.evaluated_app_id(config);
                let extra = self.driver.open_link(
                    &c.link,
                    app_id.as_deref(),
                    c.auto_verify.unwrap_or(false),
                    c.browser.unwrap_or(false),
                )?;
                Ok((true, extra))
            }
            MaestroCommand::SetLocation(c) => {
                let parse_coord = |raw: &str, what: &str| {
                    raw.trim().parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| {
                        anyhow::Error::new(InvalidNumericError(format!("{what} `{raw}` is not a number")))
                    })
                };
                let lat = parse_coord(&c.latitude, "latitude")?;
                let lon = parse_coord(&c.longitude, "longitude")?;
                Ok((true, self.driver.set_location(lat, lon)?))
            }
            MaestroCommand::Travel(c) => {
                self.execute_travel(c)?;
                Ok((true, Value::Null))
            }
            MaestroCommand::SetOrientation(c) => Ok((true, self.driver.set_orientation(&c.orientation)?)),
            MaestroCommand::AddMedia(c) => Ok((true, self.driver.add_media(&c.media_paths)?)),
            MaestroCommand::StartRecording(c) => {
                let out_path = match &self.opts.test_output_dir {
                    Some(dir) => dir.join("startRecording").join(format!("{}.mp4", c.path)),
                    None => PathBuf::from(format!("{}.mp4", c.path)),
                };
                let extra = self.driver.start_recording(&out_path)?;
                self.recording_open = true;
                Ok((false, extra))
            }
            MaestroCommand::StopRecording(_) => {
                if self.recording_open {
                    self.recording_open = false;
                    Ok((false, self.driver.stop_recording()?))
                } else {
                    Ok((false, Value::Null))
                }
            }
            MaestroCommand::SetAirplaneMode(c) => {
                let enabled = c.value == OnOff::Enable;
                Ok((true, self.driver.set_airplane_mode(enabled)?))
            }
            MaestroCommand::ToggleAirplaneMode(_) => {
                let cur = self.driver.is_airplane_mode_enabled()?;
                Ok((true, self.driver.set_airplane_mode(!cur)?))
            }
            MaestroCommand::SetDarkMode(c) => {
                let enabled = c.value == OnOff::Enable;
                Ok((true, self.driver.set_dark_mode(enabled)?))
            }
            MaestroCommand::ToggleDarkMode(_) => {
                let cur = self.driver.is_dark_mode_enabled()?;
                Ok((true, self.driver.set_dark_mode(!cur)?))
            }
            MaestroCommand::AssertDarkMode(_) => self.assert_theme_mode(true),
            MaestroCommand::AssertLightMode(_) => self.assert_theme_mode(false),
            MaestroCommand::EvalScript(c) => {
                // `Orchestra.kt:777`: evaluate `${...}` expressions inside `script_string`.
                let _ = self.js.evaluate_scripts(&c.script_string)?;
                Ok((true, Value::Null))
            }
            MaestroCommand::RunScript(c) => {
                if !self.evaluate_condition(c.condition.as_ref(), c.optional, None)? {
                    return Err(CommandSkipped.into());
                }
                let env_map: HashMap<String, String> =
                    c.env.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
                let script_dir = c.script_dir.as_deref().map(Path::new);
                self.js.run_script_file(&c.script, &env_map, &c.source_description, script_dir)?;
                Ok((true, Value::Null))
            }
            MaestroCommand::RunFlow(c) => {
                if !self.evaluate_condition(c.condition.as_ref(), c.optional, None)? {
                    return Err(CommandSkipped.into());
                }
                if c.optional {
                    self.optional_depth += 1;
                }
                let res = self.run_sub_flow(&c.commands, config, c.config.as_ref());
                if c.optional {
                    self.optional_depth -= 1;
                }
                Ok((res?, Value::Null))
            }
            MaestroCommand::Repeat(c) => {
                let max_runs: usize = match &c.times {
                    Some(raw) => raw.trim().parse::<f64>().ok().map(|f| f.max(0.0) as usize).unwrap_or(usize::MAX),
                    None => usize::MAX,
                };
                if c.optional {
                    self.optional_depth += 1;
                }
                let res = (|| -> Result<(bool, Value)> {
                    let mut counter = 0usize;
                    let mut mutated = false;
                    loop {
                        crate::interrupt::check()?;
                        if counter >= max_runs {
                            break;
                        }
                        // `Orchestra.kt:966-970`: evaluate `while` condition fresh on every iteration.
                        if let Some(raw_cond) = &c.condition {
                            let eval_cond = evaluate_condition_scripts(raw_cond, &mut self.js)?;
                            if !self.evaluate_condition(Some(&eval_cond), c.optional, None)? {
                                break;
                            }
                        }
                        let m = self.run_sub_flow(&c.commands, config, None)?;
                        mutated |= m;
                        counter += 1;
                        let meta = CommandMetadata {
                            number_of_runs: Some(counter),
                            evaluated_command: None,
                            log_messages: Vec::new(),
                        };
                        if let Some(cb) = self.callbacks.on_command_metadata_update.as_mut() {
                            cb(raw_cmd, &meta);
                        }
                    }
                    if counter == 0 {
                        return Err(CommandSkipped.into());
                    }
                    Ok((mutated, json!({ "runs": counter })))
                })();
                if c.optional {
                    self.optional_depth -= 1;
                }
                res
            }
            MaestroCommand::Retry(c) => {
                let max_retries = c
                    .max_retries
                    .as_deref()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .unwrap_or(1)
                    .clamp(0, MAX_RETRIES_ALLOWED);
                let mut attempt = 0;
                loop {
                    self.failure_captured = false;
                    let has_more = attempt < max_retries;
                    if has_more {
                        self.retry_attempts_remaining += 1;
                    }
                    self.retry_attempts.push(attempt as usize + 1);
                    let attempt_res = self.run_sub_flow(&c.commands, config, c.config.as_ref());
                    self.retry_attempts.pop();
                    if has_more {
                        self.retry_attempts_remaining = self.retry_attempts_remaining.saturating_sub(1);
                    }
                    match attempt_res {
                        Ok(mutated) => return Ok((mutated, json!({ "attempts": attempt + 1 }))),
                        Err(e) if is_maestro_error(&e) => {
                            if attempt == max_retries {
                                return Err(e);
                            }
                            attempt += 1;
                        }
                        Err(e) => {
                            // `Orchestra.kt:999-1005`: non-Maestro errors (transport death,
                            // JS evaluation errors, runtime panics) propagate immediately.
                            return Err(e);
                        }
                    }
                }
            }
            MaestroCommand::AssertWithAi(_)
            | MaestroCommand::AssertNoDefectsWithAi(_)
            | MaestroCommand::ExtractTextWithAi(_) => Err(MaestroError::cloud_api_key(
                "AI commands (assertWithAI, assertNoDefectsWithAI, extractTextWithAI) are post-MVP and require a configured AI provider.",
            )
            .into()),
        }
    }

    fn execute_travel(&mut self, c: &Travel) -> Result<()> {
        if c.points.is_empty() {
            return Ok(());
        }
        let speed_mps = c.speed_mps.unwrap_or(4.0).max(0.001);
        let parse_pt = |p: &GeoPoint| -> Result<(f64, f64)> {
            let lat: f64 = p
                .latitude
                .trim()
                .parse()
                .ok()
                .filter(|v: &f64| v.is_finite())
                .ok_or_else(|| anyhow!("invalid latitude `{}`", p.latitude))?;
            let lon: f64 = p
                .longitude
                .trim()
                .parse()
                .ok()
                .filter(|v: &f64| v.is_finite())
                .ok_or_else(|| anyhow!("invalid longitude `{}`", p.longitude))?;
            Ok((lat, lon))
        };

        let mut start = parse_pt(&c.points[0])?;
        self.driver.set_location(start.0, start.1)?;
        for next_raw in &c.points[1..] {
            let end = parse_pt(next_raw)?;
            let steps = 50usize;
            let dist = haversine_meters(start.0, start.1, end.0, end.1);
            let sleep_ms = ((dist / speed_mps) * 1000.0) as u64 / steps as u64;
            let lat_step = (end.0 - start.0) / steps as f64;
            let lon_step = (end.1 - start.1) / steps as f64;
            for i in 1..=steps {
                crate::interrupt::check()?;
                let lat = if i == steps { end.0 } else { start.0 + lat_step * i as f64 };
                let lon = if i == steps { end.1 } else { start.1 + lon_step * i as f64 };
                self.driver.set_location(lat, lon)?;
                if self.opts.travel_sleep && sleep_ms > 0 {
                    crate::interrupt::sleep(std::time::Duration::from_millis(sleep_ms))?;
                }
            }
            start = end;
        }
        Ok(())
    }

    fn assert_theme_mode(&mut self, expect_dark: bool) -> Result<(bool, Value)> {
        let is_dark = self.driver.is_dark_mode_enabled()?;
        if is_dark != expect_dark {
            let expected = if expect_dark { "enabled" } else { "disabled" };
            let actual = if is_dark { "enabled" } else { "disabled" };
            return Err(AssertionError(format!(
                "Assertion failed: expected dark mode to be {expected}, but it was {actual}"
            ))
            .into());
        }
        Ok((false, json!({ "darkMode": is_dark, "brightness": if is_dark { "dark" } else { "light" } })))
    }

    /// Exact port of `Orchestra.evaluateCondition` (`Orchestra.kt:1077-1150`).
    fn evaluate_condition(
        &mut self,
        condition: Option<&Condition>,
        command_optional: bool,
        timeout_ms: Option<u64>,
    ) -> Result<bool> {
        let Some(cond) = condition else {
            return Ok(true);
        };

        if let Some(expected_platform) = cond.platform
            && expected_platform != self.driver.device_info().platform
        {
            return Ok(false);
        }

        if let Some(value) = &cond.script_condition
            && !is_condition_truthy(value)
        {
            return Ok(false);
        }

        let effective_timeout = self.adjusted_timeout(timeout_ms.unwrap_or(self.opts.optional_lookup_timeout_ms));

        if let Some(sel) = &cond.visible {
            match self.driver.assert_visible(sel, effective_timeout, command_optional) {
                Ok(_) => {}
                Err(e) if is_not_found_or_assertion(&e) => return Ok(false),
                Err(e) => return Err(e),
            }
        }

        if let Some(sel) = &cond.not_visible {
            match self.driver.assert_not_visible(sel, effective_timeout, command_optional) {
                Ok(_) => {}
                Err(e) if is_not_found_or_assertion(&e) => return Ok(false),
                Err(e) => return Err(e),
            }
        }

        Ok(true)
    }

    fn lookup_timeout(&self, optional: bool) -> u64 {
        let base = if optional { self.opts.optional_lookup_timeout_ms } else { self.opts.lookup_timeout_ms };
        self.adjusted_timeout(base)
    }

    fn adjusted_timeout(&self, base_ms: u64) -> u64 {
        adjusted_timeout(base_ms, self.last_interaction)
    }
}

/// Exact port of `Orchestra.evaluateCondition` truthiness rules (`Orchestra.kt:1092-1114`).
pub fn is_condition_truthy(value: &str) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    if value.eq_ignore_ascii_case("false") {
        return false;
    }
    if value == "undefined" || value == "null" {
        return false;
    }
    if let Ok(n) = value.trim().parse::<f64>()
        && n == 0.0
    {
        return false;
    }
    true
}

/// Port of `Orchestra.adjustedToLatestInteraction` (`Orchestra.kt:1830-1833`).
pub fn adjusted_timeout(base_ms: u64, last_interaction: Instant) -> u64 {
    let elapsed_ms = last_interaction.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    base_ms.saturating_sub(elapsed_ms)
}

fn is_not_found_or_assertion(e: &anyhow::Error) -> bool {
    if e.is::<AssertionError>() {
        return true;
    }
    if let Some(m) = e.downcast_ref::<MaestroError>() {
        return matches!(
            m.kind,
            crate::backend::MaestroErrorKind::ElementNotFound | crate::backend::MaestroErrorKind::AssertionFailure
        );
    }
    if let Some(d) = e.downcast_ref::<crate::driver::DriverError>() {
        return matches!(d.code.as_str(), "ELEMENT_NOT_FOUND" | "ASSERTION");
    }
    false
}

fn haversine_meters(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().atan2((1.0 - a).sqrt())
}
