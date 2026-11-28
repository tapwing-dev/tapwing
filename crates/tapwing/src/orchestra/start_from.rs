//! Resolution and static validation of `--start-from <step|label>` and `--attach`.

use anyhow::{Result, bail};

use crate::flow::ParsedFlow;
use crate::flow::ir::{MaestroCommand, MaestroConfig};

/// Execution plan derived from `--start-from` and `--attach` for one flow.
#[derive(Debug)]
pub struct ResolvedStart<'a> {
    /// Commands slice to execute (starting at the target top-level step).
    pub commands: &'a [MaestroCommand],
    /// Number of leading top-level executable steps skipped.
    pub skipped_steps: usize,
    /// 1-based step number where execution starts (`skipped_steps + 1`).
    pub start_index_1_based: usize,
    /// Raw `--start-from` selector passed by the user, if any.
    pub start_from: Option<String>,
    /// True if `onFlowStart` exists and was skipped because earlier steps were skipped.
    pub skipped_on_flow_start: bool,
    /// True when the first executed step is `launchApp` and `--attach` should
    /// convert it into `driver.warm_attach(app_id)` instead of restarting the app.
    pub warm_attach_leading_launch: bool,
    /// True when `driver.warm_attach(app_id)` must run before executing `commands`.
    pub needs_pre_attach: bool,
    /// True whenever warm attach will be used for this flow run.
    pub warm_attach_active: bool,
}

/// Returns true for commands that occupy a numbered top-level step (`Orchestra::push_step`).
pub fn is_numbered_step(cmd: &MaestroCommand) -> bool {
    !matches!(cmd, MaestroCommand::ApplyConfiguration(_) | MaestroCommand::DefineVariables(_))
}

/// Resolves `--start-from <step|label>` and `--attach` against `filtered`
/// (which already has top-level `DefineVariables` removed).
pub fn resolve_start_from<'a>(
    filtered: &'a [MaestroCommand],
    config: Option<&MaestroConfig>,
    start_from: Option<&str>,
    attach: bool,
    flow_label: &str,
) -> Result<ResolvedStart<'a>> {
    let steps: Vec<(usize, usize, &MaestroCommand)> = filtered
        .iter()
        .enumerate()
        .filter(|(_, cmd)| is_numbered_step(cmd))
        .enumerate()
        .map(|(step_zero, (filtered_idx, cmd))| (filtered_idx, step_zero + 1, cmd))
        .collect();

    let has_on_flow_start = config.and_then(|c| c.on_flow_start.as_ref()).is_some_and(|hook| !hook.commands.is_empty());

    let (slice_idx, skipped_steps, target_cmd, start_from_owned) = match start_from {
        None => match steps.first() {
            Some((_, _, first_cmd)) => (0usize, 0usize, Some(*first_cmd), None),
            None => (0usize, 0usize, None, None),
        },
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("--start-from must not be empty");
            }
            if steps.is_empty() {
                bail!("--start-from `{raw}` cannot be used on `{flow_label}` because the flow has no top-level steps");
            }
            let (filtered_idx, step_1_based, cmd) = if trimmed.chars().all(|c| c.is_ascii_digit()) {
                let n = trimmed.parse::<usize>().map_err(|_| {
                    anyhow::anyhow!("Invalid --start-from `{raw}`: step index is out of range for usize")
                })?;
                if n == 0 {
                    bail!("Invalid --start-from `0`: step indices are 1-based (>= 1).");
                }
                let Some(found) = steps.iter().find(|(_, num, _)| *num == n).copied() else {
                    bail!(
                        "--start-from {n} is out of range for `{flow_label}` (flow has {} top-level step(s))",
                        steps.len()
                    );
                };
                found
            } else {
                let matches: Vec<(usize, usize, &MaestroCommand)> =
                    steps.iter().filter(|(_, _, cmd)| cmd.label() == Some(raw)).copied().collect();
                match matches.as_slice() {
                    [] => {
                        let available: Vec<String> = steps
                            .iter()
                            .filter_map(|(_, num, cmd)| cmd.label().map(|l| format!("step {num}: `{l}`")))
                            .collect();
                        if available.is_empty() {
                            bail!(
                                "--start-from label `{raw}` not found in `{flow_label}` (no top-level steps define `label:`)"
                            );
                        }
                        bail!(
                            "--start-from label `{raw}` not found in `{flow_label}` (available top-level labels: {})",
                            available.join(", ")
                        );
                    }
                    [single] => *single,
                    many => {
                        let nums: Vec<String> = many.iter().map(|(_, num, _)| num.to_string()).collect();
                        bail!(
                            "Ambiguous --start-from label `{raw}` in `{flow_label}` matches multiple top-level steps: steps {}",
                            nums.join(", ")
                        );
                    }
                }
            };
            (filtered_idx, step_1_based - 1, Some(cmd), Some(raw.to_string()))
        }
    };

    let target_is_launch = target_cmd.is_some_and(|c| matches!(c, MaestroCommand::LaunchApp(_)));
    let skipped_on_flow_start = skipped_steps > 0 && has_on_flow_start;

    let (warm_attach_leading_launch, needs_pre_attach, warm_attach_active) = if skipped_steps > 0 {
        if target_is_launch {
            if attach { (true, false, true) } else { (false, false, false) }
        } else {
            (false, true, true)
        }
    } else if attach {
        if target_is_launch { (true, has_on_flow_start, true) } else { (false, true, true) }
    } else {
        (false, false, false)
    };

    Ok(ResolvedStart {
        commands: &filtered[slice_idx..],
        skipped_steps,
        start_index_1_based: skipped_steps + 1,
        start_from: start_from_owned,
        skipped_on_flow_start,
        warm_attach_leading_launch,
        needs_pre_attach,
        warm_attach_active,
    })
}

