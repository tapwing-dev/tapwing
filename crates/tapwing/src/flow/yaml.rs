//! YAML documents as a node tree with source positions, with scalars typed
//! the way Maestro's parser (Jackson YAML 2.17 on SnakeYAML) types them.
//!
//! Typing rules that matter for compatibility (all verified against
//! Maestro 2.12.1 with `compat/harness`):
//! - plain `yes/no/on/off/true/false` in any of the YAML 1.1 spellings are
//!   booleans; quoted scalars are always strings;
//! - a scalar bound to a string field keeps its source text (`010`, `1.50`,
//!   `0x10` stay as written); only tree-based decoders see canonical values;
//! - an alias (`*name`) reads as the string `name`. Jackson does not resolve
//!   aliases in this mode, so neither do we.

use std::fmt;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// 1-based line and column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

impl From<Marker> for Pos {
    fn from(m: Marker) -> Self {
        Pos { line: m.line(), col: m.col() + 1 }
    }
}

#[derive(Clone, Debug)]
pub enum Node {
    Scalar(Scalar),
    Seq(Vec<Node>, Pos),
    Map(Vec<(Node, Node)>, Pos),
}

#[derive(Clone, Debug)]
pub struct Scalar {
    /// Source text (for quoted scalars: the unescaped content).
    pub text: String,
    /// Plain (unquoted, untagged) scalars are typed; everything else is a string.
    pub plain: bool,
    pub pos: Pos,
}

/// A scalar's type as Jackson's YAML parser reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum Typed {
    Null,
    Bool(bool),
    Int(i64),
    /// Integer outside i64 (Jackson: `BigInteger`), as decimal digits.
    BigInt(String),
    Float(f64),
    Str(String),
}

impl Node {
    pub fn pos(&self) -> Pos {
        match self {
            Node::Scalar(s) => s.pos,
            Node::Seq(_, p) | Node::Map(_, p) => *p,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Node::Scalar(s) if s.typed() == Typed::Null)
    }

    pub fn as_scalar(&self) -> Option<&Scalar> {
        match self {
            Node::Scalar(s) => Some(s),
            _ => None,
        }
    }

    /// Map entries with later duplicates winning (Jackson binding semantics),
    /// in first-occurrence order.
    pub fn map_entries(&self) -> Option<Vec<(&str, &Node, Pos)>> {
        let Node::Map(entries, _) = self else { return None };
        let mut out: Vec<(&str, &Node, Pos)> = Vec::new();
        for (k, v) in entries {
            let key = match k {
                Node::Scalar(s) => s.text.as_str(),
                // Complex keys do not occur in flows; Jackson would reject them.
                _ => "",
            };
            if let Some(slot) = out.iter_mut().find(|(name, _, _)| *name == key) {
                slot.1 = v;
            } else {
                out.push((key, v, k.pos()));
            }
        }
        Some(out)
    }
}

impl Scalar {
    pub fn typed(&self) -> Typed {
        if !self.plain {
            return Typed::Str(self.text.clone());
        }
        type_plain(&self.text)
    }
}

