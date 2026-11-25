//! Synchronous facade over the Android driver gRPC service
//! (`maestro_android.MaestroDriver`, served by Tapwing's
//! `dev.tapwing.driver`).
//!
//! The rest of Tapwing is synchronous; this client owns a small tokio runtime
//! and blocks on each RPC.
//!
//! **Reconnect.** When the transport to the device dies mid-run (the adb
//! server restarts, the USB link drops, the on-device process is killed) the
//! `adb forward` and the instrumentation are gone and every later RPC fails
//! with a transport error. A client given a [`RestartFn`] ([`NativeDriverClient::set_restart`])
//! notices that, restarts the whole session through the supplied
//! [`RestartFn`] (forward, `am instrument`, readiness probe) and repeats the
//! RPC, at most [`MAX_RESTARTS`] times per run. Only transport errors count:
//! a driver that answers with an error status, or an RPC that times out, is
//! never restarted. If the restart itself fails the original error is kept
//! and the lane stays down.
//!
//! **Replay.** Reads and idempotent writes are repeated after any transport
//! failure. Input (`tap`, `inputText`, `eraseAllText`) is repeated only when
//! the failed request provably never left this host ([`Fault::NotSent`]):
//!
//! * the TCP connect was refused (nothing listens on the forwarded port), or
//! * the peer accepted the connection and closed or reset it before it sent a
//!   byte, which [`super::connect`] detects before the client has written
//!   one. This is what a dead driver looks like behind `adb forward`.
//!
//! Every other transport failure is ambiguous ([`Fault::Broken`]) and input
//! is not repeated after it: a connection that had been up, whether it broke
//! before, while or after the request was written; a connect that timed out;
//! a peer that stayed silent past the grace period and was then written to.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::runtime::Runtime;
use tonic::Status;
use tonic::transport::{Channel, Endpoint};

use super::connect::{ClosedBeforeServerSpoke, SERVER_FIRST_GRACE, ServerFirstConnector};

/// Generated protobuf messages and the tonic client (plus server stubs in tests).
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod pb {
    tonic::include_proto!("maestro_android");

    #[cfg(test)]
    mod server_pb {
        include!(concat!(env!("OUT_DIR"), "/server/maestro_android.rs"));
    }

    #[cfg(test)]
    pub use server_pb::maestro_driver_server;
}

use pb::maestro_driver_client::MaestroDriverClient;

/// Per-RPC deadline. A hierarchy dump on a busy UI can take seconds.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Most ASCII characters sent in one `inputText` RPC. The driver types ASCII
/// one key at a time, 75 ms apart (Maestro's pace), so a text of 400
/// characters alone would outlast [`RPC_TIMEOUT`]; 100 take 7.5 s and leave
/// room for a slow device.
const ASCII_INPUT_CHUNK: usize = 100;

/// Splits `text` into the pieces sent as one `inputText` RPC each.
///
/// Only pure ASCII is split: the driver types it key by key whatever the
/// request boundaries are, so the result on the device is the same. Other
/// text goes through one accessibility action (or the IME), whose time does
/// not grow with the length like that and whose result at the cursor is not
/// worth re-deriving per piece.
fn input_text_chunks(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = text;
    if text.is_ascii() {
        while rest.len() > ASCII_INPUT_CHUNK
            && let Some((head, tail)) = rest.split_at_checked(ASCII_INPUT_CHUNK)
        {
            chunks.push(head);
            rest = tail;
        }
    }
    chunks.push(rest);
    chunks
}

/// Longest teardown waits for the driver to remove its mock-location providers.
const DISABLE_LOCATION_TIMEOUT: Duration = Duration::from_secs(3);

/// How many times one client restarts its session after a transport failure.
/// A driver that keeps dying is a real problem, not a blip to hide.
pub const MAX_RESTARTS: u32 = 3;

/// Starts a replacement session (the old one is already torn down) and
/// returns its client, whose own recovery is left unset.
pub type RestartFn = Box<dyn FnMut() -> Result<NativeDriverClient> + Send>;