/// Validates `--start-from <step|label>` upfront before device setup.
pub fn validate_start_from_static(flow: &ParsedFlow, start_from: &str) -> Result<()> {
    let filtered: Vec<MaestroCommand> =
        flow.commands.iter().filter(|c| !matches!(c, MaestroCommand::DefineVariables(_))).cloned().collect();
    let label = flow.path.display().to_string();
    resolve_start_from(&filtered, Some(&flow.config), Some(start_from), false, &label).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::flow::Loader;

    use super::*;

    fn parse(src: &str) -> ParsedFlow {
        Loader::new().parse(Path::new("flow.yaml"), src).unwrap()
    }

    #[test]
    fn resolve_by_index_and_label_and_rejects_invalid_inputs() {
        let flow = parse(
            r#"
appId: com.example.app
onFlowStart:
  - back
---
- launchApp
- tapOn:
    text: Login
    label: Tap login
- tapOn:
    text: Next
"#,
        );
        let filtered: Vec<MaestroCommand> =
            flow.commands.iter().filter(|c| !matches!(c, MaestroCommand::DefineVariables(_))).cloned().collect();

        // Numeric index (1-based step 2) skips launchApp and skips onFlowStart, implies attach.
        let r = resolve_start_from(&filtered, Some(&flow.config), Some("2"), false, "flow.yaml").unwrap();
        assert_eq!(r.skipped_steps, 1);
        assert_eq!(r.start_index_1_based, 2);
        assert!(r.skipped_on_flow_start);
        assert!(r.needs_pre_attach);
        assert!(r.warm_attach_active);
        assert!(!r.warm_attach_leading_launch);

        // Label match
        let r2 = resolve_start_from(&filtered, Some(&flow.config), Some("Tap login"), false, "flow.yaml").unwrap();
        assert_eq!(r2.skipped_steps, 1);
        assert_eq!(r2.start_index_1_based, 2);

        // Attach from step 1 keeps onFlowStart pre-attach + replaces leading launchApp.
        let r3 = resolve_start_from(&filtered, Some(&flow.config), None, true, "flow.yaml").unwrap();
        assert_eq!(r3.skipped_steps, 0);
        assert!(r3.warm_attach_leading_launch);
        assert!(r3.needs_pre_attach);
        assert!(r3.warm_attach_active);

        // Out-of-range & zero errors
        assert!(resolve_start_from(&filtered, None, Some("0"), false, "flow.yaml").is_err());
        assert!(resolve_start_from(&filtered, None, Some("4"), false, "flow.yaml").is_err());
        assert!(resolve_start_from(&filtered, None, Some("missing label"), false, "flow.yaml").is_err());
    }
}
