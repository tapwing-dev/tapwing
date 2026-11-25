//! Pure-Rust ADB smartsocket & emulator console client.
//!
//! Speaks the ADB server wire protocol directly over loopback TCP
//! (`127.0.0.1:5037` by default, or `ANDROID_ADB_SERVER_HOST` /
//! `ANDROID_ADB_SERVER_PORT`), avoiding the ~50–75 ms process spawn overhead
//! of shelling out to the `adb` CLI binary for every operation.

mod emu;
#[cfg(test)]
pub(crate) mod fake;
mod legacy_shell;
mod sync;

pub use sync::SYNC_DATA_MAX;

use anyhow::{Context, Result, bail};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

/// Default ADB server host when `ANDROID_ADB_SERVER_HOST` is not set.
pub const DEFAULT_ADB_HOST: &str = "127.0.0.1";

/// Default ADB server TCP port when `ANDROID_ADB_SERVER_PORT` is not set.
pub const DEFAULT_ADB_PORT: u16 = 5037;

/// Safety cap on a single inbound `shell,v2` packet (16 MiB).
const MAX_SHELL_V2_PACKET: usize = 16 * 1024 * 1024;

/// Default TCP connect timeout to the local ADB server.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Default read/write timeout for an active ADB command stream.
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Quick connect probe timeout used by [`AdbClient::is_available`].
const AVAILABILITY_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Shell v2 packet IDs (`adb/shell_protocol.h`).
const SHELL_V2_ID_STDOUT: u8 = 1;
const SHELL_V2_ID_STDERR: u8 = 2;
const SHELL_V2_ID_EXIT: u8 = 3;

/// Explicit `FAIL<4-hex-len><msg>` status returned by the ADB server or device.
#[derive(Debug)]
struct AdbFailError(String);

impl std::fmt::Display for AdbFailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ADB error: {}", self.0)
    }
}

impl std::error::Error for AdbFailError {}

/// Captured output and exit status of a remote `adb shell` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl ShellOutput {
    /// Returns `true` when the remote command exited with status `0`.
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }

    /// Concatenates `stdout` and `stderr` into a single string.
    pub fn combined(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}{}", self.stdout, self.stderr)
        }
    }
}

/// Lightweight, thread-safe client configuration for talking to the local ADB
/// server daemon and Android Emulator console over TCP.
///
/// Each method opens a short-lived loopback [`TcpStream`] to the ADB server
/// (`~0.1 ms` on localhost), performs the length-prefixed protocol exchange,
/// and closes the stream when complete.
#[derive(Clone, Debug)]
pub struct AdbClient {
    addr: SocketAddr,
    connect_timeout: Duration,
    io_timeout: Duration,
    emu_auth_token_path: Option<PathBuf>,
}

impl Default for AdbClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AdbClient {
    /// Creates an [`AdbClient`] targeting the ADB server address resolved from
    /// `ANDROID_ADB_SERVER_HOST` and `ANDROID_ADB_SERVER_PORT` (defaulting to
    /// `127.0.0.1:5037`).
    pub fn new() -> Self {
        let host = std::env::var("ANDROID_ADB_SERVER_HOST").ok();
        let port = std::env::var("ANDROID_ADB_SERVER_PORT").ok();
        Self::with_addr(resolve_server_addr(host.as_deref(), port.as_deref()))
    }

    /// Creates an [`AdbClient`] targeting an explicit [`SocketAddr`].
    pub fn with_addr(addr: SocketAddr) -> Self {
        Self {
            addr,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            io_timeout: DEFAULT_IO_TIMEOUT,
            emu_auth_token_path: emu::default_emu_auth_token_path(),
        }
    }

    /// Configures the TCP connect and read/write timeouts.
    pub fn with_timeouts(mut self, connect_timeout: Duration, io_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self.io_timeout = io_timeout;
        self
    }

    /// Overrides the path to `.emulator_console_auth_token` used by
    /// [`AdbClient::emu_command`].
    pub fn with_emu_auth_token_path(mut self, path: Option<PathBuf>) -> Self {
        self.emu_auth_token_path = path;
        self
    }

    /// Returns the configured ADB server [`SocketAddr`].
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Probes whether the ADB server daemon is reachable on the configured
    /// loopback socket.
    pub fn is_available(&self) -> bool {
        let timeout = self.connect_timeout.min(AVAILABILITY_PROBE_TIMEOUT);
        TcpStream::connect_timeout(&self.addr, timeout).is_ok()
    }

    /// Lists all connected devices and their states via `host:devices`
    /// (e.g. `[("emulator-5554", "device"), ("R5CX123", "offline")]`).
    pub fn devices(&self) -> Result<Vec<(String, String)>> {
        let mut stream = self.connect()?;
        send_command(&mut stream, "host:devices").context("sending host:devices")?;
        let payload = read_hex_length_prefixed_string(&mut stream).context("reading host:devices payload")?;
        Ok(parse_devices_payload(&payload))
    }

    /// Lists the serials of connected devices that are ready (`"device"` state).
    pub fn ready_devices(&self) -> Result<Vec<String>> {
        Ok(self.devices()?.into_iter().filter_map(|(serial, state)| (state == "device").then_some(serial)).collect())
    }

    /// Executes a command on `serial` via `shell,v2,TERM=dumb:<cmd>`, returning
    /// separated `stdout`, `stderr`, and `exit_code`.
    ///
    /// Falls back to the legacy `shell:<cmd>` service (stdout and stderr
    /// merged into `stdout`, exit status recovered by [`legacy_shell`]) only
    /// when the `shell,v2` request is rejected with `FAIL` **and** the device
    /// does not advertise the `shell_v2` feature. Any other `FAIL`, a socket
    /// drop or a timeout is returned as the error it is.
    pub fn shell(&self, serial: &str, cmd: &str) -> Result<ShellOutput> {
        let mut stream = self.connect_transport(serial)?;
        let v2_cmd = format!("shell,v2,TERM=dumb:{cmd}");
        match send_command(&mut stream, &v2_cmd) {
            Ok(()) => read_shell_v2_output(&mut stream).with_context(|| format!("reading shell,v2 output for `{cmd}`")),
            // A device that cannot open the service answers a bare `FAIL closed`,
            // the same as for any other refused service, so the reason is taken
            // from the feature list instead of the message. When that cannot be
            // read either, the rejection is reported rather than guessed around.
            Err(err) if err.is::<AdbFailError>() && matches!(self.supports_shell_v2(serial), Ok(false)) => {
                self.legacy_shell(serial, cmd)
            }
            Err(err) => Err(err).with_context(|| format!("sending `{v2_cmd}`")),
        }
    }

