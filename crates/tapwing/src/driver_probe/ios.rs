//! The iOS Simulator half of the probe: the installed `.app` is a directory on
//! the host (`simctl get_app_container <udid> <bundle> app`), so its Dart payload
//! is read in-process, never through a spawned `grep`.
//!
//! * A Simulator build (always debug) keeps the Dart code in
//!   `Frameworks/App.framework/flutter_assets/kernel_blob.bin`; `App.framework/App`
//!   is a stub next to it. An AOT build keeps it in `App.framework/App` itself.
//! * No `Frameworks/App.framework` and no `Frameworks/Flutter.framework`: not a
//!   Flutter app.
//! * The install fingerprint is a digest of the code-signature resource lists,
//!   which hold a hash of every file of the bundle. It depends on the bundle's
//!   content only, so it survives `clearState` (which reinstalls the same bundle
//!   into a new container path) and changes with any different build.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::cache::DriverCache;
use super::scan::{Scanned, scan_file};
use super::{Engine, Inspection, Payload, Report, classify, resolve};
use crate::process::run_with_timeout_or_cancel;

/// An AOT `App` binary is megabytes; the Simulator's stub next to a kernel blob is tens of KB.
const MIN_AOT_BYTES: u64 = 1024 * 1024;
/// Largest signature or plist file read whole for the fingerprint.
const MAX_SMALL_FILE: u64 = 4 * 1024 * 1024;
/// `simctl get_app_container` answers in ~100 ms; this bounds a wedged CoreSimulator.
const CONTAINER_TIMEOUT: Duration = Duration::from_secs(10);

const APP_FRAMEWORK: &str = "Frameworks/App.framework";
const FLUTTER_FRAMEWORK: &str = "Frameworks/Flutter.framework";

/// The installed `.app` of `bundle_id` on simulator `udid`.
pub(crate) fn app_bundle_dir(udid: &str, bundle_id: &str, deadline: Instant, cancel: &AtomicBool) -> Result<PathBuf> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        bail!("the probe deadline passed");
    }
    let mut cmd = Command::new("xcrun");
    cmd.args(["simctl", "get_app_container", udid, bundle_id, "app"]);
    let out = run_with_timeout_or_cancel(&mut cmd, left.min(CONTAINER_TIMEOUT), cancel)
        .context("simctl get_app_container")?;
    if !out.status.success() {
        bail!("simctl get_app_container: {}", out.stderr_lossy().trim());
    }
    let path = out.stdout_lossy().trim().to_string();
    if path.is_empty() {
        bail!("simctl get_app_container printed no path");
    }
    Ok(PathBuf::from(path))
}

fn read_small(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_SMALL_FILE + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_SMALL_FILE).then_some(bytes)
}

/// Identity of the installed build, or `None` when the bundle carries no
/// signature resources to derive one from (then nothing is cached about it).
pub(crate) fn fingerprint(app: &Path) -> Option<String> {
    let top = read_small(&app.join("_CodeSignature/CodeResources"))?;
    let mut parts: Vec<Vec<u8>> = vec![b"ios-bundle-1".to_vec(), top];
    for rel in ["Info.plist", "Frameworks/App.framework/_CodeSignature/CodeResources"] {
        // An absent file is part of the identity too, so a bundle that gained or lost it differs.
        parts.push(read_small(&app.join(rel)).unwrap_or_else(|| format!("no {rel}").into_bytes()));
    }
    for rel in ["Frameworks/App.framework/App", "Frameworks/App.framework/flutter_assets/kernel_blob.bin"] {
        let len = std::fs::metadata(app.join(rel)).map(|m| m.len()).ok();
        parts.push(format!("{rel}:{len:?}").into_bytes());
    }
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    Some(crate::ios_native_lane::sha256_hex(&refs)[..32].to_string())
}

/// Reads the Dart payloads of the bundle at `app`. See the module docs for what is judged where.
pub(crate) fn inspect_bundle(app: &Path, deadline: Instant, cancel: &AtomicBool) -> Inspection {
    let scope = app.file_name().map_or_else(|| "the app bundle".to_string(), |n| n.to_string_lossy().into_owned());
    let mut inspection = Inspection { scope, engine: Engine::NotFound, payloads: Vec::new(), gaps: Vec::new() };
    if !app.join("Info.plist").is_file() {
        inspection.gaps.push(format!("{} is not an app bundle (no Info.plist)", app.display()));
        return inspection;
    }
    let has_framework = |rel: &str| app.join(rel).is_dir();
    if !has_framework(APP_FRAMEWORK) && !has_framework(FLUTTER_FRAMEWORK) {
        return inspection;
    }
    inspection.engine = Engine::Found;

    let kernel = app.join(APP_FRAMEWORK).join("flutter_assets/kernel_blob.bin");
    let aot = app.join(APP_FRAMEWORK).join("App");
    for (path, label, counts_alone) in [(kernel, "kernel_blob.bin", true), (aot, "App", false)] {
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = format!("{APP_FRAMEWORK}/{}", if label == "App" { "App" } else { "flutter_assets/kernel_blob.bin" });
        match scan_file(&path, deadline, cancel) {
            Ok(Scanned { bytes, found }) => {
                // A small `App` next to a kernel blob is the Simulator's stub, not a payload:
                // it only counts when it holds a marker, or is large enough to be an AOT snapshot.
                if counts_alone || bytes >= MIN_AOT_BYTES || !found.is_empty() {
                    inspection.payloads.push(Payload { name, bytes, found });
                }
            }
            Err(e) => inspection.gaps.push(format!("{name}: {e:#}")),
        }
    }
    inspection
}

/// Judges the installed app `bundle_id` on simulator `udid`, answering from
/// `cache` when this exact install was judged before.
pub(crate) fn probe(
    udid: &str,
    bundle_id: &str,
    deadline: Instant,
    cancel: &AtomicBool,
    cache: Option<&DriverCache>,
) -> Report {
    if cancel.load(Ordering::Relaxed) {
        return Report::unknown("cancelled");
    }
    let app = match app_bundle_dir(udid, bundle_id, deadline, cancel) {
        Ok(p) => p,
        Err(e) => return Report::unknown(format!("{e:#}")),
    };
    probe_bundle(udid, bundle_id, &app, deadline, cancel, cache)
}

/// [`probe`] once the bundle directory is known.
pub(crate) fn probe_bundle(
    udid: &str,
    bundle_id: &str,
    app: &Path,
    deadline: Instant,
    cancel: &AtomicBool,
    cache: Option<&DriverCache>,
) -> Report {
    resolve(udid, bundle_id, fingerprint(app), cache, || classify(&inspect_bundle(app, deadline, cancel)))
}

#[cfg(test)]
mod tests;