/// Screen size in physical pixels (`deviceInfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

/// Whether an RPC may be sent again after the transport failed under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Replay {
    /// Reads and idempotent writes: always safe to repeat.
    Always,
    /// Input (`tap`, `inputText`, `eraseAllText`): repeated only when the
    /// request provably never left the host ([`Fault::NotSent`]), so a lost
    /// reply can never turn into a double tap or doubled text.
    IfNotSent,
}

/// How a failed RPC relates to the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// The driver answered, or the RPC timed out: not a transport failure.
    None,
    /// No byte of the request left this host: the connect was refused, or the
    /// peer closed the new connection before either side had sent anything.
    NotSent,
    /// The connection broke; the request may or may not have been processed.
    Broken,
}

fn classify(status: &Status) -> Fault {
    use std::error::Error;
    use std::io::ErrorKind;
    let mut not_sent = false;
    let mut broken = false;
    let mut src: Option<&(dyn Error + 'static)> = status.source();
    while let Some(e) = src {
        // `Endpoint::timeout` / `Request::set_timeout` wrap `TimeoutExpired` inside
        // `tonic::transport::Error`; an RPC timeout is never a transport drop.
        if e.is::<tonic::TimeoutExpired>() {
            return Fault::None;
        }
        // The connector's own verdict. The `io::Error` under it (a reset) says
        // how the peer went away and must not count as a broken connection.
        if e.is::<ClosedBeforeServerSpoke>() {
            return Fault::NotSent;
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            match io.kind() {
                ErrorKind::ConnectionRefused => not_sent = true,
                ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe
                | ErrorKind::UnexpectedEof
                | ErrorKind::NotConnected => broken = true,
                _ => {}
            }
        }
        broken |= e.is::<tonic::transport::Error>();
        src = e.source();
    }
    if not_sent {
        return Fault::NotSent;
    }
    // Hand-built statuses (and tonic versions that drop the source) still carry the text.
    if status.code() == tonic::Code::Unavailable && status.message().contains("tcp connect error") {
        return Fault::NotSent;
    }
    // Mid-response body drops come through `Body::poll_frame` -> `Status::map_error` without
    // a `tonic::transport::Error` wrapper (`Code::Unknown`, `"h2 protocol error: ..."`).
    if broken
        || (status.code() == tonic::Code::Unknown
            && (status.message() == "transport error" || status.message().starts_with("h2 protocol error:")))
    {
        return Fault::Broken;
    }
    Fault::None
}

fn describe(what: &str, s: &Status) -> anyhow::Error {
    anyhow!("native driver `{what}` failed: {} ({:?})", s.message(), s.code())
}

pub struct NativeDriverClient {
    /// Tears the session down (stops the instrumentation, removes the port
    /// forward) when dropped. Declared first so it goes before the runtime.
    keepalive: Option<Box<dyn Send>>,
    rt: Runtime,
    inner: MaestroDriverClient<Channel>,
    screen: Option<ScreenSize>,
    mock_location_enabled: bool,
    recovery: Option<RestartFn>,
    restarts: u32,
    restart_notices: Vec<String>,
}

impl NativeDriverClient {
    /// Connects to the driver on `127.0.0.1:<host_port>` (the `adb forward`
    /// end). The connection is established lazily by the first RPC, so this
    /// succeeds while the on-device server is still starting.
    pub fn connect(host_port: u16) -> Result<Self> {
        Self::connect_with_grace(host_port, SERVER_FIRST_GRACE)
    }

    /// [`Self::connect`] with the time a new connection waits for the driver
    /// to speak first (see [`super::connect`]).
    pub(super) fn connect_with_grace(host_port: u16, server_first_grace: Duration) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name("tapwing-native-grpc")
            .build()
            .context("starting the native-lane tokio runtime")?;
        let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{host_port}"))
            .context("invalid native driver endpoint")?
            .connect_timeout(Duration::from_secs(5))
            .timeout(RPC_TIMEOUT);
        let channel = {
            let _guard = rt.enter();
            endpoint.connect_with_connector_lazy(ServerFirstConnector::new(server_first_grace))
        };
        let inner = MaestroDriverClient::new(channel)
            .max_decoding_message_size(64 * 1024 * 1024)
            .max_encoding_message_size(64 * 1024 * 1024);
        Ok(Self {
            keepalive: None,
            rt,
            inner,
            screen: None,
            mock_location_enabled: false,
            recovery: None,
            restarts: 0,
            restart_notices: Vec::new(),
        })
    }

    /// Hands the client ownership of its session's teardown guards: they run
    /// (stopping the instrumentation, removing the port forward) when the
    /// client is dropped or replaced by a restart.
    pub fn set_keepalive(&mut self, keepalive: Box<dyn Send>) {
        self.keepalive = Some(keepalive);
    }

    /// Lets the client start a replacement session after a transport failure.
    pub fn set_restart(&mut self, restart: RestartFn) {
        self.recovery = Some(restart);
    }

    /// How many times this client restarted its session.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// One user-facing line per restart since the last call.
    pub fn take_restart_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.restart_notices)
    }

    fn call_once<Req, T, F, Fut>(&mut self, req: Req, f: &F) -> Result<T, Status>
    where
        F: Fn(MaestroDriverClient<Channel>, Req) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
    {
        let client = self.inner.clone();
        self.rt.block_on(f(client, req)).map(tonic::Response::into_inner)
    }

    /// Runs one RPC. On a transport failure restarts the session (when this
    /// client can) and, if `replay` allows, repeats the RPC once.
    fn call<Req, T, F, Fut>(&mut self, what: &str, replay: Replay, req: Req, f: F) -> Result<T>
    where
        Req: Clone,
        F: Fn(MaestroDriverClient<Channel>, Req) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
    {
        self.call_replaying(what, replay, req, f, |_| Ok(()))
    }

    fn call_replaying<Req, T, F, Fut, R>(
        &mut self,
        what: &str,
        replay: Replay,
        req: Req,
        f: F,
        before_replay: R,
    ) -> Result<T>
    where
        Req: Clone,
        F: Fn(MaestroDriverClient<Channel>, Req) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
        R: FnOnce(&mut Self) -> Result<()>,
    {
        let status = match self.call_once(req.clone(), &f) {
            Ok(v) => return Ok(v),
            Err(s) => s,
        };
        crate::interrupt::check()?;
        let fault = classify(&status);
        if fault == Fault::None || self.recovery.is_none() {
            return Err(describe(what, &status));
        }
        if self.restarts >= MAX_RESTARTS {
            return Err(describe(what, &status).context(format!(
                "the native driver lost its connection again after {MAX_RESTARTS} restarts; giving up"
            )));
        }
        let cause = format!("`{what}`: {}", status.message());
        self.restart(&cause).map_err(|e| {
            describe(what, &status)
                .context(format!("the native driver lost its connection and could not be restarted: {e:#}"))
        })?;
        if replay == Replay::IfNotSent && fault != Fault::NotSent {
            return Err(describe(what, &status).context(
                "the native driver lost its connection while sending this request and was restarted; the \
                 request was not repeated because it may already have been applied",
            ));
        }
        before_replay(self)?;
        self.call_once(req, &f).map_err(|s| {
            describe(what, &s)
                .context("the native driver was restarted after losing its connection, but the request failed again")
        })
    }

    /// Replaces the dead session with a freshly started one.
    fn restart(&mut self, cause: &str) -> Result<()> {
        let mut recovery = self.recovery.take().ok_or_else(|| anyhow!("this client cannot restart"))?;
        // Tear the dead session down first: its `force-stop` would otherwise kill the new instrumentation.
        drop(self.keepalive.take());
        // On failure `recovery` stays dropped: the lane is down and every later RPC
        // fails fast instead of retrying a restart that already failed.
        let mut fresh = recovery()?;
        fresh.recovery = Some(recovery);
        fresh.restarts = self.restarts + 1;
        fresh.restart_notices = std::mem::take(&mut self.restart_notices);
        fresh.restart_notices.push(format!(
            "native driver connection lost ({cause}); restarted it (restart {}/{MAX_RESTARTS})",
            fresh.restarts
        ));
        // The dead session cannot answer a `disableLocationUpdates`: don't wait for it on drop.
        self.mock_location_enabled = false;
        // The new session starts with its screen size uncached and without mock-location providers.
        *self = fresh;
        Ok(())
    }

    pub fn device_info(&mut self) -> Result<ScreenSize> {
        let r = self.call("deviceInfo", Replay::Always, (), |mut c, ()| async move {
            c.device_info(pb::DeviceInfoRequest {}).await
        })?;
        Ok(ScreenSize { width: r.width_pixels, height: r.height_pixels })
    }

    /// Bounded readiness probe used during startup so cancellation and startup
    /// deadlines are respected without waiting for the 30 s default RPC timeout
    /// and without populating the cached screen size prematurely.
    pub fn device_info_with_timeout(&mut self, timeout: Duration) -> Result<ScreenSize> {
        let client = self.inner.clone();
        let fut = async move {
            let mut req = tonic::Request::new(pb::DeviceInfoRequest {});
            req.set_timeout(timeout);
            let mut c = client;
            tokio::time::timeout(timeout, c.device_info(req)).await
        };
        let r = match self.rt.block_on(fut) {
            Ok(Ok(resp)) => resp.into_inner(),
            Ok(Err(s)) => return Err(anyhow!("native driver `deviceInfo` failed: {} ({:?})", s.message(), s.code())),
            Err(_) => return Err(anyhow!("native driver `deviceInfo` timed out after {} ms", timeout.as_millis())),
        };
        Ok(ScreenSize { width: r.width_pixels, height: r.height_pixels })
    }

    pub fn mock_location_enabled(&self) -> bool {
        self.mock_location_enabled
    }

    pub fn enable_mock_location_providers(&mut self) -> Result<()> {
        self.call("enableMockLocationProviders", Replay::Always, (), |mut c, ()| async move {
            c.enable_mock_location_providers(pb::EmptyRequest {}).await
        })?;
        self.mock_location_enabled = true;
        Ok(())
    }

    /// Removes the mock-location providers this session added. Left registered they
    /// outlive the run and the phone keeps reporting the last mocked position.
    ///
    /// One bounded attempt, never a restart: it runs while the session is torn down
    /// (see the `Drop` impl), when a dead driver is simply not worth waiting for.
    pub fn disable_location_updates(&mut self) -> Result<()> {
        let client = self.inner.clone();
        let fut = async move {
            let mut req = tonic::Request::new(pb::EmptyRequest {});
            req.set_timeout(DISABLE_LOCATION_TIMEOUT);
            let mut c = client;
            tokio::time::timeout(DISABLE_LOCATION_TIMEOUT, c.disable_location_updates(req)).await
        };
        match self.rt.block_on(fut) {
            Ok(Ok(_)) => {
                self.mock_location_enabled = false;
                Ok(())
            }
            Ok(Err(s)) => Err(describe("disableLocationUpdates", &s)),
            Err(_) => Err(anyhow!(
                "native driver `disableLocationUpdates` timed out after {} ms",
                DISABLE_LOCATION_TIMEOUT.as_millis()
            )),
        }
    }

    /// Screen size, fetched once and cached (call [`Self::invalidate_screen`]
    /// after a rotation).
    pub fn screen_size(&mut self) -> Result<ScreenSize> {
        if let Some(s) = self.screen {
            return Ok(s);
        }
        let s = self.device_info()?;
        self.screen = Some(s);
        Ok(s)
    }

    pub fn invalidate_screen(&mut self) {
        self.screen = None;
    }

    /// UiAutomator XML of the active window(s).
    pub fn view_hierarchy(&mut self) -> Result<String> {
        let r = self.call("viewHierarchy", Replay::Always, (), |mut c, ()| async move {
            c.view_hierarchy(pb::ViewHierarchyRequest {}).await
        })?;
        Ok(r.hierarchy)
    }

    pub fn tap(&mut self, x: u32, y: u32) -> Result<()> {
        self.call(
            "tap",
            Replay::IfNotSent,
            (x, y),
            |mut c, (x, y)| async move { c.tap(pb::TapRequest { x, y }).await },
        )?;
        Ok(())
    }

    /// Types `text`. A long ASCII text goes out in several RPCs (see
    /// [`input_text_chunks`]) so that none of them outlasts the RPC deadline;
    /// each is replayed or not on its own, so nothing is ever typed twice.
    pub fn input_text(&mut self, text: &str) -> Result<()> {
        let mut typed = 0;
        for chunk in input_text_chunks(text) {
            if typed > 0 {
                crate::interrupt::check()?;
            }
            self.call("inputText", Replay::IfNotSent, chunk.to_string(), |mut c, text| async move {
                c.input_text(pb::InputTextRequest { text }).await
            })
            .map_err(|e| match typed {
                0 => e,
                _ => e.context(format!("inputText stopped after {typed} of {} characters", text.len())),
            })?;
            typed += chunk.len();
        }
        Ok(())
    }

    pub fn erase_all_text(&mut self, characters_to_erase: u32) -> Result<()> {
        self.call("eraseAllText", Replay::IfNotSent, characters_to_erase, |mut c, characters_to_erase| async move {
            c.erase_all_text(pb::EraseAllTextRequest { characters_to_erase }).await
        })?;
        Ok(())
    }

    pub fn set_location(&mut self, latitude: f64, longitude: f64) -> Result<()> {
        let reenable_mock = self.mock_location_enabled;
        self.call_replaying(
            "setLocation",
            Replay::Always,
            (latitude, longitude),
            |mut c, (latitude, longitude)| async move {
                c.set_location(pb::SetLocationRequest { latitude, longitude }).await
            },
            |this| {
                if reenable_mock {
                    this.call_once((), &|mut c, ()| async move {
                        c.enable_mock_location_providers(pb::EmptyRequest {}).await
                    })
                    .map_err(|s| {
                        describe("enableMockLocationProviders", &s).context(
                            "the native driver was restarted after losing its connection, but re-enabling mock \
                             location providers failed",
                        )
                    })?;
                    this.mock_location_enabled = true;
                }
                Ok(())
            },
        )?;
        Ok(())
    }

    /// PNG bytes of the current screen.
    pub fn screenshot(&mut self) -> Result<Vec<u8>> {
        let r = self.call("screenshot", Replay::Always, (), |mut c, ()| async move {
            c.screenshot(pb::ScreenshotRequest {}).await
        })?;
        Ok(r.bytes)
    }

    pub fn is_window_updating(&mut self, app_id: &str) -> Result<bool> {
        let r = self.call("isWindowUpdating", Replay::Always, app_id.to_string(), |mut c, app_id| async move {
            c.is_window_updating(pb::CheckWindowUpdatingRequest { app_id }).await
        })?;
        Ok(r.is_window_updating)
    }
}

