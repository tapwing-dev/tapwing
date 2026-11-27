//! UI commands over the native lane: Tapwing's Android driver (UiAutomator)
//! behind gRPC, with Maestro's own waiting model (poll the hierarchy until the
//! selector matches or the timeout elapses, settle by comparing consecutive
//! hierarchies).

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use super::apk::Adb;
use super::client::NativeDriverClient;
use super::hierarchy::Hierarchy;
use crate::backend::{ScrollUntilVisibleRequest, SwipeRequest, TapElementRequest, TapPointRequest};
use crate::flow::ir::{Direction, ElementSelector};
pub use crate::native_core::ops::ColdStart;
use crate::native_core::ops::{self as core_ops, NativeHierarchy, NativeSurface};
use crate::ui_tree::Bounds;

/// `AndroidDriver.longPress`: a 3 s in-place swipe.
const LONG_PRESS_MS: u64 = 3000;
/// `ScreenshotUtils.waitForAppToSettle`: up to 10 comparisons 200 ms apart.
const SETTLE_ATTEMPTS: u32 = 10;
const SETTLE_INTERVAL: Duration = Duration::from_millis(200);
/// Pause between hierarchy polls during `settle`.
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// `AndroidDriver.backPress` waits 300 ms for the navigation to start.
const BACK_PAUSE: Duration = Duration::from_millis(300);

/// Default timeout for `waitForAnimationToEnd` on the native lane (5 000 ms).
pub const DEFAULT_ANIMATION_TIMEOUT_MS: u64 = 5000;
/// Interval between consecutive screenshot captures in `wait_for_animation_to_end`.
const ANIMATION_POLL_INTERVAL: Duration = Duration::from_millis(150);
/// Pixel similarity threshold (99.5%, matching Maestro's `SCREENSHOT_DIFF_THRESHOLD = 0.005`).
const STATIC_SCREEN_MATCH_PERCENT: f64 = 99.5;

/// Native-lane command executor, borrowed per call from the warmed-up session.
pub struct NativeOps<'a> {
    client: &'a mut NativeDriverClient,
    adb: &'a Adb,
    /// Armed for the next element lookup only.
    cold_start: Option<ColdStart>,
    /// Extra windows granted by `poll_find` (0 or 1 per arming).
    cold_start_retries: u32,
}

impl NativeSurface for NativeOps<'_> {
    type Hierarchy = Hierarchy;

    const SWIPE_UP_START_Y: f64 = 0.5;

    fn screen(&mut self) -> Result<(i32, i32)> {
        let s = self.client.screen_size()?;
        Ok((s.width as i32, s.height as i32))
    }

    fn hierarchy_for_poll(&mut self) -> Result<Hierarchy> {
        self.hierarchy_fast()
    }

    fn hierarchy_for_negative(&mut self) -> Result<Hierarchy> {
        self.hierarchy()
    }

    fn settle(&mut self, initial: Option<Hierarchy>, timeout_ms: Option<i32>) -> Result<Hierarchy> {
        NativeOps::settle(self, initial, timeout_ms)
    }

    fn tap_xy(&mut self, x: i32, y: i32, long_press: bool) -> Result<()> {
        let (x, y) = (x.max(0), y.max(0));
        if long_press {
            self.adb.shell(&format!(
                "cmd input swipe {x} {y} {x} {y} {LONG_PRESS_MS} 2>/dev/null || input swipe {x} {y} {x} {y} {LONG_PRESS_MS}"
            ))?;
        } else {
            self.client.tap(x as u32, y as u32)?;
        }
        Ok(())
    }

    fn raw_swipe(&mut self, start: (i32, i32), end: (i32, i32), duration_ms: i64) -> Result<()> {
        let (sx, sy, ex, ey) = (start.0.max(0), start.1.max(0), end.0.max(0), end.1.max(0));
        let dur = duration_ms.max(1);
        self.adb.shell(&format!(
            "cmd input swipe {sx} {sy} {ex} {ey} {dur} 2>/dev/null || input swipe {sx} {sy} {ex} {ey} {dur}"
        ))?;
        Ok(())
    }

    fn take_cold_start(&mut self) -> Option<ColdStart> {
        self.cold_start.take()
    }

    fn note_cold_start_retry(&mut self) {
        self.cold_start_retries += 1;
    }
}

impl<'a> NativeOps<'a> {
    pub fn new(client: &'a mut NativeDriverClient, adb: &'a Adb) -> Self {
        Self { client, adb, cold_start: None, cold_start_retries: 0 }
    }

    /// Bounded cold-start retry for the next element lookup. On a loaded
    /// emulator a freshly cleared Flutter app can take longer than the whole
    /// lookup budget to put anything into the UiAutomator tree. If the
    /// lookup times out and **no** hierarchy it read showed content from
    /// `app_id` (the app was never on screen, so the miss says nothing about
    /// the flow), the lookup gets one more window of `extra`. A lookup that
    /// saw the app at any time fails exactly as before. Lookups only read
    /// the hierarchy, so nothing is repeated on the device; the hierarchies
    /// the lookup fetches anyway are inspected, so it costs no extra RPC.
    pub fn arm_cold_start(&mut self, cold: ColdStart) {
        self.cold_start = Some(cold);
    }

    /// Extra lookup windows granted since this executor was created.
    pub fn cold_start_retries(&self) -> u32 {
        self.cold_start_retries
    }

    /// Test seam: the arming the next lookup would use.
    #[cfg(test)]
    pub(crate) fn armed_cold_start(&self) -> Option<&ColdStart> {
        self.cold_start.as_ref()
    }

    /// Test seam: counts a granted window as `poll_find` does.
    #[cfg(test)]
    pub(crate) fn note_cold_start_retry(&mut self) {
        self.cold_start_retries += 1;
    }

    fn hierarchy_with_idle(&mut self, skip_wait_for_idle: bool) -> Result<Hierarchy> {
        let screen = self.client.screen_size()?;
        let xml = self.client.view_hierarchy_with_idle(skip_wait_for_idle)?;
        Hierarchy::parse(&xml, screen.width as i32, screen.height as i32)
    }

    pub fn hierarchy(&mut self) -> Result<Hierarchy> {
        self.hierarchy_with_idle(false)
    }

    /// Hierarchy snapshot that skips `uiDevice.waitForIdle(500)` on the device
    /// while still flushing `uiAutomation.serviceInfo = null`. Safe only for
    /// positive lookup polling and failure diagnostics (not `settle` or
    /// negative assertions).
    pub fn hierarchy_fast(&mut self) -> Result<Hierarchy> {
        self.hierarchy_with_idle(true)
    }

    #[cfg(test)]
    fn poll_find(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<(crate::ui_tree::TreeNode, Hierarchy)> {
        core_ops::poll_find(self, sel, timeout_ms)
    }

    #[cfg(test)]
    fn wait_until_visible(&mut self, sel: &ElementSelector, fallback: &Hierarchy) -> Result<Hierarchy> {
        core_ops::wait_until_visible(self, sel, fallback)
    }

    /// `ScreenshotUtils.waitForAppToSettle` (hierarchy based): returns once two
    /// consecutive hierarchies are identical, or the budget runs out.
    pub fn settle(&mut self, initial: Option<Hierarchy>, timeout_ms: Option<i32>) -> Result<Hierarchy> {
        if matches!(timeout_ms, Some(ms) if ms <= 0) {
            return match initial {
                Some(h) => Ok(h),
                None => self.hierarchy(),
            };
        }
        let mut latest = match initial {
            Some(h) => h,
            None => self.hierarchy()?,
        };
        match timeout_ms {
            Some(ms) => {
                let end = Instant::now() + Duration::from_millis(ms as u64);
                loop {
                    let remaining = end.saturating_duration_since(Instant::now());
                    if !remaining.is_zero() {
                        crate::interrupt::sleep(SETTLE_POLL_INTERVAL.min(remaining))?;
                    }
                    let after = self.hierarchy()?;
                    if after.signature() == latest.signature() {
                        return Ok(after);
                    }
                    latest = after;
                    if Instant::now() >= end {
                        return Ok(latest);
                    }
                }
            }
            None => {
                for attempt in 0..SETTLE_ATTEMPTS {
                    let after = self.hierarchy()?;
                    if after.signature() == latest.signature() {
                        return Ok(after);
                    }
                    latest = after;
                    if attempt + 1 < SETTLE_ATTEMPTS {
                        crate::interrupt::sleep(SETTLE_INTERVAL)?;
                    }
                }
                Ok(latest)
            }
        }
    }

    pub fn tap_on_element(&mut self, req: &TapElementRequest) -> Result<Value> {
        core_ops::tap_on_element(self, req)
    }

    pub fn tap_on_point(&mut self, req: &TapPointRequest) -> Result<Value> {
        core_ops::tap_on_point(self, req)
    }

    pub fn assert_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        core_ops::assert_visible(self, sel, timeout_ms)
    }

