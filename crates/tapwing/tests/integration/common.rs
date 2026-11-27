//! Shared helpers across Tapwing integration tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tapwing::backend::{AssertionError, MaestroError, MaestroErrorKind};
use tapwing::fake_driver::{FakeDriver, FakeEvent};
use tapwing::flow::{self, ParsedFlow};
use tapwing::orchestra::{FlowResult, Orchestra, OrchestraOpts};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// RAII temporary directory isolated by PID, monotonic sequence, and label.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tapwing_it_{}_{}_{}", std::process::id(), id, label));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }

    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&full, content).expect("write fixture file");
        full
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

pub fn compat_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat").canonicalize().expect("compat dir")
}

pub fn fixture_path(case_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../compat/maestro-fixtures/maestro-test")
        .join(format!("{case_name}.yaml"))
}

pub fn load_case(case_name: &str) -> ParsedFlow {
    let p = fixture_path(case_name);
    flow::parse_file(&p).unwrap_or_else(|e| panic!("failed to parse {case_name}: {}", e.render()))
}

pub fn default_opts(flow: &ParsedFlow) -> OrchestraOpts {
    OrchestraOpts {
        lookup_timeout_ms: 0,
        optional_lookup_timeout_ms: 0,
        flow_path: flow.path.clone(),
        ..Default::default()
    }
}

pub fn run_case(case_name: &str, driver: &mut FakeDriver) -> FlowResult {
    let flow = load_case(case_name);
    let mut orch = Orchestra::new(driver, default_opts(&flow));
    orch.run_parsed_flow(&flow)
}

/// Mirrors Truth's `containsAtLeastElementsIn(expected).inOrder()`.
pub fn assert_events_in_order(actual: &[FakeEvent], expected: &[FakeEvent]) {
    let mut pos = 0usize;
    for exp in expected {
        if let Some(found) = actual[pos..].iter().position(|a| a == exp) {
            pos += found + 1;
        } else {
            panic!("Expected event {exp:?} in order after index {pos}.\nActual events: {actual:#?}");
        }
    }
}

pub fn is_assertion_failure(err: &anyhow::Error) -> bool {
    err.is::<AssertionError>()
        || err.downcast_ref::<MaestroError>().is_some_and(|m| m.kind == MaestroErrorKind::AssertionFailure)
}

pub fn minimal_flutter_pubspec(name: &str) -> String {
    format!(
        "\
name: {name}
description: Fixture app.
publish_to: 'none'

environment:
  sdk: ^3.11.0

dependencies:
  flutter:
    sdk: flutter

dev_dependencies:
  flutter_test:
    sdk: flutter

flutter:
  uses-material-design: true
"
    )
}
