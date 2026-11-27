//! The shim (`native/ios_hid/shim.m`) behind [`HidBackend`]. macOS builds compile the shim in
//! `build.rs`; every other target gets a stub that reports the backend as unavailable.

use super::{HidBackend, HidError};

/// Error classes the shim puts in front of its messages (see `shim.m`).
const UNAVAILABLE_PREFIX: &str = "[unavailable] ";
const SENT_PREFIX: &str = "[sent] ";

/// Reads the shim's error text. `[unavailable]` is a backend that cannot serve, `[sent]` an input
/// call that failed after a message went to SimulatorKit, anything else a plain call failure.
pub(super) fn classify_shim_error(msg: &str) -> HidError {
    if let Some(m) = msg.strip_prefix(UNAVAILABLE_PREFIX) {
        HidError::Unavailable(m.to_string())
    } else if let Some(m) = msg.strip_prefix(SENT_PREFIX) {
        HidError::InputMaybeSent(m.to_string())
    } else {
        HidError::Failed(msg.to_string())
    }
}

#[cfg(target_os = "macos")]
pub(super) use mac::open;

#[cfg(not(target_os = "macos"))]
pub(super) fn open(_udid: &str) -> Result<Box<dyn HidBackend>, HidError> {
    Err(HidError::Unavailable("the iOS Simulator backend only exists in macOS builds".into()))
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CStr, CString, c_char, c_double, c_int};
    use std::process::Command;

    use super::{HidBackend, HidError, classify_shim_error};
    use crate::ios_native_lane::hid::{Button, Screen, Snapshot};

    /// `ap_snapshot_opts` in `shim.h`.
    #[repr(C)]
    struct SnapshotOpts {
        pid: c_int,
        has_point: c_int,
        x: c_double,
        y: c_double,
        all_attrs: c_int,
        springboard: c_int,
        max_depth: c_int,
        enable_ax: c_int,
    }

    unsafe extern "C" {
        fn ap_screen(dev_dir: *const c_char, udid: *const c_char, err: *mut *mut c_char) -> *mut c_char;
        fn ap_snapshot(
            dev_dir: *const c_char,
            udid: *const c_char,
            timeout_s: c_double,
            opts: *const SnapshotOpts,
            err: *mut *mut c_char,
        ) -> *mut c_char;
        fn ap_touch(
            dev_dir: *const c_char,
            udid: *const c_char,
            xs: *const c_double,
            ys: *const c_double,
            n: c_int,
            hold_ms: c_int,
            step_ms: c_int,
            err: *mut *mut c_char,
        ) -> c_int;
        fn ap_button(dev_dir: *const c_char, udid: *const c_char, button: c_int, err: *mut *mut c_char) -> c_int;
        fn ap_frame(
            dev_dir: *const c_char,
            udid: *const c_char,
            quality: c_double,
            len: *mut usize,
            err: *mut *mut c_char,
        ) -> *mut c_char;
        fn ap_free(s: *mut c_char);
    }

    /// Takes the text of a shim string and frees it.
    ///
    /// # Safety
    /// `p` is NULL or a NUL-terminated string malloc'd by the shim that nothing else frees.
    unsafe fn take_string(p: *mut c_char) -> Option<String> {
        if p.is_null() {
            return None;
        }
        // SAFETY: guaranteed by the caller.
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        // SAFETY: `p` came from the shim and is not used again.
        unsafe { ap_free(p) };
        Some(s)
    }

    fn shim_error(err: *mut c_char) -> HidError {
        // SAFETY: `err` is the out pointer the shim just filled (or left NULL).
        let msg = unsafe { take_string(err) }.unwrap_or_else(|| "the shim failed without a message".to_string());
        classify_shim_error(&msg)
    }

    pub(super) struct ShimBackend {
        dev_dir: CString,
        udid: CString,
    }

    /// The Xcode developer directory: `DEVELOPER_DIR`, else `xcode-select -p`.
    fn developer_dir() -> Result<String, HidError> {
        if let Some(dir) = std::env::var("DEVELOPER_DIR").ok().filter(|d| !d.is_empty()) {
            return Ok(dir);
        }
        let out = Command::new("xcode-select")
            .arg("-p")
            .output()
            .map_err(|e| HidError::Unavailable(format!("could not run xcode-select: {e}")))?;
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || dir.is_empty() {
            return Err(HidError::Unavailable("xcode-select -p found no Xcode".into()));
        }
        Ok(dir)
    }

    /// Opens the shim for `udid` and proves it works by reading the screen size, which loads the
    /// private frameworks and finds the booted device.
    pub(crate) fn open(udid: &str) -> Result<Box<dyn HidBackend>, HidError> {
        let dev_dir = CString::new(developer_dir()?).map_err(|_| HidError::Unavailable("bad developer dir".into()))?;
        let udid = CString::new(udid).map_err(|_| HidError::Unavailable("bad simulator id".into()))?;
        let mut backend = ShimBackend { dev_dir, udid };
        backend.screen().map_err(|e| match e {
            // A failed first read is a backend that cannot serve, whatever the class was.
            HidError::Failed(m) | HidError::InputMaybeSent(m) => HidError::Unavailable(m),
            other => other,
        })?;
        Ok(Box::new(backend))
    }

    impl HidBackend for ShimBackend {
        fn screen(&mut self) -> Result<Screen, HidError> {
            let mut err = std::ptr::null_mut();
            // SAFETY: both strings are NUL-terminated and outlive the call; `err` is a valid out pointer.
            let json = unsafe { take_string(ap_screen(self.dev_dir.as_ptr(), self.udid.as_ptr(), &mut err)) };
            let Some(json) = json else { return Err(shim_error(err)) };
            let v: serde_json::Value =
                serde_json::from_str(&json).map_err(|e| HidError::Failed(format!("bad screen JSON: {e}")))?;
            let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).filter(|n| n.is_finite() && *n > 0.0);
            match (num("width"), num("height"), num("scale")) {
                (Some(width), Some(height), Some(scale)) => Ok(Screen { width, height, scale }),
                _ => Err(HidError::Failed(format!("unexpected screen JSON: {json}"))),
            }
        }

        fn snapshot(&mut self, req: &Snapshot) -> Result<String, HidError> {
            let opts = SnapshotOpts {
                pid: 0,
                has_point: 0,
                x: 0.0,
                y: 0.0,
                all_attrs: 0,
                springboard: 0,
                max_depth: c_int::try_from(req.max_depth).unwrap_or(0),
                enable_ax: 0,
            };
            let mut err = std::ptr::null_mut();
            // SAFETY: as in `screen`; `opts` outlives the call.
            let json = unsafe {
                take_string(ap_snapshot(self.dev_dir.as_ptr(), self.udid.as_ptr(), req.timeout_s, &opts, &mut err))
            };
            json.ok_or_else(|| shim_error(err))
        }

        fn touch(&mut self, path: &[(f64, f64)], hold_ms: u32, step_ms: u32) -> Result<(), HidError> {
            if path.is_empty() {
                return Err(HidError::Failed("a touch needs at least one point".into()));
            }
            let xs: Vec<f64> = path.iter().map(|p| p.0).collect();
            let ys: Vec<f64> = path.iter().map(|p| p.1).collect();
            let n = c_int::try_from(path.len()).map_err(|_| HidError::Failed("too many touch points".into()))?;
            let to_c = |ms: u32| c_int::try_from(ms).unwrap_or(c_int::MAX);
            let mut err = std::ptr::null_mut();
            // SAFETY: `xs` and `ys` hold `n` values and outlive the call.
            let rc = unsafe {
                ap_touch(
                    self.dev_dir.as_ptr(),
                    self.udid.as_ptr(),
                    xs.as_ptr(),
                    ys.as_ptr(),
                    n,
                    to_c(hold_ms),
                    to_c(step_ms),
                    &mut err,
                )
            };
            if rc == 0 { Ok(()) } else { Err(shim_error(err)) }
        }

        fn button(&mut self, button: Button) -> Result<(), HidError> {
            let code = match button {
                Button::Home => 0,
                Button::Lock => 1,
            };
            let mut err = std::ptr::null_mut();
            // SAFETY: as in `screen`.
            let rc = unsafe { ap_button(self.dev_dir.as_ptr(), self.udid.as_ptr(), code, &mut err) };
            if rc == 0 { Ok(()) } else { Err(shim_error(err)) }
        }

        fn frame_png(&mut self) -> Result<Vec<u8>, HidError> {
            let mut err = std::ptr::null_mut();
            let mut len = 0usize;
            // SAFETY: as in `screen`; `len` is a valid out pointer.
            let p = unsafe { ap_frame(self.dev_dir.as_ptr(), self.udid.as_ptr(), 0.0, &mut len, &mut err) };
            if p.is_null() {
                return Err(shim_error(err));
            }
            // SAFETY: the shim returned `len` bytes at `p`, malloc'd; they are copied and freed here.
            let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) }.to_vec();
            // SAFETY: `p` is not used after this.
            unsafe { ap_free(p) };
            Ok(bytes)
        }
    }
}
