//! Maestro flow files: parsing into the IR Maestro 2.12.1 builds
//! (`YamlCommandReader.readCommands`).
//!
//! A flow file is two YAML documents: the config (`appId`, `env`, hooks,
//! ...) and the command list. Sub-flows (`runFlow: file`, `retry: file`) and
//! scripts are read at parse time, as Maestro does, so a parsed flow is
//! self-contained. `${...}` expressions are left for the runtime.

pub mod commands;
pub mod decode;
pub mod error;
pub mod ir;
pub mod yaml;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use commands::{Ctx, config_string, ext_value, hook, normalize, with_env};
use decode::{Mode, Obj};
pub use error::{FlowError, Problem};
pub use ir::{MaestroCommand, MaestroConfig};
use yaml::{Node, Pos};

/// A parsed flow file.
#[derive(Clone, Debug)]
pub struct ParsedFlow {
    pub path: PathBuf,
    pub config: MaestroConfig,
    /// What Maestro's `readCommands` returns: `ApplyConfiguration` first,
    /// then the env definition (if any), then the commands.
    pub commands: Vec<MaestroCommand>,
}

impl ParsedFlow {
    pub fn app_id(&self) -> &str {
        self.config.app_id.as_deref().unwrap_or_default()
    }
}

/// An error either in the file being parsed or in a file it pulled in.
#[derive(Debug)]
pub enum FlowFailure {
    Here(Problem),
    File(FlowError),
}

impl FlowFailure {
    pub fn here(p: Problem) -> Self {
        FlowFailure::Here(p)
    }

    fn at(self, path: &Path) -> FlowError {
        match self {
            FlowFailure::Here(p) => FlowError::at(path, p),
            FlowFailure::File(e) => e,
        }
    }
}

impl From<FlowError> for FlowFailure {
    fn from(e: FlowError) -> Self {
        FlowFailure::File(e)
    }
}

/// Parses flow files, following sub-flow references.
#[derive(Default)]
pub struct Loader {
    depth: Cell<usize>,
    /// Identity (see [`identity`]) of every file being parsed, outermost first.
    /// A sub-flow already in here is a cycle.
    in_progress: RefCell<Vec<PathBuf>>,
    /// Finished files by normalized path, so a flow included from many places
    /// is read and parsed once.
    parsed: RefCell<HashMap<PathBuf, ParsedFlow>>,
    #[cfg(test)]
    files_parsed: Cell<usize>,
}

/// Sub-flow nesting limit, a backstop behind the cycle check for chains the
/// check cannot see as a cycle. Maestro has no limit and overflows its stack.
const MAX_DEPTH: usize = 64;

