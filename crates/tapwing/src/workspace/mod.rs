//! Which flows `tapwing test` runs, following Maestro 2.12.1's
//! `WorkspaceExecutionPlanner.plan`:
//!
//! - a single file runs as is (tags ignored, config only with `--config`);
//! - directories are walked for flow files (`Filters.isFlowFile`), then
//!   filtered by the workspace config's `flows` globs (default `*`: top-level
//!   files only, so sub-flows in sub-folders do not run on their own; `!`
//!   negates), then by `includeTags` / `excludeTags` (CLI and config merged);
//! - `executionOrder.flowsOrder` flows run first, in that order.
//!
//! Every selected flow is parsed before a device is touched, so one broken
//! file fails the whole run up front, as in Maestro. Error messages match
//! Maestro's `ValidationError` texts. Flows within one directory run in
//! path order (Maestro takes the file system's walk order).

pub mod config;
pub mod glob;

use std::fmt;
use std::path::{Path, PathBuf};

use crate::flow::{self, FlowError, ParsedFlow};
use config::WorkspaceConfig;
use glob::Glob;

/// What to run, in order: [`Self::sequence`] first, then [`Self::flows`].
#[derive(Debug)]
pub struct ExecutionPlan {
    /// `executionOrder.flowsOrder` flows, run one after another first.
    pub sequence: Vec<ParsedFlow>,
    /// Keep running the sequence after a failure (Maestro default `true`).
    /// The other flows always run.
    pub continue_on_failure: bool,
    pub flows: Vec<ParsedFlow>,
    pub config: WorkspaceConfig,
}

impl ExecutionPlan {
    /// All flows in run order, and how many of them form the leading sequence.
    pub fn into_ordered(self) -> (Vec<ParsedFlow>, usize) {
        let n = self.sequence.len();
        let mut all = self.sequence;
        all.extend(self.flows);
        (all, n)
    }
}

/// Why no plan could be made.
#[derive(Debug)]
pub enum PlanError {
    /// Maestro's `ValidationError` (message text matches Maestro).
    Validation(String),
    /// A flow or the workspace config does not parse.
    Flow(FlowError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::Validation(m) => f.write_str(m),
            PlanError::Flow(e) => f.write_str(e.render().trim_end()),
        }
    }
}

impl std::error::Error for PlanError {}

impl From<FlowError> for PlanError {
    fn from(e: FlowError) -> Self {
        PlanError::Flow(e)
    }
}

/// `parseFlowName`: the flow's `name:`, else its file name without extension.
pub fn flow_name(flow: &ParsedFlow) -> String {
    flow.config.name.clone().unwrap_or_else(|| file_stem(&flow.path))
}

/// Builds the execution plan for `input` (files and/or directories).
pub fn plan(
    input: &[PathBuf],
    include_tags: &[String],
    exclude_tags: &[String],
    config: Option<&Path>,
) -> Result<ExecutionPlan, PlanError> {
    let loader = flow::Loader::new();
    plan_with_loader(input, include_tags, exclude_tags, config, &loader)
}

