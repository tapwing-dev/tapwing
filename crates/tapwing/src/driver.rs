//! Connection to the in-app driver. The app dials out to us
//! (device localhost:7781 --adb reverse--> host port).

use crate::backend::TransportError;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// Upper bound on a single JSON-RPC line from the in-app driver (prevents OOM
/// abort on a corrupted or runaway peer stream).
const MAX_RPC_LINE: usize = 64 * 1024 * 1024;

/// Maximum time an accepted TCP socket has to deliver its initial `hello` line
/// before being dropped (prevents a half-open socket from a dying process from
/// stalling `launchApp` for seconds).
const HELLO_READ_TIMEOUT: Duration = Duration::from_millis(300);

/// Per-read socket timeout for RPC calls. The driver emits a `progress` event
/// every 2 s while any command is executing, so 10 s catches dead/hung peers
/// promptly without ever timing out a legitimate long-running command.
const RPC_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Socket read granularity inside [`Driver::call`].
const RPC_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Silence after which [`Driver::call`] starts checking that the app
/// process is still alive (normal responses arrive within milliseconds;
/// long commands send a `progress` event every 2 s).
const LIVENESS_PROBE_AFTER: Duration = Duration::from_secs(1);

/// Interval between liveness checks while the app stays silent.
const LIVENESS_PROBE_EVERY: Duration = Duration::from_secs(1);

/// Checks whether the app process `pid` is alive: `Some(false)` only when it
/// is certainly gone, `None` when the check itself failed.
pub type LivenessProbe = Box<dyn FnMut(i64) -> Option<bool> + Send>;

pub struct Driver {
    listener: TcpListener,
    conn: Option<Conn>,
    pub hello: Option<Value>,
    last_pid: Option<i64>,
    /// F7 app health issues received on responses, not yet taken.
    issues: Vec<ReceivedIssue>,
    /// Incremented for every accepted connection (request ids restart at 1).
    epoch: u64,
    /// Step boundary set by [`Driver::take_issue_batch`]: issues caused by
    /// requests before it belong to the previous step.
    boundary: (u64, u64),
    /// Highest issue `seq` seen (for `appIssues` re-reads).
    max_seq: u64,
    /// Highest cumulative `dropped` counter reported on the current connection.
    dropped_seen: u64,
    /// Dropped issues not yet handed off via [`Driver::take_issue_batch`].
    pending_dropped: u64,
    /// Detects a dead app while a call waits (see [`Driver::set_liveness_probe`]).
    liveness: Option<LivenessProbe>,
    /// Driver `platform` a hello must report (see [`Driver::expect_platform`]).
    expected_platform: Option<&'static str>,
    /// F8b HTTP request log entries received on responses, not yet taken.
    http: Vec<ReceivedIssue>,
    /// Step boundary set by [`Driver::take_http_batch`].
    http_boundary: (u64, u64),
    /// Highest HTTP entry `seq` seen on the current connection.
    http_max_seq: u64,
    /// Highest cumulative `droppedHttp` counter on the current connection.
    http_dropped_seen: u64,
    /// Dropped HTTP entries not yet handed off via [`Driver::take_http_batch`].
    http_pending_dropped: u64,
    /// Whether the connected driver confirmed `configure` `httpLog: true`.
    http_log: bool,
    /// No message for this long fails a call ([`RPC_READ_TIMEOUT`] by default).
    silence_timeout: Duration,
    /// The next accepted hello is the same process dialing in again: its issue and
    /// request-log sequence numbers continue, so the host watermarks must survive.
    keep_watermarks: bool,
    /// Test seam: runs the instant `disconnect()` has closed the old connection, standing in
    /// for a peer that redials the moment it sees EOF.
    #[cfg(test)]
    on_conn_closed: Option<Box<dyn FnMut() + Send>>,
}

struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}

/// An issue (or HTTP log entry) plus the connection epoch it arrived on.
struct ReceivedIssue {
    epoch: u64,
    issue: Value,
}

/// Outcome of accepting a driver connection, including diagnostics for
/// rejected stale/half-open connections during `launchApp`.
#[derive(Debug, Clone)]
pub struct AcceptOutcome {
    pub hello: Value,
    pub rejected_connections: usize,
}

/// Error reported by the driver (e.g. ELEMENT_NOT_FOUND).
#[derive(Debug)]
pub struct DriverError {
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for DriverError {}

/// A call got no message at all (no response, no `progress` heartbeat) for
/// the silence window while the app process was not known to be dead.
#[derive(Debug)]
pub struct RpcSilenceError {
    pub method: String,
    pub silence: Duration,
}

impl std::fmt::Display for RpcSilenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "connection to app lost during `{}`: no response for {} (app hung or unreachable)",
            self.method,
            human_duration(self.silence)
        )
    }
}
impl std::error::Error for RpcSilenceError {}

/// Whether `e` is a silence timeout of [`Driver::call`] (not a closed socket,
/// a dead process, or a driver error).
pub fn is_rpc_silence(e: &anyhow::Error) -> bool {
    e.is::<RpcSilenceError>()
}

/// `10 s` for whole seconds (the production message), `300 ms` otherwise.
pub(crate) fn human_duration(d: Duration) -> String {
    if d.subsec_millis() == 0 && d.as_secs() > 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{} ms", d.as_millis())
    }
}