impl Loader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse_file(&self, path: &Path) -> Result<ParsedFlow, FlowError> {
        let path = normalize(path);
        if let Some(done) = self.parsed.borrow().get(&path) {
            return Ok(done.clone());
        }
        let source = std::fs::read_to_string(&path).map_err(|e| {
            FlowError::at(&path, Problem::new("Failed to read file", e.to_string(), Pos { line: 1, col: 1 }))
        })?;
        if self.depth.get() >= MAX_DEPTH {
            return Err(FlowError::at(
                &path,
                Problem::failed(format!("Sub-flows nest deeper than {MAX_DEPTH} levels"), Pos { line: 1, col: 1 }),
            ));
        }
        #[cfg(test)]
        self.files_parsed.set(self.files_parsed.get() + 1);
        self.depth.set(self.depth.get() + 1);
        self.in_progress.borrow_mut().push(identity(&path));
        let result = self.parse(&path, &source);
        self.in_progress.borrow_mut().pop();
        self.depth.set(self.depth.get() - 1);
        if let Ok(flow) = &result {
            self.parsed.borrow_mut().insert(path, flow.clone());
        }
        result
    }

    /// Parses the sub-flow `path` referenced at `pos` of the file being parsed.
    ///
    /// A file that is still being parsed further up is a cycle. Sub-flows are
    /// expanded at parse time whatever their `when:` says, so Maestro
    /// overflows its stack on one; with fan-out (two files running each other
    /// twice) the nesting limit alone would be reached only after 2^64 parses.
    pub(crate) fn include(&self, path: &Path, pos: Pos) -> Result<ParsedFlow, FlowFailure> {
        let path = normalize(path);
        if let Some(chain) = self.cycle_through(&identity(&path)) {
            return Err(FlowFailure::here(Problem::new("Sub-flow Cycle", format!("runFlow cycle: {chain}"), pos)));
        }
        Ok(self.parse_file(&path)?)
    }

    /// `a.yaml → b.yaml → a.yaml` if `target` is already being parsed, with
    /// paths relative to the directory of the outermost flow where possible.
    fn cycle_through(&self, target: &Path) -> Option<String> {
        let in_progress = self.in_progress.borrow();
        let start = in_progress.iter().position(|p| p == target)?;
        let base = in_progress.first().and_then(|root| root.parent());
        let name = |p: &Path| base.and_then(|b| p.strip_prefix(b).ok()).unwrap_or(p).display().to_string();
        let mut chain: Vec<String> = in_progress[start..].iter().map(|p| name(p)).collect();
        chain.push(name(target));
        Some(chain.join(" → "))
    }

    /// Parse `source` as the flow at `path` (absolute, normalized).
    pub fn parse(&self, path: &Path, source: &str) -> Result<ParsedFlow, FlowError> {
        self.parse_inner(path, source).map_err(|f| f.at(path))
    }

    fn parse_inner(&self, path: &Path, source: &str) -> Result<ParsedFlow, FlowFailure> {
        let docs = yaml::load(source).map_err(|e| FlowFailure::here(Problem::failed(e.message, e.pos)))?;
        let start = Pos { line: 1, col: 1 };
        let config_node = match docs.first() {
            Some(n @ Node::Map(..)) => n,
            other => {
                return Err(FlowFailure::here(Problem::new(
                    "Config Section Required",
                    "Flow files must start with a config section. Eg:\n\n```yaml\nappId: com.example.app # <-- config section\n---\n- launchApp\n```",
                    other.map_or(start, Node::pos),
                )));
            }
        };
        let header = Header::bind(config_node).map_err(FlowFailure::here)?;
        for (hook, field) in [(header.on_flow_start, "onFlowStart"), (header.on_flow_complete, "onFlowComplete")] {
            if let Some(hook) = hook {
                commands::check_hook_syntax(hook, field).map_err(FlowFailure::here)?;
            }
        }
        let items = match docs.get(1) {
            Some(Node::Seq(items, _)) => items,
            other => {
                return Err(FlowFailure::here(Problem::new(
                    "Commands Section Required",
                    "Flow files must have a list of commands after the config section. Eg:\n\n```yaml\nappId: com.example.app\n---\n- launchApp\n```",
                    other.map_or(config_node.pos(), Node::pos),
                )));
            }
        };
        let app_id = header.app_id.clone();
        let ctx = Ctx { flow_path: path, app_id: &app_id, loader: self };
        for item in items {
            commands::check_syntax(item).map_err(FlowFailure::here)?;
        }
        let mut commands = Vec::new();
        for item in items {
            commands.extend(commands::decode(item, &ctx)?);
        }
        let commands = with_env(commands, &header.env, config_node.pos()).map_err(FlowFailure::here)?;
        let config = header.into_config(&ctx)?;
        let mut all = vec![MaestroCommand::ApplyConfiguration(ir::ApplyConfiguration {
            config: config.clone(),
            label: None,
            optional: false,
        })];
        all.extend(commands);
        Ok(ParsedFlow { path: path.to_path_buf(), config, commands: all })
    }
}