    /// Passes once the selector no longer matches; fails when it still does at the timeout.
    pub fn assert_not_visible(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<Value> {
        core_ops::assert_not_visible(self, sel, timeout_ms)
    }

    /// Element bounds in screen pixels (driver units on Android).
    pub fn find_element_bounds(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<[i64; 4]> {
        let (node, _) = core_ops::poll_find(self, sel, timeout_ms)?;
        let b = node.bounds.unwrap_or(Bounds::new(0, 0, 0, 0));
        Ok([i64::from(b.left), i64::from(b.top), i64::from(b.right), i64::from(b.bottom)])
    }

    pub fn copy_text_from(&mut self, sel: &ElementSelector, timeout_ms: u64) -> Result<(String, Value)> {
        core_ops::copy_text_from(self, sel, timeout_ms)
    }

    /// The driver types any text itself (ASCII key by key, Unicode without an IME switch).
    pub fn input_text(&mut self, text: &str) -> Result<Value> {
        self.client.input_text(text)?;
        Ok(json!({ "lane": "native" }))
    }

    /// Screenshot-based `waitForAnimationToEnd` matching Maestro's
    /// `ScreenshotUtils.waitUntilScreenIsStatic`: captures consecutive PNG frames
    /// via `self.client.screenshot()`, sleeps `150 ms` between checks, and
    /// compares pixel similarity against a 99.5% threshold until two consecutive
    /// frames match or `timeout_ms` (default 5 000 ms) elapses. Falls back to
    /// hierarchy `settle` only when `screenshot()` returns an error (e.g. `FLAG_SECURE`).
    pub fn wait_for_animation_to_end(&mut self, timeout_ms: Option<u64>) -> Result<Value> {
        let budget = timeout_ms.unwrap_or(DEFAULT_ANIMATION_TIMEOUT_MS);
        let fallback_settle = |this: &mut Self, rem_ms: u64| -> Result<Value> {
            this.settle(None, Some(rem_ms.min(i32::MAX as u64) as i32))?;
            Ok(json!({ "lane": "native" }))
        };

        let mut prev_bytes = match self.client.screenshot() {
            Ok(b) => b,
            Err(e) if crate::interrupt::is_interrupted_error(&e) => return Err(e),
            Err(_) => return fallback_settle(self, budget),
        };
        let Ok(mut prev_img) = image::load_from_memory(&prev_bytes).map(image::DynamicImage::into_rgba8) else {
            return fallback_settle(self, budget);
        };

        let deadline = Instant::now() + Duration::from_millis(budget);
        loop {
            let rem = deadline.saturating_duration_since(Instant::now());
            if !rem.is_zero() {
                crate::interrupt::sleep(ANIMATION_POLL_INTERVAL.min(rem))?;
            }
            let rem_after_sleep = deadline.saturating_duration_since(Instant::now()).as_millis() as u64;
            let curr_bytes = match self.client.screenshot() {
                Ok(b) => b,
                Err(e) if crate::interrupt::is_interrupted_error(&e) => return Err(e),
                Err(_) => return fallback_settle(self, rem_after_sleep),
            };
            if curr_bytes == prev_bytes {
                return Ok(json!({ "lane": "native" }));
            }
            let Ok(curr_img) = image::load_from_memory(&curr_bytes).map(image::DynamicImage::into_rgba8) else {
                return fallback_settle(self, rem_after_sleep);
            };
            if prev_img.dimensions() == curr_img.dimensions() {
                let similarity = crate::screenshot::match_percentage(
                    &prev_img,
                    &curr_img,
                    crate::screenshot::DEFAULT_PIXEL_TOLERANCE,
                );
                if similarity >= STATIC_SCREEN_MATCH_PERCENT {
                    return Ok(json!({ "lane": "native" }));
                }
            }
            if Instant::now() >= deadline {
                return Ok(json!({ "lane": "native" }));
            }
            prev_bytes = curr_bytes;
            prev_img = curr_img;
        }
    }

    pub fn erase_text(&mut self, characters: i32) -> Result<Value> {
        self.client.erase_all_text(characters.max(0) as u32)?;
        Ok(json!({ "lane": "native" }))
    }

    pub fn back_press(&mut self) -> Result<Value> {
        self.adb.shell("cmd input keyevent 4 2>/dev/null || input keyevent 4")?;
        std::thread::sleep(BACK_PAUSE);
        Ok(json!({ "lane": "native" }))
    }

    /// `AndroidDriver.hideKeyboard`: Back (which dismisses an open IME first),
    /// then settle. Like Maestro, it navigates back when no keyboard is shown.
    pub fn hide_keyboard(&mut self) -> Result<Value> {
        self.adb.shell("cmd input keyevent 4 2>/dev/null || input keyevent 4")?;
        std::thread::sleep(BACK_PAUSE);
        self.settle(None, None)?;
        Ok(json!({ "lane": "native" }))
    }

    pub fn scroll(&mut self) -> Result<Value> {
        core_ops::swipe_direction(self, Direction::Up, 400)?;
        self.settle(None, None)?;
        Ok(json!({ "lane": "native" }))
    }

    pub fn swipe(&mut self, req: &SwipeRequest) -> Result<Value> {
        core_ops::swipe(self, req)
    }

    /// `Orchestra.scrollUntilVisible`: look (500 ms), then swipe from the centre.
    pub fn scroll_until_visible(&mut self, req: &ScrollUntilVisibleRequest) -> Result<Value> {
        core_ops::scroll_until_visible(self, req)
    }

    /// PNG of the current screen through the driver (uncompressed capture).
    pub fn screenshot(&mut self) -> Result<Vec<u8>> {
        self.client.screenshot()
    }

    /// Sets mock GPS location on physical Android devices; the driver grants itself
    /// `android:mock_location` and enables mock providers once per session.
    pub fn set_location(&mut self, latitude: f64, longitude: f64) -> Result<()> {
        if !self.client.mock_location_enabled() {
            self.client.enable_mock_location_providers()?;
        }
        self.client.set_location(latitude, longitude)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::{Arc, Mutex};

    use tonic::{Request, Response, Status, Streaming};

    const POLL_INTERVAL: Duration = Duration::from_millis(50);

    use super::*;
    use crate::backend::{MaestroError, MaestroErrorKind, TapPointRequest};
    use crate::native_lane::client::pb::{
        self,
        maestro_driver_server::{MaestroDriver, MaestroDriverServer},
    };
    use crate::native_lane::connect::testing::{Peer, serve as serve_peer};
    use crate::ui_tree::is_element_not_found;

    const DIALOG: &str = r#"<hierarchy rotation="0">
  <node text="" bounds="[0,0][1080,2400]" enabled="true">
    <node text="Allow Sample App to access this device's location?" bounds="[160,950][920,1100]" enabled="true"/>
    <node text="While using the app" clickable="true" enabled="true" bounds="[160,1150][920,1250]"/>
    <node text="Don&apos;t allow" clickable="true" enabled="true" bounds="[160,1370][920,1470]"/>
  </node>
</hierarchy>"#;

    const APP: &str = r#"<hierarchy rotation="0">
  <node text="Permission: granted" content-desc="" enabled="true" bounds="[100,300][900,400]"/>
  <node text="Request Location Permission" clickable="true" enabled="true" bounds="[100,500][900,600]"/>
</hierarchy>"#;

    #[derive(Default)]
    pub(in crate::native_lane) struct FakeState {
        pub(in crate::native_lane) xml: String,
        taps: Vec<(u32, u32)>,
        typed: Vec<String>,
        erased: Vec<u32>,
        /// Tapping inside this rectangle switches the screen to `APP`.
        dismiss_on: Option<(u32, u32, u32, u32)>,
        hierarchy_calls: usize,
        /// `skip_wait_for_idle` flags received by `view_hierarchy`, in call order.
        hierarchy_requests: Vec<bool>,
        /// Simulated `uiDevice.waitForIdle(500)` block when `skip_wait_for_idle == false`.
        wait_for_idle_delay: Duration,
        /// Once `hierarchy_calls` reaches `.0`, the screen switches to the XML in `.1`.
        swap_xml_at_call: Option<(usize, String)>,
        screenshots: Vec<std::result::Result<Vec<u8>, String>>,
        screenshot_calls: usize,
        mock_location_enabled_calls: usize,
        mock_location_disabled_calls: usize,
        locations: Vec<(f64, f64)>,
        /// `inputText` answers with an error once this many texts were typed.
        fail_typing_after: Option<usize>,
        /// Optional hook invoked after a mutation RPC (`tap`/`inputText`/`eraseAllText`)
        /// has been recorded in state, before the response is returned.
        on_mutation: Option<Box<dyn FnMut() + Send>>,
    }

    struct FakeDriver(Arc<Mutex<FakeState>>);

    #[tonic::async_trait]
    impl MaestroDriver for FakeDriver {
        async fn device_info(&self, _: Request<pb::DeviceInfoRequest>) -> Result<Response<pb::DeviceInfo>, Status> {
            Ok(Response::new(pb::DeviceInfo { width_pixels: 1080, height_pixels: 2400 }))
        }
        async fn view_hierarchy(
            &self,
            req: Request<pb::ViewHierarchyRequest>,
        ) -> Result<Response<pb::ViewHierarchyResponse>, Status> {
            let skip_wait_for_idle = req.into_inner().skip_wait_for_idle;
            let (xml, delay) = {
                let mut s = self.0.lock().unwrap();
                s.hierarchy_calls += 1;
                s.hierarchy_requests.push(skip_wait_for_idle);
                if let Some((at, xml)) = s.swap_xml_at_call.clone()
                    && s.hierarchy_calls >= at
                {
                    s.xml = xml;
                }
                (s.xml.clone(), s.wait_for_idle_delay)
            };
            if !skip_wait_for_idle && !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Ok(Response::new(pb::ViewHierarchyResponse { hierarchy: xml }))
        }
        async fn screenshot(
            &self,
            _: Request<pb::ScreenshotRequest>,
        ) -> Result<Response<pb::ScreenshotResponse>, Status> {
            let mut s = self.0.lock().unwrap();
            let idx = s.screenshot_calls;
            s.screenshot_calls += 1;
            if s.screenshots.is_empty() {
                return Ok(Response::new(pb::ScreenshotResponse { bytes: vec![0x89, b'P', b'N', b'G'] }));
            }
            let entry = s.screenshots.get(idx).unwrap_or_else(|| s.screenshots.last().unwrap());
            match entry {
                Ok(bytes) => Ok(Response::new(pb::ScreenshotResponse { bytes: bytes.clone() })),
                Err(msg) => Err(Status::internal(msg.clone())),
            }
        }
        async fn tap(&self, r: Request<pb::TapRequest>) -> Result<Response<pb::TapResponse>, Status> {
            let t = r.into_inner();
            let mut s = self.0.lock().unwrap();
            s.taps.push((t.x, t.y));
            if let Some((l, tp, rt, b)) = s.dismiss_on
                && (l..=rt).contains(&t.x)
                && (tp..=b).contains(&t.y)
            {
                s.xml = APP.to_string();
            }
            if let Some(cb) = s.on_mutation.as_mut() {
                cb();
            }
            Ok(Response::new(pb::TapResponse {}))
        }
        async fn input_text(
            &self,
            r: Request<pb::InputTextRequest>,
        ) -> Result<Response<pb::InputTextResponse>, Status> {
            let mut s = self.0.lock().unwrap();
            if s.fail_typing_after == Some(s.typed.len()) {
                return Err(Status::internal("keyboard gone"));
            }
            s.typed.push(r.into_inner().text);
            if let Some(cb) = s.on_mutation.as_mut() {
                cb();
            }
            Ok(Response::new(pb::InputTextResponse {}))
        }
        async fn erase_all_text(
            &self,
            r: Request<pb::EraseAllTextRequest>,
        ) -> Result<Response<pb::EraseAllTextResponse>, Status> {
            let mut s = self.0.lock().unwrap();
            s.erased.push(r.into_inner().characters_to_erase);
            if let Some(cb) = s.on_mutation.as_mut() {
                cb();
            }
            Ok(Response::new(pb::EraseAllTextResponse {}))
        }
        async fn set_location(
            &self,
            r: Request<pb::SetLocationRequest>,
        ) -> Result<Response<pb::SetLocationResponse>, Status> {
            let r = r.into_inner();
            self.0.lock().unwrap().locations.push((r.latitude, r.longitude));
            Ok(Response::new(pb::SetLocationResponse {}))
        }
        async fn is_window_updating(
            &self,
            _: Request<pb::CheckWindowUpdatingRequest>,
        ) -> Result<Response<pb::CheckWindowUpdatingResponse>, Status> {
            Ok(Response::new(pb::CheckWindowUpdatingResponse { is_window_updating: false }))
        }
        async fn launch_app(
            &self,
            _: Request<pb::LaunchAppRequest>,
        ) -> Result<Response<pb::LaunchAppResponse>, Status> {
            Ok(Response::new(pb::LaunchAppResponse {}))
        }
        async fn add_media(
            &self,
            _: Request<Streaming<pb::AddMediaRequest>>,
        ) -> Result<Response<pb::AddMediaResponse>, Status> {
            Ok(Response::new(pb::AddMediaResponse {}))
        }
        async fn enable_mock_location_providers(
            &self,
            _: Request<pb::EmptyRequest>,
        ) -> Result<Response<pb::EmptyResponse>, Status> {
            self.0.lock().unwrap().mock_location_enabled_calls += 1;
            Ok(Response::new(pb::EmptyResponse {}))
        }
        async fn disable_location_updates(
            &self,
            _: Request<pb::EmptyRequest>,
        ) -> Result<Response<pb::EmptyResponse>, Status> {
            self.0.lock().unwrap().mock_location_disabled_calls += 1;
            Ok(Response::new(pb::EmptyResponse {}))
        }
    }

    /// A fake driver serving on a free loopback port until stopped or dropped.
    pub(in crate::native_lane) struct Served {
        pub(in crate::native_lane) port: u16,
        thread: Option<std::thread::JoinHandle<()>>,
        stop: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Served {
        /// Stops the server and waits until its port is closed.
        fn stop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    impl Drop for Served {
        fn drop(&mut self) {
            self.stop();
        }
    }

    pub(in crate::native_lane) fn serve(state: Arc<Mutex<FakeState>>) -> Served {
        let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let svc = MaestroDriverServer::new(FakeDriver(state));
        let server = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                let incoming = tonic::transport::server::TcpIncoming::from(listener);
                let _ = port_tx.send(port);
                // Abort the fake server on stop instead of waiting for graceful HTTP/2 GOAWAY:
                // `NativeDriverClient` uses a `current_thread` runtime that is idle between RPCs.
                let server = tokio::spawn(async move {
                    let _ = tonic::transport::Server::builder().add_service(svc).serve_with_incoming(incoming).await;
                });
                let _ = stopped.await;
                server.abort();
                let _ = server.await;
            });
        });
        let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake driver bound");
        Served { port, thread: Some(server), stop: Some(stop) }
    }

    /// A client for the fake driver on `port`, once it answers.
    fn connect_ready(port: u16) -> NativeDriverClient {
        let mut client = NativeDriverClient::connect(port).unwrap();
        let mut up = false;
        for _ in 0..100 {
            if client.device_info_with_timeout(Duration::from_millis(200)).is_ok() {
                up = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(up, "fake driver did not start");
        client
    }

    struct Fixture {
        state: Arc<Mutex<FakeState>>,
        client: NativeDriverClient,
        adb: Adb,
        _served: Served,
    }

    fn fixture(xml: &str) -> Fixture {
        let state = Arc::new(Mutex::new(FakeState { xml: xml.to_string(), ..Default::default() }));
        let served = serve(state.clone());
        let client = connect_ready(served.port);
        Fixture { state, client, adb: Adb::new("unused-in-these-tests"), _served: served }
    }

    fn text(t: &str) -> ElementSelector {
        ElementSelector { text_regex: Some(t.into()), ..Default::default() }
    }

    fn tap_req(sel: ElementSelector, timeout_ms: u64) -> TapElementRequest {
        TapElementRequest {
            selector: sel,
            timeout_ms,
            optional: false,
            long_press: false,
            repeat: None,
            relative_point: None,
            retry_if_no_change: false,
            wait_until_visible: false,
            wait_to_settle_timeout_ms: None,
            app_id: None,
        }
    }

    #[test]
    fn taps_the_system_dialog_button_then_sees_the_app() {
        let mut f = fixture(DIALOG);
        f.state.lock().unwrap().dismiss_on = Some((160, 1150, 920, 1250));
        let mut ops = NativeOps::new(&mut f.client, &f.adb);

        ops.assert_visible(&text("While using the app"), 1000).unwrap();
        let res = ops.tap_on_element(&tap_req(text("While using the app"), 1000)).unwrap();
        assert_eq!((res["x"].as_i64(), res["y"].as_i64()), (Some(540), Some(1200)));
        assert_eq!(res["lane"], "native");

        // The dialog is gone, the app screen is visible again.
        ops.assert_not_visible(&text("While using the app"), 500).unwrap();
        ops.assert_visible(&text("Permission: granted"), 500).unwrap();
        assert_eq!(f.state.lock().unwrap().taps, vec![(540, 1200)]);
    }

    /// Launcher still in front: the cold app put nothing into the tree yet.
    const LAUNCHER: &str = r#"<hierarchy rotation="0">
  <node text="" package="com.android.launcher3" enabled="true" bounds="[0,0][1080,2400]">
    <node text="Phone" package="com.android.launcher3" enabled="true" bounds="[100,2000][300,2200]"/>
  </node>
</hierarchy>"#;

    /// The app's window exists, but its `FlutterView` has no semantics yet.
    const BARE_FLUTTER: &str = r#"<hierarchy rotation="0">
  <node text="" resource-id="" content-desc="" class="android.widget.FrameLayout" package="com.example.app" enabled="true" bounds="[0,0][1080,2400]">
    <node text="" resource-id="" content-desc="" class="android.view.View" package="com.example.app" enabled="true" bounds="[0,0][1080,2400]"/>
  </node>
</hierarchy>"#;

    /// The app is drawn, with its first screen.
    const APP_READY: &str = r#"<hierarchy rotation="0">
  <node text="" package="com.example.app" enabled="true" bounds="[0,0][1080,2400]">
    <node text="Playground" resource-id="playground_button" package="com.example.app" clickable="true" enabled="true" bounds="[100,500][900,600]"/>
  </node>
</hierarchy>"#;

    fn id(i: &str) -> ElementSelector {
        ElementSelector { id_regex: Some(i.into()), ..Default::default() }
    }

    fn cold(extra_ms: u64) -> ColdStart {
        ColdStart {
            app_id: "com.example.app".into(),
            extra: Duration::from_millis(extra_ms),
            log_prefix: String::new(),
        }
    }

    #[test]
    fn a_cold_app_that_shows_up_late_gets_one_extra_window_and_the_tap_lands() {
        for initial in [LAUNCHER, BARE_FLUTTER] {
            let mut f = fixture(initial);
            // Each window polls every 50 ms: the 150 ms lookup is long over
            // when the app shows up at the 8th hierarchy read.
            f.state.lock().unwrap().swap_xml_at_call = Some((8, APP_READY.to_string()));
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            ops.arm_cold_start(cold(3000));
            let t = Instant::now();
            let res = ops.tap_on_element(&tap_req(id("playground_button"), 150)).unwrap();
            assert_eq!((res["x"].as_i64(), res["y"].as_i64()), (Some(500), Some(550)));
            assert!(t.elapsed() < Duration::from_millis(1500), "{:?}", t.elapsed());
            assert_eq!(ops.cold_start_retries(), 1);
            assert_eq!(f.state.lock().unwrap().taps, vec![(500, 550)], "tapped once");
        }
    }

    #[test]
    fn an_armed_lookup_on_a_healthy_launch_costs_no_extra_rpc() {
        let mut f = fixture(APP_READY);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        ops.arm_cold_start(cold(3000));
        ops.assert_visible(&id("playground_button"), 17_000).unwrap();
        assert_eq!(ops.cold_start_retries(), 0);
        assert_eq!(f.state.lock().unwrap().hierarchy_calls, 1, "the found-at-once lookup reads one hierarchy");
    }

    #[test]
    fn a_real_miss_on_a_drawn_app_fails_without_the_extra_window() {
        let mut f = fixture(APP_READY);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        ops.arm_cold_start(cold(5000));
        let t = Instant::now();
        let err = ops.assert_visible(&id("missing_button"), 200).unwrap_err();
        assert!(t.elapsed() < Duration::from_millis(1000), "{:?}", t.elapsed());
        assert!(is_element_not_found(&err));
        assert_eq!(ops.cold_start_retries(), 0);
    }

    #[test]
    fn an_app_that_never_shows_up_fails_after_exactly_one_extra_window_with_the_same_error() {
        let plain = {
            let mut f = fixture(LAUNCHER);
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            ops.assert_visible(&id("playground_button"), 200).unwrap_err().to_string()
        };
        let mut f = fixture(LAUNCHER);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        ops.arm_cold_start(cold(300));
        let t = Instant::now();
        let err = ops.tap_on_element(&tap_req(id("playground_button"), 200)).unwrap_err();
        let waited = t.elapsed();
        assert!(waited >= Duration::from_millis(500) && waited < Duration::from_millis(800), "{waited:?}");
        assert!(is_element_not_found(&err));
        assert_eq!(err.to_string(), plain, "same message as without the retry");
        assert_eq!(ops.cold_start_retries(), 1);
        assert!(f.state.lock().unwrap().taps.is_empty());
        // The arming is spent: the next lookup has the plain budget.
        let t = Instant::now();
        ops.assert_visible(&id("playground_button"), 200).unwrap_err();
        assert!(t.elapsed() < Duration::from_millis(450), "{:?}", t.elapsed());
        assert_eq!(ops.cold_start_retries(), 1);
    }

    #[test]
    fn cold_start_retry_does_not_fire_if_app_appeared_before_timeout() {
        // Starts on launcher (app absent), app appears at call 2 (without target),
        // then disappears again at call 3 — because the app was seen during the
        // initial window, the miss is real and must not trigger the extra window.
        let mut f = fixture(LAUNCHER);
        f.state.lock().unwrap().swap_xml_at_call = Some((2, APP_READY.to_string()));
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        ops.arm_cold_start(cold(5000));
        let t = Instant::now();
        let err = ops.assert_visible(&id("missing_button"), 200).unwrap_err();
        assert!(t.elapsed() < Duration::from_millis(1000), "{:?}", t.elapsed());
        assert!(is_element_not_found(&err));
        assert_eq!(ops.cold_start_retries(), 0);
    }

    #[test]
    fn missing_element_reports_what_is_on_screen() {
        let mut f = fixture(DIALOG);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        let t = Instant::now();
        let err = ops.assert_visible(&text("Allow all the time"), 200).unwrap_err();
        assert!(t.elapsed() >= Duration::from_millis(150), "polled until the timeout");
        let me = err.downcast_ref::<MaestroError>().expect("MaestroError");
        assert_eq!(me.kind, MaestroErrorKind::ElementNotFound);
        assert!(me.message.contains("Allow all the time"), "{}", me.message);
        assert!(me.message.contains("\"While using the app\""), "{}", me.message);
        let err = ops.tap_on_element(&tap_req(text("Nope"), 0)).unwrap_err();
        assert!(is_element_not_found(&err));
        assert!(f.state.lock().unwrap().taps.is_empty());
    }

    #[test]
    fn assert_not_visible_fails_while_the_element_stays() {
        let mut f = fixture(DIALOG);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        let err = ops.assert_not_visible(&text("While using the app"), 100).unwrap_err();
        assert_eq!(err.downcast_ref::<MaestroError>().unwrap().kind, MaestroErrorKind::AssertionFailure);
        ops.assert_not_visible(&text("Something else"), 100).unwrap();
    }

    #[test]
    fn point_taps_text_input_erase_and_copy() {
        let mut f = fixture(APP);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        let res = ops
            .tap_on_point(&TapPointRequest {
                point: "50%, 25%".into(),
                long_press: false,
                repeat: None,
                retry_if_no_change: false,
                wait_to_settle_timeout_ms: None,
            })
            .unwrap();
        assert_eq!((res["x"].as_i64(), res["y"].as_i64()), (Some(540), Some(600)));
        let bad = TapPointRequest {
            point: "101%, 5%".into(),
            long_press: false,
            repeat: None,
            retry_if_no_change: false,
            wait_to_settle_timeout_ms: None,
        };
        assert!(ops.tap_on_point(&bad).is_err());

        ops.input_text("hello 42").unwrap();
        ops.erase_text(50).unwrap();

        let (copied, _) = ops.copy_text_from(&text("Permission: .*"), 500).unwrap();
        assert_eq!(copied, "Permission: granted");
        let bounds = ops.find_element_bounds(&text("Request Location Permission"), 500).unwrap();
        assert_eq!(bounds, [100, 500, 900, 600]);

        let s = f.state.lock().unwrap();
        assert_eq!(s.typed, vec!["hello 42".to_string()]);
        assert_eq!(s.erased, vec![50]);
        assert_eq!(s.taps, vec![(540, 600)]);
    }

    #[test]
    fn long_ascii_text_is_typed_in_several_rpcs_and_stops_at_the_first_failure() {
        let mut f = fixture(APP);
        let long: String = ('a'..='z').cycle().take(250).collect();
        f.client.input_text(&long).unwrap();
        {
            let typed = &f.state.lock().unwrap().typed;
            assert_eq!(typed.iter().map(String::len).collect::<Vec<_>>(), vec![100, 100, 50]);
            assert_eq!(typed.concat(), long);
        }

        // The driver refuses the third piece: nothing after it is sent, and the error
        // says how far the text got.
        let mut f = fixture(APP);
        f.state.lock().unwrap().fail_typing_after = Some(2);
        let err = format!("{:#}", f.client.input_text(&long).unwrap_err());
        assert!(
            err.contains("inputText stopped after 200 of 250 characters") && err.contains("keyboard gone"),
            "{err}"
        );
        assert_eq!(f.state.lock().unwrap().typed.concat(), long[..200]);
    }

    #[test]
    fn input_text_goes_through_the_rpc_without_an_ime_switch() {
        let mut f = fixture(APP);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        // `adb` points at a device that does not exist: any IME shell command would fail.
        ops.input_text("Příliš žluťoučký kůň 🚀").unwrap();
        ops.input_text("ascii").unwrap();
        assert_eq!(f.state.lock().unwrap().typed, vec!["Příliš žluťoučký kůň 🚀".to_string(), "ascii".to_string()]);
    }

    fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba(rgba)));
        crate::screenshot::encode_png(&img).unwrap()
    }

