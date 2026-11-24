//! Flow errors with the same titles Maestro reports (`MaestroFlowParser.
//! wrapException`), plus a source frame for the terminal.

use std::fmt;
use std::path::{Path, PathBuf};

use super::yaml::Pos;

/// A problem found while decoding, before it is tied to a file.
#[derive(Clone, Debug)]
pub struct Problem {
    pub title: String,
    pub message: String,
    pub pos: Pos,
}

impl Problem {
    pub fn new(title: impl Into<String>, message: impl Into<String>, pos: Pos) -> Self {
        Problem { title: title.into(), message: message.into(), pos }
    }

    /// Jackson `UnrecognizedPropertyException`.
    pub fn unknown_property(name: &str, pos: Pos) -> Self {
        Self::new(format!("Unknown Property: {name}"), format!("The property `{name}` is not recognized"), pos)
    }

    /// Kotlin `MissingKotlinParameterException` (Maestro titles it like a
    /// config field even inside commands).
    pub fn missing(name: &str, pos: Pos) -> Self {
        Self::new(
            format!("Config Field Required: {name}"),
            format!("The config section is missing a required field: `{name}`."),
            pos,
        )
    }

    /// Jackson `MismatchedInputException` (wrong YAML shape or type).
    pub fn format(field: &str, pos: Pos) -> Self {
        Self::new(format!("Incorrect Format: {field}"), format!("The format for {field} is incorrect"), pos)
    }

    /// Anything else (Maestro's catch-all "Parsing Failed").
    pub fn failed(message: impl Into<String>, pos: Pos) -> Self {
        Self::new("Parsing Failed", message, pos)
    }
}

/// A flow file that Maestro would refuse.
#[derive(Clone, Debug)]
pub struct FlowError {
    pub title: String,
    pub message: String,
    pub path: PathBuf,
    pub pos: Pos,
}

impl FlowError {
    pub fn at(path: &Path, p: Problem) -> Self {
        FlowError { title: p.title, message: p.message, path: path.to_path_buf(), pos: p.pos }
    }

    /// `path:line:col: title`, the offending line with a caret, then the message.
    pub fn render(&self) -> String {
        let mut out = format!("{}:{}: {}\n", self.path.display(), self.pos, self.title);
        if let Ok(src) = std::fs::read_to_string(&self.path)
            && let Some(line) = src.lines().nth(self.pos.line.saturating_sub(1))
        {
            let n = self.pos.line.to_string();
            out +=
                &format!("{n} | {line}\n{} | {}^\n", " ".repeat(n.len()), " ".repeat(self.pos.col.saturating_sub(1)));
        }
        if !self.message.is_empty() {
            out += &self.message;
            out.push('\n');
        }
        out
    }
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}:{}\n{}", self.title, self.path.display(), self.pos, self.message)
    }
}

impl std::error::Error for FlowError {}
