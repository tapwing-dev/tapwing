//! A recording fake of the ADB server, and of a device behind it, for tests.
//!
//! [`FakeAdb`] listens on a loopback port and speaks the smartsocket services
//! the host commands use (`shell,v2`, `forward`, `killforward`), recording
//! every request, so a test can assert the exact scripts Tapwing sends without
//! an `adb` binary, a server or a device.
//!
//! [`ShDevice`] is what answers those scripts when a test wants more than
//! canned output: it runs them under the host's `sh` with the device commands
//! (`am`, `pm`, `pidof`, ...) replaced by shell functions that record their
//! arguments. That is how a test sees which arguments a hostile input ends up
//! as after the device's shell has parsed the generated script.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use super::AdbClient;

/// What a device shell command printed and how it exited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShellReply {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: u8,
}

impl ShellReply {
    pub(crate) fn ok(stdout: impl Into<String>) -> Self {
        Self { stdout: stdout.into(), ..Self::default() }
    }
}

/// One request the fake server received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// The script of a `shell,v2` service.
    Shell(String),
    /// A `host-serial:<serial>:` service, without that prefix (`forward:tcp:0;tcp:7001`).
    Host(String),
}

struct Shared {
    requests: Mutex<Vec<Request>>,
    shell: Box<dyn Fn(&str) -> ShellReply + Send + Sync>,
    /// The port an ephemeral `forward` reports.
    forward_port: AtomicU16,
    /// How many of the next device transports fail with `device offline`.
    offline: AtomicUsize,
    stop: AtomicBool,
}

/// The fake ADB server. Bound once; serves until dropped.
pub(crate) struct FakeAdb {
    addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl FakeAdb {
    /// Starts a server whose device answers each shell script with `shell`.
    pub(crate) fn start(shell: impl Fn(&str) -> ShellReply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake ADB server");
        let addr = listener.local_addr().expect("fake ADB server address");
        let shared = Arc::new(Shared {
            requests: Mutex::new(Vec::new()),
            shell: Box::new(shell),
            forward_port: AtomicU16::new(1),
            offline: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let serving = shared.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if serving.stop.load(Ordering::Acquire) {
                    break;
                }
                // A client that hangs up early (an availability probe) is not an error.
                if let Ok(stream) = stream {
                    let _ = serve(&serving, stream);
                }
            }
        });
        Self { addr, shared, thread: Some(thread) }
    }

    /// A server whose device runs every script on `device`.
    pub(crate) fn with_device(device: &Arc<ShDevice>) -> Self {
        let device = device.clone();
        Self::start(move |script| device.run(script))
    }

    /// Makes an ephemeral `forward` report `port` as the host port it bound.
    pub(crate) fn forward_to(self, port: u16) -> Self {
        self.shared.forward_port.store(port, Ordering::Release);
        self
    }

    /// Makes the next `transports` attempts to reach the device fail with `device offline`.
    pub(crate) fn go_offline_for(&self, transports: usize) {
        self.shared.offline.store(transports, Ordering::Release);
    }

    pub(crate) fn client(&self) -> AdbClient {
        AdbClient::with_addr(self.addr).with_timeouts(Duration::from_secs(2), Duration::from_secs(10))
    }

    /// Every request so far, in the order the server accepted them.
    pub(crate) fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().expect("fake ADB requests").clone()
    }

    /// The shell scripts among [`Self::requests`].
    pub(crate) fn shells(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter_map(|r| match r {
                Request::Shell(script) => Some(script),
                Request::Host(_) => None,
            })
            .collect()
    }

    /// The `host-serial` services among [`Self::requests`].
    pub(crate) fn host_requests(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter_map(|r| match r {
                Request::Host(service) => Some(service),
                Request::Shell(_) => None,
            })
            .collect()
    }

    /// Checks every script received so far with `sh -n` (skipped where there is no `sh`).
    pub(crate) fn assert_scripts_parse(&self) {
        if !sh_available() {
            return;
        }
        let scripts = self.shells();
        if let Some(problem) = sh_syntax_error(&scripts) {
            panic!("the device shell would reject a script: {problem}\n{scripts:#?}");
        }
    }
}