    /// Whether `serial` advertises the `shell_v2` feature
    /// (`host-serial:<serial>:features`, what the `adb` CLI checks before it
    /// picks a shell protocol). Devices before Android 7 report no features.
    fn supports_shell_v2(&self, serial: &str) -> Result<bool> {
        let mut stream = self.connect()?;
        let cmd = format!("host-serial:{serial}:features");
        send_command(&mut stream, &cmd).with_context(|| format!("sending `{cmd}`"))?;
        let features = read_hex_length_prefixed_string(&mut stream).with_context(|| format!("reading `{cmd}`"))?;
        Ok(features.split(',').any(|feature| feature.trim() == "shell_v2"))
    }

    /// Runs `cmd` over the legacy `shell:` service on a fresh transport (the
    /// rejected `shell,v2` request closed the previous one).
    fn legacy_shell(&self, serial: &str, cmd: &str) -> Result<ShellOutput> {
        let marker = legacy_shell::next_exit_marker();
        let mut stream = self.connect_transport(serial)?;
        let legacy_cmd = format!("shell:{}", legacy_shell::legacy_shell_script(cmd, &marker));
        send_command(&mut stream, &legacy_cmd).with_context(|| format!("sending legacy `shell:{cmd}`"))?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).with_context(|| format!("reading legacy shell output for `{cmd}`"))?;
        legacy_shell::parse_legacy_shell_output(&raw, &marker)
            .with_context(|| format!("reading legacy shell output for `{cmd}`"))
    }

    /// Executes `exec:<cmd>` on `serial` and streams raw, unmangled binary
    /// bytes until EOF (no PTY `\r\n` translation; suitable for
    /// `screencap -p`).
    pub fn exec_out(&self, serial: &str, cmd: &str) -> Result<Vec<u8>> {
        let mut stream = self.connect_transport(serial)?;
        let exec_cmd = format!("exec:{cmd}");
        send_command(&mut stream, &exec_cmd).with_context(|| format!("sending `{exec_cmd}`"))?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).with_context(|| format!("reading exec output for `{cmd}`"))?;
        Ok(buf)
    }

    /// Creates a reverse socket forward on `serial` (`reverse:forward:<remote>;<local>`,
    /// e.g. `remote = "tcp:9876"`, `local = "tcp:54321"`).
    pub fn reverse(&self, serial: &str, remote: &str, local: &str) -> Result<()> {
        let mut stream = self.connect_transport(serial)?;
        let cmd = format!("reverse:forward:{remote};{local}");
        send_command(&mut stream, &cmd).with_context(|| format!("sending `{cmd}`"))?;
        read_optional_status(&mut stream).with_context(|| format!("completing `{cmd}`"))
    }

    /// Removes a reverse socket forward on `serial` (`reverse:killforward:<remote>`).
    pub fn reverse_remove(&self, serial: &str, remote: &str) -> Result<()> {
        let mut stream = self.connect_transport(serial)?;
        let cmd = format!("reverse:killforward:{remote}");
        send_command(&mut stream, &cmd).with_context(|| format!("sending `{cmd}`"))?;
        read_optional_status(&mut stream).with_context(|| format!("completing `{cmd}`"))
    }

    /// Creates a host-to-device port forward (`host-serial:<serial>:forward:<local>;<remote>`,
    /// e.g. `local = "tcp:9876"`, `remote = "tcp:9876"`).
    pub fn forward(&self, serial: &str, local: &str, remote: &str) -> Result<()> {
        let mut stream = self.connect()?;
        let cmd = format!("host-serial:{serial}:forward:{local};{remote}");
        send_command(&mut stream, &cmd).with_context(|| format!("sending `{cmd}`"))?;
        read_optional_status(&mut stream).with_context(|| format!("completing `{cmd}`"))
    }

    /// Creates an ephemeral host-to-device port forward
    /// (`host-serial:<serial>:forward:tcp:0;<remote>`) and returns the host TCP
    /// port allocated by the ADB server.
    ///
    /// Wire protocol (`adb/adb.cpp` / `adb/adb_listeners.cpp`): on error (bad
    /// serial, bind failure), the server replies with `FAIL<4-hex-len><msg>` as
    /// the first status (handled by [`send_command`]). On success, it replies
    /// with two `OKAY` statuses (first for command dispatch, second for the
    /// bound listener) followed by a `<4-hex-len><ascii-port>` frame carrying
    /// the assigned port (e.g. `OKAYOKAY000554321`). The second status is still
    /// checked with [`read_status`] to guard against unexpected `FAIL` replies.
    pub fn forward_ephemeral(&self, serial: &str, remote: &str) -> Result<u16> {
        let mut stream = self.connect()?;
        let cmd = format!("host-serial:{serial}:forward:tcp:0;{remote}");
        send_command(&mut stream, &cmd).with_context(|| format!("sending `{cmd}`"))?;
        read_status(&mut stream).with_context(|| format!("completing `{cmd}`"))?;
        let payload = read_hex_length_prefixed_string(&mut stream)
            .with_context(|| format!("reading ephemeral port for `{cmd}`"))?;
        parse_ephemeral_port(&payload)
    }

    /// Removes a host-to-device port forward (`host-serial:<serial>:killforward:<local>`).
    pub fn forward_remove(&self, serial: &str, local: &str) -> Result<()> {
        let mut stream = self.connect()?;
        let cmd = format!("host-serial:{serial}:killforward:{local}");
        send_command(&mut stream, &cmd).with_context(|| format!("sending `{cmd}`"))?;
        read_optional_status(&mut stream).with_context(|| format!("completing `{cmd}`"))
    }

    fn connect(&self) -> Result<TcpStream> {
        let stream = TcpStream::connect_timeout(&self.addr, self.connect_timeout)
            .with_context(|| format!("connecting to ADB server at {}", self.addr))?;
        stream.set_read_timeout(Some(self.io_timeout)).context("setting ADB socket read timeout")?;
        stream.set_write_timeout(Some(self.io_timeout)).context("setting ADB socket write timeout")?;
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }

    fn connect_transport(&self, serial: &str) -> Result<TcpStream> {
        let mut stream = self.connect()?;
        let cmd = format!("host:transport:{serial}");
        send_command(&mut stream, &cmd).with_context(|| format!("selecting device `{serial}` ({cmd})"))?;
        Ok(stream)
    }
}

