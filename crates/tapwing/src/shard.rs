//! Multi-device sharding validation, plan partitioning, and per-shard output
//! directory resolution matching Maestro 2.12.1 (`TestCommand.kt`,
//! `Sharder.kt`).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::flow::ParsedFlow;
use crate::workspace::ExecutionPlan;

/// How flows are distributed across connected devices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShardMode {
    /// Single-device execution (no sharding flags passed).
    #[default]
    None,
    /// `--shard-split <N>` (or deprecated `--shards <N>`): round-robin across up to `N` shards.
    Split(usize),
    /// `--shard-all <N>`: run the entire flow list redundantly on `N` shards.
    All(usize),
}

impl ShardMode {
    /// Raw shard count requested on the command line (`1` when [`ShardMode::None`]).
    pub fn requested_shards(self) -> usize {
        match self {
            Self::None => 1,
            Self::Split(n) | Self::All(n) => n,
        }
    }
}

/// Validated sharding configuration computed before any device is touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardRequest {
    pub mode: ShardMode,
    pub requested_shards: usize,
    pub effective_shards: usize,
    pub requested_devices: Vec<String>,
    pub warnings: Vec<String>,
}

/// Execution slice assigned to a single shard worker.
#[derive(Clone, Debug)]
pub struct ShardPlan {
    /// `0..effective_shards` when `effective_shards > 1`, or `None` for single-shard runs.
    pub shard_index: Option<usize>,
    /// Leading sequential flows (`executionOrder.flowsOrder`), non-empty only on single-shard runs.
    pub sequence: Vec<ParsedFlow>,
    /// Flows assigned to this shard.
    pub flows: Vec<ParsedFlow>,
    /// Whether sequential flow execution continues after a failure.
    pub continue_on_failure: bool,
}

impl ShardPlan {
    /// Console log prefix for this shard (`""` when single-shard, `"[shard N] "` when sharded).
    pub fn log_prefix(&self) -> String {
        match self.shard_index {
            Some(i) => format!("[shard {}] ", i + 1),
            None => String::new(),
        }
    }
}

/// Splits a `--device` / `--udid` argument on commas, trimming whitespace and
/// dropping empty segments.
pub fn parse_device_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Validates `--shard-split`, `--shard-all`, `--shards`, `--device`, `--port`,
/// and sequential execution compatibility before touching any device.
pub fn resolve_shard_request(
    shard_split: Option<usize>,
    shard_all: Option<usize>,
    shards: Option<usize>,
    device: Option<&str>,
    port: u16,
    sequence_len: usize,
    flow_count: usize,
) -> Result<ShardRequest> {
    if shard_split.is_some() && shard_all.is_some() {
        bail!("Options --shard-split and --shard-all are mutually exclusive.");
    }
    if shards.is_some() && shard_all.is_some() {
        bail!("Options --shards and --shard-all are mutually exclusive.");
    }
    if shards.is_some() && shard_split.is_some() {
        bail!("Options --shards and --shard-split cannot be used together (use --shard-split).");
    }

    let mut warnings = Vec::new();
    let mode = if let Some(n) = shard_split {
        if n == 0 {
            bail!("Shard count must be at least 1.");
        }
        ShardMode::Split(n)
    } else if let Some(n) = shard_all {
        if n == 0 {
            bail!("Shard count must be at least 1.");
        }
        ShardMode::All(n)
    } else if let Some(n) = shards {
        if n == 0 {
            bail!("Shard count must be at least 1.");
        }
        warnings.push("warning: --shards is deprecated; use --shard-split or --shard-all instead".to_string());
        ShardMode::Split(n)
    } else {
        ShardMode::None
    };

    let requested_shards = mode.requested_shards();
    if requested_shards > 1 && sequence_len > 0 {
        bail!("Cannot run sharded tests with sequential execution");
    }

    let requested_devices = parse_device_list(device);
    if requested_devices.len() > 1 && mode == ShardMode::None {
        bail!("Multiple --device values require --shard-split or --shard-all.");
    }

    let effective_shards = match mode {
        ShardMode::None => 1,
        ShardMode::Split(n) => {
            let eff = n.min(flow_count).max(1);
            if n > flow_count {
                warnings.push(format!(
                    "warning: Requested {n} shards, but it cannot be higher than the number of flows ({flow_count}). \
                     Will use {eff} shards instead."
                ));
            }
            eff
        }
        ShardMode::All(n) => n,
    };

    if effective_shards > 1 && port != 0 {
        bail!(
            "--port cannot be fixed when running multiple shards (omit --port or pass --port 0 for per-shard \
             ephemeral ports)."
        );
    }

    Ok(ShardRequest { mode, requested_shards, effective_shards, requested_devices, warnings })
}

