//! Synchronous HTTP client for Tapwing's iOS XCTest driver
//! (`TapwingRunnerUITests-Runner`, an XCUITest that serves HTTP on
//! `$TEST_RUNNER_PORT`).
//!
//! Endpoints, HTTP methods and JSON field names follow `xcuitest.XCTestDriverClient`
//! and `xcuitest.api.*` of `maestro-ios-driver.jar` (Maestro 2.12.1). Requests
//! are serialized like the Kotlin client's Jackson mapper (camelCase properties,
//! `Set<String>` as arrays). The client owns one [`ureq::Agent`], so every call
//! reuses a single keep-alive HTTP/1.1 connection to the runner.
//!
//! **Restart.** When the runner dies mid-run (the XCTest runner app crashes or
//! is killed, `xcodebuild` exits) its port stops answering and every later call
//! would fail. A client given a [`RestartFn`] ([`IosNativeClient::set_restart`])
//! notices that, tears the dead runner down, starts a new one through the
//! [`RestartFn`] (`xcodebuild`, port, `/status` readiness probe) and repeats the
//! call, at most [`MAX_RESTARTS`] times per run. An ordinary HTTP answer,
//! including an error status, never restarts. If the restart itself fails the
//! original error is kept and the lane stays down.
//!
//! **Stalls.** A runner can also be alive and wedged: a private XCTest call that
//! never returns holds its request gate, so every call times out against a
//! process that never exits. Each call sends its time budget
//! ([`DEADLINE_HEADER`]); the runner answers a request that ran out of it with
//! `deadlineExceeded` (the handler is still running) or `runnerBusy` (the
//! request was never started). Such an answer, like a plain host-side timeout
//! of a live runner (a runner built before the deadline existed), is a stall.
//! One stall only fails its call; [`STALLS_BEFORE_RESTART`] in a row restart
//! the runner, within the same [`MAX_RESTARTS`] budget. Any other answer
//! resets the count.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use ureq::Agent;

/// Per-call deadline. A hierarchy dump of a busy screen can take seconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Slowest typing rate the deadline of `inputText` / `eraseText` allows for.
/// The runner types 30 characters per second; half of that leaves room for a
/// busy simulator, so long text gets a deadline that grows with its length
/// instead of running past a fixed one while the runner keeps typing.
const TYPING_CHARS_PER_SEC: u64 = 15;
/// Connecting to the loopback runner either works at once or not at all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Largest accepted response body (a hierarchy / @3x PNG screenshot).
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// How many times one client restarts its runner (after a transport failure
/// or a stall). A runner that keeps dying is a real problem, not a blip to hide.
pub const MAX_RESTARTS: u32 = 3;

/// Consecutive stalled calls after which a live runner is restarted. One slow
/// call (a huge hierarchy) is not proof of a wedged runner; two in a row are.
pub const STALLS_BEFORE_RESTART: u32 = 2;

/// Request header with the time this call may still take, in milliseconds. The
/// runner answers before it runs out, so its timeout is an answer the host can
/// classify instead of a silent socket timeout. Older runners ignore it.
pub const DEADLINE_HEADER: &str = "X-Tapwing-Deadline-Ms";
/// `errorCode` of a runner whose handler ran past the deadline and still runs.
const CODE_DEADLINE_EXCEEDED: &str = "deadlineExceeded";
/// `errorCode` of a request the runner never started (busy or wedged).
const CODE_RUNNER_BUSY: &str = "runnerBusy";

/// Starts a replacement runner (the old one is already torn down) and returns
/// its client, whose own recovery is left unset.
pub type RestartFn = Box<dyn FnMut() -> Result<IosNativeClient> + Send>;

/// The process serving a client (`xcodebuild test-without-building`).
/// Dropping it stops the runner and frees its port.
pub trait RunnerProcess: Send {
    /// `Some(description)` once the process has exited.
    fn exited(&mut self) -> Option<String>;
}

/// `GET /deviceInfo` (`xcuitest.api.DeviceInfo`): logical points and physical pixels.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IosDeviceInfo {
    pub width_pixels: u32,
    pub height_pixels: u32,
    pub width_points: u32,
    pub height_points: u32,
}

/// Error body of the runner (`xcuitest.api.Error`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    #[serde(default)]
    error_message: String,
    #[serde(default)]
    error_code: String,
}

/// Whether a call may be sent again after the transport failed under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Replay {
    /// Reads and idempotent writes (`setOrientation` to an absolute value):
    /// always safe to repeat.
    Always,
    /// Input (`touch`, `swipeV2`, `inputText`, `eraseText`, `pressKey`,
    /// `pressButton`): repeated only when the request provably never left the
    /// host (the connection was refused), so a lost reply can never turn into a
    /// double tap, doubled text or a second `Lock` press.
    IfNotSent,
}

/// How a failed call relates to the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// Not a transport failure (a timeout of a live runner, a bad request).
    None,
    /// The connection could not be established, so nothing was sent.
    Refused,
    /// The connection broke; the request may or may not have been processed.
    Broken,
}

/// Reads a ureq error. `ureq` probes a pooled keep-alive connection before
/// reusing it and opens a fresh one when the runner closed it, so a runner
/// that died between calls shows up as a refused connection. A runner that
/// dies during a call closes the socket before or in the middle of its reply
/// (`UnexpectedEof` "Peer disconnected", or a reset). Timeouts and everything
/// else are not transport drops ([`IosNativeClient::fault`] looks at timeouts).
fn classify(e: &ureq::Error) -> Fault {
    use std::io::ErrorKind;
    match e {
        ureq::Error::Io(io) => match io.kind() {
            ErrorKind::ConnectionRefused => Fault::Refused,
            ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof
            | ErrorKind::NotConnected => Fault::Broken,
            _ => Fault::None,
        },
        _ => Fault::None,
    }
}

/// What a restart is curing; picks the wording of its messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trouble {
    /// The transport broke: the runner process is gone.
    Lost,
    /// [`STALLS_BEFORE_RESTART`] calls in a row timed out against a live runner.
    Stalled,
}

impl Trouble {
    /// "the iOS driver {..}".
    fn happened(self) -> &'static str {
        match self {
            Self::Lost => "lost its connection",
            Self::Stalled => "stopped answering",
        }
    }

    /// "restarted after {..}".
    fn after(self) -> &'static str {
        match self {
            Self::Lost => "losing its connection",
            Self::Stalled => "it stopped answering",
        }
    }

    fn notice(self) -> &'static str {
        match self {
            Self::Lost => "connection lost",
            Self::Stalled => "stopped answering",
        }
    }
}

/// A failed attempt that a restart may cure.
struct Failed {
    trouble: Trouble,
    /// The user-facing error of the attempt.
    error: anyhow::Error,
    /// For the restart notice.
    cause: String,
    /// The request provably never ran on the runner, so even input may be repeated.
    never_ran: bool,
}

/// The runner answered with an HTTP error status (its own, readable message): the
/// opposite of a transport or timeout failure, which says nothing about its answer.
#[derive(Debug)]
pub struct RunnerAnswerError(String);

impl std::fmt::Display for RunnerAnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RunnerAnswerError {}

/// Whether `e` is the runner's own HTTP error answer (see [`RunnerAnswerError`]).
pub fn is_runner_answer(e: &anyhow::Error) -> bool {
    e.is::<RunnerAnswerError>()
}

/// An HTTP exchange that got an answer: status and the whole body.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

/// Facade over the runner's HTTP API; one persistent connection.
pub struct IosNativeClient {
    agent: Agent,
    base: String,
    device: Option<IosDeviceInfo>,
    /// Stops the runner when dropped. Declared after `agent`, so the
    /// connection closes before the runner dies.
    runner: Option<Box<dyn RunnerProcess>>,
    recovery: Option<RestartFn>,
    restarts: u32,
    restart_notices: Vec<String>,
    call_timeout: Duration,
    /// Stalled calls in a row (see the module docs).
    stalls: u32,
}

impl IosNativeClient {
    /// Client for the runner on `127.0.0.1:<port>`. Nothing is sent yet, so
    /// this succeeds while `xcodebuild` is still starting the runner.
    pub fn connect(port: u16) -> Self {
        Self::connect_with_timeout(port, CALL_TIMEOUT)
    }