/// What makes two normalized paths the same flow for the cycle check: the
/// file name in its canonical directory, so a directory symlink that leads
/// back into the tree cannot hide a cycle behind ever longer paths. The file
/// itself is not resolved: sub-flow paths are relative to where a link lives,
/// not to its target, so a symlinked file is a flow of its own.
fn identity(path: &Path) -> PathBuf {
    match (path.parent().and_then(|dir| dir.canonicalize().ok()), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

/// `YamlConfig`: the first document of a flow.
struct Header<'a> {
    app_id: String,
    name: Option<String>,
    tags: Option<Vec<String>>,
    env: ir::ValueMap,
    on_flow_start: Option<&'a Node>,
    on_flow_complete: Option<&'a Node>,
    properties: ir::ValueMap,
    ext: ir::ValueMap,
}

const HEADER_FIELDS: &[&str] = &["name", "appId", "url", "tags", "env", "onFlowStart", "onFlowComplete", "properties"];

impl<'a> Header<'a> {
    fn bind(node: &'a Node) -> Result<Self, Problem> {
        let o = Obj::open(node, "config", Mode::Stream)?;
        let tags = match o.entries().iter().find(|(k, _, _)| *k == "tags") {
            None => Some(Vec::new()),
            Some((_, v, _)) if v.is_null() => None,
            Some(_) => o.str_list("tags")?,
        };
        let app_id = match (o.string("url")?, o.string("appId")?) {
            (Some(url), _) => url,
            (None, Some(id)) => id,
            (None, None) => {
                return Err(Problem::new(
                    "Config Field Required",
                    "Either 'url' or 'appId' must be specified in the config section.",
                    node.pos(),
                ));
            }
        };
        let mut ext = ir::ValueMap::new();
        for (key, value, _) in o.entries() {
            if !HEADER_FIELDS.contains(key) {
                ext.insert(key.to_string(), ext_value(value));
            }
        }
        let name = match o.get("name") {
            Some(n) => Some(config_string(n, "name")?),
            None => None,
        };
        Ok(Header {
            app_id,
            name,
            tags,
            env: o.str_map("env")?.unwrap_or_default(),
            on_flow_start: o.get("onFlowStart"),
            on_flow_complete: o.get("onFlowComplete"),
            properties: o.str_map("properties")?.unwrap_or_default(),
            ext,
        })
    }

    /// `YamlConfig.toCommand`: hooks are expanded like any command list.
    fn into_config(self, ctx: &Ctx) -> Result<MaestroConfig, FlowFailure> {
        Ok(MaestroConfig {
            app_id: Some(self.app_id),
            name: self.name,
            tags: self.tags,
            ext: self.ext,
            on_flow_start: self.on_flow_start.map(|n| hook(n, "onFlowStart", ctx)).transpose()?,
            on_flow_complete: self.on_flow_complete.map(|n| hook(n, "onFlowComplete", ctx)).transpose()?,
            properties: self.properties,
        })
    }
}

/// Parse one flow file.
pub fn parse_file(path: &Path) -> Result<ParsedFlow, FlowError> {
    Loader::new().parse_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fresh directory holding `files` (name, commands section).
    fn flows(files: &[(&str, &str)]) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tapwing-flow-loader-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, commands) in files {
            let file = dir.join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, format!("appId: com.example.app\n---\n{commands}")).unwrap();
        }
        dir.canonicalize().unwrap()
    }

    #[test]
    fn a_flow_running_itself_keeps_maestros_error() {
        let dir = flows(&[("a.yaml", "- runFlow: ./a.yaml\n")]);
        let err = parse_file(&dir.join("a.yaml")).unwrap_err();
        assert_eq!(err.title, "Invalid File Path");
        assert!(err.message.starts_with("Referenced Flow file can't be the same as the main Flow file"));
    }

    #[test]
    fn a_two_file_cycle_with_fan_out_is_reported_at_once_with_its_chain() {
        // Each file runs the other twice (once behind `when:`, once via `retry`
        // and a non-normalized path): without the in-progress set this is
        // 2^64 parses before the nesting limit.
        let dir = flows(&[
            ("a.yaml", "- runFlow: b.yaml\n- runFlow:\n    file: sub/../b.yaml\n    when:\n      true: ${false}\n"),
            ("b.yaml", "- retry:\n    file: ./a.yaml\n- runFlow: a.yaml\n"),
            ("sub/.keep", ""),
        ]);
        let loader = Loader::new();
        let err = loader.parse_file(&dir.join("a.yaml")).unwrap_err();
        assert_eq!(err.title, "Sub-flow Cycle");
        assert_eq!(err.message, "runFlow cycle: a.yaml → b.yaml → a.yaml");
        assert_eq!((err.path.as_path(), err.pos.line), (dir.join("b.yaml").as_path(), 4), "points at the retry");
        assert_eq!(loader.files_parsed.get(), 2);
    }

    #[test]
    fn a_diamond_passes_and_parses_the_shared_flow_once() {
        let dir = flows(&[
            ("root.yaml", "- runFlow: left.yaml\n- runFlow: right.yaml\n- runFlow: left.yaml\n"),
            ("left.yaml", "- runFlow: shared.yaml\n- runFlow: ./sub/../shared.yaml\n"),
            ("right.yaml", "- runFlow:\n    file: shared.yaml\n    env:\n      WHO: right\n"),
            ("shared.yaml", "- launchApp\n"),
            ("sub/.keep", ""),
        ]);
        let loader = Loader::new();
        let cached = loader.parse_file(&dir.join("root.yaml")).unwrap();
        assert_eq!(loader.files_parsed.get(), 4, "root, left, right and shared, each once");

        // The cache must not change what a flow parses to.
        let fresh_shared = parse_file(&dir.join("shared.yaml")).unwrap();
        let uncached = parse_file(&dir.join("root.yaml")).unwrap();
        assert_eq!(format!("{:?}", cached.commands), format!("{:?}", uncached.commands));
        assert!(format!("{:?}", cached.commands).contains(&format!("{:?}", fresh_shared.commands[1])));
    }

    #[cfg(unix)]
    #[test]
    fn a_cycle_through_a_directory_symlink_is_reported() {
        // `loop` points back at the flow directory, so every level has a new,
        // longer path to the same file.
        let dir = flows(&[("a.yaml", "- runFlow: loop/a.yaml\n- runFlow: loop/a.yaml\n")]);
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();
        let loader = Loader::new();
        let err = loader.parse_file(&dir.join("a.yaml")).unwrap_err();
        assert_eq!(err.title, "Sub-flow Cycle");
        assert_eq!(err.message, "runFlow cycle: a.yaml → a.yaml");
        assert_eq!(loader.files_parsed.get(), 1, "the alias is never read");
    }

    #[test]
    fn the_nesting_limit_still_stops_a_chain_without_a_cycle() {
        let names: Vec<String> = (0..=MAX_DEPTH).map(|i| format!("f{i}.yaml")).collect();
        let bodies: Vec<String> = (0..=MAX_DEPTH)
            .map(|i| if i == MAX_DEPTH { "- launchApp\n".to_string() } else { format!("- runFlow: f{}.yaml\n", i + 1) })
            .collect();
        let files: Vec<(&str, &str)> =
            names.iter().map(String::as_str).zip(bodies.iter().map(String::as_str)).collect();
        let dir = flows(&files);
        // The parser recurses per level; unoptimized frames at the limit need
        // more than the 2 MiB of a test thread (the CLI parses on the main thread).
        let deep = std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(move || {
            let err = parse_file(&dir.join("f0.yaml")).unwrap_err();
            assert_eq!(err.message, "Sub-flows nest deeper than 64 levels");
            // One level less is a valid flow.
            assert!(parse_file(&dir.join("f1.yaml")).is_ok());
        });
        deep.unwrap().join().unwrap();
    }
}
