//! Android `screenrecord` lifecycle (`startRecording`, `stopRecording`, and
//! `Drop`-safe teardown).

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::super::adb_client::AdbClient;
use super::Android;
use super::helpers::wait_with_timeout;

/// Minimum length of a screen recording (`Maestro.startScreenRecording`
/// pads `stopRecording` to 3 s so a too-fast flow still yields a playable file).
pub(in crate::device) const MIN_RECORDING: Duration = Duration::from_secs(3);

/// How long the recorder may take to create its output file before the start
/// is reported as failed (`SCREEN_RECORDING_START_TIMEOUT_MS`).
const RECORDING_START_TIMEOUT_MS: u32 = 10_000;

/// How long a recorder gets to finalize its file after SIGINT.
pub(in crate::device) const RECORDER_EXIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Maestro records Android at 100 kbit/s (`--bit-rate '100000'`).
const SCREENRECORD_BIT_RATE: u32 = 100_000;

/// Counter that keeps on-device recording temp files unique within a process.
static RECORDING_SEQ: AtomicU32 = AtomicU32::new(0);

/// An in-flight `screenrecord` on the device (see [`Android::start_recording`]).
pub(super) struct AndroidRecording {
    /// The local `adb shell` process that owns the on-device recorder.
    child: Child,
    /// PID of the on-device `screenrecord` (`None` only in tests).
    remote_pid: Option<String>,
    /// Temp file on the device the recorder writes to.
    remote_path: String,
    /// Host file the recording is pulled to on stop.
    out_path: PathBuf,
    started: Instant,
}

impl AndroidRecording {
    /// Interrupts the on-device recorder and waits for the local `adb shell`
    /// to end. Without a known PID (or after a timeout) the local process is
    /// killed instead. Returns whether the recorder exited by itself.
    fn stop_recorder(&mut self, adb: &AdbClient, serial: &str) -> bool {
        let Some(pid) = self.remote_pid.clone() else {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return false;
        };
        let _ = adb.shell(serial, &format!("kill -2 {pid}"));
        let exited = wait_with_timeout(&mut self.child, RECORDER_EXIT_TIMEOUT).is_some();
        if !exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        exited
    }
}

impl Drop for Android {
    /// Never leaves a `screenrecord` running on the device.
    fn drop(&mut self) {
        if let Some(mut rec) = self.recording.take() {
            rec.stop_recorder(&self.adb, &self.serial);
            let _ = self.adb.shell(&self.serial, &format!("rm -f {0} {0}.log", rec.remote_path));
        }
    }
}

impl Android {
    /// `startRecording` (`AndroidDriver.startScreenRecording`,
    /// `AndroidDriver.kt:507-545`): a background `screenrecord` writing to a
    /// temp file on the device. Returns `false` (and does nothing) when a
    /// recording is already running, like `Maestro.startScreenRecording`.
    pub fn start_recording(&mut self, out_path: &Path) -> Result<bool> {
        if self.recording.is_some() {
            return Ok(false);
        }
        if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let remote_path = remote_recording_path(std::process::id(), RECORDING_SEQ.fetch_add(1, Ordering::Relaxed));
        let remote_log = format!("{remote_path}.log");
        self.shell(&format!("rm -f {remote_path} {remote_log}"))?;
        let mut child = Command::new("adb")
            .args(["-s", &self.serial, "shell", &screenrecord_command(self.api_level()?, &remote_path)])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting adb shell screenrecord")?;
        // The remote shell prints its PID first and redirects `screenrecord`'s
        // output into `remote_log` before `exec`, so stdout hits EOF right away
        // (no SIGPIPE risk while recording). Bound the PID read with a timeout.
        let child_stdout = child.stdout.take().context("adb stdout was not piped")?;
        let (pid_tx, pid_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(child_stdout);
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            let _ = pid_tx.send(first);
        });
        let first =
            pid_rx.recv_timeout(Duration::from_millis(u64::from(RECORDING_START_TIMEOUT_MS))).unwrap_or_default();
        let pid = first.trim().to_string();
        let mut rec = AndroidRecording {
            child,
            remote_pid: pid.parse::<u32>().is_ok().then(|| pid.clone()),
            remote_path: remote_path.clone(),
            out_path: out_path.to_path_buf(),
            started: Instant::now(),
        };
        let Some(pid) = rec.remote_pid.clone() else {
            rec.stop_recorder(&self.adb, &self.serial);
            let detail = self.read_and_remove_remote_log(&remote_log);
            bail!("Failed to start screen recording: no PID from the device ({detail})");
        };
        // Wait for the recorder to create its file (or die), like `awaitRecordingStart`.
        let probe = format!(
            "N=0; while [ ! -e {remote_path} ] && kill -0 {pid} 2>/dev/null && [ $N -lt {} ]; do sleep 0.02; N=$((N+1)); done; \
             if [ -e {remote_path} ]; then echo TAPWING_REC:STARTED; else echo TAPWING_REC:FAILED; fi",
            RECORDING_START_TIMEOUT_MS / 20
        );
        let started = self.shell(&probe).map(|o| o.contains("TAPWING_REC:STARTED"));
        if !matches!(started, Ok(true)) {
            rec.stop_recorder(&self.adb, &self.serial);
            let detail = self.read_and_remove_remote_log(&remote_log);
            let _ = self.shell(&format!("rm -f {remote_path}"));
            bail!(
                "Failed to capture screen recording on the device. Note that some Android emulators do not support \
                 screen recording. Try using a different Android emulator (eg. Pixel 5 / API 30): {detail}"
            );
        }
        rec.started = Instant::now();
        self.recording = Some(rec);
        Ok(true)
    }

    fn read_and_remove_remote_log(&self, remote_log: &str) -> String {
        let out = self.shell(&format!("cat {remote_log} 2>/dev/null; rm -f {remote_log}")).unwrap_or_default();
        let trimmed = out.trim();
        if trimmed.is_empty() { "no output".to_string() } else { trimmed.to_string() }
    }

    /// `stopRecording`: pads the recording to [`MIN_RECORDING`], interrupts the
    /// recorder with SIGINT (so the MP4 is finalized), pulls the file to the
    /// path given to [`Self::start_recording`] and removes the device copy.
    pub fn stop_recording(&mut self) -> Result<PathBuf> {
        let Some(mut rec) = self.recording.take() else {
            bail!("no screen recording is in progress");
        };
        if let Some(rest) = MIN_RECORDING.checked_sub(rec.started.elapsed())
            && let Err(err) = crate::interrupt::sleep(rest)
        {
            rec.stop_recorder(&self.adb, &self.serial);
            let _ = self.adb.shell(&self.serial, &format!("rm -f {0} {0}.log", rec.remote_path));
            return Err(err);
        }
        let exited = rec.stop_recorder(&self.adb, &self.serial);
        let pulled = self.pull(&rec.remote_path, &rec.out_path);
        let _ = self.shell(&format!("rm -f {0} {0}.log", rec.remote_path));
        pulled.with_context(|| format!("pulling the screen recording to {}", rec.out_path.display()))?;
        if !exited {
            bail!(
                "screenrecord did not exit within {} s after SIGINT; {} may be truncated",
                RECORDER_EXIT_TIMEOUT.as_secs(),
                rec.out_path.display()
            );
        }
        Ok(rec.out_path)
    }

    /// Whether a screen recording is currently running.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }
}