impl Drop for FakeAdb {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // Wakes the accept loop so it sees the flag.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_command(stream: &mut TcpStream) -> io::Result<String> {
    let mut len_hex = [0u8; 4];
    stream.read_exact(&mut len_hex)?;
    let len = std::str::from_utf8(&len_hex)
        .ok()
        .and_then(|hex| usize::from_str_radix(hex, 16).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad ADB command length"))?;
    let mut command = vec![0u8; len];
    stream.read_exact(&mut command)?;
    String::from_utf8(command).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_fail(stream: &mut TcpStream, message: &str) -> io::Result<()> {
    stream.write_all(format!("FAIL{:04x}{message}", message.len()).as_bytes())
}

fn write_shell_packet(stream: &mut TcpStream, id: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    stream.write_all(&[id])?;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(payload)
}

fn serve(shared: &Shared, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let command = read_command(&mut stream)?;
    if command.starts_with("host:transport:") {
        let offline = shared.offline.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1)).is_ok();
        if offline {
            return write_fail(&mut stream, "device offline");
        }
        stream.write_all(b"OKAY")?;
        return serve_device(shared, &mut stream);
    }
    // `host-serial:<serial>:<service>`; test serials have no colon.
    let Some((_, service)) = command.strip_prefix("host-serial:").and_then(|rest| rest.split_once(':')) else {
        return write_fail(&mut stream, "unknown host service");
    };
    shared.requests.lock().expect("fake ADB requests").push(Request::Host(service.to_string()));
    if service.starts_with("forward:tcp:0;") {
        let port = shared.forward_port.load(Ordering::Acquire).to_string();
        stream.write_all(format!("OKAYOKAY{:04x}{port}", port.len()).as_bytes())
    } else if service.starts_with("forward:") || service.starts_with("killforward:") {
        stream.write_all(b"OKAYOKAY")
    } else {
        write_fail(&mut stream, "unknown host service")
    }
}

fn serve_device(shared: &Shared, stream: &mut TcpStream) -> io::Result<()> {
    let service = read_command(stream)?;
    let Some(script) = service.strip_prefix("shell,v2,TERM=dumb:") else {
        return write_fail(stream, "unknown device service");
    };
    shared.requests.lock().expect("fake ADB requests").push(Request::Shell(script.to_string()));
    stream.write_all(b"OKAY")?;
    let reply = (shared.shell)(script);
    write_shell_packet(stream, 1, reply.stdout.as_bytes())?;
    write_shell_packet(stream, 2, reply.stderr.as_bytes())?;
    write_shell_packet(stream, 3, &[reply.exit_code])
}

/// A `sh` command that does not inherit this process's sockets.
///
/// Where a socket cannot be created close-on-exec in one step (macOS: `socket`
/// or `accept`, then `ioctl`), a child started by any thread in between
/// inherits it. A listener another test drops then stays open in that child,
/// and a port that test expects to refuse connections accepts them. Tests are
/// the only place that start processes this often, so the stand-in for the
/// device's shell closes what it inherited before it runs.
pub(crate) fn sh() -> Command {
    let mut sh = Command::new("sh");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        /// Above any descriptor a test process has open.
        const HIGHEST_FD: libc::c_int = 4096;
        // SAFETY: the closure runs between `fork` and `exec` and only calls `fcntl`,
        // which is async-signal-safe; it touches no memory of the parent.
        unsafe {
            sh.pre_exec(|| {
                for fd in 3..HIGHEST_FD {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
                Ok(())
            });
        }
    }
    sh
}

/// Whether there is a POSIX `sh` to check and run scripts with.
pub(crate) fn sh_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| cfg!(unix) && sh().args(["-c", "exit 0"]).status().is_ok_and(|s| s.success()))
}

/// What `sh -n` says about the first of `scripts` that does not parse. One
/// process checks them all.
pub(crate) fn sh_syntax_error(scripts: &[String]) -> Option<String> {
    const CHECK_EACH: &str =
        r#"i=0; for s; do sh -n -c "$s" || { echo "in script $i" >&2; exit 1; }; i=$((i+1)); done"#;
    let out = sh().args(["-c", CHECK_EACH, "sh"]).args(scripts).output().expect("running sh -n");
    (!out.status.success()).then(|| String::from_utf8_lossy(&out.stderr).trim().to_string())
}

/// Separators of the call log: between the arguments of one call, and between calls.
/// Neither can come out of a YAML flow, unlike a space or a newline.
const ARG_END: char = '\u{1f}';
const CALL_END: char = '\u{1e}';

/// The function stubs use to log a call: `am() { rec am "$@"; }`.
const REC: &str = r#"rec() { printf '%s\037' "$@" >> "$TAPWING_FAKE_CALLS"; printf '\036' >> "$TAPWING_FAKE_CALLS"; }"#;

/// A device made of shell functions (see the module docs). Each one has a
/// scratch directory (`$TAPWING_FAKE_DIR`) for state its stubs keep between
/// commands, removed when it is dropped.
pub(crate) struct ShDevice {
    dir: PathBuf,
    stubs: String,
}