fn type_plain(t: &str) -> Typed {
    match t {
        "" | "~" | "null" | "Null" | "NULL" => return Typed::Null,
        "true" | "True" | "TRUE" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => return Typed::Bool(true),
        "false" | "False" | "FALSE" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => return Typed::Bool(false),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return Typed::Float(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => return Typed::Float(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Typed::Float(f64::NAN),
        _ => {}
    }
    if let Some(i) = parse_int(t) {
        return i;
    }
    if is_float(t)
        && let Ok(f) = t.replace('_', "").parse::<f64>()
    {
        return Typed::Float(f);
    }
    Typed::Str(t.to_string())
}

fn parse_int(t: &str) -> Option<Typed> {
    let (neg, body) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() {
        return None;
    }
    let (radix, digits) = if let Some(h) = body.strip_prefix("0x") {
        (16, h)
    } else if let Some(b) = body.strip_prefix("0b") {
        (2, b)
    } else if body.len() > 1 && body.starts_with('0') {
        (8, &body[1..])
    } else {
        (10, body)
    };
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if clean.is_empty() || !clean.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    if radix == 10 && !digits.chars().next()?.is_ascii_digit() {
        return None;
    }
    let signed = if neg { format!("-{clean}") } else { clean.clone() };
    match i64::from_str_radix(&signed, radix) {
        Ok(v) => Some(Typed::Int(v)),
        Err(_) if radix == 10 => Some(Typed::BigInt(signed)),
        Err(_) => None,
    }
}

fn is_float(t: &str) -> bool {
    // [-+]?(\.[0-9]+|[0-9][0-9_]*(\.[0-9_]*)?)([eE][-+]?[0-9]+)?
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let start = i;
    let mut digits = 0;
    while i < b.len() && (b[i].is_ascii_digit() || (b[i] == b'_' && i > start)) {
        digits += usize::from(b[i] != b'_');
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            digits += usize::from(b[i] != b'_');
            i += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let exp_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return false;
        }
    }
    i == b.len()
}

/// Parse error from the YAML scanner.
#[derive(Debug)]
pub struct SyntaxError {
    pub message: String,
    pub pos: Pos,
}

/// All documents of a YAML stream.
pub fn load(source: &str) -> Result<Vec<Node>, SyntaxError> {
    let mut builder = Builder { source, chars: None, stack: Vec::new(), docs: Vec::new() };
    let mut parser = Parser::new_from_str(source);
    parser
        .load(&mut builder, true)
        .map_err(|e| SyntaxError { message: e.info().to_string(), pos: (*e.marker()).into() })?;
    Ok(builder.docs)
}

enum Open {
    Seq(Vec<Node>, Pos),
    Map(Vec<(Node, Node)>, Option<Node>, Pos),
}

struct Builder<'a> {
    source: &'a str,
    /// Populated lazily on the first `Event::Alias`, because `yaml-rust2` reports
    /// char indices rather than byte offsets and most flows contain no aliases.
    chars: Option<Vec<char>>,
    stack: Vec<Open>,
    docs: Vec<Node>,
}

impl Builder<'_> {
    fn push(&mut self, node: Node) {
        match self.stack.last_mut() {
            None => self.docs.push(node),
            Some(Open::Seq(items, _)) => items.push(node),
            Some(Open::Map(entries, key, _)) => match key.take() {
                None => *key = Some(node),
                Some(k) => entries.push((k, node)),
            },
        }
    }

    /// The anchor name of `*name` at `mark` (yaml-rust2 only reports an id).
    fn alias_name(&mut self, mark: Marker) -> String {
        let chars = self.chars.get_or_insert_with(|| self.source.chars().collect());
        let tail = chars.get(mark.index()..).unwrap_or_default();
        let name = tail.strip_prefix(&['*']).unwrap_or(tail);
        name.iter().copied().take_while(|c| !c.is_whitespace() && !matches!(c, ',' | ']' | '}')).collect()
    }
}

impl MarkedEventReceiver for Builder<'_> {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        match ev {
            Event::Scalar(text, style, _, tag) => {
                let plain = style == TScalarStyle::Plain && tag.is_none();
                self.push(Node::Scalar(Scalar { text, plain, pos: mark.into() }));
            }
            Event::Alias(_) => {
                let name = self.alias_name(mark);
                self.push(Node::Scalar(Scalar { text: name, plain: false, pos: mark.into() }));
            }
            Event::SequenceStart(..) => self.stack.push(Open::Seq(Vec::new(), mark.into())),
            Event::MappingStart(..) => self.stack.push(Open::Map(Vec::new(), None, mark.into())),
            Event::SequenceEnd | Event::MappingEnd => {
                let node = match self.stack.pop() {
                    Some(Open::Seq(items, pos)) => Node::Seq(items, pos),
                    Some(Open::Map(entries, _, pos)) => Node::Map(entries, pos),
                    None => return,
                };
                self.push(node);
            }
            _ => {}
        }
    }
}

