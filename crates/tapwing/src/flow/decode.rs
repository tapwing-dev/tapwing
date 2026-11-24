//! Jackson-style data binding over [`Node`]s: strict property names, scalar
//! coercion, and the null/default rules of the Kotlin module.
//!
//! Two binding modes exist because Maestro uses both:
//! - [`Mode::Stream`]: a POJO bound straight from the parser. A string field
//!   gets the scalar's source text (`1.50` stays `1.50`).
//! - [`Mode::Tree`]: a custom deserializer that first reads a `JsonNode`
//!   tree (selectors, swipe, ...). Numbers are already parsed there, so a
//!   string field gets their canonical text (`1.50` becomes `1.5`).

use serde_json::{Number, Value};

use super::error::Problem;
use super::ir::ValueMap;
use super::yaml::{Node, Pos, Scalar, Typed, canonical_text, java_double};

pub type DResult<T> = Result<T, Problem>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Stream,
    Tree,
}

/// A YAML mapping bound to a class with the given properties.
pub struct Obj<'a> {
    entries: Vec<(&'a str, &'a Node, Pos)>,
    pub pos: Pos,
    pub mode: Mode,
}

impl<'a> Obj<'a> {
    /// `node` must be a mapping whose keys are all in `allowed`.
    pub fn bind(node: &'a Node, allowed: &[&str], field: &str, mode: Mode) -> DResult<Self> {
        let entries = node.map_entries().ok_or_else(|| Problem::format(field, node.pos()))?;
        if let Some((name, _, pos)) = entries.iter().find(|(name, _, _)| !allowed.contains(name)) {
            return Err(Problem::unknown_property(name, *pos));
        }
        Ok(Obj { entries, pos: node.pos(), mode })
    }

    /// Like [`Obj::bind`] but keeps unknown keys (`@JsonAnySetter`, or
    /// `Map` access in delegating creators).
    pub fn open(node: &'a Node, field: &str, mode: Mode) -> DResult<Self> {
        let entries = node.map_entries().ok_or_else(|| Problem::format(field, node.pos()))?;
        Ok(Obj { entries, pos: node.pos(), mode })
    }

    pub fn keys(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.entries.iter().map(|(k, _, _)| *k)
    }

    pub fn has(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _, _)| *k == key)
    }

    /// The value, or None when absent or null (Kotlin treats both alike:
    /// nullable fields get null, defaulted fields keep their default).
    pub fn get(&self, key: &str) -> Option<&'a Node> {
        self.entries.iter().find(|(k, _, _)| *k == key).map(|(_, v, _)| *v).filter(|v| !v.is_null())
    }

    pub fn entries(&self) -> &[(&'a str, &'a Node, Pos)] {
        &self.entries
    }

    pub fn string(&self, key: &str) -> DResult<Option<String>> {
        self.get(key).map(|n| string(n, key, self.mode)).transpose()
    }

    pub fn req_string(&self, key: &str) -> DResult<String> {
        self.string(key)?.ok_or_else(|| Problem::missing(key, self.pos))
    }

    pub fn bool(&self, key: &str) -> DResult<Option<bool>> {
        self.get(key).map(|n| boolean(n, key)).transpose()
    }

    pub fn int(&self, key: &str) -> DResult<Option<i32>> {
        self.get(key).map(|n| int(n, key)).transpose()
    }

    pub fn long(&self, key: &str) -> DResult<Option<i64>> {
        self.get(key).map(|n| long(n, key)).transpose()
    }

    pub fn double(&self, key: &str) -> DResult<Option<f64>> {
        self.get(key).map(|n| double(n, key)).transpose()
    }

    /// `Map<String, String>`: values become strings, nulls stay null.
    pub fn str_map(&self, key: &str) -> DResult<Option<ValueMap>> {
        self.get(key).map(|n| str_map(n, key, self.mode)).transpose()
    }

    /// `Map<String, Any>`: values keep their YAML types.
    pub fn any_map(&self, key: &str) -> DResult<Option<ValueMap>> {
        self.get(key).map(|n| any_map(n, key)).transpose()
    }

    /// `List<String>`.
    pub fn str_list(&self, key: &str) -> DResult<Option<Vec<String>>> {
        self.get(key).map(|n| str_list(n, key, self.mode)).transpose()
    }
}

/// A string field. Scalars coerce; collections are a format error.
pub fn string(n: &Node, field: &str, mode: Mode) -> DResult<String> {
    let s = scalar(n, field)?;
    Ok(match mode {
        Mode::Stream => s.text.clone(),
        Mode::Tree => canonical_text(s).unwrap_or_default(),
    })
}

