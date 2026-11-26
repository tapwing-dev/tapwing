use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use super::*;
use crate::device::adb_client::fake::{FakeAdb, ShDevice, sh_available};
use crate::driver_probe::{Presence, SkipReason, VerdictSource};

// -- the scripts' text and parsers, on output captured from emulator-5554 (toybox) and macOS (Info-ZIP) --

const TOYBOX_LISTING: &str = "TW_PM:0
TW_APK:/data/app/~~YnST3Xyt_1kcJU1PWsukxQ==/dev.tapwing.sample_app-Z7a53hL0RziS4NEQ4BGJxw==/base.apk
TW_STAT:32638531:1795731160
Archive:  /data/app/~~YnST3Xyt_1kcJU1PWsukxQ==/dev.tapwing.sample_app-Z7a53hL0RziS4NEQ4BGJxw==/base.apk
  Length      Date    Time    Name
---------  ---------- -----   ----
  1770456  1981-01-01 01:01   lib/arm64-v8a/libvmservice_snapshot.so
  7996376  1981-01-01 01:01   lib/arm64-v8a/libapp.so
 12140840  1981-01-01 01:01   lib/arm64-v8a/libflutter.so
---------                     -------
 21907672                     3 files
TW_UNZIP_RC:0
TW_END
";

fn out(stdout: &str) -> ShellOutput {
    ShellOutput { stdout: stdout.to_string(), stderr: String::new(), exit_code: 0 }
}

#[test]
fn the_listing_of_a_real_profile_apk_parses_into_engine_payload_and_fingerprint() {
    let listing = parse_listing(&out(TOYBOX_LISTING)).unwrap();
    assert_eq!(listing.apks.len(), 1);
    let apk = &listing.apks[0];
    assert_eq!((apk.size, apk.mtime, apk.problem.clone()), (Some(32_638_531), Some(1_795_731_160), None));
    assert_eq!(apk.entries.len(), 3);
    assert!(listing.fingerprint().is_some_and(|f| f.len() == 32));

    let plan = plan_scan(&listing);
    assert_eq!(plan.engine, Engine::Found);
    assert!(plan.gaps.is_empty(), "{:?}", plan.gaps);
    assert_eq!(plan.targets.len(), 1);
    assert_eq!(plan.targets[0].name, "base.apk: lib/arm64-v8a/libapp.so");
    assert_eq!(plan.targets[0].bytes, 7_996_376);
}

#[test]
fn info_zip_listings_with_us_dates_parse_the_same() {
    let text = TOYBOX_LISTING.replace("1981-01-01", "01-01-1981");
    let listing = parse_listing(&out(&text)).unwrap();
    assert_eq!(listing.apks[0].entries.len(), 3);
    assert_eq!(listing.apks[0].problem, None);
}

#[test]
fn a_listing_that_cannot_be_trusted_is_a_gap_not_a_verdict() {
    let apk_problem = |text: &str| parse_listing(&out(text)).unwrap().apks[0].problem.clone().unwrap();
    assert!(apk_problem(&TOYBOX_LISTING.replace("TW_UNZIP_RC:0", "TW_UNZIP_RC:1")).contains("exit 1"));
    assert!(apk_problem(&TOYBOX_LISTING.replace(" files", " files extra")).contains("no summary"));
    assert!(apk_problem(&TOYBOX_LISTING.replace("3 files", "4 files")).contains("promised 4"));
    assert!(apk_problem(&TOYBOX_LISTING.replace(" 21907672                     3 files\n", "")).contains("no summary"));
}

#[test]
fn a_script_that_did_not_finish_or_a_missing_app_is_an_error_not_an_empty_listing() {
    assert!(parse_listing(&out(&TOYBOX_LISTING.replace("TW_END\n", ""))).unwrap_err().contains("did not run"));
    assert!(parse_listing(&out("TW_PM:1\nTW_END\n")).unwrap_err().contains("not installed"));
    assert!(parse_listing(&out("TW_PM:0\nTW_END\n")).unwrap_err().contains("no APK"));
    assert!(parse_listing(&out("")).unwrap_err().contains("did not run"));
}