/// Java's `Double.toString` (what Maestro prints for doubles): shortest
/// round-trip digits, `E` notation outside [1e-3, 1e7).
pub fn java_double(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let abs = v.abs();
    if (1e-3..1e7).contains(&abs) {
        let s = format!("{v}");
        return if s.contains('.') { s } else { format!("{s}.0") };
    }
    // Scientific: d.dddE±x with at least one fractional digit.
    let s = format!("{v:e}"); // e.g. "1e7", "1.5e-4"
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let mantissa = if mantissa.contains('.') { mantissa.to_string() } else { format!("{mantissa}.0") };
    format!("{mantissa}E{exp}")
}

/// Canonical text of a scalar as Jackson's tree model prints it (`asText()`).
pub fn canonical_text(s: &Scalar) -> Option<String> {
    Some(match s.typed() {
        Typed::Null => return None,
        Typed::Bool(b) => b.to_string(),
        Typed::Int(i) => i.to_string(),
        Typed::BigInt(d) => d,
        Typed::Float(f) => java_double(f),
        Typed::Str(t) => t,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_scalar_typing_matches_jackson() {
        assert_eq!(type_plain("yes"), Typed::Bool(true));
        assert_eq!(type_plain("Off"), Typed::Bool(false));
        assert_eq!(type_plain("~"), Typed::Null);
        assert_eq!(type_plain("010"), Typed::Int(8));
        assert_eq!(type_plain("0x10"), Typed::Int(16));
        assert_eq!(type_plain("1_000"), Typed::Int(1000));
        assert_eq!(type_plain("1.50"), Typed::Float(1.5));
        assert_eq!(type_plain("1e3"), Typed::Float(1000.0));
        assert_eq!(type_plain("12345678901234567890"), Typed::BigInt("12345678901234567890".into()));
        assert_eq!(type_plain("50%"), Typed::Str("50%".into()));
        assert_eq!(type_plain("1.2.3"), Typed::Str("1.2.3".into()));
    }

    #[test]
    fn java_double_formatting() {
        assert_eq!(java_double(0.0), "0.0");
        assert_eq!(java_double(43.723), "43.723");
        assert_eq!(java_double(1e7), "1.0E7");
        assert_eq!(java_double(1e-4), "1.0E-4");
        assert_eq!(java_double(7900.0), "7900.0");
        assert_eq!(java_double(1.5e-4), "1.5E-4");
    }

    #[test]
    fn alias_reads_as_anchor_name() {
        let docs = load("- tapOn: &t foo\n- tapOn: *t\n").unwrap();
        let Node::Seq(items, _) = &docs[0] else { panic!() };
        let entries = items[1].map_entries().unwrap();
        assert_eq!(entries[0].1.as_scalar().unwrap().text, "t");
    }

    #[test]
    fn alias_after_multibyte_utf8_and_delimiters_resolves_char_index() {
        let src = "# café 日本語 🚀\n- tapOn: &anchor_one \"héllo 🎉\"\n- tapOn: &other bar\n- tapOn: [*anchor_one, *other]\n- tapOn: {id: *anchor_one}\n";
        let docs = load(src).unwrap();
        let Node::Seq(items, _) = &docs[0] else { panic!() };
        let seq_entries = items[2].map_entries().unwrap();
        let Node::Seq(seq, _) = seq_entries[0].1 else { panic!() };
        assert_eq!(seq[0].as_scalar().unwrap().text, "anchor_one");
        assert_eq!(seq[1].as_scalar().unwrap().text, "other");
        let map_entries = items[3].map_entries().unwrap();
        let inner = map_entries[0].1.map_entries().unwrap();
        assert_eq!(inner[0].1.as_scalar().unwrap().text, "anchor_one");
    }
}
