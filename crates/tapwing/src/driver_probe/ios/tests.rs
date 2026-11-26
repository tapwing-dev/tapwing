use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::*;
use crate::driver_probe::{Presence, VerdictSource};

fn far() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

/// A scratch `.app` directory, removed on drop.
struct Bundle(PathBuf);

impl Bundle {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tapwing-ios-probe-{}-{tag}.app", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> &Self {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        self
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// A Simulator Flutter build: stub `App`, kernel blob with `kernel`, signed.
    fn flutter_debug(tag: &str, kernel: &[u8]) -> Self {
        let b = Self::new(tag);
        b.write("Info.plist", b"plist")
            .write("_CodeSignature/CodeResources", format!("top {tag}").as_bytes())
            .write("Frameworks/Flutter.framework/Flutter", &[0u8; 64])
            .write("Frameworks/App.framework/App", &[0u8; 4096])
            .write("Frameworks/App.framework/_CodeSignature/CodeResources", format!("app {tag}").as_bytes())
            .write("Frameworks/App.framework/flutter_assets/kernel_blob.bin", kernel);
        b
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn judge(b: &Bundle) -> crate::driver_probe::Verdict {
    classify(&inspect_bundle(b.path(), far(), &AtomicBool::new(false)))
}

#[test]
fn a_simulator_build_with_the_driver_is_present_and_dials() {
    let b =
        Bundle::flutter_debug("drv", b"..package:tapwing_driver/src/x.dart..tapwing_port..tapwing_driver_hello_v1..");
    let v = judge(&b);
    assert_eq!((v.presence, v.dials), (Presence::Present, true), "{v:?}");
    assert!(v.evidence.contains("kernel_blob.bin"), "{}", v.evidence);
}

#[test]
fn a_plain_simulator_build_is_absent_with_the_kernel_blob_read_completely() {
    let b = Bundle::flutter_debug("plain", b"package:flutter/material.dart Tapwing Sample dev.tapwing.sample_app");
    let v = judge(&b);
    assert_eq!(v.presence, Presence::Absent, "{v:?}");
    assert!(v.evidence.contains("kernel_blob.bin") && v.evidence.contains("completely"), "{}", v.evidence);
}

#[test]
fn an_aot_app_binary_is_the_payload_when_there_is_no_kernel_blob() {
    let b = Bundle::new("aot");
    let mut aot = vec![b'.'; MIN_AOT_BYTES as usize + 10];
    b.write("Info.plist", b"p").write("Frameworks/Flutter.framework/Flutter", b"e");
    b.write("Frameworks/App.framework/App", &aot);
    assert_eq!(judge(&b).presence, Presence::Absent);
    aot.extend_from_slice(b"TAPWING_PORT");
    b.write("Frameworks/App.framework/App", &aot);
    let v = judge(&b);
    assert_eq!((v.presence, v.dials), (Presence::Present, true));
}

#[test]
fn a_flutter_bundle_with_only_a_stub_app_binary_and_no_kernel_blob_is_unknown() {
    let b = Bundle::new("stub");
    b.write("Info.plist", b"p")
        .write("Frameworks/Flutter.framework/Flutter", b"e")
        .write("Frameworks/App.framework/App", &[0u8; 4096]);
    let v = judge(&b);
    assert_eq!(v.presence, Presence::Unknown, "{v:?}");
    assert!(v.evidence.contains("no Dart payload"), "{}", v.evidence);
}

#[test]
fn a_bundle_without_flutter_frameworks_is_not_a_flutter_app() {
    let b = Bundle::new("native");
    b.write("Info.plist", b"p").write("Preferences", b"native executable");
    let v = judge(&b);
    assert_eq!(v.presence, Presence::Absent, "{v:?}");
    assert!(v.evidence.contains("not a Flutter app"), "{}", v.evidence);
}

#[test]
fn a_directory_that_is_not_an_app_bundle_is_unknown() {
    let b = Bundle::new("notapp");
    b.write("something", b"x");
    assert_eq!(judge(&b).presence, Presence::Unknown);
    let missing = std::env::temp_dir().join("tapwing-no-such-bundle.app");
    let v = classify(&inspect_bundle(&missing, far(), &AtomicBool::new(false)));
    assert_eq!(v.presence, Presence::Unknown);
}

#[test]
fn a_payload_that_cannot_be_read_in_time_is_unknown_not_absent() {
    let b = Bundle::flutter_debug("slow", b"plain kernel");
    let past = Instant::now() - Duration::from_millis(1);
    let v = classify(&inspect_bundle(b.path(), past, &AtomicBool::new(false)));
    assert_eq!(v.presence, Presence::Unknown, "{v:?}");
    assert!(v.evidence.contains("deadline"), "{}", v.evidence);
    let v = classify(&inspect_bundle(b.path(), far(), &AtomicBool::new(true)));
    assert_eq!(v.presence, Presence::Unknown);
    assert!(v.evidence.contains("cancelled"), "{}", v.evidence);
}

#[cfg(unix)]
#[test]
fn an_unreadable_payload_is_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let b = Bundle::flutter_debug("denied", b"plain kernel");
    let blob = b.path().join("Frameworks/App.framework/flutter_assets/kernel_blob.bin");
    std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A superuser reads it anyway; the rest of this test only holds for everyone else.
    if std::fs::File::open(&blob).is_err() {
        let v = judge(&b);
        assert_eq!(v.presence, Presence::Unknown, "{v:?}");
    }
    std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn the_fingerprint_follows_the_bundle_content_not_its_location() {
    let a = Bundle::flutter_debug("fp", b"kernel one");
    let copy = Bundle::new("fp-copy");
    // The same bundle reinstalled into another container path.
    for rel in [
        "Info.plist",
        "_CodeSignature/CodeResources",
        "Frameworks/Flutter.framework/Flutter",
        "Frameworks/App.framework/App",
        "Frameworks/App.framework/_CodeSignature/CodeResources",
        "Frameworks/App.framework/flutter_assets/kernel_blob.bin",
    ] {
        copy.write(rel, &std::fs::read(a.path().join(rel)).unwrap());
    }
    let fp = fingerprint(a.path()).expect("a signed bundle has a fingerprint");
    assert_eq!(fingerprint(copy.path()), Some(fp.clone()), "same content, other path");
    assert_eq!(fp.len(), 32);

    // A different build: new signature resources, or a payload of another size.
    copy.write("Frameworks/App.framework/_CodeSignature/CodeResources", b"app changed");
    assert_ne!(fingerprint(copy.path()), Some(fp.clone()));
    let grown = Bundle::flutter_debug("fp", b"kernel one, grown");
    assert_ne!(fingerprint(grown.path()), Some(fp), "same signatures, different payload size");

    // No signature resources, no identity: nothing is cached about it.
    let unsigned = Bundle::new("fp-unsigned");
    unsigned.write("Info.plist", b"p");
    assert_eq!(fingerprint(unsigned.path()), None);
}

#[test]
fn a_cached_verdict_answers_without_reading_the_payload_and_a_new_install_is_read_again() {
    let dir = std::env::temp_dir().join(format!("tapwing-ios-probe-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = DriverCache::at(dir.join("driver-probe.json"));
    let plain = Bundle::flutter_debug("cached", b"plain kernel");

    let first = probe_bundle("UDID-1", "dev.example.app", plain.path(), far(), &AtomicBool::new(false), Some(&cache));
    assert_eq!((first.verdict.presence, first.source), (Presence::Absent, VerdictSource::Scan));

    // Same content under another path (clearState reinstall): answered from the cache, even
    // when a read could no longer succeed.
    let reinstalled = Bundle::flutter_debug("cached", b"plain kernel");
    let past = Instant::now() - Duration::from_millis(1);
    let second =
        probe_bundle("UDID-1", "dev.example.app", reinstalled.path(), past, &AtomicBool::new(false), Some(&cache));
    assert_eq!((second.verdict.presence, second.source), (Presence::Absent, VerdictSource::Cache));
    assert_eq!(second.fingerprint, first.fingerprint);

    // The driver build installed over it is another install: read again, and now present.
    let driver = Bundle::flutter_debug("cached-driver", b"plain kernel plus tapwing_port");
    let third = probe_bundle("UDID-1", "dev.example.app", driver.path(), far(), &AtomicBool::new(false), Some(&cache));
    assert_eq!((third.verdict.presence, third.source), (Presence::Present, VerdictSource::Scan));
    assert_ne!(third.fingerprint, first.fingerprint);
    let _ = std::fs::remove_dir_all(dir);
}