#[test]
fn a_stat_that_failed_leaves_the_install_without_a_fingerprint() {
    let text = TOYBOX_LISTING.replace("TW_STAT:32638531:1795731160", "TW_STAT:?");
    let listing = parse_listing(&out(&text)).unwrap();
    assert_eq!(listing.fingerprint(), None, "an identity built from less than size and mtime could repeat");
}

#[test]
fn plan_scan_table() {
    fn apk(name: &str, entries: &[(&str, u64)]) -> ApkListing {
        ApkListing {
            path: format!("/data/app/x/{name}"),
            size: Some(1),
            mtime: Some(1),
            entries: entries.iter().map(|(n, l)| (n.to_string(), *l)).collect(),
            promised: Some(entries.len()),
            problem: None,
        }
    }
    struct Case {
        name: &'static str,
        listing: Listing,
        engine: Engine,
        targets: Vec<&'static str>,
        gaps: usize,
    }
    let flutter = [("lib/arm64-v8a/libflutter.so", 9u64), ("lib/arm64-v8a/libapp.so", 5)];
    let cases = [
        Case {
            name: "profile apk",
            listing: Listing { apks: vec![apk("base.apk", &flutter)], libs: vec![] },
            engine: Engine::Found,
            targets: vec!["base.apk: lib/arm64-v8a/libapp.so"],
            gaps: 0,
        },
        Case {
            name: "debug apk reads the kernel blob",
            listing: Listing {
                apks: vec![apk(
                    "base.apk",
                    &[("lib/arm64-v8a/libflutter.so", 9), ("assets/flutter_assets/kernel_blob.bin", 50)],
                )],
                libs: vec![],
            },
            engine: Engine::Found,
            targets: vec!["base.apk: assets/flutter_assets/kernel_blob.bin"],
            gaps: 0,
        },
        Case {
            name: "app bundle splits: engine and payload live in the config split",
            listing: Listing {
                apks: vec![apk("base.apk", &[]), apk("split_config.arm64_v8a.apk", &flutter)],
                libs: vec![],
            },
            engine: Engine::Found,
            targets: vec!["split_config.arm64_v8a.apk: lib/arm64-v8a/libapp.so"],
            gaps: 0,
        },
        Case {
            name: "a universal apk lists every abi's payload",
            listing: Listing {
                apks: vec![apk(
                    "base.apk",
                    &[("lib/arm64-v8a/libflutter.so", 9), ("lib/arm64-v8a/libapp.so", 5), ("lib/x86_64/libapp.so", 5)],
                )],
                libs: vec![],
            },
            engine: Engine::Found,
            targets: vec!["base.apk: lib/arm64-v8a/libapp.so", "base.apk: lib/x86_64/libapp.so"],
            gaps: 0,
        },
        Case {
            name: "no engine anywhere",
            listing: Listing {
                apks: vec![apk("base.apk", &[("lib/arm64-v8a/libreactnativejni.so", 3)])],
                libs: vec![],
            },
            engine: Engine::NotFound,
            targets: vec![],
            gaps: 0,
        },
        Case {
            name: "libraries extracted next to the apk count",
            listing: Listing {
                apks: vec![apk("base.apk", &[])],
                libs: vec![
                    DiskLib { path: "/data/app/x/lib/arm64/libflutter.so".into(), bytes: 9 },
                    DiskLib { path: "/data/app/x/lib/arm64/libapp.so".into(), bytes: 5 },
                ],
            },
            engine: Engine::Found,
            targets: vec!["/data/app/x/lib/arm64/libapp.so"],
            gaps: 0,
        },
        Case {
            name: "a deferred component of the Dart code is not inspected",
            listing: Listing {
                apks: vec![apk("base.apk", &[flutter[0], flutter[1], ("lib/arm64-v8a/app.so-2.part.so", 4)])],
                libs: vec![],
            },
            engine: Engine::Found,
            targets: vec!["base.apk: lib/arm64-v8a/libapp.so"],
            gaps: 1,
        },
        Case {
            name: "an empty payload",
            listing: Listing {
                apks: vec![apk("base.apk", &[flutter[0], ("lib/arm64-v8a/libapp.so", 0)])],
                libs: vec![],
            },
            engine: Engine::Found,
            targets: vec![],
            gaps: 1,
        },
        Case {
            name: "more payload than the scan will read",
            listing: Listing {
                apks: vec![apk("base.apk", &[flutter[0], ("lib/arm64-v8a/libapp.so", MAX_SCAN_BYTES + 1)])],
                libs: vec![],
            },
            engine: Engine::Found,
            targets: vec![],
            gaps: 1,
        },
        Case {
            name: "an apk whose listing failed is a gap, not an apk without libraries",
            listing: Listing {
                apks: vec![ApkListing { problem: Some("`unzip -l` failed (exit 2)".into()), ..apk("split.apk", &[]) }],
                libs: vec![],
            },
            engine: Engine::NotFound,
            targets: vec![],
            gaps: 1,
        },
    ];
    for c in cases {
        let plan = plan_scan(&c.listing);
        assert_eq!(plan.engine, c.engine, "{}", c.name);
        assert_eq!(plan.targets.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), c.targets, "{}", c.name);
        assert_eq!(plan.gaps.len(), c.gaps, "{}: {:?}", c.name, plan.gaps);
    }
}