pub(crate) fn plan_with_loader(
    input: &[PathBuf],
    include_tags: &[String],
    exclude_tags: &[String],
    config: Option<&Path>,
    loader: &flow::Loader,
) -> Result<ExecutionPlan, PlanError> {
    // `TestCommand` makes every input absolute and de-duplicates it.
    let mut inputs: Vec<PathBuf> = Vec::new();
    for p in input.iter().map(|p| absolute(p)) {
        if !inputs.contains(&p) {
            inputs.push(p);
        }
    }
    let config = config.map(absolute);
    if let Some(missing) = inputs.iter().find(|p| !p.exists()) {
        return Err(PlanError::Validation(format!("Flow path does not exist: {}", missing.display())));
    }
    if let Some(cfg_path) = &config
        && !cfg_path.exists()
    {
        return Err(PlanError::Validation(format!("The config file {} does not exist.", cfg_path.display())));
    }

    if let [single] = inputs.as_slice()
        && single.is_file()
    {
        let flow = loader.parse_file(single)?;
        let config = config.as_deref().map(config::read).transpose()?.unwrap_or_default();
        return Ok(ExecutionPlan { sequence: Vec::new(), continue_on_failure: true, flows: vec![flow], config });
    }

    let (files, dirs): (Vec<PathBuf>, Vec<PathBuf>) = inputs.into_iter().partition(|p| p.is_file());
    // Resolved before filtering, so an auto-discovered config is never a flow.
    let config_path = config.or_else(|| dirs.iter().find_map(|d| find_config_file(d)));
    let config_ref = config_path.as_deref();

    let flow_files: Vec<PathBuf> =
        files.into_iter().filter(|f| is_flow_file_with_loader(f, config_ref, loader)).collect();
    let mut dir_flows: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        let mut found = Vec::new();
        walk(dir, &mut found);
        found.retain(|p| is_flow_file_with_loader(p, config_ref, loader));
        found.sort();
        dir_flows.extend(found);
    }
    if flow_files.is_empty() && dir_flows.is_empty() {
        return Err(PlanError::Validation(format!(
            "Flow directories do not contain any Flow files: {}",
            join_paths(&dirs)
        )));
    }

    let ws = config_ref.map(config::read).transpose()?.unwrap_or_default();
    let globs = ws.flows.clone().unwrap_or_else(|| vec!["*".to_string()]);
    let (positive, negative): (Vec<&String>, Vec<&String>) = globs.iter().partition(|g| !g.starts_with('!'));
    let negative: Vec<&str> = negative.iter().map(|g| &g[1..]).collect();
    if positive.is_empty() && !negative.is_empty() {
        let negated: Vec<String> = negative.iter().map(|g| format!("!{g}")).collect();
        let example: Vec<String> =
            std::iter::once("*".to_string()).chain(negated.iter().cloned()).map(|g| format!("  - \"{g}\"")).collect();
        return Err(PlanError::Validation(format!(
            "Flow inclusion patterns contain only negation patterns, so no Flows would match:\n{}\n\n\
             Add a positive pattern to select the Flows to run. For example, to run all Flows except the negated ones:\n\n\
             flows:\n{}",
            yaml_list(&negated),
            example.join("\n")
        )));
    }
    let compile =
        |g: &str| Glob::new(g).map_err(|e| PlanError::Validation(format!("Invalid Flow inclusion pattern `{g}`: {e}")));
    let positive_globs = positive.iter().map(|g| compile(g)).collect::<Result<Vec<_>, _>>()?;
    let negative_globs = negative.iter().map(|g| compile(g)).collect::<Result<Vec<_>, _>>()?;
    // Maestro matches `glob:<dir>/<pattern>` for every input directory.
    let matches_any = |path: &Path, globs: &[Glob]| {
        dirs.iter().filter_map(|d| relative_slash_path(path, d)).any(|rel| globs.iter().any(|g| g.matches(&rel)))
    };

    let mut selected: Vec<PathBuf> = flow_files;
    for p in dir_flows {
        if matches_any(&p, &positive_globs) && !matches_any(&p, &negative_globs) && !selected.contains(&p) {
            selected.push(p);
        }
    }
    if selected.is_empty() {
        let message = if globs.len() == 1 && globs[0] == "*" {
            format!(
                "Top-level directories do not contain any Flows: {}\n\
                 To configure Maestro to run Flows in subdirectories, check out the following resources:\n  \
                 * https://maestro.mobile.dev/cli/test-suites-and-reports#inclusion-patterns\n  \
                 * https://blog.mobile.dev/maestro-best-practices-structuring-your-test-suite-54ec390c5c82",
                join_paths(&dirs)
            )
        } else {
            let positive: Vec<String> = positive.iter().map(|g| g.to_string()).collect();
            format!("Flow inclusion pattern(s) did not match any Flow files:\n{}", yaml_list(&positive))
        };
        return Err(PlanError::Validation(message));
    }

    let parsed = selected.iter().map(|p| loader.parse_file(p)).collect::<Result<Vec<_>, _>>()?;

    let all_include: Vec<String> =
        include_tags.iter().cloned().chain(ws.include_tags.iter().flatten().cloned()).collect();
    let all_exclude: Vec<String> =
        exclude_tags.iter().cloned().chain(ws.exclude_tags.iter().flatten().cloned()).collect();
    let all: Vec<ParsedFlow> = parsed
        .into_iter()
        .filter(|f| {
            let tags = f.config.tags.as_deref().unwrap_or_default();
            (all_include.is_empty() || tags.iter().any(|t| all_include.contains(t)))
                && (all_exclude.is_empty() || !tags.iter().any(|t| all_exclude.contains(t)))
        })
        .collect();
    if all.is_empty() {
        return Err(PlanError::Validation(format!(
            "Include / Exclude tags did not match any Flows:\n\nInclude Tags:\n{}\n\nExclude Tags:\n{}",
            yaml_list(&all_include),
            yaml_list(&all_exclude)
        )));
    }

    let flows_order = ws.execution_order.as_ref().map(|o| o.flows_order.as_slice()).unwrap_or_default();
    let sequence_idx = flows_in_sequence(&all, flows_order)?;
    let continue_on_failure = ws.execution_order.as_ref().map_or(Some(true), |o| o.continue_on_failure) == Some(true);
    let mut slots: Vec<Option<ParsedFlow>> = all.into_iter().map(Some).collect();
    let sequence: Vec<ParsedFlow> =
        sequence_idx.iter().filter_map(|&i| slots.get_mut(i).and_then(Option::take)).collect();
    let flows: Vec<ParsedFlow> = slots.into_iter().flatten().collect();
    Ok(ExecutionPlan { sequence, continue_on_failure, flows, config: ws })
}