impl Drop for NativeDriverClient {
    /// The mock-location providers a session added are registered with the OS, not with the
    /// driver process, so stopping the instrumentation does not remove them. Ask the driver
    /// to, while it is still running (the guards that stop it are dropped after this).
    fn drop(&mut self) {
        if self.mock_location_enabled {
            let _ = self.disable_location_updates();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pb::*;
    use prost::Message;

    fn round_trip<M: Message + Default + PartialEq + std::fmt::Debug>(m: &M) -> M {
        M::decode(m.encode_to_vec().as_slice()).expect("decode")
    }

    #[test]
    fn proto_messages_round_trip() {
        let tap = TapRequest { x: 540, y: 1600 };
        assert_eq!(round_trip(&tap), tap);
        let info = DeviceInfo { width_pixels: 1080, height_pixels: 2400 };
        assert_eq!(round_trip(&info), info);
        let hier = ViewHierarchyResponse { hierarchy: "<hierarchy rotation=\"0\"/>".into() };
        assert_eq!(round_trip(&hier), hier);
        let loc = SetLocationRequest { latitude: 50.0755, longitude: 14.4378 };
        assert_eq!(round_trip(&loc), loc);
        let erase = EraseAllTextRequest { characters_to_erase: 50 };
        assert_eq!(round_trip(&erase), erase);
        let launch = LaunchAppRequest {
            package_name: "com.example".into(),
            arguments: vec![ArgumentValue { key: "k".into(), value: "v".into(), r#type: "String".into() }],
        };
        assert_eq!(round_trip(&launch), launch);
        let media = AddMediaRequest {
            payload: Some(Payload { data: vec![1, 2, 3] }),
            media_name: "a".into(),
            media_ext: "png".into(),
        };
        assert_eq!(round_trip(&media), media);
    }

    #[test]
    fn long_ascii_text_is_split_so_no_rpc_outlasts_the_deadline() {
        use super::{ASCII_INPUT_CHUNK, RPC_TIMEOUT, input_text_chunks};

        assert_eq!(input_text_chunks(""), vec![""], "an empty text is still one RPC");
        assert_eq!(input_text_chunks("hello 42"), vec!["hello 42"]);
        let exact = "a".repeat(ASCII_INPUT_CHUNK);
        assert_eq!(input_text_chunks(&exact), vec![exact.as_str()], "no empty trailing piece");

        let long: String = ('a'..='z').cycle().take(2 * ASCII_INPUT_CHUNK + 50).collect();
        let chunks = input_text_chunks(&long);
        assert_eq!(chunks.iter().map(|c| c.len()).collect::<Vec<_>>(), vec![100, 100, 50]);
        assert_eq!(chunks.concat(), long, "nothing lost, nothing reordered");

        // The driver types one ASCII key every 75 ms: a full piece stays well inside the deadline.
        let nominal = std::time::Duration::from_millis(75) * ASCII_INPUT_CHUNK as u32;
        assert!(nominal * 3 <= RPC_TIMEOUT, "{nominal:?} per piece against {RPC_TIMEOUT:?}");

        // Anything else is one action on the device and stays one request.
        let unicode = "ž".repeat(3 * ASCII_INPUT_CHUNK);
        assert_eq!(input_text_chunks(&unicode), vec![unicode.as_str()]);
        let mixed = format!("{}🚀", "a".repeat(3 * ASCII_INPUT_CHUNK));
        assert_eq!(input_text_chunks(&mixed), vec![mixed.as_str()]);
    }

    #[test]
    fn proto_wire_format_matches_maestro() {
        // `TapRequest { x = 1, y = 2 }`: field 1 varint, field 2 varint.
        assert_eq!(TapRequest { x: 1, y: 2 }.encode_to_vec(), vec![0x08, 0x01, 0x10, 0x02]);
        // `CheckWindowUpdatingResponse { isWindowUpdating = true }`: field 1 varint.
        assert_eq!(CheckWindowUpdatingResponse { is_window_updating: true }.encode_to_vec(), vec![0x08, 0x01]);
        // `ViewHierarchyResponse { hierarchy = "x" }`: field 1 length-delimited.
        assert_eq!(ViewHierarchyResponse { hierarchy: "x".into() }.encode_to_vec(), vec![0x0a, 0x01, b'x']);
    }

    #[test]
    fn classifies_transport_failures_from_real_tonic_errors() {
        use super::*;
        use crate::native_lane::connect::testing::{Peer, serve};

        /// The status of one `tap` against `port`, with a short wait for a silent peer.
        fn tap_status(port: u16) -> Status {
            let c = NativeDriverClient::connect_with_grace(port, Duration::from_millis(50)).unwrap();
            let client = c.inner.clone();
            c.rt.block_on(async move {
                let mut c = client;
                c.tap(pb::TapRequest { x: 1, y: 1 }).await
            })
            .unwrap_err()
        }

        // Nothing listens: refused before anything is sent.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(classify(&tap_status(closed)), Fault::NotSent);

        // `adb forward` to a dead device end: accepted, then closed (or reset) before the
        // server said anything. The client had not written a byte.
        for peer in [Peer::Closes, Peer::Resets] {
            let (port, seen) = serve(peer);
            let status = tap_status(port);
            assert_eq!(classify(&status), Fault::NotSent, "{peer:?}: {status:?}");
            assert!(!seen.recv().unwrap(), "{peer:?} read a request");
        }

        // The server took the whole request and reset the connection instead of answering:
        // it may have applied it. The same when it never sent its own preface, because the
        // client stopped waiting and wrote the request.
        for peer in [Peer::ReadsRequestThenResets, Peer::SilentReadsRequestThenResets] {
            let (port, seen) = serve(peer);
            let status = tap_status(port);
            assert_eq!(classify(&status), Fault::Broken, "{peer:?}: {status:?}");
            assert!(seen.recv().unwrap(), "{peer:?} never got the request");
        }

        // The driver answered (or timed out): never a transport fault.
        for status in [
            Status::internal("capture failed"),
            Status::not_found("no such node"),
            Status::deadline_exceeded("Timeout expired"),
            Status::unknown("something else"),
        ] {
            assert_eq!(classify(&status), Fault::None, "{status:?}");
        }
        // A hand-built connect failure still counts, so a tonic upgrade that drops the source is caught.
        assert_eq!(classify(&Status::unavailable("tcp connect error")), Fault::NotSent);
        assert_eq!(classify(&Status::unknown("transport error")), Fault::Broken);
    }

    #[test]
    fn hanging_rpc_timeout_must_not_trigger_restart() {
        use super::*;
        use pb::maestro_driver_server::{MaestroDriver, MaestroDriverServer};
        use std::sync::{Arc, Mutex};
        use tonic::{Request, Response, Streaming};

        struct HangingDriver;

        #[tonic::async_trait]
        impl MaestroDriver for HangingDriver {
            async fn device_info(&self, _: Request<pb::DeviceInfoRequest>) -> Result<Response<pb::DeviceInfo>, Status> {
                Ok(Response::new(pb::DeviceInfo { width_pixels: 1080, height_pixels: 2400 }))
            }
            async fn view_hierarchy(
                &self,
                _: Request<pb::ViewHierarchyRequest>,
            ) -> Result<Response<pb::ViewHierarchyResponse>, Status> {
                std::future::pending::<()>().await;
                unreachable!()
            }
            async fn screenshot(
                &self,
                _: Request<pb::ScreenshotRequest>,
            ) -> Result<Response<pb::ScreenshotResponse>, Status> {
                unreachable!()
            }
            async fn tap(&self, _: Request<pb::TapRequest>) -> Result<Response<pb::TapResponse>, Status> {
                unreachable!()
            }
            async fn input_text(
                &self,
                _: Request<pb::InputTextRequest>,
            ) -> Result<Response<pb::InputTextResponse>, Status> {
                unreachable!()
            }
            async fn erase_all_text(
                &self,
                _: Request<pb::EraseAllTextRequest>,
            ) -> Result<Response<pb::EraseAllTextResponse>, Status> {
                unreachable!()
            }
            async fn set_location(
                &self,
                _: Request<pb::SetLocationRequest>,
            ) -> Result<Response<pb::SetLocationResponse>, Status> {
                unreachable!()
            }
            async fn is_window_updating(
                &self,
                _: Request<pb::CheckWindowUpdatingRequest>,
            ) -> Result<Response<pb::CheckWindowUpdatingResponse>, Status> {
                unreachable!()
            }
            async fn launch_app(
                &self,
                _: Request<pb::LaunchAppRequest>,
            ) -> Result<Response<pb::LaunchAppResponse>, Status> {
                unreachable!()
            }
            async fn add_media(
                &self,
                _: Request<Streaming<pb::AddMediaRequest>>,
            ) -> Result<Response<pb::AddMediaResponse>, Status> {
                unreachable!()
            }
            async fn enable_mock_location_providers(
                &self,
                _: Request<pb::EmptyRequest>,
            ) -> Result<Response<pb::EmptyResponse>, Status> {
                unreachable!()
            }
            async fn disable_location_updates(
                &self,
                _: Request<pb::EmptyRequest>,
            ) -> Result<Response<pb::EmptyResponse>, Status> {
                unreachable!()
            }
        }

        let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                let incoming = tonic::transport::server::TcpIncoming::from(listener);
                let _ = port_tx.send(port);
                let server = tokio::spawn(async move {
                    let _ = tonic::transport::Server::builder()
                        .add_service(MaestroDriverServer::new(HangingDriver))
                        .serve_with_incoming(incoming)
                        .await;
                });
                let _ = stop_rx.await;
                server.abort();
                let _ = server.await;
            });
        });
        let port = port_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // Build a client identical to `NativeDriverClient::connect`, with a short
        // per-RPC `Endpoint::timeout` so we exercise the real timeout status without waiting 30 s.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
            .unwrap()
            .tcp_nodelay(true)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_millis(80));
        let channel = {
            let _guard = rt.enter();
            endpoint.connect_lazy()
        };
        let mut client = NativeDriverClient {
            keepalive: None,
            rt,
            inner: MaestroDriverClient::new(channel),
            screen: None,
            mock_location_enabled: false,
            recovery: None,
            restarts: 0,
            restart_notices: Vec::new(),
        };

