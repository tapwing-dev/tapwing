//! Bounded host subprocesses: [`run_with_timeout`] runs a short-lived tool
//! (`xcrun simctl`, `adb install`, `adb start-server`, ...) to completion and
//! kills it when it overruns its deadline, so a wedged CoreSimulator service or
//! ADB daemon can never hang a run or a `Drop` guard.
//!
//! Long-lived children (`screenrecord`, `am instrument`, `xcodebuild`) are
//! spawned and owned by their callers and do not go through here.

use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

/// First poll interval; doubles up to [`MAX_POLL`] while the child is silent,
/// so a tool that exits within milliseconds is not held back by the poll.
const MIN_POLL: Duration = Duration::from_millis(1);
const MAX_POLL: Duration = Duration::from_millis(50);

/// Most bytes kept per stream; anything beyond is read and discarded so the
/// child never blocks on a full pipe.
const MAX_CAPTURE: usize = 16 * 1024 * 1024;

/// Longest command description quoted in an error message.
const MAX_LABEL: usize = 200;

/// Exit status and captured output of a finished child.
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Runs `cmd` to completion with stdin closed and stdout/stderr captured.
///
/// The child is killed and reaped, and an error naming the command is
/// returned, when it is still running after `timeout` or when `SIGINT` /
/// `SIGTERM` arrives while it runs (the error then carries
/// [`crate::interrupt::InterruptedError`]).
///
/// A signal that was already pending when the call started does not abort it:
/// such calls are the cleanup the interrupt itself triggered (RAII guards
/// restoring the device while the stack unwinds), so they keep their full
/// deadline. A second signal still exits the process immediately.
///
/// Never panics and starts no threads, so it is safe to call from `Drop`.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output> {
    run_cancellable(cmd, timeout, None)
}

/// [`run_with_timeout`] that also kills the child once `cancel` is set (the
/// flag background warm-up threads are stopped with).
pub fn run_with_timeout_or_cancel(cmd: &mut Command, timeout: Duration, cancel: &AtomicBool) -> Result<Output> {
    run_cancellable(cmd, timeout, Some(cancel))
}

fn run_cancellable(cmd: &mut Command, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Output> {
    let cleanup_after_interrupt = crate::interrupt::is_interrupted();
    run_polled(cmd, timeout, &mut |pause| {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(anyhow!("cancelled"));
        }
        if cleanup_after_interrupt {
            std::thread::sleep(pause);
            return Ok(());
        }
        crate::interrupt::sleep(pause)
    })
}

/// The poll loop behind [`run_with_timeout`]. `pause` sleeps between polls;
/// an error from it (interrupt, cancellation) kills the child and is returned.
fn run_polled(cmd: &mut Command, timeout: Duration, pause: &mut dyn FnMut(Duration) -> Result<()>) -> Result<Output> {
    let label = describe(cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("starting `{label}`"))?;
    let mut pipes = Pipes::attach(&mut child);
    let deadline = Instant::now() + timeout;
    let mut step = MIN_POLL;
    let failure = loop {
        let progressed = pipes.drain();
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout, stderr) = pipes.finish();
                return Ok(Output { status, stdout, stderr });
            }
            Ok(None) => {}
            Err(e) => break anyhow!(e).context(format!("waiting for `{label}`")),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break anyhow!("`{label}` did not finish within {} and was killed", format_duration(timeout));
        }
        step = if progressed { MIN_POLL } else { (step * 2).min(MAX_POLL) };
        if let Err(e) = pause(step.min(remaining)) {
            break e.context(format!("`{label}` was killed before it finished"));
        }
    };
    kill_and_reap(&mut child);
    Err(failure)
}