/// Sends a 4-hex-digit length-prefixed ADB smartsocket command and verifies the
/// immediate `OKAY` / `FAIL` response status.
pub fn send_command<S: Read + Write>(stream: &mut S, cmd: &str) -> Result<()> {
    if cmd.len() > 0xffff {
        bail!("ADB command too long ({} bytes, max 65535)", cmd.len());
    }
    let framed = format!("{:04x}{cmd}", cmd.len());
    stream.write_all(framed.as_bytes()).with_context(|| format!("writing ADB command `{cmd}`"))?;
    stream.flush().with_context(|| format!("flushing ADB command `{cmd}`"))?;
    read_status(stream)
}

/// Reads a 4-byte ADB status header (`OKAY` or `FAIL<4-hex-len><msg>`).
pub fn read_status<R: Read>(stream: &mut R) -> Result<()> {
    let mut status = [0u8; 4];
    stream.read_exact(&mut status).context("reading 4-byte ADB status")?;
    parse_status_after_tag(stream, &status)
}

/// Reads an optional second `OKAY` / `FAIL` status sent by `forward` /
/// `reverse` services, treating an immediate clean EOF after the initial
/// `OKAY` as success.
fn read_optional_status<R: Read>(stream: &mut R) -> Result<()> {
    let mut status = [0u8; 4];
    let mut read = 0usize;
    while read < 4 {
        match stream.read(&mut status[read..]) {
            Ok(0) if read == 0 => return Ok(()),
            Ok(0) => bail!("unexpected EOF while reading secondary ADB status"),
            Ok(n) => read += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("reading secondary ADB status"),
        }
    }
    parse_status_after_tag(stream, &status)
}

fn parse_status_after_tag<R: Read>(stream: &mut R, status: &[u8; 4]) -> Result<()> {
    match status {
        b"OKAY" => Ok(()),
        b"FAIL" => {
            let msg = read_hex_length_prefixed_string(stream).unwrap_or_else(|_| "unknown ADB failure".to_string());
            Err(anyhow::Error::new(AdbFailError(msg)))
        }
        other => bail!("invalid ADB status header: {:?}", String::from_utf8_lossy(other)),
    }
}

/// Reads a `<4-hex-digit-len><utf8-payload>` frame from an ADB stream.
fn read_hex_length_prefixed_string<R: Read>(stream: &mut R) -> Result<String> {
    let mut len_hex = [0u8; 4];
    stream.read_exact(&mut len_hex).context("reading 4-hex-digit ADB payload length")?;
    let len_str = std::str::from_utf8(&len_hex).context("ADB payload length is not valid ASCII")?;
    let len =
        usize::from_str_radix(len_str, 16).with_context(|| format!("invalid hex length `{len_str}` in ADB frame"))?;
    read_exact_string(stream, len)
}

fn read_exact_string<R: Read>(stream: &mut R, len: usize) -> Result<String> {
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).with_context(|| format!("reading {len}-byte ADB payload"))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn parse_ephemeral_port(payload: &str) -> Result<u16> {
    let port: u16 =
        payload.trim().parse().with_context(|| format!("invalid ephemeral forward port payload: {payload:?}"))?;
    if port == 0 {
        bail!("ADB server returned port 0 for ephemeral forward");
    }
    Ok(port)
}

/// Parses `host:devices` payload lines (`<serial>\t<state>`).
pub fn parse_devices_payload(payload: &str) -> Vec<(String, String)> {
    payload
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mut parts = trimmed.splitn(2, ['\t', ' ']);
            let serial = parts.next()?.trim();
            let state = parts.next()?.trim();
            if serial.is_empty() || state.is_empty() {
                return None;
            }
            Some((serial.to_string(), state.to_string()))
        })
        .collect()
}

/// Demultiplexes ADB `shell,v2` packets (`1 = stdout`, `2 = stderr`,
/// `3 = exit_code`) until EOF. Requires a `SHELL_V2_ID_EXIT` packet so a
/// dropped connection or crashed `adbd` is never mistaken for exit status 0.
fn read_shell_v2_output<R: Read>(stream: &mut R) -> Result<ShellOutput> {
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut exit_code: Option<i32> = None;

    loop {
        let mut header = [0u8; 5];
        let mut read = 0usize;
        while read < 5 {
            match stream.read(&mut header[read..]) {
                Ok(0) if read == 0 => {
                    let Some(code) = exit_code else {
                        bail!("unexpected EOF in shell,v2 stream before exit status packet");
                    };
                    return Ok(ShellOutput {
                        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                        exit_code: code,
                    });
                }
                Ok(0) => bail!("truncated shell,v2 packet header ({read}/5 bytes)"),
                Ok(n) => read += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("reading shell,v2 packet header"),
            }
        }

        let id = header[0];
        let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if len > MAX_SHELL_V2_PACKET {
            bail!("shell,v2 packet too large ({len} bytes, max {MAX_SHELL_V2_PACKET})");
        }
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).with_context(|| format!("reading {len}-byte shell,v2 payload (id={id})"))?;

        match id {
            SHELL_V2_ID_STDOUT => stdout_bytes.extend_from_slice(&payload),
            SHELL_V2_ID_STDERR => stderr_bytes.extend_from_slice(&payload),
            SHELL_V2_ID_EXIT => {
                exit_code = Some(match payload.len() {
                    0 => 0,
                    1 => i32::from(payload[0]),
                    4.. => i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
                    _ => i32::from(payload[0]),
                });
            }
            _ => {}
        }
    }
}

