//! Parser compatibility with Maestro 2.12.1.
//!
//! Every YAML file under `compat/maestro-fixtures` (Maestro's own test and
//! e2e flows) is parsed by Tapwing and compared with what Maestro produced
//! for it (`compat/maestro-ir`, see `compat/dump_maestro_ir.sh`):
//! - parsed flows: the IR must be identical, field by field;
//! - rejected flows: Tapwing must reject them too, with the same error title.
//!
//! Two corpora: Maestro's own fixtures, and `compat/probes` (Tapwing's
//! edge cases for Jackson's typing quirks and every command shape).
//! Per-file reports are written to `target/compat-<corpus>.txt`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

fn compat_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat").canonicalize().expect("compat dir")
}

fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            yaml_files(&p, out);
        } else if matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")) {
            out.push(p);
        }
    }
}

/// Error title as Maestro prints it: first line, before " at <path>".
fn title_of(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default();
    first.split(" at ").next().unwrap_or(first).split(": <ROOT>").next().unwrap_or(first).to_string()
}

/// The golden files come from a mapper with `NON_NULL` inclusion, which
/// also drops null *map values* (e.g. `permissions: {camera: ~}`). The IR
/// keeps them, as Maestro's objects do, so drop them before comparing.
fn drop_null_entries(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|_, x| !x.is_null());
            m.values_mut().for_each(drop_null_entries);
        }
        Value::Array(a) => a.iter_mut().for_each(drop_null_entries),
        _ => {}
    }
}

/// First differing JSON path, for the report.
fn first_diff(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(va), Some(vb)) => first_diff(&format!("{path}.{k}"), va, vb),
                (va, vb) => Some(format!("{path}.{k}: tapwing={} maestro={}", short(va), short(vb))),
            })
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: tapwing has {} items, maestro {}", x.len(), y.len()));
            }
            x.iter().zip(y).enumerate().find_map(|(i, (va, vb))| first_diff(&format!("{path}[{i}]"), va, vb))
        }
        (Value::Number(x), Value::Number(y)) if x.as_f64() == y.as_f64() => None,
        _ if a == b => None,
        _ => Some(format!("{path}: tapwing={} maestro={}", short(Some(a)), short(Some(b)))),
    }
}

fn short(v: Option<&Value>) -> String {
    match v {
        None => "<absent>".into(),
        Some(v) => {
            let s = v.to_string();
            if s.len() > 120 { format!("{}…", &s[..120]) } else { s }
        }
    }
}

#[test]
fn maestro_fixtures_parse_like_maestro() {
    check_corpus("maestro-fixtures", "maestro-ir");
}

#[test]
fn edge_case_probes_parse_like_maestro() {
    check_corpus("probes", "probes-ir");
}

fn check_corpus(fixtures_dir: &str, golden_dir: &str) {
    let root = compat_dir();
    let fixtures = root.join(fixtures_dir);
    let golden_dir = root.join(golden_dir);
    let root_str = fixtures.display().to_string();
    let mut files = Vec::new();
    yaml_files(&fixtures, &mut files);

    let mut report = String::new();
    let (mut same, mut mismatched) = (0, Vec::new());
    for file in &files {
        let rel = file.strip_prefix(&fixtures).unwrap().display().to_string();
        let golden: Value = serde_json::from_str(
            &std::fs::read_to_string(golden_dir.join(format!("{rel}.json"))).expect("golden file"),
        )
        .unwrap();
        let ours = match tapwing::flow::parse_file(file) {
            Ok(flow) => {
                let json = serde_json::to_string(&json!({ "commands": flow.commands })).unwrap();
                let mut v = serde_json::from_str::<Value>(&json.replace(&root_str, "<ROOT>")).unwrap();
                drop_null_entries(&mut v);
                v
            }
            Err(e) => json!({ "error": { "title": e.title, "message": e.message, "pos": e.pos.to_string() } }),
        };
        let verdict = match (&ours.get("commands"), &golden.get("commands")) {
            (Some(a), Some(b)) => first_diff("", a, b).map(|d| format!("IR differs at {d}")),
            (None, None) => {
                let theirs = title_of(golden["error"]["message"].as_str().unwrap_or_default());
                let mine = ours["error"]["title"].as_str().unwrap_or_default().to_string();
                (mine != theirs).then(|| format!("error title: tapwing=\"{mine}\" maestro=\"{theirs}\""))
            }
            (Some(_), None) => Some(format!(
                "tapwing accepted, maestro rejected: {}",
                golden["error"]["message"].as_str().unwrap_or_default().lines().next().unwrap_or_default()
            )),
            (None, Some(_)) => Some(format!(
                "tapwing rejected ({}: {}), maestro accepted",
                ours["error"]["title"].as_str().unwrap_or_default(),
                ours["error"]["message"].as_str().unwrap_or_default().lines().next().unwrap_or_default()
            )),
        };
        match verdict {
            None => {
                same += 1;
                report += &format!("ok    {rel}\n");
            }
            Some(why) => {
                report += &format!("DIFF  {rel}\n      {why}\n");
                mismatched.push(format!("{rel}: {why}"));
            }
        }
    }
    let summary =
        format!("{fixtures_dir}: {same}/{} parse like Maestro 2.12.1, {} differ", files.len(), mismatched.len());
    report = format!("{summary}\n\n{report}");
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/compat-{fixtures_dir}.txt"));
    std::fs::write(&target, &report).unwrap();
    println!("{summary} (report: {})", target.display());
    assert!(mismatched.is_empty(), "{summary}:\n{}", mismatched.join("\n"));
}