struct PendingConn {
    stream: TcpStream,
    buf: Vec<u8>,
    accepted_at: Instant,
}

impl Driver {
    pub fn listen(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).with_context(|| format!("binding 127.0.0.1:{port}"))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            conn: None,
            hello: None,
            last_pid: None,
            issues: Vec::new(),
            epoch: 0,
            boundary: (0, 0),
            max_seq: 0,
            dropped_seen: 0,
            pending_dropped: 0,
            liveness: None,
            expected_platform: None,
            http: Vec::new(),
            http_boundary: (0, 0),
            http_max_seq: 0,
            http_dropped_seen: 0,
            http_pending_dropped: 0,
            http_log: false,
            silence_timeout: RPC_READ_TIMEOUT,
            keep_watermarks: false,
            #[cfg(test)]
            on_conn_closed: None,
        })
    }

    /// Overrides the 10 s silence window of [`Driver::call`] (tests).
    pub fn set_silence_timeout(&mut self, timeout: Duration) {
        self.silence_timeout = timeout;
    }

    /// How long a call may go without any message before it fails.
    pub fn silence_timeout(&self) -> Duration {
        self.silence_timeout
    }

    /// Rejects hellos from apps on another platform. iOS simulators share the
    /// host loopback, so an instrumented app left running there keeps dialing
    /// its last session port and could otherwise answer an Android session
    /// (or vice versa). Hellos without a `platform` field are accepted.
    pub fn expect_platform(&mut self, platform: &'static str) {
        self.expected_platform = Some(platform);
    }

    /// The host port actually bound (useful when listening on port 0).
    pub fn port(&self) -> Result<u16> {
        Ok(self.listener.local_addr()?.port())
    }

    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    /// Installs a check that the app process is alive, run while a call has
    /// waited more than 1 s without any message (then every second), so a
    /// crashed app fails the call within ~1–2 s instead of the 10 s timeout.
    pub fn set_liveness_probe(&mut self, probe: LivenessProbe) {
        self.liveness = Some(probe);
    }

    /// PID reported by the most recently accepted `hello`, if any.
    pub fn last_pid(&self) -> Option<i64> {
        self.last_pid
    }

    /// Monotonic connection epoch (incremented on every accepted `hello`).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Forget the current connection (e.g. the app is being restarted).
    ///
    /// Stale pending connections are drained *before* the current one is
    /// closed: closing it wakes the peer with an EOF, and an app that dials in
    /// again at once (the cold-start retry) would otherwise land in the backlog
    /// and be drained and dropped here as if it were stale.
    pub fn disconnect(&mut self) {
        while let Ok((s, _)) = self.listener.accept() {
            drop(s);
        }
        self.conn = None;
        self.hello = None;
        #[cfg(test)]
        if let Some(hook) = self.on_conn_closed.as_mut() {
            hook();
        }
    }

    /// Wait for the app to connect and say hello (any PID).
    pub fn accept(&mut self, timeout: Duration) -> Result<&Value> {
        self.accept_with_pid(timeout, None, &[])?;
        self.hello.as_ref().ok_or_else(|| anyhow!("missing hello after accept"))
    }

    /// [`Self::accept_with_pid`] for the SAME process dialing in again after the host
    /// dropped its connection (the cold-start retry): the app's issue and request-log
    /// sequence numbers continue across connections, so the host's watermarks are kept
    /// and nothing already delivered is fetched or counted a second time.
    pub fn accept_same_process(&mut self, timeout: Duration, pid: Option<i64>) -> Result<AcceptOutcome> {
        self.keep_watermarks = true;
        let res = self.accept_with_pid(timeout, pid, &[]);
        self.keep_watermarks = false;
        res
    }

    /// Wait for the app to connect and say hello, polling pending sockets in
    /// parallel with a short 300 ms per-connection hello cap and optional PID
    /// verification (`expected_pid` must match when set; hellos from
    /// `reject_pids` are dropped so a dying prior instance cannot hijack a
    /// fresh launch).
    pub fn accept_with_pid(
        &mut self,
        timeout: Duration,
        expected_pid: Option<i64>,
        reject_pids: &[i64],
    ) -> Result<AcceptOutcome> {
        let started = Instant::now();
        let deadline = started + timeout;
        let mut pending: Vec<PendingConn> = Vec::new();
        let mut rejected_connections = 0usize;

        loop {
            crate::interrupt::check()?;
            let now = Instant::now();
            if now > deadline {
                bail!("app did not connect within {} ms (is tapwing_driver in the build?)", timeout.as_millis());
            }

            // Accept any newly arrived connections without blocking.
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        if stream.set_nonblocking(true).is_ok() && stream.set_nodelay(true).is_ok() {
                            pending.push(PendingConn { stream, buf: Vec::with_capacity(256), accepted_at: now });
                        } else {
                            rejected_connections += 1;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }

            // Poll every pending socket for a newline-terminated `hello` event.
            let mut idx = 0;
            let mut tmp = [0u8; 1024];
            while idx < pending.len() {
                let mut drop_conn = false;
                let mut accepted_hello: Option<Value> = None;

                match pending[idx].stream.read(&mut tmp) {
                    Ok(0) => {
                        drop_conn = true;
                    }
                    Ok(n) => {
                        pending[idx].buf.extend_from_slice(&tmp[..n]);
                        if let Some(nl) = pending[idx].buf.iter().position(|&b| b == b'\n') {
                            let line = String::from_utf8_lossy(&pending[idx].buf[..nl]);
                            if let Ok(msg) = serde_json::from_str::<Value>(&line)
                                && msg["event"] == "hello"
                            {
                                let hello_pid = msg["params"]["pid"].as_i64();
                                let pid_ok = expected_pid.is_none_or(|p| hello_pid == Some(p))
                                    && hello_pid.is_none_or(|p| !reject_pids.contains(&p));
                                let hello_platform = msg["params"]["platform"].as_str();
                                let platform_ok =
                                    self.expected_platform.is_none_or(|p| hello_platform.is_none_or(|h| h == p));
                                if pid_ok && platform_ok {
                                    // The driver sends nothing after hello until it gets a
                                    // request, so no bytes past the newline are lost here.
                                    let stream = pending.swap_remove(idx).stream;
                                    stream.set_nonblocking(false)?;
                                    stream.set_write_timeout(Some(RPC_READ_TIMEOUT))?;
                                    let reader = BufReader::new(stream.try_clone()?);
                                    let params = msg["params"].clone();
                                    self.last_pid = hello_pid;
                                    self.conn = Some(Conn { reader, writer: stream, next_id: 1 });
                                    self.epoch += 1;
                                    if !self.keep_watermarks {
                                        self.max_seq = 0;
                                        self.dropped_seen = 0;
                                        self.http_max_seq = 0;
                                        self.http_dropped_seen = 0;
                                    }
                                    self.http_log = false;
                                    self.hello = Some(params.clone());
                                    accepted_hello = Some(params);
                                } else {
                                    drop_conn = true;
                                }
                            } else {
                                drop_conn = true;
                            }
                        } else if pending[idx].buf.len() > 64 * 1024 {
                            drop_conn = true;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        if now.duration_since(pending[idx].accepted_at) > HELLO_READ_TIMEOUT {
                            drop_conn = true;
                        }
                    }
                    Err(_) => {
                        drop_conn = true;
                    }
                }

                if let Some(hello) = accepted_hello {
                    // Remaining pending sockets are stale/extra; drop them.
                    rejected_connections += pending.len();
                    return Ok(AcceptOutcome { hello, rejected_connections });
                }

                if drop_conn {
                    pending.swap_remove(idx);
                    rejected_connections += 1;
                } else {
                    idx += 1;
                }
            }

            // Keep 1 ms responsiveness during the initial fast-connect window,
            // then ramp to 10 ms to avoid thousands of idle wakeups on long
            // black-box fallback waits.
            let step = if pending.is_empty() && started.elapsed() >= Duration::from_millis(200) {
                Duration::from_millis(10)
            } else {
                Duration::from_millis(1)
            };
            std::thread::sleep(step.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// Re-reads app health issues the driver buffered since the last one
    /// seen (after native-lane steps and at flow end, when no response
    /// carried them). Issues arrive through [`Driver::call`] like any other.
    pub fn poll_issues(&mut self) {
        if self.is_connected() {
            let since = self.max_seq;
            if let Ok(res) = self.call("appIssues", json!({ "since": since })) {
                let epoch = self.epoch;
                if let Some(d) = res.get("dropped").and_then(Value::as_u64)
                    && d > self.dropped_seen
                {
                    self.pending_dropped += d - self.dropped_seen;
                    self.dropped_seen = d;
                }
                for issue in res["issues"].as_array().into_iter().flatten() {
                    let seq = issue["seq"].as_u64().unwrap_or(0);
                    if seq > since {
                        self.max_seq = self.max_seq.max(seq);
                        self.issues.push(ReceivedIssue { epoch, issue: issue.clone() });
                    }
                }
            }
        }
    }

    /// Takes the issues received since the last call, split into those
    /// caused by requests of the step that just ran (`current`) and those
    /// caused by earlier requests (`late`), then starts a new step.
    pub fn take_issue_batch(&mut self) -> crate::app_health::IssueBatch {
        let mut batch =
            crate::app_health::IssueBatch { dropped: std::mem::take(&mut self.pending_dropped), ..Default::default() };
        let (current, late) = split_by_boundary(self.issues.drain(..), self.boundary);
        batch.current = current;
        batch.late = late;
        self.boundary = self.next_boundary();
        batch
    }

    fn next_boundary(&self) -> (u64, u64) {
        (self.epoch, self.conn.as_ref().map_or(0, |c| c.next_id))
    }

    /// F8b: records whether the driver confirmed `configure` `httpLog: true`
    /// (older drivers omit the field).
    pub fn set_http_log(&mut self, confirmed: bool) {
        self.http_log = confirmed;
    }

    /// F8b: whether the connected driver keeps an HTTP request log.
    pub fn http_log_enabled(&self) -> bool {
        self.http_log && self.is_connected()
    }

    /// F8b: re-reads HTTP log entries the driver finished since the last one
    /// seen (after native-lane steps and at flow end) and returns the
    /// requests still in flight.
    pub fn poll_http(&mut self) -> Vec<Value> {
        if !self.http_log_enabled() {
            return Vec::new();
        }
        let since = self.http_max_seq;
        let Ok(res) = self.call("httpLog", json!({ "since": since })) else {
            return Vec::new();
        };
        if let Some(d) = res.get("dropped").and_then(Value::as_u64)
            && d > self.http_dropped_seen
        {
            self.http_pending_dropped += d - self.http_dropped_seen;
            self.http_dropped_seen = d;
        }
        let epoch = self.epoch;
        for entry in res["entries"].as_array().into_iter().flatten() {
            let seq = entry["seq"].as_u64().unwrap_or(0);
            if seq > since {
                self.http_max_seq = self.http_max_seq.max(seq);
                self.http.push(ReceivedIssue { epoch, issue: entry.clone() });
            }
        }
        res["inFlight"].as_array().cloned().unwrap_or_default()
    }

    /// F8b: takes the HTTP log entries received since the last call, split
    /// like [`Driver::take_issue_batch`] by the request that started them.
    pub fn take_http_batch(&mut self) -> crate::trace::TraceBatch {
        let (current, late) = split_by_boundary(self.http.drain(..), self.http_boundary);
        self.http_boundary = self.next_boundary();
        crate::trace::TraceBatch {
            current,
            late,
            dropped: std::mem::take(&mut self.http_pending_dropped),
            ..Default::default()
        }
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let Driver {
            conn: conn_slot,
            last_pid,
            liveness,
            issues,
            max_seq,
            dropped_seen,
            pending_dropped,
            epoch,
            http,
            http_max_seq,
            http_dropped_seen,
            http_pending_dropped,
            silence_timeout,
            ..
        } = self;
        let silence_timeout = *silence_timeout;
        let conn = conn_slot.as_mut().ok_or_else(|| TransportError("app is not connected".into()))?;
        // Short reads so a dead app is noticed without waiting for the full
        // silence timeout: adb reverse keeps the host socket open after the
        // app process dies.
        conn.reader.get_ref().set_read_timeout(Some(RPC_POLL_INTERVAL))?;
        let id = conn.next_id;
        conn.next_id += 1;
        let req = json!({ "id": id, "method": method, "params": params });
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        if let Err(e) = conn.writer.write_all(line.as_bytes()) {
            *conn_slot = None;
            return Err(TransportError(format!("connection to app lost: {e}")).into());
        }
        let mut buf: Vec<u8> = Vec::with_capacity(256);
        let mut last_activity = Instant::now();
        let mut next_probe = last_activity + LIVENESS_PROBE_AFTER;
        loop {
            if let Err(e) = crate::interrupt::check() {
                *conn_slot = None;
                return Err(e);
            }
            // Bytes of a partial line stay in `buf` across poll timeouts; bound
            // total line length so a runaway stream cannot trigger OOM.
            let remaining = MAX_RPC_LINE.saturating_sub(buf.len());
            match (&mut conn.reader).take(remaining as u64 + 1).read_until(b'\n', &mut buf) {
                Ok(0) => {
                    *conn_slot = None;
                    return Err(TransportError(format!("app closed the connection during `{method}`")).into());
                }
                Ok(_) if buf.len() > MAX_RPC_LINE => {
                    *conn_slot = None;
                    return Err(TransportError(format!(
                        "driver response during `{method}` exceeded {MAX_RPC_LINE} bytes"
                    ))
                    .into());
                }
                Ok(_) if !buf.ends_with(b"\n") => {
                    *conn_slot = None;
                    return Err(TransportError(format!("app closed the connection during `{method}`")).into());
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if let Err(int_err) = crate::interrupt::check() {
                        *conn_slot = None;
                        return Err(int_err);
                    }
                    let now = Instant::now();
                    // The driver emits `{"event":"progress"}` every 2 s while a
                    // command runs, so 10 s of silence means a dead or hung peer.
                    if now.duration_since(last_activity) >= silence_timeout {
                        *conn_slot = None;
                        return Err(RpcSilenceError { method: method.to_string(), silence: silence_timeout }.into());
                    }
                    if now >= next_probe {
                        next_probe = now + LIVENESS_PROBE_EVERY;
                        if let (Some(probe), Some(pid)) = (liveness.as_mut(), *last_pid)
                            && probe(pid) == Some(false)
                        {
                            *conn_slot = None;
                            return Err(TransportError(format!("app process {pid} died during `{method}`")).into());
                        }
                    }
                    continue;
                }
                Err(e) => {
                    *conn_slot = None;
                    return Err(TransportError(format!("connection to app lost during `{method}`: {e}")).into());
                }
            }
            last_activity = Instant::now();
            next_probe = last_activity + LIVENESS_PROBE_AFTER;
            let msg: Value = match serde_json::from_slice(&buf) {
                Ok(v) => v,
                Err(e) => {
                    *conn_slot = None;
                    return Err(TransportError(format!("invalid JSON from driver during `{method}`: {e}")).into());
                }
            };
            buf.clear();
            // Skip asynchronous events (such as `progress` heartbeats) and stale responses.
            if msg.get("event").is_some() || msg["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(d) = msg.get("droppedIssues").and_then(Value::as_u64)
                && d > *dropped_seen
            {
                *pending_dropped += d - *dropped_seen;
                *dropped_seen = d;
            }
            if let Some(list) = msg.get("issues").and_then(Value::as_array) {
                for issue in list {
                    *max_seq = (*max_seq).max(issue["seq"].as_u64().unwrap_or(0));
                    issues.push(ReceivedIssue { epoch: *epoch, issue: issue.clone() });
                }
            }
            if let Some(d) = msg.get("droppedHttp").and_then(Value::as_u64)
                && d > *http_dropped_seen
            {
                *http_pending_dropped += d - *http_dropped_seen;
                *http_dropped_seen = d;
            }
            if let Some(list) = msg.get("http").and_then(Value::as_array) {
                for entry in list {
                    *http_max_seq = (*http_max_seq).max(entry["seq"].as_u64().unwrap_or(0));
                    http.push(ReceivedIssue { epoch: *epoch, issue: entry.clone() });
                }
            }
            if let Some(err) = msg.get("error") {
                return Err(DriverError {
                    code: err["code"].as_str().unwrap_or("ERROR").to_string(),
                    message: err["message"].as_str().unwrap_or("driver error").to_string(),
                    details: err.clone(),
                }
                .into());
            }
            return Ok(msg["result"].clone());
        }
    }
}

/// Splits events into those caused by requests at or after `boundary`
/// (`current`) and those caused by earlier requests (`late`).
fn split_by_boundary(
    events: impl Iterator<Item = ReceivedIssue>,
    (b_epoch, b_id): (u64, u64),
) -> (Vec<Value>, Vec<Value>) {
    let (mut current, mut late) = (Vec::new(), Vec::new());
    for ReceivedIssue { epoch, issue } in events {
        let is_current = match issue["request"].as_u64() {
            Some(req) => epoch > b_epoch || req >= b_id,
            // Before the first request of a connection: startup events
            // belong to the step that (re)connected the app.
            None => epoch > b_epoch,
        };
        if is_current { current.push(issue) } else { late.push(issue) }
    }
    (current, late)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_with_pid_rejects_stale_and_half_open_and_call_skips_progress() {
        let mut drv = Driver::listen(0).unwrap();
        let port = drv.port().unwrap();

        let server_handle = std::thread::spawn(move || {
            // 1. Half-open connection that never sends hello (must not block 5 s).
            let _half_open = TcpStream::connect(("127.0.0.1", port)).unwrap();
            // 2. Stale process with old PID 100 (must be rejected).
            let mut stale = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(stale, r#"{{"event":"hello","params":{{"protocol":1,"pid":100}}}}"#).unwrap();
            // 3. New process with PID 200 (must be accepted promptly).
            std::thread::sleep(Duration::from_millis(20));
            let mut fresh = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(fresh, r#"{{"event":"hello","params":{{"protocol":1,"pid":200}}}}"#).unwrap();

            let mut reader = BufReader::new(fresh.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let id = req["id"].as_u64().unwrap();
            // Emit a progress event with the same `id` before the actual response.
            writeln!(fresh, r#"{{"event":"progress","id":{id}}}"#).unwrap();
            writeln!(fresh, r#"{{"id":{id},"result":"pong","us":5}}"#).unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });

        let t0 = Instant::now();
        let outcome = drv.accept_with_pid(Duration::from_secs(2), Some(200), &[100]).unwrap();
        assert!(t0.elapsed() < Duration::from_secs(1), "half-open socket stalled accept");
        assert_eq!(outcome.hello["pid"], 200);
        assert!(outcome.rejected_connections >= 1);

        let res = drv.call("ping", json!({})).unwrap();
        assert_eq!(res, "pong");
        server_handle.join().unwrap();
    }

    #[test]
    fn accept_rejects_hello_from_another_platform() {
        let mut drv = Driver::listen(0).unwrap();
        drv.expect_platform("android");
        let port = drv.port().unwrap();
        let handle = std::thread::spawn(move || {
            // An iOS simulator app dialing the same host port must not win.
            let mut ios = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(ios, r#"{{"event":"hello","params":{{"protocol":1,"pid":7,"platform":"ios"}}}}"#).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            let mut android = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(android, r#"{{"event":"hello","params":{{"protocol":1,"pid":8,"platform":"android"}}}}"#).unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        let outcome = drv.accept_with_pid(Duration::from_secs(2), None, &[]).unwrap();
        assert_eq!(outcome.hello["pid"], 8);
        assert!(outcome.rejected_connections >= 1);
        handle.join().unwrap();
    }

    /// Connects a fake app with `pid`, runs `serve` on its socket, and
    /// returns the accepted driver.
    fn fake_app(pid: i64, serve: impl FnOnce(TcpStream) + Send + 'static) -> (Driver, std::thread::JoinHandle<()>) {
        let mut drv = Driver::listen(0).unwrap();
        let port = drv.port().unwrap();
        let handle = std::thread::spawn(move || {
            let mut app = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(app, r#"{{"event":"hello","params":{{"protocol":1,"pid":{pid}}}}}"#).unwrap();
            serve(app);
        });
        drv.accept_with_pid(Duration::from_secs(2), Some(pid), &[]).unwrap();
        (drv, handle)
    }

    fn read_request_id(app: &TcpStream) -> u64 {
        let mut line = String::new();
        BufReader::new(app.try_clone().unwrap()).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()["id"].as_u64().unwrap()
    }

    #[test]
    fn dead_app_fails_the_call_within_seconds_even_with_the_socket_open() {
        // adb reverse keeps the host socket open after the app dies: the app
        // side here never answers and never closes.
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (mut drv, handle) = fake_app(4321, move |app| {
            let _ = read_request_id(&app);
            let _ = stop_rx.recv_timeout(Duration::from_secs(15));
            drop(app);
        });
        let probes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = probes.clone();
        drv.set_liveness_probe(Box::new(move |pid| {
            assert_eq!(pid, 4321);
            // Alive at the first check, gone at the second.
            Some(seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0)
        }));
        let t0 = Instant::now();
        let err = drv.call("tapOn", json!({})).unwrap_err();
        let waited = t0.elapsed();
        assert_eq!(err.to_string(), "app process 4321 died during `tapOn`");
        assert!(waited >= Duration::from_millis(1900) && waited < Duration::from_secs(3), "{waited:?}");
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(!drv.is_connected());
        stop_tx.send(()).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn slow_partial_responses_and_unknown_probes_keep_waiting() {
        let (mut drv, handle) = fake_app(7, |mut app| {
            let id = read_request_id(&app);
            // Half a line, a pause longer than the poll interval and the
            // first probe, then the rest.
            write!(app, r#"{{"id":{id},"result":"#).unwrap();
            app.flush().unwrap();
            std::thread::sleep(Duration::from_millis(1300));
            writeln!(app, r#""pong","us":1,"issues":[{{"seq":3,"id":3,"kind":"uncaughtError","request":{id}}}]}}"#)
                .unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        // A probe that cannot tell (adb hiccup) never fails the call.
        drv.set_liveness_probe(Box::new(|_| None));
        assert_eq!(drv.call("assertVisible", json!({})).unwrap(), "pong");
        assert_eq!(drv.take_issue_batch().current.len(), 1);
        handle.join().unwrap();
    }

    #[test]
    fn http_entries_piggyback_split_by_step_and_reread_with_in_flight() {
        let (mut drv, handle) = fake_app(11, |mut app| {
            let mut reader = BufReader::new(app.try_clone().unwrap());
            let mut next = || {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                serde_json::from_str::<Value>(&line).unwrap()
            };
            // Step 1 (request 1): one entry started by it.
            let id = next()["id"].as_u64().unwrap();
            writeln!(app, r#"{{"id":{id},"result":null,"us":1,"http":[{{"seq":1,"method":"GET","url":"http://h/a","request":{id}}}]}}"#).unwrap();
            // Step 2 (request 2): a late entry from request 1, two dropped.
            let id2 = next()["id"].as_u64().unwrap();
            writeln!(app, r#"{{"id":{id2},"result":null,"us":1,"droppedHttp":2,"http":[{{"seq":2,"method":"GET","url":"http://h/b","request":{id}}}]}}"#).unwrap();
            // Flow end: `httpLog` re-read returns one new entry and one in flight.
            let req = next();
            assert_eq!(req["method"], "httpLog");
            assert_eq!(req["params"]["since"], 2);
            let id3 = req["id"].as_u64().unwrap();
            writeln!(app, r#"{{"id":{id3},"result":{{"entries":[{{"seq":2,"url":"dup"}},{{"seq":3,"method":"GET","url":"http://h/c","request":{id2}}}],"inFlight":[{{"url":"http://h/slow","state":"inFlight"}}],"dropped":3}},"us":1}}"#).unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        assert!(!drv.http_log_enabled(), "not confirmed by configure yet");
        assert!(drv.poll_http().is_empty(), "no httpLog RPC without the log");
        drv.set_http_log(true);
        drv.take_http_batch();
        drv.call("tapOn", json!({})).unwrap();
        let first = drv.take_http_batch();
        assert_eq!(first.current.len(), 1);
        assert!(first.late.is_empty());
        drv.call("tapOn", json!({})).unwrap();
        let second = drv.take_http_batch();
        assert!(second.current.is_empty());
        assert_eq!(second.late[0]["url"], "http://h/b");
        assert_eq!(second.dropped, 2);
        let in_flight = drv.poll_http();
        assert_eq!(in_flight[0]["url"], "http://h/slow");
        let tail = drv.take_http_batch();
        assert_eq!(tail.late.len(), 1, "seq 2 is not re-delivered");
        assert_eq!(tail.late[0]["url"], "http://h/c");
        assert_eq!(tail.dropped, 1);
        handle.join().unwrap();
    }

    #[test]
    fn silence_is_a_typed_error_with_the_production_message() {
        let err = anyhow::Error::new(RpcSilenceError { method: "configure".into(), silence: RPC_READ_TIMEOUT });
        assert!(is_rpc_silence(&err));
        assert_eq!(
            err.to_string(),
            "connection to app lost during `configure`: no response for 10 s (app hung or unreachable)"
        );
        assert!(!is_rpc_silence(&anyhow!("app closed the connection during `configure`")));

        let (mut drv, handle) = fake_app(5, |app| {
            let _ = read_request_id(&app);
            // Stays silent until the host hangs up.
            let _ = BufReader::new(app).read_line(&mut String::new());
        });
        drv.set_silence_timeout(Duration::from_millis(300));
        let t0 = Instant::now();
        let err = drv.call("waitIdle", json!({})).unwrap_err();
        assert!(is_rpc_silence(&err));
        assert!(t0.elapsed() >= Duration::from_millis(300) && t0.elapsed() < Duration::from_secs(2));
        assert!(err.to_string().contains("no response for 300 ms"), "{err}");
        assert!(!drv.is_connected());
        drop(drv);
        handle.join().unwrap();
    }

    /// Regression (deterministic): `disconnect()` used to close the connection first and drain the
    /// backlog afterwards, so a peer that redialed on EOF was dropped as stale. The test hook dials
    /// in at the exact moment the old connection is closed.
    #[test]
    fn disconnect_keeps_a_reconnect_triggered_by_closing_the_old_connection() {
        let (mut drv, handle) = fake_app(60, |app| {
            let _ = BufReader::new(app).read_line(&mut String::new());
        });
        let port = drv.port().unwrap();
        let redial: std::sync::Arc<std::sync::Mutex<Option<TcpStream>>> = Default::default();
        let slot = redial.clone();
        drv.on_conn_closed = Some(Box::new(move || {
            let mut again = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(again, r#"{{"event":"hello","params":{{"protocol":1,"pid":60}}}}"#).unwrap();
            *slot.lock().unwrap() = Some(again);
        }));
        drv.disconnect();
        assert!(!drv.is_connected());
        let outcome = drv
            .accept_same_process(Duration::from_secs(2), Some(60))
            .unwrap_or_else(|e| panic!("the reconnect was drained by disconnect(): {e:#}"));
        assert_eq!(outcome.hello["pid"], 60);
        assert_eq!(outcome.rejected_connections, 0);
        handle.join().unwrap();
    }

    /// Same scenario with a real peer thread that redials on EOF, many times over.
    #[test]
    fn disconnect_keeps_a_real_peers_immediate_redial() {
        let mut drv = Driver::listen(0).unwrap();
        let port = drv.port().unwrap();
        for round in 0..50u32 {
            let pid = 1000 + i64::from(round);
            let handle = std::thread::spawn(move || {
                let mut app = TcpStream::connect(("127.0.0.1", port)).unwrap();
                writeln!(app, r#"{{"event":"hello","params":{{"protocol":1,"pid":{pid}}}}}"#).unwrap();
                // Blocks until `disconnect()` closes the host side (EOF), then dials in at once.
                let _ = BufReader::new(app.try_clone().unwrap()).read_line(&mut String::new());
                let mut again = TcpStream::connect(("127.0.0.1", port)).unwrap();
                writeln!(again, r#"{{"event":"hello","params":{{"protocol":1,"pid":{pid}}}}}"#).unwrap();
                // Keep the redialed socket open until the host has accepted it.
                let _ = BufReader::new(again).read_line(&mut String::new());
            });
            drv.accept_with_pid(Duration::from_secs(2), Some(pid), &[]).unwrap();
            drv.disconnect();
            assert!(!drv.is_connected());
            let outcome = drv
                .accept_same_process(Duration::from_secs(2), Some(pid))
                .unwrap_or_else(|e| panic!("round {round}: the reconnect was drained by disconnect(): {e:#}"));
            assert_eq!(outcome.hello["pid"], pid);
            drv.disconnect();
            handle.join().unwrap();
        }
    }

    #[test]
    fn disconnect_still_drops_stale_pending_connections_and_resets_state() {
        let (mut drv, handle) = fake_app(21, |app| {
            // Hold the connection until the host hangs up.
            let _ = BufReader::new(app).read_line(&mut String::new());
        });
        // A stale process dialed in while the old connection was still live.
        let mut stale = TcpStream::connect(("127.0.0.1", drv.port().unwrap())).unwrap();
        writeln!(stale, r#"{{"event":"hello","params":{{"protocol":1,"pid":99}}}}"#).unwrap();
        assert!(drv.hello.is_some() && drv.is_connected());
        // Give the loopback accept queue a moment to hold the stale socket.
        std::thread::sleep(Duration::from_millis(50));
        drv.disconnect();
        assert!(drv.hello.is_none() && !drv.is_connected());
        // The stale socket was drained (closed by the host): nothing to accept anymore.
        let err = drv.accept_with_pid(Duration::from_millis(200), None, &[]).unwrap_err();
        assert!(err.to_string().contains("did not connect"), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn calling_without_a_connection_is_a_clear_error() {
        let mut drv = Driver::listen(0).unwrap();
        assert!(!drv.is_connected());
        assert!(drv.last_pid().is_none());
        let err = drv.call("ping", json!({})).unwrap_err();
        assert_eq!(err.to_string(), "app is not connected");
        assert!(err.downcast_ref::<TransportError>().is_some());
        assert!(crate::backend::is_transport_error(&err));
        // Idle polling helpers never touch the network.
        drv.poll_issues();
        assert!(drv.poll_http().is_empty());
        drv.disconnect();
    }

    #[test]
    fn accept_times_out_with_an_actionable_message() {
        let mut drv = Driver::listen(0).unwrap();
        let err = drv.accept(Duration::from_millis(50)).unwrap_err();
        assert_eq!(err.to_string(), "app did not connect within 50 ms (is tapwing_driver in the build?)");
    }

    #[test]
    fn a_hello_without_platform_passes_the_platform_check_and_malformed_hellos_are_rejected() {
        let mut drv = Driver::listen(0).unwrap();
        drv.expect_platform("ios");
        let port = drv.port().unwrap();
        let handle = std::thread::spawn(move || {
            // Not JSON, wrong event, then a hello without `platform` (accepted).
            // Wait for the server to reject and drop each bad socket (EOF) before dialing `ok`
            // so the rejection count is deterministic under load.
            let mut junk = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(junk, "not json").unwrap();
            let _ = BufReader::new(junk).read_line(&mut String::new());

            let mut other = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(other, r#"{{"event":"progress","params":{{}}}}"#).unwrap();
            let _ = BufReader::new(other).read_line(&mut String::new());

            let mut ok = TcpStream::connect(("127.0.0.1", port)).unwrap();
            writeln!(ok, r#"{{"event":"hello","params":{{"protocol":1,"pid":5}}}}"#).unwrap();
            // Stay open until the host disconnects / drops `drv`.
            let _ = BufReader::new(ok).read_line(&mut String::new());
        });
        let outcome = drv.accept_with_pid(Duration::from_secs(2), None, &[]).unwrap();
        assert_eq!(outcome.hello["pid"], 5);
        assert_eq!(drv.last_pid(), Some(5));
        assert!(outcome.rejected_connections >= 2, "{outcome:?}");
        drop(drv);
        handle.join().unwrap();
    }

    #[test]
    fn progress_heartbeats_extend_the_silence_timeout() {
        let (mut drv, handle) = fake_app(31, |mut app| {
            let id = read_request_id(&app);
            // 4 x 200 ms of heartbeats: 800 ms in total, longer than the 600 ms silence window
            // with generous margin against scheduler jitter.
            for _ in 0..4 {
                std::thread::sleep(Duration::from_millis(200));
                writeln!(app, r#"{{"event":"progress","id":{id}}}"#).unwrap();
            }
            writeln!(app, r#"{{"id":{id},"result":"done","us":1}}"#).unwrap();
            let _ = BufReader::new(app).read_line(&mut String::new());
        });
        drv.set_silence_timeout(Duration::from_millis(600));
        assert_eq!(drv.call("waitIdle", json!({})).unwrap(), "done");
        assert!(drv.is_connected());
        drop(drv);
        handle.join().unwrap();
    }

    #[test]
    fn driver_errors_carry_code_message_and_details_and_keep_the_connection() {
        let (mut drv, handle) = fake_app(41, |mut app| {
            let mut reader = BufReader::new(app.try_clone().unwrap());
            for _ in 0..2 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let id = serde_json::from_str::<Value>(&line).unwrap()["id"].as_u64().unwrap();
                writeln!(
                    app,
                    r#"{{"id":{id},"error":{{"code":"ELEMENT_NOT_FOUND","message":"no such element","x":1}}}}"#
                )
                .unwrap();
            }
            let _ = reader.read_line(&mut String::new());
        });
        let err = drv.call("tapOn", json!({})).unwrap_err();
        assert_eq!(err.to_string(), "no such element");
        let de = err.downcast_ref::<DriverError>().expect("a DriverError");
        assert_eq!(de.code, "ELEMENT_NOT_FOUND");
        assert_eq!(de.details["x"], 1);
        assert!(!is_rpc_silence(&err));
        // A driver error is an answer, not a transport failure.
        assert!(drv.is_connected());
        let err = drv.call("tapOn", json!({})).unwrap_err();
        assert!(err.downcast_ref::<DriverError>().is_some());
        drop(drv);
        handle.join().unwrap();
    }

    #[test]
    fn invalid_json_from_the_driver_drops_the_connection() {
        let (mut drv, handle) = fake_app(51, |mut app| {
            let _ = read_request_id(&app);
            writeln!(app, "this is not json").unwrap();
            let _ = BufReader::new(app).read_line(&mut String::new());
        });
        let err = drv.call("tapOn", json!({})).unwrap_err();
        assert!(err.to_string().starts_with("invalid JSON from driver during `tapOn`"), "{err}");
        assert!(err.downcast_ref::<TransportError>().is_some());
        assert!(crate::backend::is_transport_error(&err));
        assert!(!drv.is_connected());
        drop(drv);
        handle.join().unwrap();
    }

    #[test]
    fn human_duration_formats_whole_seconds_and_milliseconds() {
        assert_eq!(human_duration(Duration::from_secs(10)), "10 s");
        assert_eq!(human_duration(Duration::from_millis(300)), "300 ms");
        assert_eq!(human_duration(Duration::from_millis(1500)), "1500 ms");
        assert_eq!(human_duration(Duration::ZERO), "0 ms");
    }

    #[test]
    fn closed_socket_still_fails_immediately() {
        let (mut drv, handle) = fake_app(9, |app| {
            let _ = read_request_id(&app);
            drop(app);
        });
        let t0 = Instant::now();
        let err = drv.call("tapOn", json!({})).unwrap_err();
        assert_eq!(err.to_string(), "app closed the connection during `tapOn`");
        assert!(crate::backend::is_transport_error(&err));
        assert!(t0.elapsed() < Duration::from_millis(500));
        handle.join().unwrap();
    }
}