/// Partitions an [`ExecutionPlan`] into `effective_shards` [`ShardPlan`]s.
///
/// - When `effective_shards <= 1`: returns a single [`ShardPlan`] with
///   `shard_index: None`, preserving both `plan.sequence` and `plan.flows`.
/// - `ShardMode::All(n)` (`n > 1`): returns `n` copies of `plan.flows`, each
///   with `shard_index: Some(i)`.
/// - `ShardMode::Split(_)` (`effective_shards > 1`): distributes `plan.flows`
///   round-robin (`i % effective_shards`) matching Maestro's `chunked`/stride
///   assignment (`[F0, F1, F2, F3, F4]` across 2 shards -> `[F0, F2, F4]` and
///   `[F1, F3]`).
pub fn partition_plan(plan: ExecutionPlan, mode: ShardMode, effective_shards: usize) -> Vec<ShardPlan> {
    if effective_shards <= 1 {
        return vec![ShardPlan {
            shard_index: None,
            sequence: plan.sequence,
            flows: plan.flows,
            continue_on_failure: plan.continue_on_failure,
        }];
    }

    match mode {
        ShardMode::All(_) => (0..effective_shards)
            .map(|i| ShardPlan {
                shard_index: Some(i),
                sequence: Vec::new(),
                flows: plan.flows.clone(),
                continue_on_failure: plan.continue_on_failure,
            })
            .collect(),
        ShardMode::Split(_) | ShardMode::None => {
            let mut buckets: Vec<Vec<ParsedFlow>> = (0..effective_shards).map(|_| Vec::new()).collect();
            for (idx, flow) in plan.flows.into_iter().enumerate() {
                let target = idx % effective_shards;
                if let Some(bucket) = buckets.get_mut(target) {
                    bucket.push(flow);
                }
            }
            buckets
                .into_iter()
                .enumerate()
                .map(|(i, flows)| ShardPlan {
                    shard_index: Some(i),
                    sequence: Vec::new(),
                    flows,
                    continue_on_failure: plan.continue_on_failure,
                })
                .collect()
        }
    }
}

/// Generates the default timestamped run folder name (`run-<epoch_ms>-<pid>`).
pub fn default_run_stamp() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("run-{ms}-{}", std::process::id())
}

/// Pure resolution of `(base_output_dir, per_shard_output_dirs)` with explicit
/// `run_stamp` (`run-<epoch_ms>-<pid>`).
///
/// Precedence for `base_output_dir`:
/// 1. `--test-output-dir`
/// 2. `config.testOutputDir`
/// 3. `--debug-output` (directly if `flatten_debug_output`, else `<dir>/<run_stamp>`)
/// 4. default `.tapwing` (directly if `flatten_debug_output`, else `.tapwing/<run_stamp>`)
///
/// Per shard `i`: `base_output_dir` when `effective_shards <= 1 || flatten_debug_output`,
/// else `base_output_dir.join(format!("shard-{}", i + 1))`.
pub fn resolve_output_dirs_with_stamp(
    cli_test_output_dir: Option<&Path>,
    config_test_output_dir: Option<&str>,
    debug_output: Option<&Path>,
    flatten_debug_output: bool,
    effective_shards: usize,
    run_stamp: &str,
) -> (PathBuf, Vec<PathBuf>) {
    let base = if let Some(dir) = cli_test_output_dir {
        dir.to_path_buf()
    } else if let Some(cfg_dir) = config_test_output_dir {
        PathBuf::from(cfg_dir)
    } else if let Some(dbg_dir) = debug_output {
        if flatten_debug_output { dbg_dir.to_path_buf() } else { dbg_dir.join(run_stamp) }
    } else if flatten_debug_output {
        PathBuf::from(".tapwing")
    } else {
        PathBuf::from(".tapwing").join(run_stamp)
    };

    let count = effective_shards.max(1);
    let per_shard = (0..count)
        .map(|i| if count <= 1 || flatten_debug_output { base.clone() } else { base.join(format!("shard-{}", i + 1)) })
        .collect();
    (base, per_shard)
}