impl ShDevice {
    /// `stubs` defines the device commands as shell functions; they call
    /// `rec <name> "$@"` to have a call show up in [`Self::calls`].
    pub(crate) fn new(stubs: &str) -> Arc<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = format!("tapwing-fake-device-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).expect("creating the fake device directory");
        Arc::new(Self { dir, stubs: stubs.to_string() })
    }

    /// The scratch directory, for a test to set up state its stubs read.
    pub(crate) fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    fn calls_file(&self) -> PathBuf {
        self.dir.join("calls")
    }

    /// Runs `script` as the device's shell would, with the stubs in place.
    pub(crate) fn run(&self, script: &str) -> ShellReply {
        let out = sh()
            .args(["-c", &format!("{REC}\n{}\n{script}", self.stubs)])
            .env("TAPWING_FAKE_CALLS", self.calls_file())
            .env("TAPWING_FAKE_DIR", &self.dir)
            .output()
            .expect("running the device script under sh");
        ShellReply {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit_code: out.status.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(255),
        }
    }

    /// The arguments of every recorded call, command name first, in call order.
    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        let log = std::fs::read_to_string(self.calls_file()).unwrap_or_default();
        log.split_terminator(CALL_END)
            .map(|call| call.split_terminator(ARG_END).map(str::to_string).collect())
            .collect()
    }

    /// [`Self::calls`] to one command, each joined with spaces (for calls
    /// whose arguments are known to have none).
    pub(crate) fn lines(&self, command: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|call| call.first().is_some_and(|c| c == command))
            .map(|c| c.join(" "))
            .collect()
    }
}

impl Drop for ShDevice {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_shell_and_host_requests_and_serves_their_replies() {
        let fake = FakeAdb::start(|script| match script {
            "false" => ShellReply { stdout: String::new(), stderr: "nope\n".into(), exit_code: 1 },
            other => ShellReply::ok(format!("ran {other}\n")),
        })
        .forward_to(40123);
        let client = fake.client();

        let out = client.shell("serial-1", "echo 'a b'").unwrap();
        assert_eq!((out.stdout.as_str(), out.exit_code), ("ran echo 'a b'\n", 0));
        let out = client.shell("serial-1", "false").unwrap();
        assert_eq!((out.stderr.as_str(), out.exit_code), ("nope\n", 1));
        assert_eq!(client.forward_ephemeral("serial-1", "tcp:7001").unwrap(), 40123);
        client.forward_remove("serial-1", "tcp:40123").unwrap();

        fake.go_offline_for(1);
        let err = format!("{:#}", client.shell("serial-1", "true").unwrap_err());
        assert!(err.contains("device offline"), "{err}");
        assert!(client.is_available());
        assert_eq!(client.shell("serial-1", "true").unwrap().stdout, "ran true\n");

        assert_eq!(
            fake.requests(),
            vec![
                Request::Shell("echo 'a b'".into()),
                Request::Shell("false".into()),
                Request::Host("forward:tcp:0;tcp:7001".into()),
                Request::Host("killforward:tcp:40123".into()),
                Request::Shell("true".into()),
            ]
        );
        assert_eq!(fake.shells().len(), 3);
        assert_eq!(fake.host_requests().len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn the_syntax_check_rejects_a_script_the_shell_cannot_parse() {
        let good = "if true; then echo \"$A\"; fi; case x in x) : ;; esac".to_string();
        assert_eq!(sh_syntax_error(&[good.clone(), "true".into()]), None);
        assert_eq!(sh_syntax_error(&[]), None);
        for bad in ["if true; then echo unfinished", "echo 'unterminated", "echo $("] {
            let problem =
                sh_syntax_error(&[good.clone(), bad.to_string()]).unwrap_or_else(|| panic!("accepted: {bad}"));
            assert!(problem.ends_with("in script 1"), "{bad}: {problem}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_sh_device_records_arguments_exactly_as_the_shell_split_them() {
        let device = ShDevice::new("am() { rec am \"$@\"; echo started; }\nfail() { rec fail; return 3; }");
        let reply = device.run("am start --es k 'two words' \"line1\nline2\" '$HOME `id`' ''; fail; exit $?");
        assert_eq!(reply, ShellReply { stdout: "started\n".into(), stderr: String::new(), exit_code: 3 });
        assert_eq!(
            device.calls(),
            vec![vec!["am", "start", "--es", "k", "two words", "line1\nline2", "$HOME `id`", ""], vec!["fail"]]
        );
        assert_eq!(device.lines("fail"), vec!["fail"]);
    }
}