#[test]
fn scan_outcomes_become_payloads_only_when_the_read_is_provably_complete() {
    let plan = || ScanPlan {
        engine: Engine::Found,
        targets: vec![Target {
            name: "base.apk: lib/arm64-v8a/libapp.so".into(),
            bytes: 100,
            source: Source::Entry { apk: "/a/base.apk".into(), entry: "lib/arm64-v8a/libapp.so".into() },
        }],
        gaps: vec![],
    };
    let judge = |stdout: &str| {
        let inspection = inspection_from_scan(plan(), "base.apk".into(), parse_scan(stdout, 1));
        crate::driver_probe::classify(&inspection)
    };
    let clean = "TW_SCAN:0\nTW_BYTES:100\nTW_GREP_RC:1\nTW_END\n";
    assert_eq!(judge(clean).presence, Presence::Absent);
    let hit = "TW_SCAN:0\nTW_BYTES:100\ntapwing_port\npackage:tapwing_driver/\nTW_GREP_RC:0\nTW_END\n";
    let v = judge(hit);
    assert_eq!((v.presence, v.dials), (Presence::Present, true));

    // Anything short of a complete read keeps "absent" out of reach.
    for (why, text, expect) in [
        ("fewer bytes than listed", "TW_SCAN:0\nTW_BYTES:40\nTW_GREP_RC:1\nTW_END\n", "read 40 of 100"),
        ("no byte count", "TW_SCAN:0\nTW_GREP_RC:1\nTW_END\n", "no scan result"),
        ("grep failed", "TW_SCAN:0\nTW_BYTES:100\nTW_GREP_RC:2\nTW_END\n", "exit 2"),
        (
            "grep claims a match we do not know",
            "TW_SCAN:0\nTW_BYTES:100\nsomething\nTW_GREP_RC:0\nTW_END\n",
            "not a marker",
        ),
        ("the script stopped", "TW_SCAN:0\nTW_BYTES:100\nTW_GREP_RC:1\n", "did not run"),
        ("nothing printed", "", "did not run"),
    ] {
        let v = judge(text);
        assert_eq!(v.presence, Presence::Unknown, "{why}: {v:?}");
        assert!(v.evidence.contains(expect), "{why}: {}", v.evidence);
    }
}