    fn connect_with_timeout(port: u16, call_timeout: Duration) -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(call_timeout))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .max_idle_connections(2)
            .max_idle_connections_per_host(2)
            .build();
        Self {
            agent: config.into(),
            base: format!("http://127.0.0.1:{port}"),
            device: None,
            runner: None,
            recovery: None,
            restarts: 0,
            restart_notices: Vec::new(),
            call_timeout,
            stalls: 0,
        }
    }

    /// Hands the client ownership of the process serving it: it is stopped
    /// when the client is dropped or replaced by a restart.
    pub fn set_runner(&mut self, runner: Box<dyn RunnerProcess>) {
        self.runner = Some(runner);
    }

    /// Lets the client start a replacement runner after a transport failure.
    pub fn set_restart(&mut self, restart: RestartFn) {
        self.recovery = Some(restart);
    }

    /// How many times this client restarted its runner.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// One user-facing line per restart since the last call.
    pub fn take_restart_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.restart_notices)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }

    /// One HTTP exchange, body read to the end (a connection with unread bytes
    /// cannot go back to the keep-alive pool). `POST` when `body` is given.
    /// `timeout` replaces the client's call timeout for this exchange; either
    /// way the runner is told the budget ([`DEADLINE_HEADER`]).
    fn exchange(&self, path: &str, body: Option<&[u8]>, timeout: Option<Duration>) -> Result<Answer, ureq::Error> {
        let url = self.url(path);
        let limit = timeout.unwrap_or(self.call_timeout);
        let budget = limit.as_millis().to_string();
        let resp = match body {
            None => self
                .agent
                .get(url)
                .config()
                .timeout_global(Some(limit))
                .build()
                .header(DEADLINE_HEADER, budget)
                .call()?,
            Some(payload) => self
                .agent
                .post(url)
                .config()
                .timeout_global(Some(limit))
                .build()
                .header("Content-Type", "application/json")
                .header(DEADLINE_HEADER, budget)
                .send(payload)?,
        };
        let status = resp.status().as_u16();
        let body = resp.into_body().with_config().limit(MAX_BODY).read_to_vec()?;
        Ok(Answer { status, body })
    }

    /// Deadline of a call that types or erases `chars` characters.
    fn typing_timeout(&self, chars: u64) -> Duration {
        self.call_timeout + Duration::from_millis(chars.saturating_mul(1000) / TYPING_CHARS_PER_SEC)
    }

    /// The runner's own message for an HTTP error status.
    fn error_detail(answer: &Answer) -> String {
        let raw = String::from_utf8_lossy(&answer.body);
        match serde_json::from_str::<ErrorBody>(&raw) {
            Ok(e) if !e.error_message.is_empty() => format!("{} ({})", e.error_message, e.error_code),
            _ => raw.trim().to_string(),
        }
    }

    /// Turns an HTTP error status into the runner's own message.
    fn finish(what: &str, answer: Answer) -> Result<Vec<u8>> {
        let status = answer.status;
        if (200..300).contains(&status) {
            return Ok(answer.body);
        }
        let detail = Self::error_detail(&answer);
        Err(RunnerAnswerError(format!("iOS driver `{what}` failed: HTTP {status}: {detail}")).into())
    }

    /// A runner-side timeout answer as a stall: `deadlineExceeded` (the handler
    /// may have applied the request and still runs) or `runnerBusy` (it never
    /// started). Not a [`RunnerAnswerError`]: the runner is hung, not answering.
    fn stall_answer(what: &str, answer: &Answer) -> Option<Failed> {
        if (200..300).contains(&answer.status) {
            return None;
        }
        let code = serde_json::from_slice::<ErrorBody>(&answer.body).ok()?.error_code;
        let never_ran = match code.as_str() {
            CODE_DEADLINE_EXCEEDED => false,
            CODE_RUNNER_BUSY => true,
            _ => return None,
        };
        let detail = Self::error_detail(answer);
        Some(Failed {
            trouble: Trouble::Stalled,
            error: anyhow!("iOS driver `{what}` timed out in the runner: {detail}"),
            cause: format!("`{what}`: {detail}"),
            never_ran,
        })
    }

    fn describe(what: &str, e: &ureq::Error) -> anyhow::Error {
        anyhow!("iOS driver `{what}` request failed: {e}")
    }

    /// [`classify`], plus a timeout of a runner whose process has exited: the
    /// port may hang instead of refusing, and only the process tells.
    fn fault(&mut self, e: &ureq::Error) -> Fault {
        match classify(e) {
            Fault::None if matches!(e, ureq::Error::Timeout(_)) => {
                if self.runner.as_mut().and_then(|r| r.exited()).is_some() { Fault::Broken } else { Fault::None }
            }
            f => f,
        }
    }

    /// One attempt of a call. `Err(Ok(failed))` is a failure a restart may
    /// cure; `Err(Err(e))` is final (an ordinary error answer, a bad request).
    fn attempt(
        &mut self,
        what: &str,
        path: &str,
        payload: Option<&[u8]>,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, Result<Failed>> {
        let err = match self.exchange(path, payload, timeout) {
            Ok(answer) => {
                return match Self::stall_answer(what, &answer) {
                    Some(stalled) => Err(Ok(stalled)),
                    None => {
                        self.stalls = 0;
                        Self::finish(what, answer).map_err(Err)
                    }
                };
            }
            Err(e) => e,
        };
        let fault = self.fault(&err);
        let error = Self::describe(what, &err);
        let cause = format!("`{what}`: {err}");
        match fault {
            // The runner process is alive and did not answer in time.
            Fault::None if matches!(err, ureq::Error::Timeout(_)) => {
                Err(Ok(Failed { trouble: Trouble::Stalled, error, cause, never_ran: false }))
            }
            Fault::None => Err(Err(error)),
            Fault::Refused | Fault::Broken => {
                Err(Ok(Failed { trouble: Trouble::Lost, error, cause, never_ran: fault == Fault::Refused }))
            }
        }
    }

    /// Runs one call. On a transport failure, or on the
    /// [`STALLS_BEFORE_RESTART`]-th stall in a row, restarts the runner (when
    /// this client can) and, if `replay` allows, repeats the call once.
    fn call(&mut self, what: &str, replay: Replay, path: &str, body: Option<&Value>) -> Result<Vec<u8>> {
        self.call_within(what, replay, path, body, None)
    }

    /// [`Self::call`] with its own deadline (`None`: the client's call timeout).
    fn call_within(
        &mut self,
        what: &str,
        replay: Replay,
        path: &str,
        body: Option<&Value>,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>> {
        let payload = body.map(serde_json::to_vec).transpose().context("encoding the iOS driver request")?;
        let failed = match self.attempt(what, path, payload.as_deref(), timeout) {
            Ok(bytes) => return Ok(bytes),
            Err(failed) => failed,
        };
        crate::interrupt::check()?;
        let Failed { trouble, error, cause, never_ran } = failed?;
        if trouble == Trouble::Stalled {
            self.stalls += 1;
            if self.stalls < STALLS_BEFORE_RESTART {
                return Err(error);
            }
        }
        if self.recovery.is_none() {
            return Err(error);
        }
        let happened = trouble.happened();
        if self.restarts >= MAX_RESTARTS {
            return Err(
                error.context(format!("the iOS driver {happened} again after {MAX_RESTARTS} restarts; giving up"))
            );
        }
        if let Err(e) = self.restart(trouble, &cause) {
            return Err(error.context(format!("the iOS driver {happened} and could not be restarted: {e:#}")));
        }
        if replay == Replay::IfNotSent && !never_ran {
            return Err(error.context(format!(
                "the iOS driver {happened} while handling this request and was restarted; the request was not \
                 repeated because it may already have been applied"
            )));
        }
        match self.attempt(what, path, payload.as_deref(), timeout) {
            Ok(bytes) => Ok(bytes),
            Err(again) => {
                let again = match again {
                    Ok(failed) => failed.error,
                    Err(e) => e,
                };
                Err(again.context(format!(
                    "the iOS driver was restarted after {}, but the request failed again",
                    trouble.after()
                )))
            }
        }
    }

    /// Replaces the dead runner with a freshly started one.
    fn restart(&mut self, trouble: Trouble, cause: &str) -> Result<()> {
        let mut recovery = self.recovery.take().ok_or_else(|| anyhow!("this client cannot restart"))?;
        // Tear the dead runner down first: its `simctl terminate` would otherwise
        // kill the new runner (same bundle id), and a fixed port would stay taken.
        drop(self.runner.take());
        // On failure `recovery` stays dropped: the lane is down and every later call
        // fails fast instead of retrying a restart that already failed.
        let mut fresh = recovery()?;
        fresh.recovery = Some(recovery);
        fresh.restarts = self.restarts + 1;
        fresh.restart_notices = std::mem::take(&mut self.restart_notices);
        fresh.restart_notices.push(format!(
            "iOS native driver {} ({cause}); restarted it (restart {}/{MAX_RESTARTS})",
            trouble.notice(),
            fresh.restarts
        ));
        // The new runner starts with its screen size uncached.
        *self = fresh;
        Ok(())
    }

    fn get_text(&mut self, what: &str, replay: Replay, path: &str) -> Result<String> {
        let bytes = self.call(what, replay, path, None)?;
        String::from_utf8(bytes).map_err(|e| anyhow!("iOS driver `{what}` response unreadable: {e}"))
    }

    fn post_text(&mut self, what: &str, replay: Replay, path: &str, body: &Value) -> Result<String> {
        let bytes = self.call(what, replay, path, Some(body))?;
        String::from_utf8(bytes).map_err(|e| anyhow!("iOS driver `{what}` response unreadable: {e}"))
    }

    /// Command whose answer carries nothing.
    fn post(&mut self, what: &str, replay: Replay, path: &str, body: &Value) -> Result<()> {
        self.call(what, replay, path, Some(body)).map(drop)
    }

    fn post_decode<T: for<'de> Deserialize<'de>>(&mut self, what: &str, path: &str, body: &Value) -> Result<T> {
        let text = self.post_text(what, Replay::Always, path, body)?;
        serde_json::from_str(&text).with_context(|| format!("iOS driver `{what}` returned unexpected JSON: {text}"))
    }

    /// Readiness probe (`GET /status`, `XCTestDriverClient.isChannelAlive`)
    /// with its own short `timeout`, so startup polling stays cancellable.
    /// Never restarts: it is how a (re)start finds out the runner is up.
    pub fn status(&self, timeout: Duration) -> Result<()> {
        let answer = self.exchange("status", None, Some(timeout)).map_err(|e| Self::describe("status", &e))?;
        Self::finish("status", answer).map(drop)
    }

    /// Screen size (`GET /deviceInfo`), fetched once and cached; call
    /// [`Self::invalidate_screen`] after a rotation.
    pub fn device_info(&mut self) -> Result<IosDeviceInfo> {
        if let Some(d) = self.device {
            return Ok(d);
        }
        let text = self.get_text("deviceInfo", Replay::Always, "deviceInfo")?;
        let d: IosDeviceInfo = serde_json::from_str(&text)
            .with_context(|| format!("iOS driver `deviceInfo` returned unexpected JSON: {text}"))?;
        self.device = Some(d);
        Ok(d)
    }

    pub fn invalidate_screen(&mut self) {
        self.device = None;
    }

    /// `POST /viewHierarchy`: the raw `{ axElement, depth }` JSON.
    pub fn view_hierarchy(&mut self, app_ids: &[String], exclude_keyboard: bool) -> Result<Value> {
        let text = self.post_text(
            "viewHierarchy",
            Replay::Always,
            "viewHierarchy",
            &json!({ "appIds": app_ids, "excludeKeyboardElements": exclude_keyboard }),
        )?;
        serde_json::from_str(&text).context("iOS driver `viewHierarchy` returned invalid JSON")
    }

    /// `POST /touch` in points; `duration` (seconds) turns the tap into a long press.
    pub fn touch(&mut self, x: f64, y: f64, duration: Option<f64>) -> Result<()> {
        self.post("touch", Replay::IfNotSent, "touch", &json!({ "x": x, "y": y, "duration": duration }))
    }

    /// `POST /swipeV2` in points (`XCTestDriverClient.swipeV2`: `appId` is null,
    /// the runner resolves the foreground app from `appIds`).
    pub fn swipe_v2(
        &mut self,
        app_id: Option<&str>,
        start: (f64, f64),
        end: (f64, f64),
        duration_secs: f64,
        app_ids: &[String],
    ) -> Result<()> {
        self.post(
            "swipeV2",
            Replay::IfNotSent,
            "swipeV2",
            &json!({
                "appId": app_id,
                "startX": start.0, "startY": start.1,
                "endX": end.0, "endY": end.1,
                "duration": duration_secs,
                "appIds": app_ids,
            }),
        )
    }

    /// `POST /inputText`. The deadline grows with the length of `text`
    /// ([`TYPING_CHARS_PER_SEC`]); the runner stops typing once it passes.
    pub fn input_text(&mut self, text: &str, app_ids: &[String]) -> Result<()> {
        let timeout = self.typing_timeout(u64::try_from(text.chars().count()).unwrap_or(u64::MAX));
        let body = json!({ "text": text, "appIds": app_ids });
        self.call_within("inputText", Replay::IfNotSent, "inputText", Some(&body), Some(timeout)).map(drop)
    }

    /// `POST /eraseText`, with a deadline that grows with `chars` like [`Self::input_text`].
    pub fn erase_text(&mut self, chars: u32, app_ids: &[String]) -> Result<()> {
        let timeout = self.typing_timeout(u64::from(chars));
        let body = json!({ "charactersToErase": chars, "appIds": app_ids });
        self.call_within("eraseText", Replay::IfNotSent, "eraseText", Some(&body), Some(timeout)).map(drop)
    }

    /// `POST /pressKey`: keyboard keys (`return`, `delete`).
    pub fn press_key(&mut self, key: &str) -> Result<()> {
        self.post("pressKey", Replay::IfNotSent, "pressKey", &json!({ "key": key }))
    }

    /// `POST /pressButton`: hardware buttons (`home`, `lock`). `lock` toggles,
    /// so a press is never repeated blindly.
    pub fn press_button(&mut self, button: &str) -> Result<()> {
        self.post("pressButton", Replay::IfNotSent, "pressButton", &json!({ "button": button }))
    }

    /// `POST /setOrientation`: `portrait`, `landscapeLeft`, `landscapeRight`, `upsideDown`.
    /// The cached screen size is dropped, since width and height swap. An
    /// absolute orientation, so repeating it is harmless.
    pub fn set_orientation(&mut self, orientation: &str) -> Result<()> {
        self.post("setOrientation", Replay::Always, "setOrientation", &json!({ "orientation": orientation }))?;
        self.invalidate_screen();
        Ok(())
    }

    /// `GET /isScreenStatic`: the runner compares consecutive screenshots.
    pub fn is_screen_static(&mut self) -> Result<bool> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Resp {
            is_screen_static: bool,
        }
        let text = self.get_text("isScreenStatic", Replay::Always, "isScreenStatic")?;
        let r: Resp = serde_json::from_str(&text)
            .with_context(|| format!("iOS driver `isScreenStatic` returned unexpected JSON: {text}"))?;
        Ok(r.is_screen_static)
    }

    /// `POST /runningApp`: bundle id of the foreground app among `app_ids`.
    pub fn running_app_id(&mut self, app_ids: &[String]) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Resp {
            running_app_bundle_id: String,
        }
        let r: Resp = self.post_decode("runningApp", "runningApp", &json!({ "appIds": app_ids }))?;
        Ok(r.running_app_bundle_id)
    }

    /// `POST /keyboard`: whether the soft keyboard is on screen.
    pub fn keyboard_visible(&mut self, app_ids: &[String]) -> Result<bool> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Resp {
            is_keyboard_visible: bool,
        }
        let r: Resp = self.post_decode("keyboard", "keyboard", &json!({ "appIds": app_ids }))?;
        Ok(r.is_keyboard_visible)
    }

    /// `GET /screenshot?compressed=false`: PNG bytes of the whole screen.
    pub fn screenshot(&mut self) -> Result<Vec<u8>> {
        self.call("screenshot", Replay::Always, "screenshot?compressed=false", None)
    }
}

