//! ADB `sync:` protocol (`SEND`/`RECV`/`DATA`/`DONE`/`FAIL`) file transfer client.

use anyhow::{Context, Result, bail};
use std::io::{BufWriter, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{AdbClient, read_exact_string, send_command};

/// Maximum payload size of a single ADB `sync:` `DATA` chunk (64 KiB).
pub const SYNC_DATA_MAX: usize = 64 * 1024;

/// Safety cap on a single inbound `sync:` `DATA` packet (4 MiB).
const MAX_SYNC_DATA_PACKET: usize = 4 * 1024 * 1024;

/// Monotonic counter keeping temporary pull files unique across concurrent calls.
static PULL_TMP_SEQ: AtomicU32 = AtomicU32::new(0);

impl AdbClient {
    /// Streams bytes from `reader` to `remote_path` on `serial` with POSIX
    /// `mode` and `mtime` using a bounded `SYNC_DATA_MAX` (64 KiB) buffer.
    pub(crate) fn push_reader<R: Read>(
        &self,
        serial: &str,
        reader: &mut R,
        remote_path: &str,
        mode: u32,
        mtime: u32,
    ) -> Result<()> {
        let mut stream = self.connect_sync(serial)?;

        // SEND request payload is "<remote_path>,<decimal_mode>".
        let send_spec = format!("{remote_path},{mode}");
        write_sync_packet(&mut stream, b"SEND", send_spec.len() as u32, send_spec.as_bytes())
            .with_context(|| format!("sending sync SEND for `{remote_path}`"))?;

        let mut buf = vec![0u8; SYNC_DATA_MAX];
        let write_outcome: std::io::Result<()> = loop {
            let n = match read_chunk(reader, &mut buf) {
                Ok(0) => break write_sync_header_io(&mut stream, b"DONE", mtime).and_then(|()| stream.flush()),
                Ok(n) => n,
                Err(read_err) => {
                    return Err(anyhow::Error::new(read_err))
                        .with_context(|| format!("reading payload for adb push `{remote_path}`"));
                }
            };
            if has_pending_sync_response(&stream) {
                break Err(std::io::Error::new(ErrorKind::BrokenPipe, "early sync response available"));
            }
            if let Err(err) = write_sync_packet_io(&mut stream, b"DATA", n as u32, &buf[..n]) {
                break Err(err);
            }
        };

        if let Err(io_err) = write_outcome {
            if is_peer_abort(&io_err)
                && let Some(fail_msg) = try_read_sync_fail(&mut stream)
            {
                let _ = write_sync_header(&mut stream, b"QUIT", 0);
                bail!("adb sync push `{remote_path}` failed: {fail_msg}");
            }
            return Err(anyhow::Error::new(io_err))
                .with_context(|| format!("sending sync DATA/DONE for `{remote_path}`"));
        }

        let (tag, len) =
            read_sync_header(&mut stream).with_context(|| format!("reading sync status for `{remote_path}`"))?;
        match &tag {
            b"OKAY" => {
                let _ = write_sync_header(&mut stream, b"QUIT", 0);
                Ok(())
            }
            b"FAIL" => {
                let msg = read_sync_fail_message(&mut stream, len);
                let _ = write_sync_header(&mut stream, b"QUIT", 0);
                bail!("adb sync push `{remote_path}` failed: {msg}");
            }
            other => bail!("unexpected sync push response tag: {}", String::from_utf8_lossy(other)),
        }
    }

    /// Pushes in-memory bytes to `remote_path` on `serial` with POSIX `mode`
    /// (e.g. `0o644`) using the ADB `sync:` (`SEND`/`DATA`/`DONE`) service.
    pub fn push(&self, serial: &str, data: &[u8], remote_path: &str, mode: u32) -> Result<()> {
        let mut slice = data;
        self.push_reader(serial, &mut slice, remote_path, mode, current_unix_mtime())
    }

    /// Streams a local file from disk to `remote_path` on `serial` with POSIX
    /// `mode` using a bounded 64 KiB buffer.
    pub fn push_file(&self, serial: &str, local_path: &Path, remote_path: &str, mode: u32) -> Result<()> {
        // Open before connecting to ADB so missing/unreadable local files fail immediately.
        let mut file = std::fs::File::open(local_path)
            .with_context(|| format!("opening local file `{}` for adb push", local_path.display()))?;
        let mtime = file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32)
            .filter(|&s| s > 0)
            .unwrap_or_else(current_unix_mtime);
        self.push_reader(serial, &mut file, remote_path, mode, mtime)
    }

    /// Streams `remote_path` from `serial` into `out` using the ADB `sync:`
    /// (`RECV`/`DATA`/`DONE`) service with a bounded scratch buffer.
    fn pull_into<W: Write>(&self, serial: &str, remote_path: &str, out: &mut W) -> Result<()> {
        let mut stream = self.connect_sync(serial)?;
        write_sync_packet(&mut stream, b"RECV", remote_path.len() as u32, remote_path.as_bytes())
            .with_context(|| format!("sending sync RECV for `{remote_path}`"))?;
        stream.flush().context("flushing sync RECV request")?;

        let mut scratch = vec![0u8; SYNC_DATA_MAX];
        loop {
            let (tag, len) = read_sync_header(&mut stream)
                .with_context(|| format!("reading sync RECV header for `{remote_path}`"))?;
            match &tag {
                b"DATA" => {
                    let chunk_len = len as usize;
                    if chunk_len > MAX_SYNC_DATA_PACKET {
                        bail!("ADB sync RECV DATA chunk too large ({chunk_len} bytes, max {MAX_SYNC_DATA_PACKET})");
                    }
                    let mut remaining = chunk_len;
                    while remaining > 0 {
                        let step = remaining.min(scratch.len());
                        stream
                            .read_exact(&mut scratch[..step])
                            .with_context(|| format!("reading {len} bytes of sync DATA for `{remote_path}`"))?;
                        out.write_all(&scratch[..step])
                            .with_context(|| format!("writing sync DATA chunk for `{remote_path}`"))?;
                        remaining -= step;
                    }
                }
                b"DONE" => {
                    out.flush().with_context(|| format!("flushing pulled output for `{remote_path}`"))?;
                    let _ = write_sync_header(&mut stream, b"QUIT", 0);
                    return Ok(());
                }
                b"FAIL" => {
                    let msg = read_sync_fail_message(&mut stream, len);
                    let _ = write_sync_header(&mut stream, b"QUIT", 0);
                    bail!("adb sync pull `{remote_path}` failed: {msg}");
                }
                other => bail!("unexpected sync RECV response tag: {}", String::from_utf8_lossy(other)),
            }
        }
    }

    /// Pulls `remote_path` from `serial` into memory using the ADB `sync:`
    /// (`RECV`/`DATA`/`DONE`) service.
    pub fn pull(&self, serial: &str, remote_path: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.pull_into(serial, remote_path, &mut out)?;
        Ok(out)
    }

    /// Streams `remote_path` from `serial` directly into `local_path` on disk
    /// via a buffered temporary sibling file and atomic rename, avoiding
    /// buffering multi-minute `screenrecord` MP4s in memory.
    pub fn pull_to_file(&self, serial: &str, remote_path: &str, local_path: &Path) -> Result<()> {
        if let Some(parent) = local_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent directory `{}` for adb pull", parent.display()))?;
        }
        let file_name = local_path.file_name().and_then(|n| n.to_str()).unwrap_or("pull");
        let seq = PULL_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = local_path.with_file_name(format!(".{file_name}.tapwing-{}-{seq}.part", std::process::id()));
        let file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("creating temporary pull file `{}`", tmp_path.display()))?;
        let mut writer = BufWriter::with_capacity(SYNC_DATA_MAX, file);
        if let Err(err) = self.pull_into(serial, remote_path, &mut writer) {
            drop(writer);
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }
        if let Err(err) = writer.flush() {
            drop(writer);
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err).with_context(|| format!("flushing `{}`", tmp_path.display()));
        }
        drop(writer);
        if let Err(err) = std::fs::rename(&tmp_path, local_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err).with_context(|| {
                format!("renaming pulled file `{}` to `{}`", tmp_path.display(), local_path.display())
            });
        }
        Ok(())
    }

    fn connect_sync(&self, serial: &str) -> Result<TcpStream> {
        let mut stream = self.connect_transport(serial)?;
        send_command(&mut stream, "sync:").with_context(|| format!("starting sync service on `{serial}`"))?;
        Ok(stream)
    }
}

