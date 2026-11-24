//! Android Emulator console TCP client (`emulator-<port>`).

use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;

use super::AdbClient;

impl AdbClient {
    /// Sends a console command (e.g. `geo fix <lon> <lat>`) directly to an
    /// Android Emulator console on `127.0.0.1:<port>` where `serial` is
    /// `emulator-<port>`.
    ///
    /// Reads the welcome banner, authenticates with
    /// `~/.emulator_console_auth_token` if present, executes `cmd`, reads the
    /// response until `OK` or `KO: ...`, and sends `quit`.
    pub fn emu_command(&self, serial: &str, cmd: &str) -> Result<String> {
        let port = parse_emulator_port(serial)?;
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let mut stream = TcpStream::connect_timeout(&addr, self.connect_timeout)
            .with_context(|| format!("connecting to emulator console at {addr} ({serial})"))?;
        stream.set_read_timeout(Some(self.io_timeout)).context("setting emulator console read timeout")?;
        stream.set_write_timeout(Some(self.io_timeout)).context("setting emulator console write timeout")?;
        let _ = stream.set_nodelay(true);

        let mut reader = BufReader::new(&mut stream);

        // 1. Consume the initial greeting banner until `OK` / `KO`.
        read_emu_response(&mut reader, "banner")?;

        // 2. Authenticate if the auth token file exists and is non-empty.
        if let Some(token_path) = &self.emu_auth_token_path
            && let Ok(raw_token) = std::fs::read_to_string(token_path)
        {
            let token = raw_token.trim();
            if !token.is_empty() {
                write_emu_line(&mut reader, &format!("auth {token}")).context("sending emulator console auth token")?;
                read_emu_response(&mut reader, "auth")?;
            }
        }

        // 3. Send the command and read its output until `OK` or `KO: ...`.
        write_emu_line(&mut reader, cmd).with_context(|| format!("sending emulator console command `{cmd}`"))?;
        let response = read_emu_response(&mut reader, cmd);

        // 4. Cleanly close the console session (best effort).
        let _ = write_emu_line(&mut reader, "quit");
        response
    }
}

pub(super) fn default_emu_auth_token_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".emulator_console_auth_token"))
}

fn parse_emulator_port(serial: &str) -> Result<u16> {
    let port_str = serial
        .strip_prefix("emulator-")
        .ok_or_else(|| anyhow!("device `{serial}` is not an Android emulator (expected `emulator-<port>`)"))?;
    port_str.parse::<u16>().with_context(|| format!("invalid emulator console port in serial `{serial}`"))
}

fn write_emu_line(reader: &mut BufReader<&mut TcpStream>, cmd: &str) -> Result<()> {
    let line = format!("{cmd}\r\n");
    let stream = reader.get_mut();
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn read_emu_response(reader: &mut BufReader<&mut TcpStream>, cmd: &str) -> Result<String> {
    let mut lines = Vec::new();
    let mut buf = String::new();
    loop {
        buf.clear();
        let n =
            reader.read_line(&mut buf).with_context(|| format!("reading line from emulator console during `{cmd}`"))?;
        if n == 0 {
            bail!("unexpected EOF from emulator console during `{cmd}`");
        }
        let trimmed = buf.trim_end_matches(['\r', '\n']);
        if trimmed == "OK" {
            return Ok(lines.join("\n"));
        }
        if trimmed == "KO" || trimmed.starts_with("KO:") || trimmed.starts_with("KO ") {
            bail!("emulator console command `{cmd}` failed: {trimmed}");
        }
        lines.push(trimmed.to_string());
    }
}