    #[test]
    fn wait_for_animation_to_end_handles_static_changing_and_error_fallback() {
        let red = png_bytes(20, 20, [220, 30, 30, 255]);
        let green = png_bytes(20, 20, [30, 220, 30, 255]);
        let blue = png_bytes(20, 20, [30, 30, 220, 255]);

        // 1. Static screen -> returns fast after 2 screenshot captures, 0 hierarchy dumps.
        {
            let mut f = fixture(APP);
            f.state.lock().unwrap().screenshots = vec![Ok(red.clone()), Ok(red.clone())];
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            let res = ops.wait_for_animation_to_end(Some(2000)).unwrap();
            assert_eq!(res["lane"], "native");
            let s = f.state.lock().unwrap();
            assert_eq!(s.screenshot_calls, 2);
            assert_eq!(s.hierarchy_calls, 0);
        }

        // 2. Changing screen -> polls until two consecutive frames match (red -> green -> blue -> blue).
        {
            let mut f = fixture(APP);
            f.state.lock().unwrap().screenshots =
                vec![Ok(red.clone()), Ok(green.clone()), Ok(blue.clone()), Ok(blue.clone())];
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            let res = ops.wait_for_animation_to_end(Some(3000)).unwrap();
            assert_eq!(res["lane"], "native");
            let s = f.state.lock().unwrap();
            assert_eq!(s.screenshot_calls, 4);
            assert_eq!(s.hierarchy_calls, 0);
        }

        // 3. Screenshot failure (e.g. FLAG_SECURE) -> falls back to hierarchy settle.
        {
            let mut f = fixture(APP);
            f.state.lock().unwrap().screenshots = vec![Err("FLAG_SECURE".into())];
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            let res = ops.wait_for_animation_to_end(Some(500)).unwrap();
            assert_eq!(res["lane"], "native");
            let s = f.state.lock().unwrap();
            assert_eq!(s.screenshot_calls, 1);
            assert!(s.hierarchy_calls >= 2, "expected hierarchy settle fallback, got {}", s.hierarchy_calls);
        }
    }