/// Loopback HTTP/1.1 stand-in for the XCTest runner, shared by the tests of
/// this module tree. Keep-alive, `Content-Length` framed, records every request.
#[cfg(test)]
pub(crate) mod mock {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    #[derive(Clone, Debug)]
    pub struct Req {
        pub method: String,
        /// Path with query string, e.g. `/screenshot?compressed=false`.
        pub target: String,
        /// The request line and headers, as sent.
        pub head: String,
        pub body: String,
    }

    impl Req {
        pub fn path(&self) -> &str {
            self.target.split('?').next().unwrap_or("")
        }
        /// Value of the header `name` (case-insensitive), if sent.
        pub fn header(&self, name: &str) -> Option<&str> {
            self.head
                .lines()
                .skip(1)
                .filter_map(|l| l.split_once(':'))
                .find_map(|(k, v)| k.trim().eq_ignore_ascii_case(name).then_some(v.trim()))
        }
        pub fn json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
        }
    }

    pub struct Resp {
        pub status: u16,
        pub content_type: &'static str,
        pub body: Vec<u8>,
    }

    impl Resp {
        pub fn json(body: &str) -> Self {
            Self { status: 200, content_type: "application/json", body: body.as_bytes().to_vec() }
        }
    }

    pub type Handler = Box<dyn FnMut(&Req) -> Resp + Send>;

    pub struct MockRunner {
        pub port: u16,
        pub requests: Arc<Mutex<Vec<Req>>>,
        pub connections: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl MockRunner {
        pub fn start(handler: Handler) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let connections = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let handler = Arc::new(Mutex::new(handler));
            let thread = {
                let (requests, connections, stop) = (requests.clone(), connections.clone(), stop.clone());
                std::thread::spawn(move || {
                    let mut workers = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                connections.fetch_add(1, Ordering::Relaxed);
                                let (h, r, s) = (handler.clone(), requests.clone(), stop.clone());
                                workers.push(std::thread::spawn(move || serve(stream, &h, &r, &s)));
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(2)),
                        }
                    }
                    for w in workers {
                        let _ = w.join();
                    }
                })
            };
            Self { port, requests, connections, stop, thread: Some(thread) }
        }

        pub fn recorded(&self) -> Vec<Req> {
            self.requests.lock().unwrap().clone()
        }

        /// Recorded requests whose path is exactly `/<path>`.
        pub fn calls(&self, path: &str) -> Vec<Req> {
            let want = format!("/{path}");
            self.recorded().into_iter().filter(|r| r.path() == want).collect()
        }
    }

    impl Drop for MockRunner {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn serve(mut stream: TcpStream, handler: &Mutex<Handler>, requests: &Mutex<Vec<Req>>, stop: &AtomicBool) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 4096];
        while !stop.load(Ordering::Relaxed) {
            // Frame one request: headers, then `Content-Length` body bytes.
            let head_end = loop {
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
                match stream.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let mut lines = head.lines();
            let mut first = lines.next().unwrap_or("").split_whitespace();
            let (method, target) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
            let len = lines
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < head_end + len {
                match stream.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(_) => return,
                }
            }
            let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).into_owned();
            buf.drain(..head_end + len);
            let req = Req { method, target, head, body };
            requests.lock().unwrap().push(req.clone());
            let resp = (handler.lock().unwrap())(&req);
            let head = format!(
                "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
                resp.status,
                resp.content_type,
                resp.body.len()
            );
            if stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&resp.body)).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{MockRunner, Resp};
    use super::*;

    fn runner() -> MockRunner {
        MockRunner::start(Box::new(|req| match req.path() {
            "/status" => Resp::json("{}"),
            "/deviceInfo" => {
                Resp::json(r#"{"widthPixels":1206,"heightPixels":2622,"widthPoints":402,"heightPoints":874}"#)
            }
            "/isScreenStatic" => Resp::json(r#"{"isScreenStatic":true}"#),
            "/runningApp" => Resp::json(r#"{"runningAppBundleId":"dev.tapwing.sampleApp"}"#),
            "/keyboard" => Resp::json(r#"{"isKeyboardVisible":false}"#),
            "/viewHierarchy" => Resp::json(r#"{"axElement":{"label":"x"},"depth":1}"#),
            "/screenshot" => {
                Resp { status: 200, content_type: "image/png", body: vec![0x89, b'P', b'N', b'G', 0, 1, 2] }
            }
            "/boom" => Resp { status: 500, content_type: "application/json", body: Vec::new() },
            "/touch" if req.body.contains("\"x\":-1") => Resp {
                status: 500,
                content_type: "application/json",
                body: br#"{"errorMessage":"Error getting main window kAXErrorCannotComplete","errorCode":"internal"}"#
                    .to_vec(),
            },
            _ => Resp::json("{}"),
        }))
    }

    fn apps() -> Vec<String> {
        vec!["dev.tapwing.sampleApp".to_string(), "com.apple.springboard".to_string()]
    }

    #[test]
    fn device_info_is_cached_until_invalidated() {
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        let d = c.device_info().unwrap();
        assert_eq!(d, IosDeviceInfo { width_pixels: 1206, height_pixels: 2622, width_points: 402, height_points: 874 });
        c.device_info().unwrap();
        assert_eq!(r.calls("deviceInfo").len(), 1, "second call served from the cache");
        assert_eq!(r.calls("deviceInfo")[0].method, "GET");
        c.invalidate_screen();
        c.device_info().unwrap();
        assert_eq!(r.calls("deviceInfo").len(), 2);
    }

    #[test]
    fn endpoints_methods_and_json_fields_match_maestro() {
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        c.status(Duration::from_millis(500)).unwrap();
        let h = c.view_hierarchy(&apps(), true).unwrap();
        assert_eq!(h["depth"], 1);
        c.touch(201.0, 437.5, None).unwrap();
        c.touch(10.0, 20.0, Some(3.0)).unwrap();
        c.swipe_v2(None, (201.0, 786.0), (201.0, 87.0), 0.333, &apps()).unwrap();
        c.input_text("héllo", &apps()).unwrap();
        c.erase_text(7, &apps()).unwrap();
        c.press_key("return").unwrap();
        c.press_button("home").unwrap();
        c.set_orientation("landscapeLeft").unwrap();
        assert!(c.is_screen_static().unwrap());
        assert_eq!(c.running_app_id(&apps()).unwrap(), "dev.tapwing.sampleApp");
        assert!(!c.keyboard_visible(&apps()).unwrap());
        assert_eq!(c.screenshot().unwrap(), vec![0x89, b'P', b'N', b'G', 0, 1, 2]);

        let one = |p: &str| r.calls(p).remove(0);
        assert_eq!(one("status").method, "GET");
        assert_eq!(one("viewHierarchy").method, "POST");
        assert_eq!(
            one("viewHierarchy").json(),
            json!({ "appIds": ["dev.tapwing.sampleApp", "com.apple.springboard"], "excludeKeyboardElements": true })
        );
        let touches = r.calls("touch");
        assert_eq!(touches[0].json(), json!({ "x": 201.0, "y": 437.5, "duration": null }));
        assert_eq!(touches[1].json()["duration"], 3.0);
        assert_eq!(
            one("swipeV2").json(),
            json!({
                "appId": null, "startX": 201.0, "startY": 786.0, "endX": 201.0, "endY": 87.0,
                "duration": 0.333, "appIds": ["dev.tapwing.sampleApp", "com.apple.springboard"]
            })
        );
        assert_eq!(one("inputText").json()["text"], "héllo");
        assert_eq!(one("eraseText").json()["charactersToErase"], 7);
        assert_eq!(one("pressKey").json(), json!({ "key": "return" }));
        assert_eq!(one("pressButton").json(), json!({ "button": "home" }));
        assert_eq!(one("setOrientation").json(), json!({ "orientation": "landscapeLeft" }));
        assert_eq!(one("isScreenStatic").method, "GET");
        assert_eq!(one("runningApp").method, "POST");
        assert_eq!(one("keyboard").method, "POST");
        let shot = one("screenshot");
        assert_eq!((shot.method.as_str(), shot.target.as_str()), ("GET", "/screenshot?compressed=false"));
    }

    #[test]
    fn set_orientation_invalidates_the_cached_screen() {
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        c.device_info().unwrap();
        c.set_orientation("landscapeRight").unwrap();
        c.device_info().unwrap();
        assert_eq!(r.calls("deviceInfo").len(), 2, "rotation swaps width and height");
    }

    #[test]
    fn all_calls_share_one_keep_alive_connection() {
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        for _ in 0..5 {
            c.status(Duration::from_millis(500)).unwrap();
            c.touch(1.0, 2.0, None).unwrap();
            c.is_screen_static().unwrap();
        }
        c.device_info().unwrap();
        assert_eq!(r.recorded().len(), 16);
        assert_eq!(r.connections.load(std::sync::atomic::Ordering::Relaxed), 1, "persistent HTTP/1.1 connection");
    }

    #[test]
    fn http_errors_carry_the_runner_message() {
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        let err = c.touch(-1.0, 0.0, None).unwrap_err().to_string();
        assert!(err.contains("`touch`") && err.contains("HTTP 500"), "{err}");
        assert!(err.contains("kAXErrorCannotComplete") && err.contains("internal"), "{err}");
        // A non-JSON error body is passed through trimmed; the empty body still names the call.
        let err = c.post("boom", Replay::Always, "boom", &json!({})).unwrap_err().to_string();
        assert!(err.contains("`boom`") && err.contains("HTTP 500"), "{err}");
    }

    #[test]
    fn status_fails_fast_when_nothing_listens() {
        // Bind and drop to get a loopback port with no listener.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = IosNativeClient::connect(port);
        let t = std::time::Instant::now();
        assert!(c.status(Duration::from_millis(300)).is_err());
        assert!(t.elapsed() < Duration::from_secs(2), "refused connection must not wait for the call timeout");
    }

    // ---- Restart after a lost runner --------------------------------------------------------

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// A loopback port nothing listens on: connecting is refused.
    fn dead_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// A raw TCP server that runs `serve` on every accepted connection.
    fn raw_server(serve: fn(TcpStream)) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((s, _)) = l.accept() {
                std::thread::spawn(move || serve(s));
            }
        });
        port
    }

    /// Reads one request head (enough for the tests' small requests).
    fn read_request(s: &mut TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    /// A runner killed while it is reading the request: the request was sent.
    fn read_then_close(mut s: TcpStream) {
        read_request(&mut s);
    }

    /// A runner that dies between the reply's headers and the end of its body.
    fn cut_mid_body(mut s: TcpStream) {
        read_request(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"axEl");
    }

    /// A runner killed with the request still unread: the kernel resets the connection.
    fn reset_with_unread_request(s: TcpStream) {
        std::thread::sleep(Duration::from_millis(100));
        drop(s);
    }

    /// A live runner that never answers.
    fn hang(mut s: TcpStream) {
        read_request(&mut s);
        std::thread::sleep(Duration::from_secs(5));
    }

    /// Stand-in for `xcodebuild`: logs its teardown, optionally reports an exit.
    struct FakeRunner {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
        exited: bool,
    }

    impl RunnerProcess for FakeRunner {
        fn exited(&mut self) -> Option<String> {
            self.exited.then(|| "xcodebuild exited with exit status: 65".to_string())
        }
    }

    impl Drop for FakeRunner {
        fn drop(&mut self) {
            self.log.lock().unwrap_or_else(|e| e.into_inner()).push(format!("teardown {}", self.name));
        }
    }

    /// A restart that connects to `port` and counts how often it ran.
    fn restart_to(port: u16, count: &Arc<Mutex<u32>>) -> RestartFn {
        let count = count.clone();
        Box::new(move || {
            *count.lock().unwrap() += 1;
            Ok(IosNativeClient::connect(port))
        })
    }

    fn transport_error(port: u16, call_timeout: Duration) -> ureq::Error {
        let c = IosNativeClient::connect_with_timeout(port, call_timeout);
        match c.exchange("viewHierarchy", Some(b"{}"), None) {
            Ok(a) => panic!("expected a transport error, got HTTP {}", a.status),
            Err(e) => e,
        }
    }

    #[test]
    fn classifies_transport_failures_from_real_sockets() {
        // Nothing listens (the runner process is gone): nothing was sent.
        let e = transport_error(dead_port(), CALL_TIMEOUT);
        assert_eq!(classify(&e), Fault::Refused, "{e:?}");

        // Accepted, then closed before reading: the request may have been read.
        let e = transport_error(raw_server(drop), CALL_TIMEOUT);
        assert_eq!(classify(&e), Fault::Broken, "{e:?}");

        // Closed with the request unread (a reset).
        let e = transport_error(raw_server(reset_with_unread_request), CALL_TIMEOUT);
        assert_eq!(classify(&e), Fault::Broken, "{e:?}");

        // Closed after the request was read, before any reply.
        let e = transport_error(raw_server(read_then_close), CALL_TIMEOUT);
        assert_eq!(classify(&e), Fault::Broken, "{e:?}");

        // Closed in the middle of the reply body.
        let e = transport_error(raw_server(cut_mid_body), CALL_TIMEOUT);
        assert_eq!(classify(&e), Fault::Broken, "{e:?}");

        // A live runner that does not answer in time: a timeout, never a transport drop.
        let t = std::time::Instant::now();
        let e = transport_error(raw_server(hang), Duration::from_millis(150));
        assert!(matches!(e, ureq::Error::Timeout(_)), "{e:?}");
        assert_eq!(classify(&e), Fault::None, "{e:?}");
        assert!(t.elapsed() < Duration::from_secs(2), "the call timeout applies: {:?}", t.elapsed());

        // Hand-built errors of other kinds are not transport drops.
        assert_eq!(classify(&ureq::Error::ConnectionFailed), Fault::None);
        assert_eq!(classify(&ureq::Error::Io(std::io::Error::other("bad"))), Fault::None);

        // An HTTP error status is an answer, not an error of the exchange.
        let r = runner();
        let c = IosNativeClient::connect(r.port);
        let a = c.exchange("boom", Some(b"{}"), None).unwrap();
        assert_eq!(a.status, 500);
    }

    type Route = (&'static str, Replay, fn(&mut IosNativeClient) -> Result<()>);

    /// Every route of the runner except the `/status` probe, with its replay policy.
    fn routes() -> Vec<Route> {
        vec![
            ("deviceInfo", Replay::Always, |c| c.device_info().map(drop)),
            ("viewHierarchy", Replay::Always, |c| c.view_hierarchy(&apps(), false).map(drop)),
            ("screenshot", Replay::Always, |c| c.screenshot().map(drop)),
            ("isScreenStatic", Replay::Always, |c| c.is_screen_static().map(drop)),
            ("runningApp", Replay::Always, |c| c.running_app_id(&apps()).map(drop)),
            ("keyboard", Replay::Always, |c| c.keyboard_visible(&apps()).map(drop)),
            ("setOrientation", Replay::Always, |c| c.set_orientation("portrait")),
            ("touch", Replay::IfNotSent, |c| c.touch(10.0, 20.0, None)),
            ("swipeV2", Replay::IfNotSent, |c| c.swipe_v2(None, (1.0, 2.0), (3.0, 4.0), 0.3, &apps())),
            ("inputText", Replay::IfNotSent, |c| c.input_text("abc", &apps())),
            ("eraseText", Replay::IfNotSent, |c| c.erase_text(3, &apps())),
            ("pressKey", Replay::IfNotSent, |c| c.press_key("return")),
            ("pressButton", Replay::IfNotSent, |c| c.press_button("lock")),
        ]
    }

    #[test]
    fn a_refused_connection_restarts_the_runner_and_repeats_every_route() {
        for (path, _, call) in routes() {
            let fresh = runner();
            let restarts = Arc::new(Mutex::new(0));
            let mut c = IosNativeClient::connect(dead_port());
            c.set_restart(restart_to(fresh.port, &restarts));
            call(&mut c).unwrap_or_else(|e| panic!("{path}: {e:#}"));
            assert_eq!(*restarts.lock().unwrap(), 1, "{path}");
            assert_eq!(fresh.calls(path).len(), 1, "{path}: the request never left the host, so it is sent once");
            assert_eq!(c.restarts(), 1, "{path}");
            let notices = c.take_restart_notices();
            assert_eq!(notices.len(), 1, "{path}: {notices:?}");
            assert!(
                notices[0].contains("restarted it (restart 1/3)") && notices[0].contains(&format!("`{path}`")),
                "{notices:?}"
            );
            assert!(c.take_restart_notices().is_empty(), "notices are reported once");
        }
    }

    #[test]
    fn a_connection_broken_after_sending_replays_reads_but_never_input() {
        for (path, replay, call) in routes() {
            let fresh = runner();
            let restarts = Arc::new(Mutex::new(0));
            let mut c = IosNativeClient::connect(raw_server(read_then_close));
            c.set_restart(restart_to(fresh.port, &restarts));
            let res = call(&mut c);
            assert_eq!(*restarts.lock().unwrap(), 1, "{path}: the dead runner is restarted either way");
            match replay {
                Replay::Always => {
                    res.unwrap_or_else(|e| panic!("{path}: {e:#}"));
                    assert_eq!(fresh.calls(path).len(), 1, "{path} is repeated on the new runner");
                }
                Replay::IfNotSent => {
                    let err = format!("{:#}", res.unwrap_err());
                    assert!(err.contains("not repeated because it may already have been applied"), "{path}: {err}");
                    assert!(err.contains(&format!("`{path}`")), "{err}");
                    assert!(fresh.calls(path).is_empty(), "{path} must never be sent twice");
                }
            }
            // The new runner serves the next call without another restart.
            c.view_hierarchy(&apps(), false).unwrap();
            assert_eq!(*restarts.lock().unwrap(), 1);
        }
    }

    #[test]
    fn a_reply_cut_mid_body_restarts_the_runner() {
        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(raw_server(cut_mid_body));
        c.set_restart(restart_to(fresh.port, &restarts));
        assert_eq!(c.view_hierarchy(&apps(), false).unwrap()["depth"], 1);
        assert_eq!(*restarts.lock().unwrap(), 1);
    }

    #[test]
    fn the_dead_runner_is_torn_down_before_the_new_one_starts() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let fresh = runner();
        let mut c = IosNativeClient::connect(dead_port());
        c.set_runner(Box::new(FakeRunner { name: "old", log: log.clone(), exited: false }));
        let (port, restart_log) = (fresh.port, log.clone());
        c.set_restart(Box::new(move || {
            restart_log.lock().unwrap().push("start new".to_string());
            let mut next = IosNativeClient::connect(port);
            next.set_runner(Box::new(FakeRunner { name: "new", log: restart_log.clone(), exited: false }));
            Ok(next)
        }));
        let d = c.device_info().unwrap();
        assert_eq!(d.width_points, 402);
        assert_eq!(*log.lock().unwrap(), ["teardown old", "start new"]);
        drop(c);
        assert_eq!(*log.lock().unwrap(), ["teardown old", "start new", "teardown new"]);
    }

    #[test]
    fn a_restart_starts_with_the_screen_size_uncached() {
        let first = runner();
        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(first.port);
        c.set_restart(restart_to(fresh.port, &restarts));
        c.device_info().unwrap();
        drop(first);
        c.view_hierarchy(&apps(), false).unwrap();
        assert_eq!(*restarts.lock().unwrap(), 1);
        c.device_info().unwrap();
        assert_eq!(fresh.calls("deviceInfo").len(), 1, "the new runner may run in another orientation");
    }

    #[test]
    fn restarts_are_capped_per_run() {
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(dead_port());
        // Every "restarted" runner is dead again.
        let count = restarts.clone();
        c.set_restart(Box::new(move || {
            *count.lock().unwrap() += 1;
            Ok(IosNativeClient::connect(dead_port()))
        }));
        for n in 1..=MAX_RESTARTS {
            let err = format!("{:#}", c.view_hierarchy(&apps(), false).unwrap_err());
            assert!(err.contains("restarted after losing its connection, but the request failed again"), "{err}");
            assert_eq!(c.restarts(), n);
        }
        let err = format!("{:#}", c.view_hierarchy(&apps(), false).unwrap_err());
        assert!(err.contains("after 3 restarts; giving up") && err.contains("`viewHierarchy`"), "{err}");
        assert_eq!(*restarts.lock().unwrap(), MAX_RESTARTS);
        assert_eq!(c.take_restart_notices().len(), MAX_RESTARTS as usize);
    }

    #[test]
    fn a_failed_restart_keeps_the_error_and_is_not_attempted_again() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let attempts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(dead_port());
        c.set_runner(Box::new(FakeRunner { name: "old", log: log.clone(), exited: true }));
        let count = attempts.clone();
        c.set_restart(Box::new(move || {
            *count.lock().unwrap() += 1;
            Err(anyhow!("xcodebuild exited during startup (exit status: 70)"))
        }));
        let err = format!("{:#}", c.touch(1.0, 2.0, None).unwrap_err());
        assert!(err.contains("could not be restarted") && err.contains("exit status: 70"), "{err}");
        assert!(err.contains("`touch` request failed") && err.contains("refused"), "original error kept: {err}");
        assert_eq!(*log.lock().unwrap(), ["teardown old"], "the dead runner is gone either way");
        // The lane is down: later calls fail fast on the transport error, with no new restart.
        let t = std::time::Instant::now();
        let err = format!("{:#}", c.view_hierarchy(&apps(), false).unwrap_err());
        assert!(err.contains("`viewHierarchy` request failed") && !err.contains("restart"), "{err}");
        assert!(t.elapsed() < Duration::from_secs(1));
        assert_eq!(*attempts.lock().unwrap(), 1);
        assert_eq!(c.restarts(), 0);
        assert!(c.take_restart_notices().is_empty());
    }

    #[test]
    fn answers_and_timeouts_of_a_live_runner_never_restart() {
        let restarts = Arc::new(Mutex::new(0));
        // An HTTP error status is the runner answering.
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        c.set_restart(restart_to(dead_port(), &restarts));
        let err = c.touch(-1.0, 0.0, None).unwrap_err().to_string();
        assert!(err.contains("HTTP 500"), "{err}");
        // A hanging call of a runner whose process is alive.
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut c = IosNativeClient::connect_with_timeout(raw_server(hang), Duration::from_millis(150));
        c.set_runner(Box::new(FakeRunner { name: "live", log: log.clone(), exited: false }));
        c.set_restart(restart_to(dead_port(), &restarts));
        let err = c.view_hierarchy(&apps(), false).unwrap_err().to_string();
        assert!(err.contains("timeout"), "{err}");
        assert_eq!(*restarts.lock().unwrap(), 0);
        assert!(log.lock().unwrap().is_empty(), "a live runner is not torn down");
        assert!(c.take_restart_notices().is_empty());
    }

    // ---- Stalls: a live runner that stopped answering ---------------------------------------

    fn timeout_answer(code: &str, message: &str) -> Resp {
        Resp {
            status: 408,
            content_type: "application/json",
            body: format!(r#"{{"errorMessage":"{message}","errorCode":"{code}","code":"{code}"}}"#).into_bytes(),
        }
    }

    /// A runner wedged in its first request: that one runs out of its deadline
    /// (`first`), every later one is refused unstarted. `/status` still answers.
    fn wedged_runner(first: &'static str) -> MockRunner {
        let seen = std::sync::atomic::AtomicUsize::new(0);
        MockRunner::start(Box::new(move |req| {
            if req.path() == "/status" {
                return Resp::json(r#"{"status":"wedged","wedged":true}"#);
            }
            if seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                timeout_answer(first, "`POST /viewHierarchy` did not finish within 28.0 s")
            } else {
                timeout_answer("runnerBusy", "not started: the runner is wedged in `POST /viewHierarchy`")
            }
        }))
    }

    #[test]
    fn every_call_sends_its_time_budget() {
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        c.device_info().unwrap();
        c.touch(1.0, 2.0, None).unwrap();
        assert_eq!(r.calls("deviceInfo")[0].header(DEADLINE_HEADER), Some("30000"));
        assert_eq!(r.calls("touch")[0].header("x-tapwing-deadline-ms"), Some("30000"));
        assert!(CALL_TIMEOUT.as_millis() == 30_000);
    }

    #[test]
    fn typing_deadlines_grow_with_the_text_and_reach_the_runner() {
        let r = runner();
        let mut c = IosNativeClient::connect(r.port);
        c.input_text("abc", &apps()).unwrap();
        // 1500 characters at the 15 characters per second floor: 100 s on top of the 30 s call timeout.
        c.input_text(&"ž".repeat(1500), &apps()).unwrap();
        c.erase_text(300, &apps()).unwrap();
        c.erase_text(0, &apps()).unwrap();
        let budget = |path: &str, i: usize| r.calls(path)[i].header(DEADLINE_HEADER).unwrap().to_string();
        assert_eq!(budget("inputText", 0), "30200");
        assert_eq!(budget("inputText", 1), "130000", "characters, not UTF-8 bytes");
        assert_eq!(budget("eraseText", 0), "50000");
        assert_eq!(budget("eraseText", 1), "30000");
        assert_eq!(c.typing_timeout(u64::MAX), CALL_TIMEOUT + Duration::from_millis(u64::MAX / 15));
    }

    #[test]
    fn a_long_text_is_not_cut_off_by_the_plain_call_timeout() {
        // The runner answers after 3x the client's call timeout; the scaled typing deadline covers it.
        fn slow_ok(mut s: TcpStream) {
            let mut seen = Vec::new();
            let mut chunk = [0_u8; 4096];
            // Wait for the whole request: its body ends with the closing brace of the JSON.
            while !seen.ends_with(b"}") {
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => seen.extend_from_slice(&chunk[..n]),
                }
            }
            std::thread::sleep(Duration::from_millis(300));
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}");
        }
        let mut c = IosNativeClient::connect_with_timeout(raw_server(slow_ok), Duration::from_millis(100));
        // 15 characters buy one more second.
        c.input_text(&"a".repeat(15), &apps()).unwrap();
        let err = c.touch(1.0, 2.0, None).unwrap_err().to_string();
        assert!(err.contains("timeout"), "other calls keep the plain call timeout: {err}");
    }

    #[test]
    fn one_stall_fails_its_call_and_the_second_in_a_row_restarts_the_runner() {
        let wedged = wedged_runner("deadlineExceeded");
        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut c = IosNativeClient::connect(wedged.port);
        c.set_runner(Box::new(FakeRunner { name: "wedged", log: log.clone(), exited: false }));
        c.set_restart(restart_to(fresh.port, &restarts));

        // The first timeout may be one slow call: it fails, nothing is restarted.
        let first = c.view_hierarchy(&apps(), false).unwrap_err();
        let text = format!("{first:#}");
        assert!(text.contains("`viewHierarchy` timed out in the runner"), "{text}");
        assert!(text.contains("did not finish within 28.0 s") && text.contains("deadlineExceeded"), "{text}");
        assert!(!is_runner_answer(&first), "a hung runner is not an answering one");
        assert_eq!(*restarts.lock().unwrap(), 0);
        assert!(log.lock().unwrap().is_empty());

        // The second one in a row proves the runner is wedged: it is replaced and the read repeated.
        assert_eq!(c.view_hierarchy(&apps(), false).unwrap()["depth"], 1);
        assert_eq!(*restarts.lock().unwrap(), 1);
        assert_eq!(*log.lock().unwrap(), ["teardown wedged"], "the live but wedged runner is torn down");
        assert_eq!(wedged.calls("viewHierarchy").len(), 2);
        assert_eq!(fresh.calls("viewHierarchy").len(), 1);
        let notices = c.take_restart_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("iOS native driver stopped answering"), "{notices:?}");
        assert!(
            notices[0].contains("wedged in `POST /viewHierarchy`") && notices[0].contains("restart 1/3"),
            "{notices:?}"
        );

        // The replacement starts with a clean count.
        c.view_hierarchy(&apps(), false).unwrap();
        assert_eq!(*restarts.lock().unwrap(), 1);
    }

    #[test]
    fn input_refused_unstarted_is_repeated_after_the_restart_but_input_in_flight_is_not() {
        // `runnerBusy`: the wedged runner never started the tap, so it is safe to send again.
        let wedged = wedged_runner("deadlineExceeded");
        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(wedged.port);
        c.set_restart(restart_to(fresh.port, &restarts));
        assert!(c.view_hierarchy(&apps(), false).is_err());
        c.touch(10.0, 20.0, None).unwrap();
        assert_eq!(*restarts.lock().unwrap(), 1);
        assert_eq!(fresh.calls("touch").len(), 1, "the tap ran exactly once");

        // `deadlineExceeded` twice: the second tap may have been applied, so it is not repeated.
        let stuck = MockRunner::start(Box::new(|_| timeout_answer("deadlineExceeded", "did not finish")));
        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(stuck.port);
        c.set_restart(restart_to(fresh.port, &restarts));
        assert!(c.touch(10.0, 20.0, None).is_err());
        let err = format!("{:#}", c.touch(10.0, 20.0, None).unwrap_err());
        assert!(err.contains("stopped answering while handling this request and was restarted"), "{err}");
        assert!(err.contains("not repeated because it may already have been applied"), "{err}");
        assert_eq!(*restarts.lock().unwrap(), 1);
        assert!(fresh.calls("touch").is_empty());
        // The lane is usable again.
        c.touch(10.0, 20.0, None).unwrap();
        assert_eq!(*restarts.lock().unwrap(), 1);
    }

    #[test]
    fn an_answer_between_two_stalls_resets_the_count() {
        // Every other call stalls: slow, but the runner keeps answering in between.
        let n = std::sync::atomic::AtomicUsize::new(0);
        let r = MockRunner::start(Box::new(move |_| {
            if n.fetch_add(1, std::sync::atomic::Ordering::Relaxed).is_multiple_of(2) {
                timeout_answer("deadlineExceeded", "did not finish")
            } else {
                Resp { status: 500, content_type: "application/json", body: Vec::new() }
            }
        }));
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(r.port);
        c.set_restart(restart_to(dead_port(), &restarts));
        for _ in 0..3 {
            assert!(c.is_screen_static().is_err(), "stall");
            assert!(c.is_screen_static().is_err(), "an HTTP 500 is an answer");
        }
        assert_eq!(*restarts.lock().unwrap(), 0);
    }

    #[test]
    fn host_side_timeouts_of_a_live_runner_without_deadlines_count_as_stalls() {
        // A runner built before the deadline existed never answers a wedged call.
        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut c = IosNativeClient::connect_with_timeout(raw_server(hang), Duration::from_millis(150));
        c.set_runner(Box::new(FakeRunner { name: "old", log: log.clone(), exited: false }));
        c.set_restart(restart_to(fresh.port, &restarts));
        let err = c.view_hierarchy(&apps(), false).unwrap_err().to_string();
        assert!(err.contains("timeout"), "{err}");
        assert_eq!(*restarts.lock().unwrap(), 0);
        assert_eq!(c.view_hierarchy(&apps(), false).unwrap()["depth"], 1);
        assert_eq!(*restarts.lock().unwrap(), 1);
        assert_eq!(*log.lock().unwrap(), ["teardown old"]);
        assert!(c.take_restart_notices()[0].contains("stopped answering"));
    }

    #[test]
    fn stall_restarts_share_the_restart_budget_and_need_a_restart_function() {
        // Without a restart function the calls just keep failing.
        let stuck = MockRunner::start(Box::new(|_| timeout_answer("runnerBusy", "wedged")));
        let mut c = IosNativeClient::connect(stuck.port);
        for _ in 0..4 {
            assert!(c.view_hierarchy(&apps(), false).is_err());
        }
        assert_eq!(c.restarts(), 0);

        // Every replacement is wedged again: the budget of restarts ends the loop.
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(stuck.port);
        c.set_restart(restart_to(stuck.port, &restarts));
        assert!(c.view_hierarchy(&apps(), false).is_err(), "first stall");
        let mut last = String::new();
        for _ in 0..20 {
            last = format!("{:#}", c.view_hierarchy(&apps(), false).unwrap_err());
            if last.contains("giving up") {
                break;
            }
        }
        assert!(last.contains("stopped answering again after 3 restarts; giving up"), "{last}");
        assert_eq!(*restarts.lock().unwrap(), MAX_RESTARTS);
        assert_eq!(c.restarts(), MAX_RESTARTS);
    }

    #[test]
    fn a_timeout_after_the_runner_process_exited_restarts_it() {
        for (path, replay, call) in [routes()[1], routes()[7]] {
            let fresh = runner();
            let restarts = Arc::new(Mutex::new(0));
            let log = Arc::new(Mutex::new(Vec::new()));
            let mut c = IosNativeClient::connect_with_timeout(raw_server(hang), Duration::from_millis(150));
            c.set_runner(Box::new(FakeRunner { name: "exited", log: log.clone(), exited: true }));
            c.set_restart(restart_to(fresh.port, &restarts));
            let res = call(&mut c);
            assert_eq!(*restarts.lock().unwrap(), 1, "{path}");
            assert_eq!(*log.lock().unwrap(), ["teardown exited"]);
            // The hung request may have been applied: input is not repeated.
            assert_eq!(res.is_ok(), replay == Replay::Always, "{path}: {res:?}");
            assert_eq!(fresh.calls(path).len(), usize::from(replay == Replay::Always), "{path}");
        }
    }

    #[test]
    fn the_status_probe_never_restarts() {
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(dead_port());
        c.set_restart(restart_to(dead_port(), &restarts));
        assert!(c.status(Duration::from_millis(200)).is_err());
        assert_eq!(*restarts.lock().unwrap(), 0);
    }

    #[test]
    fn a_healthy_runner_never_triggers_a_restart() {
        let r = runner();
        let restarts = Arc::new(Mutex::new(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut c = IosNativeClient::connect(r.port);
        c.set_runner(Box::new(FakeRunner { name: "healthy", log: log.clone(), exited: false }));
        c.set_restart(restart_to(dead_port(), &restarts));
        for _ in 0..3 {
            for (path, _, call) in routes() {
                call(&mut c).unwrap_or_else(|e| panic!("{path}: {e:#}"));
            }
        }
        assert_eq!(*restarts.lock().unwrap(), 0);
        assert_eq!(c.restarts(), 0);
        assert!(c.take_restart_notices().is_empty());
        assert!(log.lock().unwrap().is_empty());
        assert_eq!(r.connections.load(std::sync::atomic::Ordering::Relaxed), 1, "still one keep-alive connection");
    }

    /// Reads one HTTP/1.1 request (head + Content-Length body) and returns `"METHOD /target"`,
    /// or `None` on EOF before a complete head.
    fn read_http_request_line(s: &mut TcpStream) -> Option<String> {
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        let head_end = loop {
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut lines = head.lines();
        let first = lines.next().unwrap_or("");
        let mut parts = first.split_whitespace();
        let req_line = format!("{} {}", parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let content_len = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while buf.len() < head_end + content_len {
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        Some(req_line)
    }

    fn device_info_http_reply() -> Vec<u8> {
        let body = r#"{"widthPixels":1206,"heightPixels":2622,"widthPoints":402,"heightPoints":874}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn ureq_does_not_retry_post_when_pooled_connection_drops_after_send() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let received: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (conns_t, recv_t, stop_t) = (connections.clone(), received.clone(), stop.clone());
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let cid = conns_t.fetch_add(1, Ordering::SeqCst) + 1;
                        let recv = recv_t.clone();
                        workers.push(std::thread::spawn(move || {
                            s.set_nonblocking(false).unwrap();
                            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                            let Some(req1) = read_http_request_line(&mut s) else { return };
                            recv.lock().unwrap().push((cid, req1));
                            if cid == 1 {
                                let _ = s.write_all(&device_info_http_reply());
                                // Keep connection #1 open in the pool, read the second request
                                // (POST /touch), then drop without replying.
                                if let Some(req2) = read_http_request_line(&mut s) {
                                    recv.lock().unwrap().push((cid, req2));
                                }
                            }
                        }));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
            for w in workers {
                let _ = w.join();
            }
        });

        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(port);
        c.set_restart(restart_to(fresh.port, &restarts));

        // 1. Prime the keep-alive pool with GET /deviceInfo on connection #1.
        assert_eq!(c.device_info().unwrap().width_points, 402);

        // 2. Send POST /touch over the pooled connection; server receives it and drops the socket.
        // Neither ureq nor Tapwing may send POST /touch a second time.
        let err = format!("{:#}", c.touch(10.0, 20.0, None).unwrap_err());
        assert!(err.contains("not repeated because it may already have been applied"), "{err}");
        assert_eq!(*restarts.lock().unwrap(), 1);
        assert_eq!(c.restarts(), 1);
        assert!(fresh.calls("touch").is_empty(), "Tapwing must not replay POST /touch on the new runner");

        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();

        assert_eq!(connections.load(Ordering::SeqCst), 1, "ureq must not open a second connection to retry POST");
        assert_eq!(*received.lock().unwrap(), vec![(1, "GET /deviceInfo".to_string()), (1, "POST /touch".to_string())]);
        // The replacement runner is healthy for subsequent calls.
        assert_eq!(c.view_hierarchy(&apps(), false).unwrap()["depth"], 1);
        assert_eq!(*restarts.lock().unwrap(), 1);
    }

    #[test]
    fn pooled_connection_dropped_between_calls_when_runner_dies_replays_input_once() {
        let first = runner();
        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(first.port);
        c.set_restart(restart_to(fresh.port, &restarts));

        // Prime the keep-alive pool, then kill the runner completely between calls.
        c.device_info().unwrap();
        let first_recorded = first.requests.clone();
        drop(first);

        // ureq probes the pooled socket, evicts it before writing, hits ConnectionRefused -> Fault::Refused,
        // so Tapwing restarts and safely replays POST /touch once.
        c.touch(10.0, 20.0, None).unwrap();
        assert_eq!(*restarts.lock().unwrap(), 1);
        assert_eq!(c.restarts(), 1);
        assert!(
            first_recorded.lock().unwrap().iter().all(|r| r.path() != "/touch"),
            "old runner never received /touch"
        );
        assert_eq!(fresh.calls("touch").len(), 1, "new runner received /touch once");
    }

    #[test]
    fn pooled_connection_closed_by_live_server_sends_post_once_and_never_retries_mid_flight() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let conn1_closed = Arc::new(AtomicBool::new(false));
        let received: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (conns_t, c1_t, recv_t, stop_t) =
            (connections.clone(), conn1_closed.clone(), received.clone(), stop.clone());
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let cid = conns_t.fetch_add(1, Ordering::SeqCst) + 1;
                        let recv = recv_t.clone();
                        let c1 = c1_t.clone();
                        workers.push(std::thread::spawn(move || {
                            s.set_nonblocking(false).unwrap();
                            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                            let Some(req) = read_http_request_line(&mut s) else { return };
                            recv.lock().unwrap().push((cid, req));
                            if cid == 1 {
                                let _ = s.write_all(&device_info_http_reply());
                                drop(s);
                                c1.store(true, Ordering::SeqCst);
                            }
                            // Connection #2 (and any unexpected #3): read the POST and close without replying.
                        }));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
            for w in workers {
                let _ = w.join();
            }
        });

        let fresh = runner();
        let restarts = Arc::new(Mutex::new(0));
        let mut c = IosNativeClient::connect(port);
        c.set_restart(restart_to(fresh.port, &restarts));

        c.device_info().unwrap();
        while !conn1_closed.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        // Let the loopback FIN arrive before the next pool probe.
        std::thread::sleep(Duration::from_millis(20));

        let err = format!("{:#}", c.input_text("abc", &apps()).unwrap_err());
        assert!(err.contains("not repeated because it may already have been applied"), "{err}");
        assert_eq!(*restarts.lock().unwrap(), 1);
        assert!(fresh.calls("inputText").is_empty());

        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();

        assert_eq!(
            connections.load(Ordering::SeqCst),
            2,
            "conn #1 evicted on probe, conn #2 sent POST once, no conn #3 retry"
        );
        assert_eq!(
            *received.lock().unwrap(),
            vec![(1, "GET /deviceInfo".to_string()), (2, "POST /inputText".to_string())]
        );
    }

    fn cut_mid_header(mut s: TcpStream) {
        read_request(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: appl");
    }

    #[test]
    fn mid_header_eof_and_mid_body_cut_classify_as_broken_and_never_replay_input() {
        let e = transport_error(raw_server(cut_mid_header), CALL_TIMEOUT);
        assert_eq!(classify(&e), Fault::Broken, "{e:?}");

        for (label, handler) in [("mid_header", cut_mid_header as fn(TcpStream)), ("mid_body", cut_mid_body)] {
            // Reads restart and replay.
            let fresh = runner();
            let restarts = Arc::new(Mutex::new(0));
            let mut c = IosNativeClient::connect(raw_server(handler));
            c.set_restart(restart_to(fresh.port, &restarts));
            assert_eq!(c.view_hierarchy(&apps(), false).unwrap()["depth"], 1, "{label}");
            assert_eq!(*restarts.lock().unwrap(), 1, "{label}");
            assert_eq!(fresh.calls("viewHierarchy").len(), 1, "{label}");

            // Input restarts but must never be replayed.
            let fresh = runner();
            let restarts = Arc::new(Mutex::new(0));
            let mut c = IosNativeClient::connect(raw_server(handler));
            c.set_restart(restart_to(fresh.port, &restarts));
            let err = format!("{:#}", c.touch(10.0, 20.0, None).unwrap_err());
            assert!(err.contains("not repeated because it may already have been applied"), "{label}: {err}");
            assert_eq!(*restarts.lock().unwrap(), 1, "{label}");
            assert!(fresh.calls("touch").is_empty(), "{label}: input must not be repeated");
        }
    }

    #[test]
    fn failed_second_restart_preserves_first_restart_notice() {
        let second = Arc::new(Mutex::new(Some(runner())));
        let second_port = second.lock().unwrap().as_ref().unwrap().port;
        let attempts = Arc::new(Mutex::new(0u32));
        let attempts_c = attempts.clone();

        let mut c = IosNativeClient::connect(dead_port());
        c.set_restart(Box::new(move || {
            let mut n = attempts_c.lock().unwrap();
            *n += 1;
            if *n == 1 { Ok(IosNativeClient::connect(second_port)) } else { Err(anyhow!("simulator shut down")) }
        }));

        // Restart #1 succeeds; do NOT drain notices yet (simulates a multi-RPC op).
        assert_eq!(c.device_info().unwrap().width_points, 402);
        assert_eq!(c.restarts(), 1);

        // Kill runner #2; next call attempts restart #2 which fails.
        drop(second.lock().unwrap().take());
        let err = format!("{:#}", c.view_hierarchy(&apps(), false).unwrap_err());
        assert!(err.contains("could not be restarted") && err.contains("simulator shut down"), "{err}");
        assert!(err.contains("`viewHierarchy` request failed"), "{err}");
        assert_eq!(c.restarts(), 1);

        // Notice from restart #1 must still be intact and drained once.
        let notices = c.take_restart_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("restart 1/3"), "{notices:?}");
        assert!(c.take_restart_notices().is_empty());

        // Subsequent calls fail fast without attempting restart #3.
        assert!(c.view_hierarchy(&apps(), false).is_err());
        assert_eq!(*attempts.lock().unwrap(), 2);
    }
}
