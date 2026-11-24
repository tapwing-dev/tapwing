//! Workspace planning compatibility with Maestro 2.12.1.
//!
//! Every case in `compat/workspace-cases.json` (Maestro's own
//! `WorkspaceExecutionPlannerTest` / `…ErrorsTest` cases over its workspace
//! fixtures, plus a few Tapwing extras) is planned by Tapwing and compared
//! with what Maestro's `WorkspaceExecutionPlanner` decided for it
//! (`compat/workspace-plans.json`, see `compat/dump_workspace_plans.sh`):
//! - plans: the same flows (as a set: Maestro keeps the file system's walk
//!   order, Tapwing sorts), the same `executionOrder` sequence in order, and
//!   the same `continueOnFailure` when there is a sequence;
//! - validation errors: the identical message;
//! - flow syntax errors: the same title, in the same file.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tapwing::workspace::{self, PlanError};

fn compat_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat").canonicalize().expect("compat dir")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned()
}

/// `Invalid Command: x at <path>:3:3` -> (`Invalid Command: x`, `<path>`).
fn split_syntax_message(message: &str) -> (String, String) {
    let first = message.lines().next().unwrap_or_default();
    let (title, location) = first.rsplit_once(" at ").unwrap_or((first, ""));
    let path = location.split(':').next().unwrap_or_default();
    (title.to_string(), path.to_string())
}

#[test]
fn plans_match_maestro() {
    let compat = compat_dir();
    let root = compat.join("maestro-fixtures/maestro-orchestra/workspaces");
    let cases: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(compat.join("workspace-cases.json")).unwrap()).unwrap();
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(compat.join("workspace-plans.json")).unwrap()).unwrap();

    let mut failures = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let want = &expected[name];
        assert!(!want.is_null(), "{name}: no reference plan; run compat/dump_workspace_plans.sh");
        let input: Vec<PathBuf> = strings(&case["input"]).iter().map(|p| root.join(p)).collect();
        let config = case["config"].as_str().map(|c| root.join(c));
        let got =
            workspace::plan(&input, &strings(&case["includeTags"]), &strings(&case["excludeTags"]), config.as_deref());

        let problem = match (got, want.get("error")) {
            (Ok(plan), None) => {
                let mut flows: Vec<String> = plan.flows.iter().map(|f| rel(&root, &f.path)).collect();
                flows.sort();
                let sequence: Vec<String> = plan.sequence.iter().map(|f| rel(&root, &f.path)).collect();
                let want_seq = strings(&want["sequence"]);
                if flows != strings(&want["flowsToRun"]) {
                    Some(format!("flows {flows:?}, Maestro {:?}", strings(&want["flowsToRun"])))
                } else if sequence != want_seq {
                    Some(format!("sequence {sequence:?}, Maestro {want_seq:?}"))
                } else if !want_seq.is_empty() && Some(plan.continue_on_failure) != want["continueOnFailure"].as_bool()
                {
                    Some(format!("continueOnFailure {}", plan.continue_on_failure))
                } else {
                    None
                }
            }
            (Ok(_), Some(err)) => Some(format!("planned, Maestro failed: {}", err["message"])),
            (Err(e), None) => Some(format!("failed, Maestro planned: {e}")),
            (Err(PlanError::Validation(msg)), Some(err)) => {
                let want_msg = err["message"].as_str().unwrap().replace("<ROOT>", &root.to_string_lossy());
                (msg != want_msg).then(|| format!("message:\n{msg}\n--- Maestro:\n{want_msg}"))
            }
            (Err(PlanError::Flow(e)), Some(err)) => {
                let (title, path) = split_syntax_message(err["message"].as_str().unwrap());
                let path = path.replace("<ROOT>", &root.to_string_lossy());
                (e.title != title || e.path != Path::new(&path))
                    .then(|| format!("flow error `{}` in {}, Maestro `{title}` in {path}", e.title, e.path.display()))
            }
        };
        if let Some(p) = problem {
            failures.push(format!("{name}: {p}"));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

#[test]
fn flow_name_prefers_config_name() {
    let root = compat_dir().join("maestro-fixtures/maestro-orchestra/workspaces/013_execution_order");
    let plan = workspace::plan(&[root], &[], &[], None).unwrap();
    let names: Vec<String> = plan.sequence.iter().map(workspace::flow_name).collect();
    assert_eq!(names, ["flowB", "flowC", "flowD"]);
}