    const COVERED_TARGET: &str = r#"<hierarchy rotation="0">
  <node text="Covered target" clickable="true" enabled="true" bounds="[100,500][900,600]"/>
  <node text="Temporary cover" enabled="true" bounds="[100,500][900,600]"/>
</hierarchy>"#;

    const UNCOVERED_TARGET: &str = r#"<hierarchy rotation="0">
  <node text="Covered target" clickable="true" enabled="true" bounds="[100,500][900,600]"/>
</hierarchy>"#;

    #[test]
    fn wait_until_visible_waits_out_an_overlay_and_taps_again() {
        // 1. The overlay stays through the first tap's settle and disappears during wait_until_visible.
        let mut f = fixture(COVERED_TARGET);
        f.state.lock().unwrap().swap_xml_at_call = Some((6, UNCOVERED_TARGET.to_string()));
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        let mut req = tap_req(text("Covered target"), 500);
        req.wait_until_visible = true;
        ops.tap_on_element(&req).unwrap();
        let s = f.state.lock().unwrap();
        // First tap hit the overlay (screen unchanged, target covered), second one the button.
        assert_eq!(s.taps, vec![(500, 550), (500, 550)]);
        assert_eq!(s.xml, UNCOVERED_TARGET, "the second tap came after the overlay went away");
        drop(s);

        // 2. Race: the overlay is still covering the target at the moment of the first tap
        //    (after call 2), then vanishes *during* perform_tap's post-tap settle (at call 3
        //    or 4), making `outcome.changed == true` and `outcome.after.is_visible == true`
        //    even though the first tap hit the overlay. Must still re-tap the uncovered target.
        for swap_call in [3, 4] {
            let mut f = fixture(COVERED_TARGET);
            f.state.lock().unwrap().swap_xml_at_call = Some((swap_call, UNCOVERED_TARGET.to_string()));
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            let mut req = tap_req(text("Covered target"), 500);
            req.wait_until_visible = true;
            ops.tap_on_element(&req).unwrap();
            let s = f.state.lock().unwrap();
            assert_eq!(s.taps, vec![(500, 550), (500, 550)], "swap_call={swap_call} missed re-tap");
        }
    }