/// `ExecutionOrderPlanner.getFlowsToRunInSequence`: the leading run of
/// `order` names that exist among `flows` (by [`flow_name`], later flows
/// winning a shared name). Names after the first missing one are dropped and
/// those flows run normally; if the very first name is missing while later
/// ones exist, Maestro fails.
fn flows_in_sequence(flows: &[ParsedFlow], order: &[String]) -> Result<Vec<usize>, PlanError> {
    if order.is_empty() {
        return Ok(Vec::new());
    }
    // `associateBy`: insertion-ordered keys, last value wins.
    let mut by_name: Vec<(String, usize)> = Vec::new();
    for (i, f) in flows.iter().enumerate() {
        let name = flow_name(f);
        match by_name.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = i,
            None => by_name.push((name, i)),
        }
    }
    let mut order_set: Vec<&String> = Vec::new();
    for name in order {
        if !order_set.contains(&name) {
            order_set.push(name);
        }
    }
    let names_in_order: Vec<&String> = by_name.iter().map(|(n, _)| n).filter(|n| order_set.contains(n)).collect();
    if names_in_order.is_empty() {
        return Ok(Vec::new());
    }
    let result: Vec<&String> = order_set.iter().copied().take_while(|n| names_in_order.contains(n)).collect();
    if result.is_empty() {
        let missing: Vec<&str> = order_set.iter().filter(|n| !names_in_order.contains(n)).map(|n| n.as_str()).collect();
        return Err(PlanError::Validation(format!(
            "Could not find flows needed for execution in order: {}",
            missing.join(", ")
        )));
    }
    if order.iter().take(result.len()).ne(result.iter().copied()) {
        return Ok(Vec::new());
    }
    Ok(result.iter().filter_map(|n| by_name.iter().find(|(name, _)| name == *n).map(|(_, i)| *i)).collect())
}

/// `Filters.isFlowFile`: a regular `.yaml`/`.yml` file that is not the
/// workspace config, not named `config.*`, and does not look like a workspace
/// config either.
pub fn is_flow_file(path: &Path, config: Option<&Path>) -> bool {
    is_flow_file_inner(path, config, None)
}