        // Ensure the server is accepting and the HTTP/2 connection is established.
        for _ in 0..100 {
            if client.device_info_with_timeout(Duration::from_millis(200)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // 1. Direct check on `classify()` for both `Endpoint::timeout` and `Request::set_timeout`.
        let endpoint_timeout_status = {
            let mut c = client.inner.clone();
            client.rt.block_on(async move { c.view_hierarchy(pb::ViewHierarchyRequest {}).await }).unwrap_err()
        };
        assert_eq!(
            classify(&endpoint_timeout_status),
            Fault::None,
            "Endpoint::timeout produced {endpoint_timeout_status:?}"
        );

        let request_timeout_status = {
            let mut c = client.inner.clone();
            client
                .rt
                .block_on(async move {
                    let mut req = tonic::Request::new(pb::ViewHierarchyRequest {});
                    req.set_timeout(Duration::from_millis(40));
                    c.view_hierarchy(req).await
                })
                .unwrap_err()
        };
        assert_eq!(
            classify(&request_timeout_status),
            Fault::None,
            "Request::set_timeout produced {request_timeout_status:?}"
        );

        // 2. End-to-end: a hanging RPC through `call()` must NOT trigger a restart.
        let restart_calls = Arc::new(Mutex::new(0u32));
        let seen = restart_calls.clone();
        client.set_restart(Box::new(move || {
            *seen.lock().unwrap() += 1;
            Err(anyhow!("must not restart on RPC timeout"))
        }));

        let err = client.view_hierarchy().unwrap_err();
        assert_eq!(*restart_calls.lock().unwrap(), 0, "RPC timeout must not restart: {err:#}");
        assert_eq!(client.restarts(), 0);

        let _ = stop_tx.send(());
        let _ = server.join();
    }

    #[test]
    fn a_dead_driver_does_not_stall_the_mock_location_cleanup() {
        use super::*;
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut client = NativeDriverClient::connect(closed).unwrap();
        client.mock_location_enabled = true;
        let t = std::time::Instant::now();
        drop(client);
        assert!(t.elapsed() < Duration::from_secs(2), "teardown waited {:?} for a dead driver", t.elapsed());
    }
}