/// Whether a process with this pid exists on this host. Errs on the side of
/// "alive": callers use it to decide that leftovers of a dead Tapwing process
/// may be removed, so an unknown answer must keep them.
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Pid 0 and negative pids address process groups, not one process.
        let Some(pid) = libc::pid_t::try_from(pid).ok().filter(|p| *p > 0) else { return true };
        // SAFETY: signal 0 only checks for existence and permission.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        // Anything but "no such process" (`EPERM`: it exists under another user) counts as alive.
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Sends `SIGKILL` to a process that is not our child; `true` when it was delivered.
#[cfg(unix)]
pub(crate) fn kill_pid(pid: u32) -> bool {
    let Some(pid) = libc::pid_t::try_from(pid).ok().filter(|p| *p > 0) else { return false };
    // SAFETY: plain `kill(2)` on a positive pid, so one process and never a group.
    unsafe { libc::kill(pid, libc::SIGKILL) == 0 }
}

/// `SIGKILL` cannot be ignored, so the `wait` that reaps the child returns promptly.
fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// `program arg ...` for error messages, shortened to [`MAX_LABEL`] characters.
fn describe(cmd: &Command) -> String {
    let mut label = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        label.push(' ');
        label.push_str(&arg.to_string_lossy());
    }
    match label.char_indices().nth(MAX_LABEL) {
        Some((cut, _)) => format!("{}...", &label[..cut]),
        None => label,
    }
}

fn format_duration(d: Duration) -> String {
    if d.subsec_millis() == 0 { format!("{} s", d.as_secs()) } else { format!("{} ms", d.as_millis()) }
}

/// Appends `chunk` to `buf` up to [`MAX_CAPTURE`] bytes in total.
#[cfg(unix)]
fn capture(buf: &mut Vec<u8>, chunk: &[u8]) {
    let room = MAX_CAPTURE.saturating_sub(buf.len());
    buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
}

/// The child's stdout and stderr, read without ever blocking: both pipes are
/// switched to `O_NONBLOCK` and emptied on every poll, so a child that fills a
/// pipe is never stuck behind the poll loop, and a grandchild that inherited a
/// pipe and outlives the child cannot hold the caller either.
#[cfg(unix)]
struct Pipes {
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    out: Vec<u8>,
    err: Vec<u8>,
}

#[cfg(unix)]
impl Pipes {
    /// Most bytes read from one pipe per poll, so a chatty child cannot keep
    /// the loop from checking its deadline.
    const DRAIN_BUDGET: usize = 1024 * 1024;

    fn attach(child: &mut Child) -> Self {
        // A pipe that cannot be made non-blocking is closed instead: losing
        // the output is better than a read that could block forever.
        let stdout = child.stdout.take().filter(set_nonblocking);
        let stderr = child.stderr.take().filter(set_nonblocking);
        Self { stdout, stderr, out: Vec::new(), err: Vec::new() }
    }

    /// Reads what is available right now; `true` when anything was read.
    fn drain(&mut self) -> bool {
        let out = drain_pipe(&mut self.stdout, &mut self.out, Self::DRAIN_BUDGET);
        let err = drain_pipe(&mut self.stderr, &mut self.err, Self::DRAIN_BUDGET);
        out > 0 || err > 0
    }

    /// Collects what the exited child left in the pipes. Everything it wrote
    /// is already buffered by the kernel, so this never waits; the budget only
    /// bounds a grandchild that keeps writing.
    fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
        drain_pipe(&mut self.stdout, &mut self.out, MAX_CAPTURE);
        drain_pipe(&mut self.stderr, &mut self.err, MAX_CAPTURE);
        (self.out, self.err)
    }
}

#[cfg(unix)]
fn set_nonblocking<T: std::os::fd::AsRawFd>(pipe: &T) -> bool {
    let fd = pipe.as_raw_fd();
    // SAFETY: `fd` is an open descriptor owned by `pipe` for the whole call.
    // The flag is set on our end of the pipe only; the child's end is a
    // separate open file description and stays blocking.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
    }
}

