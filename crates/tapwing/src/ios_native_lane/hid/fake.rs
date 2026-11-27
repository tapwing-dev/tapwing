//! A scripted [`HidBackend`] and a session around it, for tests without a simulator.

use std::sync::{Arc, Mutex};

use super::{Button, FailPoint, Gate, HidBackend, HidError, HidSession, Screen, Snapshot};

pub(crate) const ALERT_TREE: &str = include_str!("fixtures/alert.probe.json");
pub(crate) const PLAYGROUND_TREE: &str = include_str!("fixtures/playground.probe.json");
pub(crate) const LANDSCAPE_TREE: &str = include_str!("fixtures/landscape.probe.json");

/// Adds the `process` field the shim writes to the first root of a captured tree (the phase 1
/// captures predate it).
pub(crate) fn with_process(tree: &str, process: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(tree).unwrap();
    v["roots"][0]["process"] = serde_json::json!(process);
    v.to_string()
}

pub(crate) struct FakeState {
    pub calls: Vec<String>,
    pub opens: usize,
    pub open_err: Option<HidError>,
    /// Answer of every `snapshot`.
    pub snapshot: Result<String, HidError>,
    /// Replaces `snapshot` once a touch succeeded (the alert is gone).
    pub snapshot_after_touch: Option<String>,
    /// Returned by every touch and button after the call was "sent".
    pub input_err: Option<HidError>,
    pub frames: Vec<Result<Vec<u8>, HidError>>,
    next_frame: usize,
}

impl FakeState {
    /// Frames answer in turn, starting again at the first after the last.
    pub fn set_frames(&mut self, frames: Vec<Result<Vec<u8>, HidError>>) {
        self.frames = frames;
        self.next_frame = 0;
    }

    pub fn touches(&self) -> Vec<&String> {
        self.calls.iter().filter(|c| c.starts_with("touch")).collect()
    }
}

pub(crate) struct Fake {
    pub state: Arc<Mutex<FakeState>>,
    pub warnings: Arc<Mutex<Vec<String>>>,
    pub session: Arc<HidSession>,
}

struct FakeBackend(Arc<Mutex<FakeState>>);

impl Fake {
    pub fn new(snapshot: Result<String, HidError>) -> Self {
        Self::with_fail(snapshot, None)
    }

    pub fn with_fail(snapshot: Result<String, HidError>, fail: Option<FailPoint>) -> Self {
        let state = Arc::new(Mutex::new(FakeState {
            calls: Vec::new(),
            opens: 0,
            open_err: None,
            snapshot,
            snapshot_after_touch: None,
            input_err: None,
            frames: vec![Ok(b"frame".to_vec())],
            next_frame: 0,
        }));
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let w = Arc::clone(&warnings);
        let gate = Gate::new(move |line| w.lock().unwrap().push(line.to_string()));
        let s = Arc::clone(&state);
        let opener: super::Opener = Box::new(move |_udid| {
            let mut st = s.lock().unwrap();
            st.opens += 1;
            if let Some(e) = st.open_err.clone() {
                return Err(e);
            }
            Ok(Box::new(FakeBackend(Arc::clone(&s))) as Box<dyn HidBackend>)
        });
        Self { state, warnings, session: Arc::new(HidSession::new("SIM-UDID", opener, gate, fail)) }
    }

    /// A fake with SpringBoard in front showing the permission alert; the alert is gone after a tap.
    pub fn alert() -> Self {
        let f = Self::new(Ok(with_process(ALERT_TREE, "SpringBoard")));
        f.state.lock().unwrap().snapshot_after_touch = Some(with_process(PLAYGROUND_TREE, "Runner"));
        f
    }

    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }
}

impl HidBackend for FakeBackend {
    fn screen(&mut self) -> Result<Screen, HidError> {
        self.0.lock().unwrap().calls.push("screen".into());
        Ok(Screen { width: 440.0, height: 956.0, scale: 3.0 })
    }

    fn snapshot(&mut self, _req: &Snapshot) -> Result<String, HidError> {
        let mut st = self.0.lock().unwrap();
        st.calls.push("snapshot".into());
        st.snapshot.clone()
    }

    fn touch(&mut self, path: &[(f64, f64)], hold_ms: u32, step_ms: u32) -> Result<(), HidError> {
        let mut st = self.0.lock().unwrap();
        let first = path[0];
        st.calls.push(format!("touch n={} first={},{} hold={hold_ms} step={step_ms}", path.len(), first.0, first.1));
        if let Some(e) = st.input_err.clone() {
            return Err(e);
        }
        if let Some(next) = st.snapshot_after_touch.take() {
            st.snapshot = Ok(next);
        }
        Ok(())
    }

    fn button(&mut self, button: Button) -> Result<(), HidError> {
        let mut st = self.0.lock().unwrap();
        st.calls.push(format!("button {button:?}"));
        st.input_err.clone().map_or(Ok(()), Err)
    }

    fn frame_png(&mut self) -> Result<Vec<u8>, HidError> {
        let mut st = self.0.lock().unwrap();
        st.calls.push("frame".into());
        let i = st.next_frame % st.frames.len();
        st.next_frame += 1;
        st.frames[i].clone()
    }
}