#[test]
fn the_scripts_parse_under_sh_and_quote_what_they_embed() {
    let script = listing_script("dev.example.app");
    assert!(script.contains("pm path 'dev.example.app'"));
    let target = Target {
        name: "n".into(),
        bytes: 1,
        source: Source::Entry { apk: "/data/app/it's/base.apk".into(), entry: "lib/a/libapp.so".into() },
    };
    let scan = scan_script(&[target]);
    assert!(scan.contains(r"'/data/app/it'\''s/base.apk'"), "{scan}");
    for m in MARKERS {
        assert!(scan.contains(&format!("-e '{}'", m.text)), "{} missing from the grep", m.text);
    }
    if sh_available() {
        assert_eq!(crate::device::adb_client::fake::sh_syntax_error(&[script, scan]), None);
    }
}

// -- end to end: the real scripts, against the fake adb server, over real zip files on this host --

fn zip_with(entries: &[(&str, &[u8], CompressionMethod)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes, method) in entries {
        w.start_file(*name, SimpleFileOptions::default().compression_method(*method)).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap().into_inner()
}

const STORED: CompressionMethod = CompressionMethod::Stored;
const DEFLATED: CompressionMethod = CompressionMethod::Deflated;

fn payload(extra: &[u8]) -> Vec<u8> {
    let mut v = b"\x7fELF-like Dart AOT snapshot: package:flutter/material.dart Tapwing Sample ".repeat(50);
    v.extend_from_slice(extra);
    v
}

fn unzip_works() -> bool {
    sh_available() && std::process::Command::new("unzip").arg("-v").output().is_ok_and(|o| o.status.success())
}

/// An emulator-shaped device: `pm` and `stat` are stubs (macOS `stat` has other flags), while
/// `unzip`, `grep` and `wc` are the host's.
struct Phone {
    device: Arc<ShDevice>,
    fake: FakeAdb,
}

impl Phone {
    fn new(extra_stubs: &str) -> Self {
        let stubs = format!(
            r#"pm() {{ cat "$TAPWING_FAKE_DIR/pm" 2>/dev/null; [ -s "$TAPWING_FAKE_DIR/pm" ]; }}
stat() {{ printf 'TW_STAT:%s:%s\n' "$(wc -c < "$3" | tr -d ' ')" "$(cat "$TAPWING_FAKE_DIR/mtime" 2>/dev/null || echo 1700000000)"; }}
{extra_stubs}"#
        );
        let device = ShDevice::new(&stubs);
        let fake = FakeAdb::with_device(&device);
        Self { device, fake }
    }

    fn install(&self, files: &[(&str, Vec<u8>)]) -> &Self {
        let mut pm = String::new();
        for (name, bytes) in files {
            let path = self.device.dir().join("data/app").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            if name.ends_with(".apk") {
                pm.push_str(&format!("package:{}\n", path.display()));
            }
        }
        std::fs::write(self.device.dir().join("pm"), pm).unwrap();
        self
    }

    fn touch(&self, mtime: u64) {
        std::fs::write(self.device.dir().join("mtime"), mtime.to_string()).unwrap();
    }

    fn probe(&self, cache: Option<&DriverCache>) -> Report {
        self.probe_by(Instant::now() + Duration::from_secs(30), &AtomicBool::new(false), cache)
    }

    fn probe_by(&self, deadline: Instant, cancel: &AtomicBool, cache: Option<&DriverCache>) -> Report {
        probe(&self.fake.client(), "emulator-5554", "dev.example.app", deadline, cancel, cache)
    }
}

fn flutter_apk(libapp: &[u8], method: CompressionMethod) -> Vec<u8> {
    zip_with(&[
        ("AndroidManifest.xml", b"<manifest/>", DEFLATED),
        ("lib/arm64-v8a/libflutter.so", &[7u8; 2000], method),
        ("lib/arm64-v8a/libapp.so", libapp, method),
    ])
}

#[test]
fn a_profile_build_with_the_driver_is_present_and_scanned_on_the_device() {
    if !unzip_works() {
        return;
    }
    let phone = Phone::new("");
    phone.install(&[("base.apk", flutter_apk(&payload(b"..tapwing_port..TAPWING_PORT.."), STORED))]);
    let report = phone.probe(None);
    assert_eq!((report.verdict.presence, report.verdict.dials), (Presence::Present, true), "{:?}", report.verdict);
    assert_eq!(report.source, VerdictSource::Scan);
    assert_eq!(report.skip_reason(), None);
    assert_eq!(phone.fake.shells().len(), 2, "one listing, one scan");
    phone.fake.assert_scripts_parse();
}

#[test]
fn a_plain_profile_or_release_build_is_absent_and_says_what_was_read() {
    if !unzip_works() {
        return;
    }
    let phone = Phone::new("");
    phone.install(&[("base.apk", flutter_apk(&payload(b""), STORED))]);
    let report = phone.probe(None);
    assert_eq!(report.verdict.presence, Presence::Absent, "{:?}", report.verdict);
    assert!(report.verdict.evidence.contains("base.apk: lib/arm64-v8a/libapp.so"), "{}", report.verdict.evidence);
    assert!(report.verdict.evidence.contains("completely"), "{}", report.verdict.evidence);
    assert_eq!(report.skip_reason(), Some(SkipReason::NoDriver { cached: false }));
}

#[test]
fn a_debug_build_keeps_its_kernel_blob_deflated_and_is_still_read() {
    if !unzip_works() {
        return;
    }
    let debug = |kernel: &[u8]| {
        zip_with(&[
            ("lib/arm64-v8a/libflutter.so", &[7u8; 2000], STORED),
            ("assets/flutter_assets/kernel_blob.bin", &payload(kernel), DEFLATED),
            ("assets/flutter_assets/isolate_snapshot_data", &[1u8; 500], DEFLATED),
        ])
    };
    let phone = Phone::new("");
    phone.install(&[("base.apk", debug(b""))]);
    let plain = phone.probe(None);
    assert_eq!(plain.verdict.presence, Presence::Absent, "{:?}", plain.verdict);
    assert!(plain.verdict.evidence.contains("kernel_blob.bin"));

    let phone = Phone::new("");
    phone.install(&[("base.apk", debug(b"package:tapwing_driver/src/binding.dart"))]);
    let driver = phone.probe(None);
    assert_eq!(driver.verdict.presence, Presence::Present, "{:?}", driver.verdict);
}

#[test]
fn an_app_that_is_not_flutter_is_absent_without_reading_anything() {
    if !unzip_works() {
        return;
    }
    let phone = Phone::new("");
    phone.install(&[(
        "base.apk",
        zip_with(&[("classes.dex", &[1u8; 5000], DEFLATED), ("lib/arm64-v8a/libhermes.so", &[2u8; 100], STORED)]),
    )]);
    let report = phone.probe(None);
    assert_eq!(report.verdict.presence, Presence::Absent, "{:?}", report.verdict);
    assert!(report.verdict.evidence.contains("not a Flutter app"));
    assert_eq!(phone.fake.shells().len(), 1, "no payload, no scan");
}

#[test]
fn every_split_is_inspected_and_an_unreadable_one_keeps_the_verdict_unknown() {
    if !unzip_works() {
        return;
    }
    let base = zip_with(&[("AndroidManifest.xml", b"<manifest/>", DEFLATED)]);
    let config = |extra: &[u8]| flutter_apk(&payload(extra), STORED);

    let phone = Phone::new("");
    phone.install(&[("base.apk", base.clone()), ("split_config.arm64_v8a.apk", config(b""))]);
    let report = phone.probe(None);
    assert_eq!(report.verdict.presence, Presence::Absent, "{:?}", report.verdict);
    assert!(report.verdict.evidence.contains("split_config.arm64_v8a.apk"), "{}", report.verdict.evidence);

    let phone = Phone::new("");
    phone.install(&[("base.apk", base.clone()), ("split_config.arm64_v8a.apk", config(b"TAPWING_PORT"))]);
    assert_eq!(phone.probe(None).verdict.presence, Presence::Present, "the driver lives in the config split");

    // The base alone says "no engine", but a split that cannot be listed may hold it.
    let phone = Phone::new("");
    phone.install(&[("base.apk", base), ("split_config.arm64_v8a.apk", b"this is not a zip file".to_vec())]);
    let report = phone.probe(None);
    assert_eq!(report.verdict.presence, Presence::Unknown, "{:?}", report.verdict);
    assert!(report.verdict.evidence.contains("split_config.arm64_v8a.apk"), "{}", report.verdict.evidence);
}

#[test]
fn libraries_extracted_next_to_the_apk_are_read_in_place() {
    if !unzip_works() {
        return;
    }
    let bare = zip_with(&[("AndroidManifest.xml", b"<manifest/>", DEFLATED)]);
    for (extra, expect) in [(&b""[..], Presence::Absent), (&b"tapwing_port"[..], Presence::Present)] {
        let phone = Phone::new("");
        phone.install(&[
            ("app/base.apk", bare.clone()),
            ("app/lib/arm64/libflutter.so", vec![7; 100]),
            ("app/lib/arm64/libapp.so", payload(extra)),
        ]);
        let report = phone.probe(None);
        assert_eq!(report.verdict.presence, expect, "{:?}", report.verdict);
    }
}

#[test]
fn a_device_that_cannot_run_the_tools_or_an_app_that_is_not_installed_is_unknown() {
    if !unzip_works() {
        return;
    }
    let apk = flutter_apk(&payload(b""), STORED);
    // No `unzip` on the device.
    let phone = Phone::new("unzip() { return 127; }");
    phone.install(&[("base.apk", apk.clone())]);
    let v = phone.probe(None).verdict;
    assert_eq!(v.presence, Presence::Unknown, "{v:?}");
    assert!(v.evidence.contains("exit 127"), "{}", v.evidence);

    // `grep` that fails (an old toolbox without -o).
    let phone = Phone::new("grep() { return 2; }");
    phone.install(&[("base.apk", apk.clone())]);
    let v = phone.probe(None).verdict;
    assert_eq!(v.presence, Presence::Unknown, "{v:?}");
    assert!(v.evidence.contains("grep"), "{}", v.evidence);

    // A stream that ends early: the byte count disagrees with the listing.
    let phone = Phone::new(
        r#"unzip() { if [ "$1" = -p ]; then command unzip "$@" | head -c 10; else command unzip "$@"; fi; }"#,
    );
    phone.install(&[("base.apk", apk.clone())]);
    let v = phone.probe(None).verdict;
    assert_eq!(v.presence, Presence::Unknown, "{v:?}");
    assert!(v.evidence.contains("of"), "{}", v.evidence);

    // Not installed: `pm path` prints nothing and fails.
    let phone = Phone::new("");
    phone.install(&[]);
    let v = phone.probe(None).verdict;
    assert_eq!(v.presence, Presence::Unknown, "{v:?}");
    assert!(v.evidence.contains("pm path"), "{}", v.evidence);

    // A truncated download of an APK.
    let phone = Phone::new("");
    phone.install(&[("base.apk", apk[..apk.len() / 2].to_vec())]);
    assert_eq!(phone.probe(None).verdict.presence, Presence::Unknown);
}

#[test]
fn a_probe_past_its_deadline_or_cancelled_never_reaches_the_device() {
    let phone = Phone::new("");
    phone.install(&[("base.apk", flutter_apk(&payload(b""), STORED))]);
    let past = Instant::now() - Duration::from_millis(1);
    let v = phone.probe_by(past, &AtomicBool::new(false), None).verdict;
    assert_eq!(v.presence, Presence::Unknown);
    assert!(v.evidence.contains("deadline"), "{}", v.evidence);
    let v = phone.probe_by(Instant::now() + Duration::from_secs(30), &AtomicBool::new(true), None).verdict;
    assert!(v.evidence.contains("cancelled"), "{}", v.evidence);
    assert!(phone.fake.shells().is_empty());
}

#[test]
fn an_invalid_package_id_is_refused_before_it_can_reach_a_shell() {
    let phone = Phone::new("");
    let report = probe(
        &phone.fake.client(),
        "emulator-5554",
        "a b;reboot",
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
        None,
    );
    assert_eq!(report.verdict.presence, Presence::Unknown);
    assert!(phone.fake.shells().is_empty());
}

#[test]
fn the_cache_answers_for_the_same_install_and_a_reinstall_is_read_again() {
    if !unzip_works() {
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("tapwing-android-probe-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = DriverCache::at(dir.join("driver-probe.json"));
    let phone = Phone::new("");
    phone.install(&[("base.apk", flutter_apk(&payload(b""), STORED))]);
    phone.touch(1_700_000_000);

    let first = phone.probe(Some(&cache));
    assert_eq!((first.verdict.presence, first.source), (Presence::Absent, VerdictSource::Scan));
    assert_eq!(phone.fake.shells().len(), 2);

    let second = phone.probe(Some(&cache));
    assert_eq!((second.verdict.presence, second.source), (Presence::Absent, VerdictSource::Cache));
    assert_eq!(phone.fake.shells().len(), 3, "only the listing ran: the fingerprint is the cache key");
    assert_eq!(second.skip_reason(), Some(SkipReason::NoDriver { cached: true }));

    // `adb install -r` of the driver build: other size and mtime, so another key.
    phone.install(&[("base.apk", flutter_apk(&payload(b"tapwing_port"), STORED))]);
    phone.touch(1_700_000_100);
    let third = phone.probe(Some(&cache));
    assert_eq!((third.verdict.presence, third.source), (Presence::Present, VerdictSource::Scan));
    assert_ne!(third.fingerprint, first.fingerprint);

    // Misses recorded against the install are seen by a later probe, and act only on an unreadable build.
    let key = crate::driver_probe::cache::InstallKey {
        device: "emulator-5554".into(),
        app: "dev.example.app".into(),
        fingerprint: third.fingerprint.clone().unwrap(),
    };
    cache.record_miss(&key);
    cache.record_miss(&key);
    let fourth = phone.probe(Some(&cache));
    assert_eq!(fourth.learned_misses, Some(2));
    assert_eq!(fourth.skip_reason(), None, "a build the probe proved to dial is never second-guessed by misses");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn learned_misses_skip_the_wait_for_a_build_the_probe_could_not_read() {
    if !unzip_works() {
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("tapwing-android-probe-learned-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = DriverCache::at(dir.join("driver-probe.json"));
    // `grep` is broken on this device, so the payload can never be read: unknown every time.
    let phone = Phone::new("grep() { return 2; }");
    phone.install(&[("base.apk", flutter_apk(&payload(b""), STORED))]);
    let first = phone.probe(Some(&cache));
    assert_eq!((first.verdict.presence, first.skip_reason()), (Presence::Unknown, None));
    let key = crate::driver_probe::cache::InstallKey {
        device: "emulator-5554".into(),
        app: "dev.example.app".into(),
        fingerprint: first.fingerprint.clone().unwrap(),
    };
    assert_eq!(cache.record_miss(&key), Some(1));
    assert_eq!(phone.probe(Some(&cache)).skip_reason(), None, "one miss is not enough");
    assert_eq!(cache.record_miss(&key), Some(2));
    assert_eq!(phone.probe(Some(&cache)).skip_reason(), Some(SkipReason::LearnedMisses { misses: 2 }));
    // A handshake clears what was learned.
    cache.clear_after_handshake("emulator-5554", "dev.example.app");
    assert_eq!(phone.probe(Some(&cache)).skip_reason(), None);
    let _ = std::fs::remove_dir_all(dir);
}