/// Temp file the recorder writes to on the device.
pub fn remote_recording_path(host_pid: u32, seq: u32) -> String {
    format!("/sdcard/tapwing_rec_{host_pid}_{seq}.mp4")
}

/// Remote command: prints the shell PID, then `exec`s the recorder with its
/// output redirected to `<remote_path>.log` so stdout closes cleanly and never
/// triggers SIGPIPE while recording. API 34+ can lift the 180 s limit.
pub fn screenrecord_command(api_level: u32, remote_path: &str) -> String {
    let limit = if api_level >= 34 { "--time-limit 0 " } else { "" };
    format!(
        "echo $$; exec screenrecord {limit}--bit-rate '{SCREENRECORD_BIT_RATE}' {remote_path} >{remote_path}.log 2>&1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenrecord_command_lifts_the_time_limit_on_api_34() {
        let path = remote_recording_path(4242, 1);
        assert_eq!(path, "/sdcard/tapwing_rec_4242_1.mp4");
        let old = screenrecord_command(33, &path);
        assert!(!old.contains("--time-limit"));
        assert!(old.starts_with("echo $$; exec screenrecord --bit-rate '100000' /sdcard/tapwing_rec_4242_1.mp4"));
        let new = screenrecord_command(34, &path);
        assert!(new.contains("exec screenrecord --time-limit 0 --bit-rate"));
    }

    fn sleeper() -> Child {
        Command::new("sleep").arg("60").stdout(Stdio::piped()).spawn().unwrap()
    }

    fn process_alive(pid: u32) -> bool {
        Command::new("kill").args(["-0", &pid.to_string()]).output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn stopping_without_a_recording_is_an_error_and_start_is_idempotent() {
        let mut android = Android::new("no-such-device".into());
        assert!(!android.is_recording());
        let err = android.stop_recording().unwrap_err().to_string();
        assert_eq!(err, "no screen recording is in progress");

        let child = sleeper();
        let pid = child.id();
        android.recording = Some(AndroidRecording {
            child,
            remote_pid: None,
            remote_path: "/sdcard/tapwing_rec_test.mp4".into(),
            out_path: PathBuf::from("out.mp4"),
            started: Instant::now(),
        });
        assert!(android.is_recording());
        assert!(!android.start_recording(Path::new("other.mp4")).unwrap());
        assert!(process_alive(pid));
        drop(android);
        assert!(!process_alive(pid), "recorder process survived Drop");
    }

    #[test]
    fn dropping_an_idle_android_spawns_nothing() {
        drop(Android::new("no-such-device".into()));
    }
}