/// Resolves `(base_output_dir, per_shard_output_dirs)` using [`default_run_stamp`].
pub fn resolve_output_dirs(
    cli_test_output_dir: Option<&Path>,
    config_test_output_dir: Option<&str>,
    debug_output: Option<&Path>,
    flatten_debug_output: bool,
    effective_shards: usize,
) -> (PathBuf, Vec<PathBuf>) {
    let stamp = default_run_stamp();
    resolve_output_dirs_with_stamp(
        cli_test_output_dir,
        config_test_output_dir,
        debug_output,
        flatten_debug_output,
        effective_shards,
        &stamp,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::ir::MaestroConfig;
    use crate::workspace::config::WorkspaceConfig;

    fn dummy_flow(name: &str) -> ParsedFlow {
        ParsedFlow {
            path: PathBuf::from(format!("/ws/{name}.yaml")),
            config: MaestroConfig {
                app_id: Some("com.example".into()),
                name: Some(name.into()),
                tags: None,
                ext: Default::default(),
                on_flow_start: None,
                on_flow_complete: None,
                properties: Default::default(),
            },
            commands: Vec::new(),
        }
    }

    #[test]
    fn mutual_exclusivity_and_zero_shard_validation() {
        let err = resolve_shard_request(Some(2), Some(2), None, None, 0, 0, 4).unwrap_err().to_string();
        assert_eq!(err, "Options --shard-split and --shard-all are mutually exclusive.");

        let err = resolve_shard_request(None, Some(2), Some(2), None, 0, 0, 4).unwrap_err().to_string();
        assert_eq!(err, "Options --shards and --shard-all are mutually exclusive.");

        let err = resolve_shard_request(Some(2), None, Some(2), None, 0, 0, 4).unwrap_err().to_string();
        assert_eq!(err, "Options --shards and --shard-split cannot be used together (use --shard-split).");

        for (split, all, dep) in [(Some(0), None, None), (None, Some(0), None), (None, None, Some(0))] {
            let err = resolve_shard_request(split, all, dep, None, 0, 0, 4).unwrap_err().to_string();
            assert_eq!(err, "Shard count must be at least 1.");
        }
    }

    #[test]
    fn deprecated_shards_emits_warning_and_acts_as_split() {
        let req = resolve_shard_request(None, None, Some(2), None, 0, 0, 4).unwrap();
        assert_eq!(req.mode, ShardMode::Split(2));
        assert_eq!(req.requested_shards, 2);
        assert_eq!(req.effective_shards, 2);
        assert_eq!(req.warnings, vec!["warning: --shards is deprecated; use --shard-split or --shard-all instead"]);
    }

    #[test]
    fn rejects_sharding_with_sequential_execution_multiple_devices_without_shard_and_fixed_port() {
        let err = resolve_shard_request(Some(2), None, None, None, 0, 1, 4).unwrap_err().to_string();
        assert_eq!(err, "Cannot run sharded tests with sequential execution");

        // Single shard with sequential execution is allowed.
        assert!(resolve_shard_request(Some(1), None, None, None, 0, 2, 4).is_ok());

        let err = resolve_shard_request(None, None, None, Some("emu-1, emu-2"), 0, 0, 4).unwrap_err().to_string();
        assert_eq!(err, "Multiple --device values require --shard-split or --shard-all.");

        let err = resolve_shard_request(Some(2), None, None, None, 7781, 0, 4).unwrap_err().to_string();
        assert_eq!(
            err,
            "--port cannot be fixed when running multiple shards (omit --port or pass --port 0 for per-shard ephemeral ports)."
        );
    }

    #[test]
    fn clamps_shard_split_to_flow_count_with_warning_but_keeps_shard_all() {
        let req = resolve_shard_request(Some(5), None, None, Some("d1,d2,d3"), 0, 0, 2).unwrap();
        assert_eq!(req.requested_shards, 5);
        assert_eq!(req.effective_shards, 2);
        assert_eq!(
            req.warnings,
            vec![
                "warning: Requested 5 shards, but it cannot be higher than the number of flows (2). Will use 2 shards instead."
            ]
        );

        let all_req = resolve_shard_request(None, Some(3), None, None, 0, 0, 1).unwrap();
        assert_eq!(all_req.requested_shards, 3);
        assert_eq!(all_req.effective_shards, 3);
        assert!(all_req.warnings.is_empty());
    }

    #[test]
    fn partition_plan_handles_single_split_round_robin_and_all() {
        let flows: Vec<ParsedFlow> = ["F0", "F1", "F2", "F3", "F4"].iter().map(|n| dummy_flow(n)).collect();
        let plan = ExecutionPlan {
            sequence: Vec::new(),
            continue_on_failure: false,
            flows: flows.clone(),
            config: WorkspaceConfig::default(),
        };

        let split = partition_plan(plan, ShardMode::Split(2), 2);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].shard_index, Some(0));
        assert_eq!(split[0].log_prefix(), "[shard 1] ");
        let names0: Vec<&str> = split[0].flows.iter().filter_map(|f| f.config.name.as_deref()).collect();
        assert_eq!(names0, vec!["F0", "F2", "F4"]);

        assert_eq!(split[1].shard_index, Some(1));
        assert_eq!(split[1].log_prefix(), "[shard 2] ");
        let names1: Vec<&str> = split[1].flows.iter().filter_map(|f| f.config.name.as_deref()).collect();
        assert_eq!(names1, vec!["F1", "F3"]);

        let plan_all = ExecutionPlan {
            sequence: Vec::new(),
            continue_on_failure: true,
            flows: vec![dummy_flow("A"), dummy_flow("B")],
            config: WorkspaceConfig::default(),
        };
        let all = partition_plan(plan_all, ShardMode::All(3), 3);
        assert_eq!(all.len(), 3);
        for (i, s) in all.iter().enumerate() {
            assert_eq!(s.shard_index, Some(i));
            assert_eq!(s.flows.len(), 2);
        }
    }

    #[test]
    fn resolve_output_dirs_follows_precedence_and_flatten_rules() {
        // 1. Default multi-shard creates .tapwing/<stamp>/shard-1, shard-2
        let (base, shards) = resolve_output_dirs_with_stamp(None, None, None, false, 2, "run-100-99");
        assert_eq!(base, PathBuf::from(".tapwing/run-100-99"));
        assert_eq!(
            shards,
            vec![PathBuf::from(".tapwing/run-100-99/shard-1"), PathBuf::from(".tapwing/run-100-99/shard-2")]
        );

        // 2. Flatten default -> .tapwing for all shards
        let (base, shards) = resolve_output_dirs_with_stamp(None, None, None, true, 2, "run-100-99");
        assert_eq!(base, PathBuf::from(".tapwing"));
        assert_eq!(shards, vec![PathBuf::from(".tapwing"), PathBuf::from(".tapwing")]);

        // 3. --debug-output with and without flatten
        let (base, shards) = resolve_output_dirs_with_stamp(None, None, Some(Path::new("dbg")), false, 2, "run-100-99");
        assert_eq!(base, PathBuf::from("dbg/run-100-99"));
        assert_eq!(shards, vec![PathBuf::from("dbg/run-100-99/shard-1"), PathBuf::from("dbg/run-100-99/shard-2")]);

        let (base, shards) = resolve_output_dirs_with_stamp(None, None, Some(Path::new("dbg")), true, 2, "run-100-99");
        assert_eq!(base, PathBuf::from("dbg"));
        assert_eq!(shards, vec![PathBuf::from("dbg"), PathBuf::from("dbg")]);

        // 4. config.testOutputDir beats --debug-output; --test-output-dir beats config.testOutputDir
        let (base, shards) =
            resolve_output_dirs_with_stamp(None, Some("cfg-out"), Some(Path::new("dbg")), false, 1, "run-100-99");
        assert_eq!(base, PathBuf::from("cfg-out"));
        assert_eq!(shards, vec![PathBuf::from("cfg-out")]);

        let (base, shards) = resolve_output_dirs_with_stamp(
            Some(Path::new("cli-out")),
            Some("cfg-out"),
            Some(Path::new("dbg")),
            false,
            2,
            "run-100-99",
        );
        assert_eq!(base, PathBuf::from("cli-out"));
        assert_eq!(shards, vec![PathBuf::from("cli-out/shard-1"), PathBuf::from("cli-out/shard-2")]);
    }
}