    #[test]
    fn wait_until_visible_does_not_retap_a_visible_target_or_without_the_flag() {
        let mut f = fixture(UNCOVERED_TARGET);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        let mut req = tap_req(text("Covered target"), 500);
        req.wait_until_visible = true;
        ops.tap_on_element(&req).unwrap();
        assert_eq!(f.state.lock().unwrap().taps.len(), 1, "visible target: a static screen is not a reason to retap");

        let mut f = fixture(COVERED_TARGET);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        ops.tap_on_element(&tap_req(text("Covered target"), 500)).unwrap();
        assert_eq!(f.state.lock().unwrap().taps.len(), 1, "without waitUntilVisible the tap is not repeated");
    }

    #[test]
    fn retry_if_no_change_taps_again_only_when_the_screen_is_static() {
        let mut f = fixture(APP);
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        let mut req = tap_req(text("Request Location Permission"), 500);
        req.retry_if_no_change = true;
        ops.tap_on_element(&req).unwrap();
        // Nothing changed after the first tap, so it was repeated once.
        assert_eq!(f.state.lock().unwrap().taps.len(), 2);

        let mut f = fixture(DIALOG);
        f.state.lock().unwrap().dismiss_on = Some((160, 1150, 920, 1250));
        let mut ops = NativeOps::new(&mut f.client, &f.adb);
        let mut req = tap_req(text("While using the app"), 500);
        req.retry_if_no_change = true;
        ops.tap_on_element(&req).unwrap();
        assert_eq!(f.state.lock().unwrap().taps.len(), 1, "the screen changed, no retry");
    }

    // --- reconnect after a transport drop --------------------------------

    /// A port nothing listens on.
    fn dead_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// A replacement driver for a restart, with its own state; counts the restarts.
    struct Replacement {
        state: Arc<Mutex<FakeState>>,
        restarts: Arc<Mutex<u32>>,
        served: Arc<Mutex<Vec<Served>>>,
    }

    impl Replacement {
        fn new(xml: &str) -> Self {
            let state = Arc::new(Mutex::new(FakeState { xml: xml.to_string(), ..Default::default() }));
            Self { state, restarts: Arc::new(Mutex::new(0)), served: Arc::new(Mutex::new(Vec::new())) }
        }

        fn restart_fn(&self) -> crate::native_lane::client::RestartFn {
            let (state, restarts, served) = (self.state.clone(), self.restarts.clone(), self.served.clone());
            Box::new(move || {
                *restarts.lock().unwrap() += 1;
                let s = serve(state.clone());
                let client = connect_ready(s.port);
                served.lock().unwrap().push(s);
                Ok(client)
            })
        }

        fn restarts(&self) -> u32 {
            *self.restarts.lock().unwrap()
        }
    }

    #[test]
    fn refused_connection_restarts_the_session_and_repeats_a_tap() {
        let replacement = Replacement::new(APP);
        let mut client = NativeDriverClient::connect(dead_port()).unwrap();
        client.set_restart(replacement.restart_fn());

        // The tap never left the host, so it is safe to send again after the restart.
        client.tap(10, 20).unwrap();
        assert_eq!(replacement.restarts(), 1);
        assert_eq!(replacement.state.lock().unwrap().taps, vec![(10, 20)]);
        assert_eq!(client.restarts(), 1);
        let notices = client.take_restart_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("restarted it (restart 1/3)") && notices[0].contains("`tap`"), "{notices:?}");
        assert!(client.take_restart_notices().is_empty(), "notices are reported once");