/// Like [`is_flow_file`], caching any read source or parsed YAML in `loader`
/// so a subsequent [`flow::Loader::parse_file`] avoids reading or YAML-parsing
/// the file a second time.
pub fn is_flow_file_with_loader(path: &Path, config: Option<&Path>, loader: &flow::Loader) -> bool {
    is_flow_file_inner(path, config, Some(loader))
}

fn is_flow_file_inner(path: &Path, config: Option<&Path>, loader: Option<&flow::Loader>) -> bool {
    if !path.is_file() {
        return false;
    }
    if config.is_some_and(|c| absolute(c) == absolute(path)) {
        return false;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Kotlin `Path.extension` / `nameWithoutExtension` (a dot file counts).
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name.as_str(), ""));
    if !matches!(ext, "yaml" | "yml") || stem == "config" {
        return false;
    }
    if loader.is_some_and(|l| l.has_flow_cached(path)) {
        return true;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return true;
    };
    match config::probe_workspace_config(&text) {
        config::WorkspaceConfigProbe::IsConfig => false,
        config::WorkspaceConfigProbe::NotConfigText => {
            if let Some(loader) = loader {
                loader.preload_text(path, text);
            }
            true
        }
        config::WorkspaceConfigProbe::NotConfigDocs(docs) => {
            if let Some(loader) = loader {
                loader.preload_docs(path, docs);
            }
            true
        }
    }
}

/// `config.yaml`, else `config.yml`, directly inside `dir`.
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    ["config.yaml", "config.yml"].iter().map(|n| dir.join(n)).find(|p| p.exists())
}

/// `Files.walk`: every entry below `dir`, not following symlinked directories.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// `path` relative to `dir` with `/` separators, if it lies inside `dir`.
fn relative_slash_path(path: &Path, dir: &Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(parts.join("/"))
}

fn absolute(p: &Path) -> PathBuf {
    flow::commands::normalize(p)
}

fn file_stem(p: &Path) -> String {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => name,
    }
}

fn join_paths(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}

/// `toYamlListString`: `- a` per line.
fn yaml_list(items: &[String]) -> String {
    items.iter().map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_workspace(files: &[(&str, &str)]) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tapwing-ws-cache-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            let file = dir.join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body).unwrap();
        }
        dir.canonicalize().unwrap()
    }

    #[test]
    fn plan_shares_loader_and_avoids_double_reads_and_parses() {
        let mut files: Vec<(String, String)> = vec![
            ("config.yaml".into(), "flows:\n  - \"*\"\n".into()),
            ("common/login.yaml".into(), "appId: com.example.app\n---\n- tapOn: Sign In\n".into()),
        ];
        let count = 8;
        for i in 0..count {
            files.push((
                format!("flow_{i}.yaml"),
                format!("appId: com.example.app\n---\n- runFlow: common/login.yaml\n- tapOn: Item {i}\n"),
            ));
        }
        let refs: Vec<(&str, &str)> = files.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let dir = temp_workspace(&refs);

        let loader = flow::Loader::new();
        let plan = plan_with_loader(std::slice::from_ref(&dir), &[], &[], None, &loader).unwrap();
        assert_eq!(plan.flows.len(), count);
        assert_eq!(loader.files_parsed(), count + 1, "8 top-level flows + common/login.yaml once");
        assert_eq!(loader.disk_reads(), 0, "all files were already read once during discovery");
    }

    #[test]
    fn discovery_reuses_yaml_docs_when_no_document_separator() {
        let dir = temp_workspace(&[("bad_flow.yaml", "notAConfigKey: 1\n")]);
        let loader = flow::Loader::new();
        let err = plan_with_loader(std::slice::from_ref(&dir), &[], &[], None, &loader).unwrap_err();
        assert!(matches!(err, PlanError::Flow(ref e) if e.title == "Config Field Required"));
        assert_eq!(loader.disk_reads(), 0, "discovered file was not read from disk twice");
        assert_eq!(loader.yaml_loads(), 0, "already-loaded YAML docs were reused without a second yaml::load");
    }
}