pub fn boolean(n: &Node, field: &str) -> DResult<bool> {
    let s = scalar(n, field)?;
    match s.typed() {
        Typed::Bool(b) => Ok(b),
        Typed::Int(i) => Ok(i != 0),
        Typed::Str(t) => match t.as_str() {
            "true" | "True" | "TRUE" => Ok(true),
            "false" | "False" | "FALSE" => Ok(false),
            _ => Err(Problem::format(field, n.pos())),
        },
        _ => Err(Problem::format(field, n.pos())),
    }
}

pub fn long(n: &Node, field: &str) -> DResult<i64> {
    let s = scalar(n, field)?;
    match s.typed() {
        Typed::Int(i) => Ok(i),
        // Jackson ACCEPT_FLOAT_AS_INT: truncate.
        Typed::Float(f) if f.is_finite() => Ok(f.trunc() as i64),
        Typed::Str(t) => t.trim().parse::<i64>().map_err(|_| Problem::format(field, n.pos())),
        _ => Err(Problem::format(field, n.pos())),
    }
}

pub fn int(n: &Node, field: &str) -> DResult<i32> {
    i32::try_from(long(n, field)?).map_err(|_| Problem::format(field, n.pos()))
}

pub fn double(n: &Node, field: &str) -> DResult<f64> {
    let s = scalar(n, field)?;
    match s.typed() {
        Typed::Int(i) => Ok(i as f64),
        Typed::Float(f) => Ok(f),
        Typed::BigInt(d) => d.parse().map_err(|_| Problem::format(field, n.pos())),
        Typed::Str(t) => t.trim().parse::<f64>().map_err(|_| Problem::format(field, n.pos())),
        _ => Err(Problem::format(field, n.pos())),
    }
}

pub fn str_map(n: &Node, field: &str, mode: Mode) -> DResult<ValueMap> {
    let entries = n.map_entries().ok_or_else(|| Problem::format(field, n.pos()))?;
    let mut out = ValueMap::new();
    for (k, v, _) in entries {
        let value = if v.is_null() { Value::Null } else { Value::String(string(v, field, mode)?) };
        out.insert(k.to_string(), value);
    }
    Ok(out)
}

pub fn any_map(n: &Node, field: &str) -> DResult<ValueMap> {
    let entries = n.map_entries().ok_or_else(|| Problem::format(field, n.pos()))?;
    Ok(entries.into_iter().map(|(k, v, _)| (k.to_string(), untyped(v))).collect())
}

pub fn str_list(n: &Node, field: &str, mode: Mode) -> DResult<Vec<String>> {
    let Node::Seq(items, _) = n else { return Err(Problem::format(field, n.pos())) };
    items.iter().map(|i| string(i, field, mode)).collect()
}

/// Jackson "untyped" value (`Any`, `Map<String, Any?>`).
pub fn untyped(n: &Node) -> Value {
    match n {
        Node::Scalar(s) => match s.typed() {
            Typed::Null => Value::Null,
            Typed::Bool(b) => Value::Bool(b),
            Typed::Int(i) => Value::Number(i.into()),
            Typed::BigInt(d) => {
                d.parse::<f64>().ok().and_then(Number::from_f64).map_or(Value::String(d), Value::Number)
            }
            Typed::Float(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
            Typed::Str(t) => Value::String(t),
        },
        Node::Seq(items, _) => Value::Array(items.iter().map(untyped).collect()),
        Node::Map(..) => Value::Object(
            n.map_entries().unwrap_or_default().into_iter().map(|(k, v, _)| (k.to_string(), untyped(v))).collect(),
        ),
    }
}

/// What Kotlin's `toString()` prints for an untyped scalar (delegating
/// creators taking `Any`, e.g. `inputText: 1.50` → "1.5"). `None` for types
/// Maestro rejects there (BigInteger, collections).
pub fn untyped_scalar_text(s: &Scalar) -> Option<String> {
    match s.typed() {
        Typed::Null | Typed::BigInt(_) => None,
        Typed::Bool(b) => Some(b.to_string()),
        Typed::Int(i) => Some(i.to_string()),
        Typed::Float(f) => Some(java_double(f)),
        Typed::Str(t) => Some(t),
    }
}

fn scalar<'n>(n: &'n Node, field: &str) -> DResult<&'n Scalar> {
    n.as_scalar().ok_or_else(|| Problem::format(field, n.pos()))
}