/// Reads up to `budget` bytes that are available on `pipe` into `buf` and
/// returns how many were read. Closes the pipe on EOF or a read error.
#[cfg(unix)]
fn drain_pipe<R: std::io::Read>(pipe: &mut Option<R>, buf: &mut Vec<u8>, budget: usize) -> usize {
    use std::io::ErrorKind;

    let mut chunk = [0u8; 16 * 1024];
    let mut total = 0;
    while total < budget {
        let Some(reader) = pipe.as_mut() else { break };
        match reader.read(&mut chunk) {
            Ok(0) => *pipe = None,
            Ok(n) => {
                capture(buf, &chunk[..n]);
                total += n;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(_) => *pipe = None,
        }
    }
    total
}

/// Without non-blocking pipes each stream is read to its end on a thread;
/// when a thread cannot be started the pipe is closed and its output is lost.
#[cfg(not(unix))]
struct Pipes {
    stdout: Option<std::thread::JoinHandle<Vec<u8>>>,
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
}

#[cfg(not(unix))]
impl Pipes {
    fn attach(child: &mut Child) -> Self {
        Self {
            stdout: child.stdout.take().and_then(read_on_thread),
            stderr: child.stderr.take().and_then(read_on_thread),
        }
    }

    fn drain(&mut self) -> bool {
        false
    }

    fn finish(self) -> (Vec<u8>, Vec<u8>) {
        let join = |h: Option<std::thread::JoinHandle<Vec<u8>>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
        (join(self.stdout), join(self.stderr))
    }
}

#[cfg(not(unix))]
fn read_on_thread<R: std::io::Read + Send + 'static>(mut pipe: R) -> Option<std::thread::JoinHandle<Vec<u8>>> {
    std::thread::Builder::new()
        .name("tapwing-pipe".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.by_ref().take(MAX_CAPTURE as u64).read_to_end(&mut buf);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
            buf
        })
        .ok()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::interrupt::{InterruptedError, is_interrupted_error};

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    fn real_sleep(pause: Duration) -> Result<()> {
        std::thread::sleep(pause);
        Ok(())
    }

    fn process_alive(pid: &str) -> bool {
        Command::new("kill").args(["-0", pid]).output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// The pid a `echo $$; exec sleep 60` child wrote to `file` before it was killed.
    fn recorded_pid(file: &std::path::Path) -> String {
        let pid = std::fs::read_to_string(file).unwrap().trim().to_string();
        let _ = std::fs::remove_file(file);
        assert!(pid.parse::<u32>().is_ok(), "the child recorded no pid: {pid:?}");
        pid
    }

    fn pid_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tapwing-process-test-{}-{tag}.pid", std::process::id()))
    }

    #[test]
    fn a_command_that_finishes_in_time_returns_its_status_and_both_streams() {
        let out = run_with_timeout(&mut sh("echo out; echo err >&2; exit 3"), Duration::from_secs(30)).unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout_lossy(), "out\n");
        assert_eq!(out.stderr_lossy(), "err\n");

        let ok = run_with_timeout(&mut sh("cat"), Duration::from_secs(30)).unwrap();
        assert!(ok.status.success(), "stdin is closed, so `cat` ends at once");
        assert!(ok.stdout.is_empty());
    }

    #[test]
    fn a_missing_program_is_reported_with_the_command_line() {
        let mut cmd = Command::new("tapwing-no-such-tool");
        cmd.args(["simctl", "list"]);
        let err = format!("{:#}", run_with_timeout(&mut cmd, Duration::from_secs(1)).unwrap_err());
        assert!(err.starts_with("starting `tapwing-no-such-tool simctl list`"), "{err}");
    }

    #[test]
    fn a_command_past_its_deadline_is_killed_and_the_error_names_it_and_the_deadline() {
        let file = pid_file("deadline");
        let script = format!("echo $$ > '{}'; exec sleep 60", file.display());
        let err = run_polled(&mut sh(&script), Duration::from_secs(1), &mut real_sleep).unwrap_err().to_string();
        assert!(err.starts_with("`sh -c echo $$ > "), "{err}");
        assert!(err.ends_with("exec sleep 60` did not finish within 1 s and was killed"), "{err}");
        assert!(!process_alive(&recorded_pid(&file)), "the child survived its deadline");

        assert_eq!(format_duration(Duration::from_secs(180)), "180 s");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1500 ms");
    }

    #[test]
    fn an_interrupt_kills_the_command_and_stays_an_interrupted_error() {
        let file = pid_file("interrupt");
        let script = format!("echo $$ > '{}'; exec sleep 60", file.display());
        // The pid file is the proof that the child ran; only then "the signal arrives".
        let mut interrupt_once_started = |pause: Duration| {
            if file.exists() && std::fs::read_to_string(&file).is_ok_and(|s| s.ends_with('\n')) {
                return Err(InterruptedError { signal: 2 }.into());
            }
            real_sleep(pause)
        };
        let err = run_polled(&mut sh(&script), Duration::from_secs(60), &mut interrupt_once_started).unwrap_err();
        assert!(is_interrupted_error(&err), "{err:#}");
        assert!(err.to_string().ends_with("was killed before it finished"), "{err}");
        assert!(!process_alive(&recorded_pid(&file)), "the child survived the interrupt");
    }

    #[test]
    fn a_set_cancel_flag_kills_the_command() {
        let cancel = AtomicBool::new(true);
        let err = run_with_timeout_or_cancel(&mut sh("exec sleep 60"), Duration::from_secs(60), &cancel).unwrap_err();
        assert_eq!(format!("{err:#}"), "`sh -c exec sleep 60` was killed before it finished: cancelled");
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_is_captured_without_deadlock() {
        // 1 MiB per stream, far beyond the 16-64 KiB a pipe holds: without
        // draining, the child would block in `write` and never exit.
        let script = "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2; echo done";
        let out = run_with_timeout(&mut sh(script), Duration::from_secs(60)).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 1_048_576 + 5);
        assert!(out.stdout.ends_with(b"done\n"));
        assert_eq!(out.stderr.len(), 1_048_576);
    }

    #[test]
    fn a_grandchild_holding_the_pipes_open_does_not_hold_the_caller() {
        // `sleep` inherits stdout and stderr and outlives `sh`; reading the
        // pipes to EOF would wait for it.
        let out = run_with_timeout(&mut sh("sleep 60 & echo $!"), Duration::from_secs(30)).unwrap();
        assert!(out.status.success());
        let pid = out.stdout_lossy().trim().to_string();
        assert!(process_alive(&pid), "the grandchild should still be running");
        let _ = Command::new("kill").arg(&pid).output();
    }

    #[test]
    fn pid_liveness_tells_a_running_process_from_a_reaped_one_and_keeps_unknowns() {
        assert!(pid_alive(std::process::id()));
        let mut child = sh("exec sleep 60").spawn().unwrap();
        let pid = child.id();
        assert!(pid_alive(pid));
        assert!(kill_pid(pid));
        child.wait().unwrap();
        assert!(!pid_alive(pid), "a reaped child is gone");
        assert!(!kill_pid(pid));
        assert!(pid_alive(0), "pid 0 is a process group, never reported dead");
        assert!(pid_alive(u32::MAX), "a pid that does not fit pid_t is never reported dead");
        assert!(!kill_pid(0) && !kill_pid(u32::MAX));
    }

    #[test]
    fn captured_output_is_capped_and_long_commands_are_shortened_in_messages() {
        let mut buf = vec![0u8; MAX_CAPTURE - 2];
        capture(&mut buf, b"abcdef");
        assert_eq!(buf.len(), MAX_CAPTURE);
        assert!(buf.ends_with(b"ab"));
        capture(&mut buf, b"more");
        assert_eq!(buf.len(), MAX_CAPTURE);

        let mut cmd = Command::new("xcrun");
        cmd.arg("é".repeat(300));
        let label = describe(&cmd);
        assert_eq!(label.chars().count(), MAX_LABEL + 3);
        assert!(label.starts_with("xcrun é") && label.ends_with("..."));
    }
}
