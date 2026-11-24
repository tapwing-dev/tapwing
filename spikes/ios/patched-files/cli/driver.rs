//! Connection to the in-app driver. The app dials out to us
//! (device localhost:7781 --adb reverse--> host port).

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

pub struct Driver {
    listener: TcpListener,
    conn: Option<Conn>,
    pub hello: Option<Value>,
}

struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
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

impl Driver {
    pub fn listen(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).with_context(|| format!("binding 127.0.0.1:{port}"))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, conn: None, hello: None })
    }

    /// The host port actually bound (useful when listening on port 0).
    pub fn port(&self) -> Result<u16> {
        Ok(self.listener.local_addr()?.port())
    }

    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    /// Forget the current connection (e.g. the app is being restarted).
    pub fn disconnect(&mut self) {
        self.conn = None;
        self.hello = None;
        // Drain stale pending connections from a previous process.
        while let Ok((s, _)) = self.listener.accept() {
            drop(s);
        }
    }

    /// Wait for the app to connect and say hello.
    pub fn accept(&mut self, timeout: Duration) -> Result<&Value> {
        self.accept_pid(timeout, None)
    }

    /// Like [`Driver::accept`], but ignores hellos from other processes (a
    /// previous instance still redialing while it dies).
    pub fn accept_pid(&mut self, timeout: Duration, pid: Option<i64>) -> Result<&Value> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                bail!("app did not connect within {} ms (is tapwing_driver in the build?)", timeout.as_millis());
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_nodelay(true)?;
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    stream
                        .set_read_timeout(Some(remaining.min(Duration::from_secs(5)).max(Duration::from_millis(1))))?;
                    let mut reader = BufReader::new(stream.try_clone()?);
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(n) if n > 0 => {
                            if let Ok(msg) = serde_json::from_str::<Value>(&line)
                                && msg["event"] == "hello"
                                && pid.is_none_or(|p| msg["params"]["pid"].as_i64() == Some(p))
                            {
                                stream.set_read_timeout(None)?;
                                self.conn = Some(Conn { reader, writer: stream, next_id: 1 });
                                self.hello = Some(msg["params"].clone());
                                return Ok(self.hello.as_ref().unwrap());
                            }
                        }
                        _ => {}
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let read_timeout_ms =
            params.get("timeoutMs").and_then(Value::as_u64).unwrap_or(0).saturating_add(15_000).max(30_000);
        let conn = self.conn.as_mut().ok_or_else(|| anyhow!("app is not connected"))?;
        conn.writer.set_read_timeout(Some(Duration::from_millis(read_timeout_ms)))?;
        let id = conn.next_id;
        conn.next_id += 1;
        let req = json!({ "id": id, "method": method, "params": params });
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        if let Err(e) = conn.writer.write_all(line.as_bytes()) {
            self.conn = None;
            return Err(anyhow!("connection to app lost: {e}"));
        }
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = match conn.reader.read_line(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    self.conn = None;
                    return Err(anyhow!("connection to app lost during `{method}`: {e}"));
                }
            };
            if n == 0 {
                self.conn = None;
                bail!("app closed the connection during `{method}`");
            }
            let msg: Value = match serde_json::from_str(&buf) {
                Ok(v) => v,
                Err(e) => {
                    self.conn = None;
                    return Err(anyhow!("invalid JSON from driver during `{method}`: {e}"));
                }
            };
            if msg["id"].as_u64() != Some(id) {
                continue; // event or stale response
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
