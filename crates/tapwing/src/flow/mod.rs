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

use std::cell::Cell;
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
}

/// Sub-flow nesting limit. Maestro has none and overflows its stack on a
/// cycle; we report the cycle instead.
const MAX_DEPTH: usize = 64;

impl Loader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse_file(&self, path: &Path) -> Result<ParsedFlow, FlowError> {
        let path = normalize(path);
        let source = std::fs::read_to_string(&path).map_err(|e| {
            FlowError::at(&path, Problem::new("Failed to read file", e.to_string(), Pos { line: 1, col: 1 }))
        })?;
        if self.depth.get() >= MAX_DEPTH {
            return Err(FlowError::at(
                &path,
                Problem::failed("Sub-flows nest too deep (a runFlow cycle?)", Pos { line: 1, col: 1 }),
            ));
        }
        self.depth.set(self.depth.get() + 1);
        let result = self.parse(&path, &source);
        self.depth.set(self.depth.get() - 1);
        result
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