fn current_unix_mtime() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0)
}

/// Fills `buf` from `reader` until full or EOF, retrying `ErrorKind::Interrupted`.
pub(super) fn read_chunk<R: Read + ?Sized>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn write_sync_header_io<W: Write>(stream: &mut W, tag: &[u8; 4], arg: u32) -> std::io::Result<()> {
    let mut header = [0u8; 8];
    header[..4].copy_from_slice(tag);
    header[4..].copy_from_slice(&arg.to_le_bytes());
    stream.write_all(&header)
}

fn write_sync_packet_io<W: Write>(stream: &mut W, tag: &[u8; 4], arg: u32, payload: &[u8]) -> std::io::Result<()> {
    write_sync_header_io(stream, tag, arg)?;
    stream.write_all(payload)
}

pub(super) fn write_sync_header<W: Write>(stream: &mut W, tag: &[u8; 4], arg: u32) -> Result<()> {
    write_sync_header_io(stream, tag, arg).context("writing 8-byte ADB sync header")
}

pub(super) fn write_sync_packet<W: Write>(stream: &mut W, tag: &[u8; 4], arg: u32, payload: &[u8]) -> Result<()> {
    write_sync_packet_io(stream, tag, arg, payload).context("writing ADB sync packet payload")
}

fn is_peer_abort(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::WriteZero
    )
}

/// Non-blocking check whether the peer has already written an early sync status
/// (`FAIL`) while we are still streaming `DATA` chunks.
fn has_pending_sync_response(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let mut probe = [0u8; 1];
    let has_data = matches!(stream.peek(&mut probe), Ok(n) if n > 0);
    let _ = stream.set_nonblocking(false);
    has_data
}

/// Reads a bounded ADB `sync:` `FAIL` message, preventing unbounded allocations
/// if a corrupted stream advertises a huge length.
fn read_sync_fail_message<R: Read>(stream: &mut R, len: u32) -> String {
    let n = len as usize;
    if n > MAX_SYNC_DATA_PACKET {
        return format!("ADB sync FAIL message too large ({n} bytes, max {MAX_SYNC_DATA_PACKET})");
    }
    read_exact_string(stream, n).unwrap_or_else(|_| "unknown sync failure".into())
}

/// Attempts to read a pending `FAIL<len><msg>` sync packet after `adbd` aborts
/// an in-flight `push` early (e.g. `Permission denied` or `No space left on device`).
fn try_read_sync_fail<R: Read>(stream: &mut R) -> Option<String> {
    let (tag, len) = read_sync_header(stream).ok()?;
    if &tag != b"FAIL" || (len as usize) > MAX_SYNC_DATA_PACKET {
        return None;
    }
    read_exact_string(stream, len as usize).ok()
}

pub(super) fn read_sync_header<R: Read>(stream: &mut R) -> Result<([u8; 4], u32)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).context("reading 8-byte ADB sync header")?;
    let tag = [header[0], header[1], header[2], header[3]];
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    Ok((tag, len))
}