/// Resolves the ADB server [`SocketAddr`] from optional host and port strings
/// (typically `ANDROID_ADB_SERVER_HOST` and `ANDROID_ADB_SERVER_PORT`),
/// falling back to `127.0.0.1:5037`.
pub fn resolve_server_addr(host: Option<&str>, port: Option<&str>) -> SocketAddr {
    let host = host.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(DEFAULT_ADB_HOST);
    let port = port.and_then(|p| p.trim().parse::<u16>().ok()).unwrap_or(DEFAULT_ADB_PORT);
    if let Ok(ip) = host.parse::<IpAddr>() {
        return SocketAddr::new(ip, port);
    }
    if let Ok(mut addrs) = (host, port).to_socket_addrs()
        && let Some(addr) = addrs.next()
    {
        return addr;
    }
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

#[cfg(test)]
mod tests {
    use super::sync::{read_sync_header, write_sync_header, write_sync_packet};
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    /// Helper that reads a 4-hex-digit framed command from a client socket.
    fn mock_read_command(stream: &mut TcpStream) -> String {
        let mut len_hex = [0u8; 4];
        stream.read_exact(&mut len_hex).expect("read 4-hex length");
        let len = usize::from_str_radix(std::str::from_utf8(&len_hex).unwrap(), 16).expect("valid hex len");
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).expect("read command payload");
        String::from_utf8(payload).expect("valid utf8 command")
    }

    fn mock_write_okay(stream: &mut TcpStream) {
        stream.write_all(b"OKAY").unwrap();
        stream.flush().unwrap();
    }

    fn mock_write_fail(stream: &mut TcpStream, msg: &str) {
        let frame = format!("FAIL{:04x}{msg}", msg.len());
        stream.write_all(frame.as_bytes()).unwrap();
        stream.flush().unwrap();
    }

    fn mock_write_shell_v2_packet(stream: &mut TcpStream, id: u8, payload: &[u8]) {
        let mut header = [0u8; 5];
        header[0] = id;
        header[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        stream.write_all(&header).unwrap();
        stream.write_all(payload).unwrap();
    }

    fn spawn_mock_adb_server<F>(connections: usize, handler: F) -> (AdbClient, thread::JoinHandle<()>)
    where
        F: Fn(usize, TcpStream) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap();
        let handler = Arc::new(handler);
        let handle = thread::spawn(move || {
            for idx in 0..connections {
                let (stream, _) = listener.accept().expect("accept mock connection");
                let h = Arc::clone(&handler);
                h(idx, stream);
            }
        });
        let client = AdbClient::with_addr(addr).with_timeouts(Duration::from_secs(2), Duration::from_secs(5));
        (client, handle)
    }

    #[test]
    fn resolve_server_addr_respects_host_and_port_overrides() {
        assert_eq!(resolve_server_addr(None, None), "127.0.0.1:5037".parse().unwrap());
        assert_eq!(resolve_server_addr(Some(""), Some("")), "127.0.0.1:5037".parse().unwrap());
        assert_eq!(resolve_server_addr(Some("127.0.0.2"), Some("5038")), "127.0.0.2:5038".parse().unwrap());
        assert_eq!(resolve_server_addr(Some("localhost"), Some("invalid")).port(), 5037);
    }

    #[test]
    fn is_available_probes_server_reachability() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = AdbClient::with_addr(addr);
        assert!(client.is_available());
        drop(listener);
        assert!(!client.is_available());
    }

    #[test]
    fn devices_and_ready_devices_parse_host_devices_response() {
        let (client, handle) = spawn_mock_adb_server(2, |_idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:devices");
            mock_write_okay(&mut stream);
            let body = "emulator-5554\tdevice\nR5CX999\toffline\n192.168.1.20:5555\tdevice\n";
            let framed = format!("{:04x}{body}", body.len());
            stream.write_all(framed.as_bytes()).unwrap();
        });

        let all = client.devices().unwrap();
        assert_eq!(
            all,
            vec![
                ("emulator-5554".to_string(), "device".to_string()),
                ("R5CX999".to_string(), "offline".to_string()),
                ("192.168.1.20:5555".to_string(), "device".to_string()),
            ]
        );

        let ready = client.ready_devices().unwrap();
        assert_eq!(ready, vec!["emulator-5554".to_string(), "192.168.1.20:5555".to_string()]);
        handle.join().unwrap();
    }

    #[test]
    fn shell_v2_demuxes_stdout_stderr_and_nonzero_exit_code() {
        let (client, handle) = spawn_mock_adb_server(1, |_idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "shell,v2,TERM=dumb:pm clear com.example");
            mock_write_okay(&mut stream);

            mock_write_shell_v2_packet(&mut stream, SHELL_V2_ID_STDOUT, b"out-part-1 ");
            mock_write_shell_v2_packet(&mut stream, SHELL_V2_ID_STDERR, b"err-line\n");
            mock_write_shell_v2_packet(&mut stream, SHELL_V2_ID_STDOUT, b"out-part-2\n");
            mock_write_shell_v2_packet(&mut stream, SHELL_V2_ID_EXIT, &[42]);
            stream.flush().unwrap();
        });

        let out = client.shell("emulator-5554", "pm clear com.example").unwrap();
        assert_eq!(out.stdout, "out-part-1 out-part-2\n");
        assert_eq!(out.stderr, "err-line\n");
        assert_eq!(out.exit_code, 42);
        assert!(!out.success());
        assert_eq!(out.combined(), "out-part-1 out-part-2\nerr-line\n");
        handle.join().unwrap();
    }

    /// A device as the ADB server presents it: the `shell,v2` request is
    /// answered with `v2_reply`, the feature query with `features`, and a
    /// legacy `shell:` request with whatever `legacy` returns for the command
    /// and its exit marker. Requests arrive on separate connections, in order.
    fn spawn_mock_legacy_device<L>(
        connections: usize,
        v2_fail: &'static str,
        features: Option<&'static str>,
        legacy: L,
    ) -> (AdbClient, thread::JoinHandle<()>)
    where
        L: Fn(&str, &str) -> Vec<u8> + Send + Sync + 'static,
    {
        spawn_mock_adb_server(connections, move |idx, mut stream| match idx {
            0 => {
                assert_eq!(mock_read_command(&mut stream), "host:transport:old-device");
                mock_write_okay(&mut stream);
                assert!(mock_read_command(&mut stream).starts_with("shell,v2,TERM=dumb:"));
                mock_write_fail(&mut stream, v2_fail);
            }
            1 => {
                assert_eq!(mock_read_command(&mut stream), "host-serial:old-device:features");
                match features {
                    Some(list) => {
                        mock_write_okay(&mut stream);
                        stream.write_all(format!("{:04x}{list}", list.len()).as_bytes()).unwrap();
                    }
                    None => mock_write_fail(&mut stream, "device offline"),
                }
            }
            _ => {
                assert_eq!(mock_read_command(&mut stream), "host:transport:old-device");
                mock_write_okay(&mut stream);
                let request = mock_read_command(&mut stream);
                // `shell:(<cmd>\n); echo "<marker>$?"`
                let script = request.strip_prefix("shell:(").expect("a legacy shell request");
                let (cmd, echo) = script.split_once("\n); echo \"").expect("the exit status wrapper");
                let marker = echo.strip_suffix("$?\"").expect("the exit marker");
                mock_write_okay(&mut stream);
                stream.write_all(&legacy(cmd, marker)).unwrap();
            }
        })
    }

    #[test]
    fn shell_falls_back_to_legacy_only_when_the_device_lacks_shell_v2() {
        // A pre-Android 7 device: the service is refused and no feature is advertised.
        let (client, handle) = spawn_mock_legacy_device(3, "closed", Some(""), |cmd, marker| {
            assert_eq!(cmd, "getprop ro.build.version.sdk");
            format!("23\r\n{marker}0\r\n").into_bytes()
        });
        let out = client.shell("old-device", "getprop ro.build.version.sdk").unwrap();
        assert_eq!(out, ShellOutput { stdout: "23\r\n".into(), stderr: String::new(), exit_code: 0 });
        assert!(out.success());
        handle.join().unwrap();

        // Features without `shell_v2` (an exact name, not a prefix match) also mean legacy.
        let (client, handle) = spawn_mock_legacy_device(3, "closed", Some("cmd,stat_v2,shell_v2x"), |_, marker| {
            format!("{marker}0\n").into_bytes()
        });
        assert!(client.shell("old-device", "true").unwrap().success());
        handle.join().unwrap();
    }

    #[test]
    fn legacy_shell_reports_the_real_exit_code() {
        let (client, handle) = spawn_mock_legacy_device(3, "closed", Some(""), |cmd, marker| {
            assert_eq!(cmd, "pm clear dev.missing");
            // Output without a trailing newline, then the status line.
            format!("Error: package not found{marker}1\n").into_bytes()
        });
        let out = client.shell("old-device", "pm clear dev.missing").unwrap();
        assert_eq!(out.stdout, "Error: package not found");
        assert_eq!(out.exit_code, 1);
        assert!(!out.success());
        handle.join().unwrap();

        // Bytes that are not UTF-8 before the marker do not hide the status.
        let (client, handle) = spawn_mock_legacy_device(3, "closed", Some(""), |_, marker| {
            let mut raw = vec![0x89, b'P', b'N', b'G', 0x00, 0xff, b'\n'];
            raw.extend_from_slice(format!("{marker}127\n").as_bytes());
            raw
        });
        let out = client.shell("old-device", "cat /sdcard/a.png; nope").unwrap();
        assert_eq!(out.exit_code, 127);
        assert_eq!(out.stdout, String::from_utf8_lossy(&[0x89, b'P', b'N', b'G', 0x00, 0xff, b'\n']));
        handle.join().unwrap();
    }

    #[test]
    fn legacy_shell_without_an_exit_marker_is_an_error_not_exit_code_zero() {
        // The device went away mid-command: EOF before the status line.
        let (client, handle) = spawn_mock_legacy_device(3, "closed", Some(""), |_, _| b"partial outp".to_vec());
        let err = format!("{:#}", client.shell("old-device", "long-cmd").unwrap_err());
        assert!(err.contains("reading legacy shell output for `long-cmd`"), "{err}");
        assert!(err.contains("ended without an exit status"), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn a_shell_v2_fail_on_a_device_with_shell_v2_is_an_error_not_a_fallback() {
        // Two connections only: the rejected request and the feature query. A
        // third (legacy) one would hang the mock's `join`.
        let (client, handle) =
            spawn_mock_legacy_device(2, "device offline", Some("shell_v2,cmd,stat_v2"), |_, _| unreachable!());
        let err = format!("{:#}", client.shell("old-device", "echo hi").unwrap_err());
        assert_eq!(err, "sending `shell,v2,TERM=dumb:echo hi`: ADB error: device offline");
        handle.join().unwrap();

        // The feature query itself fails: the original rejection is reported.
        let (client, handle) = spawn_mock_legacy_device(2, "closed", None, |_, _| unreachable!());
        let err = format!("{:#}", client.shell("old-device", "echo hi").unwrap_err());
        assert_eq!(err, "sending `shell,v2,TERM=dumb:echo hi`: ADB error: closed");
        handle.join().unwrap();
    }

    #[test]
    fn exec_out_preserves_arbitrary_binary_payload_without_mangling() {
        // Construct binary payload with PNG signature, embedded NULs, \r\n, and all 256 byte values.
        let mut expected = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0x00, 0x00, b'\r', b'\n'];
        expected.extend(0u8..=255u8);
        let expected_clone = expected.clone();

        let (client, handle) = spawn_mock_adb_server(1, move |_idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "exec:screencap -p");
            mock_write_okay(&mut stream);
            stream.write_all(&expected_clone).unwrap();
        });

        let bytes = client.exec_out("emulator-5554", "screencap -p").unwrap();
        assert_eq!(bytes, expected);
        handle.join().unwrap();
    }

    #[test]
    fn forward_and_reverse_commands_send_expected_wire_frames() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let (client, handle) = spawn_mock_adb_server(4, move |idx, mut stream| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            match idx {
                0 => {
                    assert_eq!(mock_read_command(&mut stream), "host-serial:emulator-5554:forward:tcp:9001;tcp:9002");
                    mock_write_okay(&mut stream);
                    mock_write_okay(&mut stream);
                }
                1 => {
                    assert_eq!(mock_read_command(&mut stream), "host-serial:emulator-5554:killforward:tcp:9001");
                    mock_write_okay(&mut stream);
                    mock_write_okay(&mut stream);
                }
                2 => {
                    assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
                    mock_write_okay(&mut stream);
                    assert_eq!(mock_read_command(&mut stream), "reverse:forward:tcp:7001;tcp:55001");
                    mock_write_okay(&mut stream);
                    mock_write_okay(&mut stream);
                }
                3 => {
                    assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
                    mock_write_okay(&mut stream);
                    assert_eq!(mock_read_command(&mut stream), "reverse:killforward:tcp:7001");
                    mock_write_okay(&mut stream);
                }
                _ => unreachable!(),
            }
        });

        client.forward("emulator-5554", "tcp:9001", "tcp:9002").unwrap();
        client.forward_remove("emulator-5554", "tcp:9001").unwrap();
        client.reverse("emulator-5554", "tcp:7001", "tcp:55001").unwrap();
        client.reverse_remove("emulator-5554", "tcp:7001").unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 4);
        handle.join().unwrap();
    }

    #[test]
    fn sync_push_and_pull_handle_multi_chunk_payloads_and_fail_errors() {
        // 150 KiB (> 2 * 64 KiB) payload to verify multi-chunk DATA splitting and reassembly.
        let large_payload: Vec<u8> = (0..(150 * 1024)).map(|i| (i % 251) as u8).collect();
        let expected_push = large_payload.clone();
        let pull_source = large_payload.clone();

        let (client, handle) = spawn_mock_adb_server(4, move |idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "sync:");
            mock_write_okay(&mut stream);

            match idx {
                0 => {
                    // Multi-chunk push
                    let (tag, spec_len) = read_sync_header(&mut stream).unwrap();
                    assert_eq!(&tag, b"SEND");
                    let spec = read_exact_string(&mut stream, spec_len as usize).unwrap();
                    assert_eq!(spec, "/data/local/tmp/blob.bin,420"); // 0o644 == 420

                    let mut received = Vec::new();
                    let mut chunk_count = 0usize;
                    loop {
                        let (ctag, clen) = read_sync_header(&mut stream).unwrap();
                        if &ctag == b"DATA" {
                            assert!(clen as usize <= SYNC_DATA_MAX);
                            let mut chunk = vec![0u8; clen as usize];
                            stream.read_exact(&mut chunk).unwrap();
                            received.extend_from_slice(&chunk);
                            chunk_count += 1;
                        } else if &ctag == b"DONE" {
                            assert!(clen > 0, "mtime should be non-zero");
                            break;
                        } else {
                            panic!("unexpected sync tag: {:?}", ctag);
                        }
                    }
                    assert_eq!(chunk_count, 3);
                    assert_eq!(received, expected_push);
                    write_sync_header(&mut stream, b"OKAY", 0).unwrap();
                }
                1 => {
                    // Multi-chunk pull
                    let (tag, path_len) = read_sync_header(&mut stream).unwrap();
                    assert_eq!(&tag, b"RECV");
                    let rpath = read_exact_string(&mut stream, path_len as usize).unwrap();
                    assert_eq!(rpath, "/data/local/tmp/blob.bin");

                    for chunk in pull_source.chunks(SYNC_DATA_MAX) {
                        write_sync_packet(&mut stream, b"DATA", chunk.len() as u32, chunk).unwrap();
                    }
                    write_sync_header(&mut stream, b"DONE", 0).unwrap();
                }
                2 => {
                    // Push FAIL
                    let (tag, spec_len) = read_sync_header(&mut stream).unwrap();
                    assert_eq!(&tag, b"SEND");
                    let _ = read_exact_string(&mut stream, spec_len as usize).unwrap();
                    loop {
                        let (ctag, clen) = read_sync_header(&mut stream).unwrap();
                        if &ctag == b"DATA" {
                            let mut discard = vec![0u8; clen as usize];
                            stream.read_exact(&mut discard).unwrap();
                        } else if &ctag == b"DONE" {
                            break;
                        }
                    }
                    let err_msg = b"Permission denied";
                    write_sync_packet(&mut stream, b"FAIL", err_msg.len() as u32, err_msg).unwrap();
                }
                3 => {
                    // Pull FAIL
                    let (tag, path_len) = read_sync_header(&mut stream).unwrap();
                    assert_eq!(&tag, b"RECV");
                    let _ = read_exact_string(&mut stream, path_len as usize).unwrap();
                    let err_msg = b"No such file or directory";
                    write_sync_packet(&mut stream, b"FAIL", err_msg.len() as u32, err_msg).unwrap();
                }
                _ => unreachable!(),
            }
        });

        let tmp_push = std::env::temp_dir().join(format!("tapwing_test_push_{}", std::process::id()));
        let tmp_pull = std::env::temp_dir().join(format!("tapwing_test_pull_{}", std::process::id()));
        std::fs::write(&tmp_push, &large_payload).unwrap();
        client.push_file("emulator-5554", &tmp_push, "/data/local/tmp/blob.bin", 0o644).unwrap();
        client.pull_to_file("emulator-5554", "/data/local/tmp/blob.bin", &tmp_pull).unwrap();
        let pulled = std::fs::read(&tmp_pull).unwrap();
        let _ = std::fs::remove_file(&tmp_push);
        let _ = std::fs::remove_file(&tmp_pull);
        assert_eq!(pulled, large_payload);

        let push_err = client.push("emulator-5554", b"hi", "/root/forbidden", 0o644).unwrap_err().to_string();
        assert!(push_err.contains("Permission denied"), "{push_err}");

        let pull_err = client.pull("emulator-5554", "/missing").unwrap_err().to_string();
        assert!(pull_err.contains("No such file or directory"), "{pull_err}");

        handle.join().unwrap();
    }

    #[test]
    fn emu_command_handles_auth_no_auth_ko_and_non_emulator_serial() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let serial = format!("emulator-{port}");

        // Create a temporary auth token file for the authenticated test case.
        let token_path = std::env::temp_dir().join(format!("tapwing_test_emu_token_{port}"));
        std::fs::write(&token_path, "secret-token-123\n").unwrap();

        let handle = thread::spawn(move || {
            // Connection 1: with auth token, command succeeds with OK.
            {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(b"Android Console: Authentication required\r\nOK\r\n").unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "auth secret-token-123\r\n");
                reader.get_mut().write_all(b"Android Console: type 'help' for a list of commands\r\nOK\r\n").unwrap();

                line.clear();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "geo fix 14.42076 50.08804\r\n");
                reader.get_mut().write_all(b"Location updated\r\nOK\r\n").unwrap();

                line.clear();
                let _ = reader.read_line(&mut line);
                assert_eq!(line, "quit\r\n");
            }

            // Connection 2: without auth token, command fails with KO.
            {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(b"Android Console: Welcome\r\nOK\r\n").unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "geo fix bad args\r\n");
                reader.get_mut().write_all(b"KO: invalid longitude\r\n").unwrap();
            }
        });

        let authed_client = AdbClient::new().with_emu_auth_token_path(Some(token_path.clone()));
        let out = authed_client.emu_command(&serial, "geo fix 14.42076 50.08804").unwrap();
        assert_eq!(out, "Location updated");

        let no_auth_client = AdbClient::new().with_emu_auth_token_path(None);
        let ko_err = no_auth_client.emu_command(&serial, "geo fix bad args").unwrap_err().to_string();
        assert!(ko_err.contains("KO: invalid longitude"), "{ko_err}");

        let non_emu_err = no_auth_client.emu_command("R5CX12345", "geo fix 0 0").unwrap_err().to_string();
        assert!(non_emu_err.contains("not an Android emulator"), "{non_emu_err}");

        let _ = std::fs::remove_file(&token_path);
        handle.join().unwrap();
    }

    #[test]
    fn shell_v2_rejects_premature_eof_without_exit_packet() {
        let (client, handle) = spawn_mock_adb_server(1, |_idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "shell,v2,TERM=dumb:long-cmd");
            mock_write_okay(&mut stream);
            mock_write_shell_v2_packet(&mut stream, SHELL_V2_ID_STDOUT, b"partial output");
            // Drop stream without sending SHELL_V2_ID_EXIT.
        });

        let err = client.shell("emulator-5554", "long-cmd").unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("before exit status packet"), "{chain}");
        handle.join().unwrap();
    }

    #[test]
    fn fail_error_propagates_on_bad_transport_or_command() {
        let (client, handle) = spawn_mock_adb_server(1, |_idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:missing-serial");
            mock_write_fail(&mut stream, "device 'missing-serial' not found");
        });

        let err = client.shell("missing-serial", "echo hello").unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("device 'missing-serial' not found"), "{chain}");
        handle.join().unwrap();
    }

    #[test]
    fn shell_does_not_fall_back_to_legacy_when_socket_drops() {
        let connections = Arc::new(AtomicUsize::new(0));
        let connections_clone = Arc::clone(&connections);
        // Only 1 connection is expected because a dropped socket must NOT trigger
        // a reconnect and legacy `shell:` retry.
        let (client, handle) = spawn_mock_adb_server(1, move |_idx, mut stream| {
            connections_clone.fetch_add(1, Ordering::SeqCst);
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "shell,v2,TERM=dumb:echo hi");
            // Drop the socket without sending OKAY or FAIL.
        });

        let err = client.shell("emulator-5554", "echo hi").unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("sending `shell,v2,TERM=dumb:echo hi`"), "{chain}");
        assert_eq!(connections.load(Ordering::SeqCst), 1);
        handle.join().unwrap();
    }

    #[test]
    fn push_recovers_pending_fail_packet_when_remote_closes_early() {
        // Large 4 MiB payload so writing DATA chunks after the server sends FAIL
        // and aborts early hits BrokenPipe / early FAIL recovery before DONE.
        let big_payload = vec![0xAB; 4 * 1024 * 1024];

        let (client, handle) = spawn_mock_adb_server(1, |_idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "sync:");
            mock_write_okay(&mut stream);

            let (tag, spec_len) = read_sync_header(&mut stream).unwrap();
            assert_eq!(&tag, b"SEND");
            let _spec = read_exact_string(&mut stream, spec_len as usize).unwrap();

            // Read the first DATA chunk, then immediately reject with FAIL and
            // shut down our write side while draining any in-flight client bytes
            // so the kernel delivers a clean FIN instead of an unread-data RST.
            let (ctag, clen) = read_sync_header(&mut stream).unwrap();
            assert_eq!(&ctag, b"DATA");
            let mut first_chunk = vec![0u8; clen as usize];
            stream.read_exact(&mut first_chunk).unwrap();

            let err_msg = b"No space left on device";
            write_sync_packet(&mut stream, b"FAIL", err_msg.len() as u32, err_msg).unwrap();
            stream.flush().unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let mut drain = [0u8; 16 * 1024];
            while let Ok(n) = stream.read(&mut drain) {
                if n == 0 {
                    break;
                }
            }
        });

        let err = client.push("emulator-5554", &big_payload, "/sdcard/huge.mp4", 0o644).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("No space left on device"), "expected remote FAIL reason, got: {msg}");
        handle.join().unwrap();
    }

    #[test]
    fn pull_to_file_streams_to_disk_and_leaves_no_partial_file_on_fail() {
        let dir = std::env::temp_dir().join(format!("tapwing_stream_pull_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok_dest = dir.join("nested/recording.mp4");
        let fail_dest = dir.join("failed.mp4");

        // 200 KiB payload across 4 DATA chunks.
        let payload: Vec<u8> = (0..(200 * 1024)).map(|i| (i % 253) as u8).collect();
        let payload_server = payload.clone();

        let (client, handle) = spawn_mock_adb_server(2, move |idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "sync:");
            mock_write_okay(&mut stream);

            let (tag, path_len) = read_sync_header(&mut stream).unwrap();
            assert_eq!(&tag, b"RECV");
            let _rpath = read_exact_string(&mut stream, path_len as usize).unwrap();

            if idx == 0 {
                for chunk in payload_server.chunks(SYNC_DATA_MAX) {
                    write_sync_packet(&mut stream, b"DATA", chunk.len() as u32, chunk).unwrap();
                }
                write_sync_header(&mut stream, b"DONE", 0).unwrap();
            } else {
                // Send one partial DATA chunk, then FAIL mid-stream.
                write_sync_packet(&mut stream, b"DATA", 1024, &payload_server[..1024]).unwrap();
                let err_msg = b"I/O error mid-stream";
                write_sync_packet(&mut stream, b"FAIL", err_msg.len() as u32, err_msg).unwrap();
            }
        });

        client.pull_to_file("emulator-5554", "/sdcard/rec.mp4", &ok_dest).unwrap();
        assert_eq!(std::fs::read(&ok_dest).unwrap(), payload);

        let err = client.pull_to_file("emulator-5554", "/sdcard/bad.mp4", &fail_dest).unwrap_err();
        assert!(err.to_string().contains("I/O error mid-stream"), "{err}");
        assert!(!fail_dest.exists(), "failed pull must not leave a truncated output file");
        // Verify no leftover `.part` temp files remain in `dir`.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(entries.is_empty(), "leftover temp files: {entries:?}");

        let _ = std::fs::remove_dir_all(&dir);
        handle.join().unwrap();
    }

    #[test]
    fn forward_ephemeral_returns_allocated_port() {
        let (client, handle) = spawn_mock_adb_server(1, |_idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host-serial:emulator-5554:forward:tcp:0;tcp:7001");
            mock_write_okay(&mut stream);
            mock_write_okay(&mut stream);
            stream.write_all(b"000554321").unwrap();
            stream.flush().unwrap();
        });

        let port = client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap();
        assert_eq!(port, 54321);
        handle.join().unwrap();
    }

    #[test]
    fn forward_ephemeral_propagates_fail_on_first_and_defensive_second_status() {
        let (client, handle) = spawn_mock_adb_server(2, |idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host-serial:emulator-5554:forward:tcp:0;tcp:7001");
            if idx == 0 {
                // Real ADB server sends FAIL as the first status on transport/listener error.
                mock_write_fail(&mut stream, "device 'emulator-5554' not found");
            } else {
                // Defensive coverage: FAIL on the second status is also propagated.
                mock_write_okay(&mut stream);
                mock_write_fail(&mut stream, "cannot bind listener: Address already in use");
            }
        });

        let first_err = format!("{:#}", client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap_err());
        assert!(first_err.contains("device 'emulator-5554' not found"), "{first_err}");

        let second_err = format!("{:#}", client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap_err());
        assert!(second_err.contains("Address already in use"), "{second_err}");
        handle.join().unwrap();
    }

    #[test]
    fn forward_ephemeral_rejects_zero_and_non_numeric_ports() {
        let (client, handle) = spawn_mock_adb_server(2, |idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host-serial:emulator-5554:forward:tcp:0;tcp:7001");
            mock_write_okay(&mut stream);
            mock_write_okay(&mut stream);
            if idx == 0 {
                stream.write_all(b"00010").unwrap();
            } else {
                stream.write_all(b"0003abc").unwrap();
            }
            stream.flush().unwrap();
        });

        let zero_err = client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap_err().to_string();
        assert!(zero_err.contains("port 0"), "{zero_err}");

        let nan_err = client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap_err().to_string();
        assert!(nan_err.contains("invalid ephemeral forward port"), "{nan_err}");
        handle.join().unwrap();
    }

    #[test]
    fn forward_ephemeral_rejects_unexpected_eof() {
        let (client, handle) = spawn_mock_adb_server(3, |idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host-serial:emulator-5554:forward:tcp:0;tcp:7001");
            match idx {
                0 => {
                    // EOF right after first OKAY (missing second status).
                    mock_write_okay(&mut stream);
                }
                1 => {
                    // EOF right after second OKAY (missing port payload).
                    mock_write_okay(&mut stream);
                    mock_write_okay(&mut stream);
                }
                2 => {
                    // Truncated port payload (header claims 5 bytes, only 2 sent).
                    mock_write_okay(&mut stream);
                    mock_write_okay(&mut stream);
                    stream.write_all(b"000554").unwrap();
                    stream.flush().unwrap();
                }
                _ => unreachable!(),
            }
        });

        let eof_status = format!("{:#}", client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap_err());
        assert!(eof_status.contains("completing") && eof_status.contains("4-byte ADB status"), "{eof_status}");

        let eof_len = format!("{:#}", client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap_err());
        assert!(eof_len.contains("reading ephemeral port") && eof_len.contains("4-hex-digit"), "{eof_len}");

        let eof_payload = format!("{:#}", client.forward_ephemeral("emulator-5554", "tcp:7001").unwrap_err());
        assert!(
            eof_payload.contains("reading ephemeral port") && eof_payload.contains("5-byte ADB payload"),
            "{eof_payload}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn push_file_fails_before_connecting_when_local_file_is_missing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = AdbClient::with_addr(addr);

        let missing = std::env::temp_dir().join(format!("tapwing_missing_push_{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        let err = client.push_file("emulator-5554", &missing, "/data/local/tmp/missing.bin", 0o644).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("opening local file") && msg.contains("tapwing_missing_push_"), "{msg}");
        assert!(
            matches!(listener.accept(), Err(ref e) if e.kind() == ErrorKind::WouldBlock),
            "push_file must not open an ADB TCP connection when the local file is missing"
        );
    }

    #[test]
    fn push_file_streams_multi_chunk_and_recovers_early_fail_mid_stream() {
        // Part A: >128 KiB (200 KiB = 4 DATA frames) round-trip from disk.
        let payload: Vec<u8> = (0..(200 * 1024)).map(|i| (i % 241) as u8).collect();
        let expected = payload.clone();

        // Part B: 4 MiB file on disk aborted mid-stream with FAIL ("No space left on device").
        let big_payload = vec![0xCD; 4 * 1024 * 1024];

        let (client, handle) = spawn_mock_adb_server(2, move |idx, mut stream| {
            assert_eq!(mock_read_command(&mut stream), "host:transport:emulator-5554");
            mock_write_okay(&mut stream);
            assert_eq!(mock_read_command(&mut stream), "sync:");
            mock_write_okay(&mut stream);

            let (tag, spec_len) = read_sync_header(&mut stream).unwrap();
            assert_eq!(&tag, b"SEND");
            let _spec = read_exact_string(&mut stream, spec_len as usize).unwrap();

            if idx == 0 {
                let mut received = Vec::new();
                let mut chunks = 0usize;
                loop {
                    let (ctag, clen) = read_sync_header(&mut stream).unwrap();
                    if &ctag == b"DATA" {
                        assert!(clen as usize <= SYNC_DATA_MAX);
                        let mut buf = vec![0u8; clen as usize];
                        stream.read_exact(&mut buf).unwrap();
                        received.extend_from_slice(&buf);
                        chunks += 1;
                    } else if &ctag == b"DONE" {
                        assert!(clen > 0, "mtime should be non-zero");
                        break;
                    } else {
                        panic!("unexpected sync tag: {:?}", ctag);
                    }
                }
                assert_eq!(chunks, 4);
                assert_eq!(received, expected);
                write_sync_header(&mut stream, b"OKAY", 0).unwrap();
            } else {
                let (ctag, clen) = read_sync_header(&mut stream).unwrap();
                assert_eq!(&ctag, b"DATA");
                let mut first = vec![0u8; clen as usize];
                stream.read_exact(&mut first).unwrap();

                let err_msg = b"No space left on device";
                write_sync_packet(&mut stream, b"FAIL", err_msg.len() as u32, err_msg).unwrap();
                stream.flush().unwrap();
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let mut drain = [0u8; 16 * 1024];
                while let Ok(n) = stream.read(&mut drain) {
                    if n == 0 {
                        break;
                    }
                }
            }
        });

        let dir = std::env::temp_dir().join(format!("tapwing_push_stream_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let small_path = dir.join("small.bin");
        let big_path = dir.join("big.bin");
        std::fs::write(&small_path, &payload).unwrap();
        std::fs::write(&big_path, &big_payload).unwrap();

        client.push_file("emulator-5554", &small_path, "/data/local/tmp/small.bin", 0o644).unwrap();
        let err = client.push_file("emulator-5554", &big_path, "/sdcard/big.bin", 0o644).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("No space left on device"), "expected early FAIL reason, got: {msg}");

        let _ = std::fs::remove_dir_all(&dir);
        handle.join().unwrap();
    }

    #[test]
    fn sync_read_chunk_retries_interrupted_and_coalesces_partial_reads() {
        struct FlakyReader {
            data: &'static [u8],
            pos: usize,
            calls: usize,
        }
        impl Read for FlakyReader {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                self.calls += 1;
                if self.calls % 2 == 1 {
                    return Err(std::io::Error::new(ErrorKind::Interrupted, "EINTR"));
                }
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                let step = 3.min(out.len()).min(self.data.len() - self.pos);
                out[..step].copy_from_slice(&self.data[self.pos..self.pos + step]);
                self.pos += step;
                Ok(step)
            }
        }

        let mut reader = FlakyReader { data: b"abcdefghij", pos: 0, calls: 0 };
        let mut buf = [0u8; 8];
        assert_eq!(sync::read_chunk(&mut reader, &mut buf).unwrap(), 8);
        assert_eq!(&buf, b"abcdefgh");
        assert_eq!(sync::read_chunk(&mut reader, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"ij");
        assert_eq!(sync::read_chunk(&mut reader, &mut buf).unwrap(), 0);
    }
}
