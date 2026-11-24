//! Exit status over the legacy `shell:` service.
//!
//! Devices without the `shell_v2` feature (before Android 7) only offer
//! `shell:<cmd>`: one byte stream with stdout and stderr merged and no exit
//! status. The status is recovered the way `adb` wrappers always have: the
//! command is followed by an `echo` of a marker and `$?`, which is parsed off
//! the end of the stream.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, bail};

use super::ShellOutput;

/// Keeps the markers of one process distinct.
static MARKER_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh marker for one command. It differs per call, so output that merely
/// looks like an older marker (a log that captured one) is never taken for
/// this command's status.
pub(super) fn next_exit_marker() -> String {
    format!("TAPWING_EXIT_{:x}_{:x}:", std::process::id(), MARKER_SEQ.fetch_add(1, Ordering::Relaxed))
}

/// Wraps `cmd` so the device prints `<marker><exit status>` after it.
///
/// The subshell makes the marker survive an `exit` or `exec` inside `cmd`,
/// and the newline before `)` keeps a trailing `# comment` from swallowing it.
pub(super) fn legacy_shell_script(cmd: &str, marker: &str) -> String {
    format!("({cmd}\n); echo \"{marker}$?\"")
}

/// Splits the raw stream of a [`legacy_shell_script`] run into the command's
/// output and its exit status.
///
/// The marker line must be the very last thing in the stream: EOF before it
/// (a dropped connection, a device that went away) is an error and never
/// status 0. Bytes are searched, not text, so output that is not UTF-8 or has
/// no trailing newline (`fooTAPWING_EXIT_..:0`) is handled; legacy shells run
/// on a PTY, so the marker line may end in `\r\n`.
pub(super) fn parse_legacy_shell_output(raw: &[u8], marker: &str) -> Result<ShellOutput> {
    let marker = marker.as_bytes();
    let found = raw.windows(marker.len()).rposition(|window| window == marker).and_then(|start| {
        let (output, tail) = raw.split_at(start);
        let status = tail.get(marker.len()..)?;
        let digits = status.iter().take_while(|b| b.is_ascii_digit()).count();
        let (digits, rest) = status.split_at(digits);
        if digits.len() > 3 || !rest.iter().all(|b| matches!(b, b'\r' | b'\n')) {
            return None;
        }
        // An exit status is 0-255; anything else is not the shell's `$?`.
        let exit_code = std::str::from_utf8(digits).ok()?.parse::<u8>().ok()?;
        Some((output, i32::from(exit_code)))
    });
    let Some((output, exit_code)) = found else {
        bail!("legacy shell stream ended without an exit status ({} bytes of output)", raw.len());
    };
    Ok(ShellOutput { stdout: String::from_utf8_lossy(output).into_owned(), stderr: String::new(), exit_code })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &str = "TAPWING_EXIT_1f_0:";

    fn parse(raw: &[u8]) -> Result<ShellOutput> {
        parse_legacy_shell_output(raw, MARKER)
    }

    #[test]
    fn the_script_reports_the_status_after_exit_exec_and_trailing_comments() {
        assert_eq!(legacy_shell_script("pm clear dev.x", MARKER), "(pm clear dev.x\n); echo \"TAPWING_EXIT_1f_0:$?\"");
        // Run the wrapper in a real `sh`: the marker must follow whatever the command does.
        for (cmd, stdout, code) in [
            ("echo hi", "hi\n", 0),
            ("printf foo", "foo", 0),
            ("echo out; exit 7", "out\n", 7),
            ("exec false", "", 1),
            ("true # trailing comment", "", 0),
            ("echo a;", "a\n", 0),
            ("no-such-tapwing-tool 2>/dev/null", "", 127),
        ] {
            let out =
                std::process::Command::new("sh").args(["-c", &legacy_shell_script(cmd, MARKER)]).output().unwrap();
            let parsed = parse(&out.stdout).unwrap_or_else(|e| panic!("`{cmd}`: {e}"));
            assert_eq!((parsed.stdout.as_str(), parsed.exit_code), (stdout, code), "`{cmd}`");
        }
    }

    #[test]
    fn markers_differ_per_call() {
        let (a, b) = (next_exit_marker(), next_exit_marker());
        assert_ne!(a, b);
        assert!(a.starts_with("TAPWING_EXIT_") && a.ends_with(':'), "{a}");
    }

    #[test]
    fn the_status_is_split_off_the_end_of_the_stream() {
        let out = parse(b"line 1\nline 2\nTAPWING_EXIT_1f_0:0\n").unwrap();
        assert_eq!(out, ShellOutput { stdout: "line 1\nline 2\n".into(), stderr: String::new(), exit_code: 0 });
        assert_eq!(parse(b"TAPWING_EXIT_1f_0:255\n").unwrap().exit_code, 255);
        assert_eq!(parse(b"TAPWING_EXIT_1f_0:1").unwrap().exit_code, 1, "EOF right after the digits");
        // No trailing newline in the command's output.
        let out = parse(b"fooTAPWING_EXIT_1f_0:42\n").unwrap();
        assert_eq!((out.stdout.as_str(), out.exit_code), ("foo", 42));
        // PTY line endings of old devices, including the doubled `\r`.
        let out = parse(b"34\r\nTAPWING_EXIT_1f_0:0\r\r\n").unwrap();
        assert_eq!((out.stdout.as_str(), out.exit_code), ("34\r\n", 0));
    }

    #[test]
    fn binary_output_and_marker_lookalikes_do_not_confuse_the_status() {
        let mut raw = vec![0x89, b'P', b'N', b'G', 0x00, 0xff, 0xfe, b'\r', b'\n'];
        raw.extend_from_slice(b"TAPWING_EXIT_1f_0:3\n");
        let out = parse(&raw).unwrap();
        assert_eq!(out.exit_code, 3);
        assert_eq!(out.stdout, String::from_utf8_lossy(&raw[..9]));

        // The command printed the marker itself (`set -x`, a captured log): only the last one counts.
        let out = parse(b"+ echo TAPWING_EXIT_1f_0:0\nTAPWING_EXIT_1f_0:0\nTAPWING_EXIT_1f_0:9\n").unwrap();
        assert_eq!((out.stdout.as_str(), out.exit_code), ("+ echo TAPWING_EXIT_1f_0:0\nTAPWING_EXIT_1f_0:0\n", 9));
        // A marker of another call is plain output.
        assert!(parse(b"TAPWING_EXIT_1f_1:0\n").is_err());
    }

    #[test]
    fn a_stream_without_a_final_status_is_an_error_never_status_zero() {
        for raw in [
            &b""[..],
            b"partial output",
            b"TAPWING_EXIT_1f_0:",
            b"TAPWING_EXIT_1f_0:\n",
            b"TAPWING_EXIT_1f_0:256\n",
            b"TAPWING_EXIT_1f_0:0000\n",
            b"TAPWING_EXIT_1f_0:x\n",
            b"TAPWING_EXIT_1f_0:0\nlate output",
            b"TAPWING_EXIT_1f_0:0 \n",
            b"TAPWING_EXIT_1f_",
        ] {
            let err = parse(raw).unwrap_err().to_string();
            assert!(err.contains("ended without an exit status"), "{:?}: {err}", String::from_utf8_lossy(raw));
        }
    }
}