        // The new session serves every later call without another restart.
        client.tap(30, 40).unwrap();
        assert_eq!(replacement.restarts(), 1);
        assert_eq!(replacement.state.lock().unwrap().taps, vec![(10, 20), (30, 40)]);
    }

    /// The three input RPCs, each sent once.
    const INPUTS: [&str; 3] = ["tap", "inputText", "eraseAllText"];

    fn send_input(client: &mut NativeDriverClient, kind: &str) -> Result<()> {
        match kind {
            "tap" => client.tap(100, 200),
            "inputText" => client.input_text("once"),
            "eraseAllText" => client.erase_all_text(5),
            other => unreachable!("{other}"),
        }
    }

    /// How many input RPCs of any kind the fake driver behind `state` received.
    fn inputs_received(state: &Mutex<FakeState>) -> usize {
        let s = state.lock().unwrap();
        s.taps.len() + s.typed.len() + s.erased.len()
    }

    #[test]
    fn input_to_a_driver_that_died_behind_adb_forward_is_replayed_exactly_once() {
        // `adb forward` to a dead device end accepts the connection and closes (or resets)
        // it. Nothing reached the device, so after the restart the input is sent again.
        for peer in [Peer::Closes, Peer::Resets] {
            for kind in INPUTS {
                let (port, seen) = serve_peer(peer);
                let replacement = Replacement::new(APP);
                let mut client = NativeDriverClient::connect(port).unwrap();
                client.set_restart(replacement.restart_fn());

                send_input(&mut client, kind).unwrap_or_else(|e| panic!("{peer:?} {kind}: {e:#}"));
                assert!(!seen.recv().unwrap(), "{peer:?} {kind}: the dead end read a request");
                assert_eq!(replacement.restarts(), 1, "{peer:?} {kind}");
                assert_eq!(inputs_received(&replacement.state), 1, "{peer:?} {kind}: delivered once, not twice");
                let notices = client.take_restart_notices();
                assert!(notices.len() == 1 && notices[0].contains(kind), "{peer:?} {kind}: {notices:?}");

                // The new session serves the next step without another restart.
                send_input(&mut client, kind).unwrap();
                assert_eq!((replacement.restarts(), inputs_received(&replacement.state)), (1, 2), "{peer:?} {kind}");
            }
        }
    }

    #[test]
    fn input_a_peer_read_before_it_reset_the_connection_is_never_replayed() {
        // The peer took the whole request off the wire and then reset the connection. Whether
        // it had sent its own preface or not, it may have applied the input.
        for peer in [Peer::ReadsRequestThenResets, Peer::SilentReadsRequestThenResets] {
            for kind in INPUTS {
                let (port, seen) = serve_peer(peer);
                let replacement = Replacement::new(APP);
                let mut client = NativeDriverClient::connect(port).unwrap();
                client.set_restart(replacement.restart_fn());

                let err = format!("{:#}", send_input(&mut client, kind).unwrap_err());
                assert!(err.contains("not repeated") && err.contains(kind), "{peer:?} {kind}: {err}");
                assert!(seen.recv().unwrap(), "{peer:?} {kind}: the peer never got the request");
                assert_eq!(replacement.restarts(), 1, "{peer:?} {kind}");
                assert_eq!(inputs_received(&replacement.state), 0, "{peer:?} {kind}: must not be sent again");

                // The driver is healthy again for the next step.
                send_input(&mut client, kind).unwrap();
                assert_eq!((replacement.restarts(), inputs_received(&replacement.state)), (1, 1), "{peer:?} {kind}");
            }
        }
    }

    #[test]
    fn a_live_driver_speaks_first_so_a_new_connection_never_waits_out_the_grace() {
        let state = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let served = serve(state.clone());
        // With a grace this long, a server that waited for the client's preface would
        // make the first call time out instead of answering.
        let mut client = NativeDriverClient::connect_with_grace(served.port, Duration::from_secs(600)).unwrap();
        client.device_info_with_timeout(Duration::from_secs(10)).unwrap();
        client.tap(1, 2).unwrap();
        assert_eq!(state.lock().unwrap().taps, vec![(1, 2)]);
    }

    #[test]
    fn reads_are_repeated_after_any_transport_failure() {
        for peer in [Peer::Closes, Peer::ReadsRequestThenResets] {
            let (port, _seen) = serve_peer(peer);
            let replacement = Replacement::new(APP);
            let mut client = NativeDriverClient::connect(port).unwrap();
            client.set_restart(replacement.restart_fn());
            let xml = client.view_hierarchy().unwrap();
            assert!(xml.contains("Request Location Permission"), "{peer:?}: {xml}");
            assert_eq!(replacement.restarts(), 1, "{peer:?}");
        }
    }

    #[test]
    fn a_server_that_goes_away_mid_run_is_replaced() {
        let state = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let mut first = serve(state);
        let mut client = connect_ready(first.port);
        let replacement = Replacement::new(DIALOG);
        client.set_restart(replacement.restart_fn());
        assert!(client.view_hierarchy().unwrap().contains("Permission: granted"));

        first.stop();
        let xml = client.view_hierarchy().unwrap();
        assert!(xml.contains("While using the app"), "served by the replacement: {xml}");
        assert_eq!(replacement.restarts(), 1);
    }

    #[test]
    fn a_restart_resets_per_session_state() {
        let replacement = Replacement::new(APP);
        let first = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let mut served = serve(first);
        let mut client = connect_ready(served.port);
        client.set_restart(replacement.restart_fn());
        client.enable_mock_location_providers().unwrap();
        assert!(client.mock_location_enabled());
        assert_eq!(client.screen_size().unwrap().width, 1080);

        served.stop();
        client.view_hierarchy().unwrap();
        assert_eq!(client.restarts(), 1);
        assert!(!client.mock_location_enabled(), "the new instrumentation has no mock providers yet");
    }

    #[test]
    fn answers_and_timeouts_never_restart() {
        let state = Arc::new(Mutex::new(FakeState {
            xml: APP.to_string(),
            screenshots: vec![Err("capture failed".into())],
            ..Default::default()
        }));
        let served = serve(state);
        let mut client = connect_ready(served.port);
        let replacement = Replacement::new(APP);
        client.set_restart(replacement.restart_fn());

        let err = format!("{:#}", client.screenshot().unwrap_err());
        assert!(err.contains("capture failed"), "{err}");
        assert_eq!(replacement.restarts(), 0, "the driver answered, so the transport is fine");
        assert_eq!(client.restarts(), 0);
    }

    #[test]
    fn a_failed_restart_keeps_the_error_and_is_not_attempted_again() {
        let calls = Arc::new(Mutex::new(0u32));
        let seen = calls.clone();
        let mut client = NativeDriverClient::connect(dead_port()).unwrap();
        client.set_restart(Box::new(move || {
            *seen.lock().unwrap() += 1;
            Err(anyhow::anyhow!("device offline"))
        }));

        let err = format!("{:#}", client.view_hierarchy().unwrap_err());
        assert!(err.contains("could not be restarted") && err.contains("device offline"), "{err}");
        assert!(err.contains("viewHierarchy") && err.contains("tcp connect error"), "keeps the original error: {err}");
        assert_eq!(*calls.lock().unwrap(), 1);

        // The lane is down: later calls fail fast on the transport error, with no new restart.
        let err = format!("{:#}", client.view_hierarchy().unwrap_err());
        assert!(err.contains("tcp connect error") && !err.contains("restarted"), "{err}");
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn restarts_are_capped_per_session() {
        let calls = Arc::new(Mutex::new(0u32));
        let seen = calls.clone();
        let mut client = NativeDriverClient::connect(dead_port()).unwrap();
        // Every "restarted" driver is dead again.
        client.set_restart(Box::new(move || {
            *seen.lock().unwrap() += 1;
            NativeDriverClient::connect(dead_port())
        }));

        let mut last = String::new();
        for _ in 0..6 {
            last = format!("{:#}", client.view_hierarchy().unwrap_err());
            if last.contains("giving up") {
                break;
            }
        }
        assert!(last.contains("giving up"), "{last}");
        assert_eq!(*calls.lock().unwrap(), crate::native_lane::client::MAX_RESTARTS);
    }

    #[test]
    fn the_dead_session_is_torn_down_before_the_replacement_starts() {
        struct Guard(Arc<Mutex<Vec<&'static str>>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.lock().unwrap().push("old session stopped");
            }
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let replacement = Replacement::new(APP);
        let mut restart = replacement.restart_fn();
        let order = log.clone();
        let mut client = NativeDriverClient::connect(dead_port()).unwrap();
        client.set_keepalive(Box::new(Guard(log.clone())));
        client.set_restart(Box::new(move || {
            order.lock().unwrap().push("replacement started");
            restart()
        }));

        client.view_hierarchy().unwrap();
        // `force-stop` of the old instrumentation would kill the new one if it ran after the start.
        assert_eq!(*log.lock().unwrap(), vec!["old session stopped", "replacement started"]);
    }

    /// Loopback TCP proxy in front of a real `Served` driver whose active socket can be
    /// torn down mid-RPC (after the server received the request, or mid-response body).
    struct KillableProxy {
        port: u16,
        /// When set, server->client copying stops and closes both sockets once `Some(n)`
        /// bytes have been forwarded since `arm_response_cut` was called (0 = cut immediately).
        cut_after: Arc<Mutex<Option<usize>>>,
        active_client: Arc<Mutex<Option<std::net::TcpStream>>>,
        active_upstream: Arc<Mutex<Option<std::net::TcpStream>>>,
    }

    impl KillableProxy {
        fn start(upstream_port: u16) -> Self {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let cut_after: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
            let active_client: Arc<Mutex<Option<std::net::TcpStream>>> = Arc::new(Mutex::new(None));
            let active_upstream: Arc<Mutex<Option<std::net::TcpStream>>> = Arc::new(Mutex::new(None));

            let cut = cut_after.clone();
            let ac = active_client.clone();
            let au = active_upstream.clone();
            std::thread::spawn(move || {
                while let Ok((client, _)) = listener.accept() {
                    let Ok(upstream) = std::net::TcpStream::connect(("127.0.0.1", upstream_port)) else {
                        break;
                    };
                    let _ = client.set_nodelay(true);
                    let _ = upstream.set_nodelay(true);
                    *ac.lock().unwrap() = client.try_clone().ok();
                    *au.lock().unwrap() = upstream.try_clone().ok();

                    let (mut c_r, mut c_w) = (client.try_clone().unwrap(), client);
                    let (mut u_r, mut u_w) = (upstream.try_clone().unwrap(), upstream);

                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = c_r.read(&mut buf) {
                            if n == 0 || u_w.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        let _ = u_w.shutdown(std::net::Shutdown::Both);
                    });

                    let cut_s2c = cut.clone();
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        let mut forwarded = 0usize;
                        while let Ok(n) = u_r.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let limit = *cut_s2c.lock().unwrap();
                            if let Some(max_bytes) = limit {
                                let allow = max_bytes.saturating_sub(forwarded).min(n);
                                if allow > 0 {
                                    let _ = c_w.write_all(&buf[..allow]);
                                    forwarded += allow;
                                }
                                if forwarded >= max_bytes {
                                    let _ = c_w.shutdown(std::net::Shutdown::Both);
                                    let _ = u_r.shutdown(std::net::Shutdown::Both);
                                    return;
                                }
                            } else if c_w.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        let _ = c_w.shutdown(std::net::Shutdown::Both);
                    });
                }
            });

            Self { port, cut_after, active_client, active_upstream }
        }

        fn kill_now(&self) {
            *self.cut_after.lock().unwrap() = Some(0);
            if let Some(s) = self.active_client.lock().unwrap().take() {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            if let Some(s) = self.active_upstream.lock().unwrap().take() {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }

        fn cut_response_after_bytes(&self, max_bytes: usize) {
            *self.cut_after.lock().unwrap() = Some(max_bytes);
        }
    }

    #[test]
    fn broken_after_request_sent_restarts_but_never_repeats_tap_input_or_erase() {
        // For each non-idempotent input RPC, drop the TCP connection AFTER the server has
        // fully received and applied the request (while sending the response back).
        for kind in ["tap", "inputText", "eraseAllText"] {
            let state = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
            let _served = serve(state.clone());
            let proxy = Arc::new(KillableProxy::start(_served.port));
            let mut client = connect_ready(proxy.port);
            let replacement = Replacement::new(APP);
            client.set_restart(replacement.restart_fn());

            let kill_proxy = proxy.clone();
            state.lock().unwrap().on_mutation = Some(Box::new(move || {
                kill_proxy.kill_now();
            }));

            let err = match kind {
                "tap" => format!("{:#}", client.tap(100, 200).unwrap_err()),
                "inputText" => format!("{:#}", client.input_text("once").unwrap_err()),
                "eraseAllText" => format!("{:#}", client.erase_all_text(5).unwrap_err()),
                _ => unreachable!(),
            };
            assert!(err.contains("not repeated") && err.contains(kind), "{kind}: {err}");
            assert_eq!(replacement.restarts(), 1, "{kind}: must restart once");

            // Original server applied it; replacement must NOT have received a duplicate.
            let orig = state.lock().unwrap();
            let repl = replacement.state.lock().unwrap();
            match kind {
                "tap" => {
                    assert_eq!(orig.taps, vec![(100, 200)]);
                    assert!(repl.taps.is_empty(), "tap must not be repeated");
                }
                "inputText" => {
                    assert_eq!(orig.typed, vec!["once".to_string()]);
                    assert!(repl.typed.is_empty(), "inputText must not be repeated");
                }
                "eraseAllText" => {
                    assert_eq!(orig.erased, vec![5]);
                    assert!(repl.erased.is_empty(), "eraseAllText must not be repeated");
                }
                _ => unreachable!(),
            }
            drop(orig);
            drop(repl);

            // Replacement is healthy for subsequent commands.
            assert!(client.view_hierarchy().unwrap().contains("Permission: granted"));
            assert_eq!(replacement.restarts(), 1);
        }
    }

    #[test]
    fn mid_response_body_drop_restarts_the_driver() {
        // Large XML ensures HTTP/2 sends the `200 OK` HEADERS frame and starts streaming DATA
        // frames before the proxy severs the connection mid-body.
        let huge_xml = format!("<hierarchy rotation=\"0\">{}</hierarchy>", "a".repeat(256 * 1024));
        let state = Arc::new(Mutex::new(FakeState { xml: huge_xml, ..Default::default() }));
        let _served = serve(state);
        let proxy = KillableProxy::start(_served.port);
        let mut client = connect_ready(proxy.port);
        let replacement = Replacement::new(DIALOG);
        client.set_restart(replacement.restart_fn());

        // Forward enough bytes for the HTTP/2 response HEADERS frame + partial DATA, then cut.
        proxy.cut_response_after_bytes(256);

        let xml = client.view_hierarchy().expect("mid-body transport drop should restart and replay read");
        assert!(xml.contains("While using the app"), "expected replacement hierarchy, got: {xml}");
        assert_eq!(replacement.restarts(), 1);
    }

    #[test]
    fn replacement_dying_on_replayed_call_errors_without_looping() {
        let calls = Arc::new(Mutex::new(0u32));
        let seen = calls.clone();
        let mut client = NativeDriverClient::connect(dead_port()).unwrap();
        client.set_restart(Box::new(move || {
            *seen.lock().unwrap() += 1;
            // Replacement is itself dead when the replayed call runs.
            NativeDriverClient::connect(dead_port())
        }));

        let err = format!("{:#}", client.view_hierarchy().unwrap_err());
        assert!(err.contains("restarted") && err.contains("failed again"), "{err}");
        assert_eq!(*calls.lock().unwrap(), 1, "one RPC must restart at most once, never loop");
        assert_eq!(client.restarts(), 1);
        assert_eq!(client.take_restart_notices().len(), 1);
    }

    #[test]
    fn notices_accumulate_across_consecutive_restarts() {
        let s1 = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let mut first = serve(s1);
        let mut client = connect_ready(first.port);

        let servers: Arc<Mutex<Vec<Served>>> = Arc::new(Mutex::new(Vec::new()));
        let servers_for_restart = servers.clone();
        client.set_restart(Box::new(move || {
            let st = Arc::new(Mutex::new(FakeState { xml: DIALOG.to_string(), ..Default::default() }));
            let s = serve(st);
            let c = connect_ready(s.port);
            servers_for_restart.lock().unwrap().push(s);
            Ok(c)
        }));

        // First drop -> restart 1/3 (do not drain notices yet).
        first.stop();
        client.view_hierarchy().unwrap();
        assert_eq!(client.restarts(), 1);

        // Second drop -> restart 2/3.
        servers.lock().unwrap()[0].stop();
        client.view_hierarchy().unwrap();
        assert_eq!(client.restarts(), 2);

        let notices = client.take_restart_notices();
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(notices[0].contains("restart 1/3"), "{notices:?}");
        assert!(notices[1].contains("restart 2/3"), "{notices:?}");
        assert!(client.take_restart_notices().is_empty());
    }

    #[test]
    fn restart_returning_client_with_recovery_already_set_keeps_outer_recovery() {
        let outer_calls = Arc::new(Mutex::new(0u32));
        let inner_calls = Arc::new(Mutex::new(0u32));
        let servers: Arc<Mutex<Vec<Served>>> = Arc::new(Mutex::new(Vec::new()));

        let oc = outer_calls.clone();
        let ic = inner_calls.clone();
        let sv = servers.clone();
        let mut client = NativeDriverClient::connect(dead_port()).unwrap();
        client.set_restart(Box::new(move || {
            *oc.lock().unwrap() += 1;
            let st = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
            let s = serve(st);
            let mut fresh = connect_ready(s.port);
            let ic2 = ic.clone();
            fresh.set_restart(Box::new(move || {
                *ic2.lock().unwrap() += 1;
                Err(anyhow::anyhow!("inner recovery must be overwritten"))
            }));
            sv.lock().unwrap().push(s);
            Ok(fresh)
        }));

        client.view_hierarchy().unwrap();
        assert_eq!(*outer_calls.lock().unwrap(), 1);
        assert_eq!(*inner_calls.lock().unwrap(), 0);

        // Second drop must still invoke the outer recovery, not the overwritten inner one.
        servers.lock().unwrap()[0].stop();
        client.view_hierarchy().unwrap();
        assert_eq!(*outer_calls.lock().unwrap(), 2);
        assert_eq!(*inner_calls.lock().unwrap(), 0);
    }

    #[test]
    fn panicking_restart_cleans_up_old_session_and_leaves_client_safe_without_retrying() {
        struct DropFlag(Arc<Mutex<bool>>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = true;
            }
        }

        let dropped = Arc::new(Mutex::new(false));
        let calls = Arc::new(Mutex::new(0u32));
        let seen = calls.clone();
        let mut client = NativeDriverClient::connect(dead_port()).unwrap();
        client.set_keepalive(Box::new(DropFlag(dropped.clone())));
        client.set_restart(Box::new(move || {
            *seen.lock().unwrap() += 1;
            panic!("boom during restart");
        }));

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| client.view_hierarchy()));
        assert!(panicked.is_err(), "expected restart panic to propagate");
        assert!(*dropped.lock().unwrap(), "old session keepalive must be dropped before restart runs");
        assert_eq!(*calls.lock().unwrap(), 1);

        // Later calls must fail fast without re-running the panicking closure.
        let err = format!("{:#}", client.view_hierarchy().unwrap_err());
        assert!(err.contains("tcp connect error"), "{err}");
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn set_location_re_enables_mock_providers_after_mid_call_restart() {
        let first_state = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let mut first = serve(first_state.clone());
        let mut client = connect_ready(first.port);
        let replacement = Replacement::new(APP);
        client.set_restart(replacement.restart_fn());
        let adb = Adb::new("unused-in-these-tests");
        let mut ops = NativeOps::new(&mut client, &adb);

        ops.set_location(50.0, 14.0).unwrap();
        assert_eq!(first_state.lock().unwrap().mock_location_enabled_calls, 1);
        assert_eq!(first_state.lock().unwrap().locations, vec![(50.0, 14.0)]);

        // Driver dies between two `setLocation` calls (e.g., mid-`travel`).
        first.stop();
        ops.set_location(50.1, 14.1).unwrap();
        assert_eq!(replacement.restarts(), 1);

        let repl = replacement.state.lock().unwrap();
        assert_eq!(repl.locations, vec![(50.1, 14.1)]);
        assert_eq!(
            repl.mock_location_enabled_calls, 1,
            "the restarted driver has no mock location providers until enableMockLocationProviders runs on it"
        );
        drop(repl);

        // A follow-up `setLocation` on the restarted session must not redundantly re-enable mock providers.
        ops.set_location(50.2, 14.2).unwrap();
        let repl = replacement.state.lock().unwrap();
        assert_eq!(repl.mock_location_enabled_calls, 1);
        assert_eq!(repl.locations, vec![(50.1, 14.1), (50.2, 14.2)]);
        assert_eq!(replacement.restarts(), 1);
    }

    // --- mock-location cleanup at teardown ---------------------------------

    #[test]
    fn dropping_the_client_removes_the_mock_providers_it_enabled() {
        let state = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let served = serve(state.clone());
        let mut client = connect_ready(served.port);
        client.enable_mock_location_providers().unwrap();
        client.set_location(50.0755, 14.4378).unwrap();
        assert_eq!(state.lock().unwrap().mock_location_disabled_calls, 0, "kept while the session is in use");

        drop(client);
        assert_eq!(state.lock().unwrap().mock_location_disabled_calls, 1, "removed when the session ends");
    }

    #[test]
    fn a_client_that_never_mocked_location_does_not_call_the_driver_on_drop() {
        let state = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let served = serve(state.clone());
        let mut client = connect_ready(served.port);
        client.view_hierarchy().unwrap();
        drop(client);
        assert_eq!(state.lock().unwrap().mock_location_disabled_calls, 0);
    }

    #[test]
    fn the_cleanup_runs_through_the_lane_handle_before_the_session_is_torn_down() {
        struct Guard(Arc<Mutex<FakeState>>, Arc<Mutex<Option<usize>>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                // Runs when the keepalive guards stop the instrumentation: the driver must have been told by now.
                *self.1.lock().unwrap() = Some(self.0.lock().unwrap().mock_location_disabled_calls);
            }
        }
        let state = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let served = serve(state.clone());
        let mut client = connect_ready(served.port);
        let seen = Arc::new(Mutex::new(None));
        client.set_keepalive(Box::new(Guard(state.clone(), seen.clone())));
        client.enable_mock_location_providers().unwrap();
        drop(crate::native_lane::NativeLaneHandle::ready_for_test(client, Adb::new("unused-in-these-tests")));
        assert_eq!(*seen.lock().unwrap(), Some(1), "disableLocationUpdates must reach the driver before it is stopped");
    }

    #[test]
    fn a_restart_does_not_ask_the_dead_session_to_clean_up_but_the_new_one_does() {
        let first = Arc::new(Mutex::new(FakeState { xml: APP.to_string(), ..Default::default() }));
        let mut served = serve(first.clone());
        let mut client = connect_ready(served.port);
        let replacement = Replacement::new(APP);
        client.set_restart(replacement.restart_fn());
        client.enable_mock_location_providers().unwrap();

        served.stop();
        let t = Instant::now();
        client.set_location(50.1, 14.1).unwrap();
        assert!(t.elapsed() < Duration::from_secs(2), "the restart waited on the dead session: {:?}", t.elapsed());
        assert_eq!(first.lock().unwrap().mock_location_disabled_calls, 0, "the dead session was not contacted");

        drop(client);
        assert_eq!(
            replacement.state.lock().unwrap().mock_location_disabled_calls,
            1,
            "the providers re-enabled for the replayed setLocation are removed at the end"
        );
    }

    #[test]
    fn positive_polls_skip_wait_for_idle_while_settle_and_negative_keep_it() {
        // 1. Direct helpers: hierarchy() sends false, hierarchy_fast() sends true.
        {
            let mut f = fixture(APP);
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            ops.hierarchy().unwrap();
            ops.hierarchy_fast().unwrap();
            assert_eq!(f.state.lock().unwrap().hierarchy_requests, vec![false, true]);
        }

        // 2. poll_find (via assert_visible): every poll sends skip_wait_for_idle == true.
        {
            let mut f = fixture(LAUNCHER);
            f.state.lock().unwrap().swap_xml_at_call = Some((3, APP_READY.to_string()));
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            ops.assert_visible(&id("playground_button"), 2_000).unwrap();
            assert_eq!(f.state.lock().unwrap().hierarchy_requests, vec![true, true, true]);
        }

        // 3. assert_not_visible: every poll sends skip_wait_for_idle == false.
        {
            let mut f = fixture(DIALOG);
            f.state.lock().unwrap().swap_xml_at_call = Some((3, APP.to_string()));
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            ops.assert_not_visible(&text("While using the app"), 2_000).unwrap();
            assert_eq!(f.state.lock().unwrap().hierarchy_requests, vec![false, false, false]);
        }

        // 4. settle: every poll sends skip_wait_for_idle == false.
        {
            let mut f = fixture(APP);
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            ops.settle(None, Some(200)).unwrap();
            assert_eq!(f.state.lock().unwrap().hierarchy_requests, vec![false, false]);
        }

        // 5. wait_until_visible: every poll sends skip_wait_for_idle == true.
        {
            let mut f = fixture(COVERED_TARGET);
            f.state.lock().unwrap().swap_xml_at_call = Some((3, UNCOVERED_TARGET.to_string()));
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            let initial = ops.hierarchy().unwrap();
            f.state.lock().unwrap().hierarchy_requests.clear();
            let uncovered = ops.wait_until_visible(&text("Covered target"), &initial).unwrap();
            assert!(uncovered.find(&text("Temporary cover")).unwrap().is_none());
            let reqs = f.state.lock().unwrap().hierarchy_requests.clone();
            assert!(
                !reqs.is_empty() && reqs.iter().all(|&skip| skip),
                "wait_until_visible must skip waitForIdle: {reqs:?}"
            );
        }
    }

    #[test]
    fn poll_find_avoids_wait_for_idle_block_on_animated_screen() {
        // Simulate an animated screen where `uiDevice.waitForIdle(500)` blocks 500 ms
        // whenever `skip_wait_for_idle == false`, and 0 ms when `skip_wait_for_idle == true`.
        let before_elapsed = {
            let mut f = fixture(LAUNCHER);
            {
                let mut s = f.state.lock().unwrap();
                s.wait_for_idle_delay = Duration::from_millis(500);
                s.swap_xml_at_call = Some((3, APP_READY.to_string()));
            }
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            let t = Instant::now();
            for i in 0..3 {
                let h = ops.hierarchy().unwrap();
                if h.find(&id("playground_button")).unwrap().is_some() {
                    break;
                }
                if i < 2 {
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
            let elapsed = t.elapsed();
            assert_eq!(f.state.lock().unwrap().hierarchy_requests, vec![false, false, false]);
            elapsed
        };

        let after_elapsed = {
            let mut f = fixture(LAUNCHER);
            {
                let mut s = f.state.lock().unwrap();
                s.wait_for_idle_delay = Duration::from_millis(500);
                s.swap_xml_at_call = Some((3, APP_READY.to_string()));
            }
            let mut ops = NativeOps::new(&mut f.client, &f.adb);
            let t = Instant::now();
            ops.poll_find(&id("playground_button"), 2_000).unwrap();
            let elapsed = t.elapsed();
            assert_eq!(f.state.lock().unwrap().hierarchy_requests, vec![true, true, true]);
            elapsed
        };

        eprintln!(
            "3-poll animated-screen lookup latency: before={} ms (skip_wait_for_idle=false), after={} ms (skip_wait_for_idle=true)",
            before_elapsed.as_millis(),
            after_elapsed.as_millis(),
        );
        assert!(
            before_elapsed >= Duration::from_millis(1500),
            "expected 3 blocking waitForIdle(500) polls >= 1500 ms, got {before_elapsed:?}"
        );
        assert!(after_elapsed < Duration::from_millis(400), "expected 3 fast polls < 400 ms, got {after_elapsed:?}");
    }
}
