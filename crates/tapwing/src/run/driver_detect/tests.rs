use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use super::*;
use crate::device::adb_client::fake::{FakeAdb, ShellReply};
use crate::device::{Android, Device, IosSim, LaunchMetrics, SimInfo};
use crate::driver::Driver;
use crate::driver_probe::cache::Known;
use crate::driver_probe::{Verdict, VerdictSource};
use crate::run::hybrid::tests::{serve_static_native, test_opts};
use crate::run::{IosNativeSlot, Opts, Session};

pub(super) const APP: &str = "dev.example.app";
pub(super) const LAUNCHER: &str = r#"<hierarchy rotation="0">
  <node text="Phone" package="com.android.launcher3" enabled="true" bounds="[0,0][1080,2400]"/>
</hierarchy>"#;

fn report(presence: Presence, dials: bool, fingerprint: Option<&str>) -> Report {
    Report {
        verdict: Verdict {
            presence,
            dials,
            evidence: "read base.apk: lib/arm64-v8a/libapp.so (7.6 MB) completely: no tapwing_driver marker".into(),
        },
        source: VerdictSource::Scan,
        fingerprint: fingerprint.map(str::to_string),
        learned_misses: None,
    }
}

pub(super) fn finished(report: Report) -> LaunchProbe {
    let probe = LaunchProbe::start_with(None, move |_| report).unwrap();
    wait_for(&probe);
    probe
}

pub(super) fn wait_for(probe: &LaunchProbe) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while probe.report().is_none() {
        assert!(Instant::now() < deadline, "the probe never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub(super) fn metrics() -> LaunchMetrics {
    LaunchMetrics {
        pid: Some(123),
        old_pids: vec![],
        granted_permissions: 0,
        unsupported_permissions: vec![],
        clear_state_fallback: None,
        stop_clear_ms: 0,
        permissions_ms: 0,
        start_ms: 1,
    }
}

pub(super) fn temp_cache(tag: &str) -> (DriverCache, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tapwing-driver-detect-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (DriverCache::at(dir.join("driver-probe.json")), dir)
}

pub(super) fn android() -> Device {
    Device::Android(Android::new("unreachable".into()))
}

#[test]
fn knowledge_follows_the_probe_from_pending_to_a_decision() {
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let pending = LaunchProbe::start_with(None, move |_| {
        let _ = gate.recv_timeout(Duration::from_secs(5));
        report(Presence::Absent, false, None)
    })
    .unwrap();
    assert_eq!(pending.knowledge(), DriverKnowledge::Pending);
    assert_eq!(pending.skip_reason(), None);
    release.send(()).unwrap();
    wait_for(&pending);
    assert_eq!(pending.knowledge(), DriverKnowledge::Skip(SkipReason::NoDriver { cached: false }));

    // Only a marker of the dial path proves the build will say hello.
    assert_eq!(finished(report(Presence::Present, true, None)).knowledge(), DriverKnowledge::Proven);
    assert_eq!(finished(report(Presence::Present, false, None)).knowledge(), DriverKnowledge::Wait);
    assert_eq!(finished(report(Presence::Unknown, false, None)).knowledge(), DriverKnowledge::Wait);
}

#[test]
fn dropping_the_probe_cancels_what_is_left_of_it() {
    let (seen, saw) = std::sync::mpsc::channel::<bool>();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let probe = LaunchProbe::start_with(None, move |cancel| {
        let _ = gate.recv_timeout(Duration::from_secs(5));
        let _ = seen.send(cancel.load(std::sync::atomic::Ordering::Relaxed));
        report(Presence::Unknown, false, None)
    })
    .unwrap();
    drop(probe);
    release.send(()).unwrap();
    assert_eq!(saw.recv_timeout(Duration::from_secs(5)), Ok(true));
}

#[test]
fn the_ios_runner_starts_only_when_the_probe_proves_there_is_no_driver() {
    let started = Arc::new(AtomicUsize::new(0));
    let starter = |counter: &Arc<AtomicUsize>| -> RunnerStarter {
        let counter = counter.clone();
        Box::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            IosNativeLaneHandle::unavailable("test runner")
        })
    };
    for (presence, expect_runner) in [(Presence::Absent, true), (Presence::Present, false), (Presence::Unknown, false)]
    {
        started.store(0, std::sync::atomic::Ordering::SeqCst);
        let probe = LaunchProbe::start_with(Some(starter(&started)), move |_| report(presence, true, None)).unwrap();
        wait_for(&probe);
        assert_eq!(probe.take_runner().is_some(), expect_runner, "{presence:?}");
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), usize::from(expect_runner));
        assert!(probe.take_runner().is_none(), "the runner is handed over once");
    }
}

#[test]
fn a_cancelled_probe_never_starts_a_runner() {
    let started = Arc::new(AtomicUsize::new(0));
    let counter = started.clone();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let probe = LaunchProbe::start_with(
        Some(Box::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            IosNativeLaneHandle::unavailable("test runner")
        })),
        move |_| {
            let _ = gate.recv_timeout(Duration::from_secs(5));
            let r = report(Presence::Absent, false, None);
            let _ = done.send(());
            r
        },
    )
    .unwrap();
    drop(probe);
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn a_launch_without_a_driver_goes_black_box_at_once_and_says_why_in_the_result() {
    let (mut native, stop, srv) = serve_static_native(LAUNCHER);
    let mut dev = android();
    let mut drv = Driver::listen(0).unwrap();
    let opts = test_opts(InputMode::Auto);
    let mut session = Session {
        dev: &mut dev,
        drv: &mut drv,
        opts: &opts,
        units_scale: 1.0,
        native: Some(&mut native),
        ios_native: None,
    };
    let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
    let probe = finished(report(Presence::Absent, false, Some("fp")));

    let res =
        backend.launch_without_handshake(&metrics(), Some(&probe), SkipReason::NoDriver { cached: false }).unwrap();
    assert_eq!(res["blackbox"], true);
    assert_eq!(res["lane"], "native");
    assert_eq!(res["driverProbe"]["verdict"], "absent");
    assert_eq!(res["driverProbe"]["source"], "scan");
    assert_eq!(res["driverProbe"]["handshakeWaitSkippedMs"], 15_000);
    assert!(res["note"].as_str().unwrap().contains("no tapwing_driver in the installed build"), "{res}");
    assert!(backend.ran_blackbox(), "the flow is black-box from here on");
    let mark = backend.blackbox();
    assert!(mark.active);
    assert!(
        mark.reason
            .as_deref()
            .is_some_and(|r| r.starts_with("no tapwing_driver in the installed build: read base.apk")),
        "{mark:?}"
    );
    drop(backend);
    drop(native);
    let _ = stop.send(());
    srv.join().unwrap();
}

#[test]
fn a_flow_that_waited_and_timed_out_has_no_precomputed_reason() {
    let mut dev = android();
    let mut drv = Driver::listen(0).unwrap();
    let opts = test_opts(InputMode::Auto);
    let mut session =
        Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
    let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
    backend.record_handshake_miss(APP);
    let mark = backend.blackbox();
    assert_eq!((mark.active, mark.reason), (true, None));
    // A reason from an earlier launch does not outlive the black-box mark it explained.
    backend.blackbox_reason = Some("stale".into());
    backend.clear_flow_blackbox(APP);
    assert_eq!(backend.blackbox(), crate::run::Blackbox::default());
}

#[test]
fn the_runner_the_probe_started_moves_into_the_lazy_ios_slot_once() {
    let mut dev = Device::Ios(IosSim::new(SimInfo { udid: "UDID".into(), name: "iPhone".into() }));
    let mut drv = Driver::listen(0).unwrap();
    let opts = test_opts(InputMode::Auto);
    let mut slot = IosNativeSlot::lazy("UDID");
    slot.record_handshake_miss(APP); // a count the adoption must keep
    let mut session = Session {
        dev: &mut dev,
        drv: &mut drv,
        opts: &opts,
        units_scale: 1.0,
        native: None,
        ios_native: Some(&mut slot),
    };
    let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
    let probe = LaunchProbe::start_with(Some(Box::new(|| IosNativeLaneHandle::unavailable("test runner"))), |_| {
        report(Presence::Absent, false, None)
    })
    .unwrap();
    wait_for(&probe);
    backend.adopt_probe_runner(&probe);
    drop(backend);
    assert!(slot.is_started() && !slot.is_usable(), "the probe's runner replaced the lazy slot");
    assert!(!slot.is_sticky_blackbox(APP));
    assert_eq!(
        slot.record_handshake_miss(APP),
        crate::native_lane::BlackboxDemotion::StickyShard { consecutive_misses: 2 }
    );

    // A slot that already has a lane keeps it; the spare is dropped.
    let spare = IosNativeLaneHandle::unavailable("spare");
    slot.adopt(spare);
    assert!(slot.is_started());
}

#[test]
fn a_miss_is_learned_only_with_the_app_alive_and_a_build_the_probe_could_not_vouch_for() {
    struct Case {
        name: &'static str,
        report: Report,
        alive: Option<bool>,
        pid: Option<i64>,
        misses: u32,
    }
    let cases = [
        Case {
            name: "unreadable build, app alive",
            report: report(Presence::Unknown, false, Some("fp")),
            alive: Some(true),
            pid: Some(7),
            misses: 1,
        },
        Case {
            name: "driver linked without a dial path, app alive",
            report: report(Presence::Present, false, Some("fp")),
            alive: Some(true),
            pid: Some(7),
            misses: 1,
        },
        Case {
            name: "the app died: a crash says nothing about the driver",
            report: report(Presence::Unknown, false, Some("fp")),
            alive: Some(false),
            pid: Some(7),
            misses: 0,
        },
        Case {
            name: "liveness unknown",
            report: report(Presence::Unknown, false, Some("fp")),
            alive: None,
            pid: Some(7),
            misses: 0,
        },
        Case {
            name: "no pid: the app may never have started",
            report: report(Presence::Unknown, false, Some("fp")),
            alive: Some(true),
            pid: None,
            misses: 0,
        },
        Case {
            name: "a build proven to dial is not learned about",
            report: report(Presence::Present, true, Some("fp")),
            alive: Some(true),
            pid: Some(7),
            misses: 0,
        },
        Case {
            name: "a build proven driver-free never waited",
            report: report(Presence::Absent, false, Some("fp")),
            alive: Some(true),
            pid: Some(7),
            misses: 0,
        },
        Case {
            name: "no install fingerprint to key the miss by",
            report: report(Presence::Unknown, false, None),
            alive: Some(true),
            pid: Some(7),
            misses: 0,
        },
    ];
    for c in cases {
        let (cache, dir) = temp_cache("learn");
        let mut dev = android();
        let mut drv = Driver::listen(0).unwrap();
        let alive = c.alive;
        drv.set_liveness_probe(Box::new(move |_| alive));
        let opts = test_opts(InputMode::Auto);
        let mut session =
            Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
        let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
        backend.cache_override = Some(cache.clone());
        let probe = finished(c.report);
        backend.learn_missed_handshake(APP, c.pid, Some(&probe));
        backend.learn_missed_handshake(APP, c.pid, None);
        let key = crate::driver_probe::cache::InstallKey {
            device: "unreachable".into(),
            app: APP.into(),
            fingerprint: "fp".into(),
        };
        assert_eq!(cache.lookup(&key).map_or(0, |e| e.misses), c.misses, "{}", c.name);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn consecutive_misses_accumulate_and_a_handshake_clears_them() {
    let (cache, dir) = temp_cache("accumulate");
    let mut dev = android();
    let mut drv = Driver::listen(0).unwrap();
    drv.set_liveness_probe(Box::new(|_| Some(true)));
    let opts = test_opts(InputMode::Auto);
    let mut session =
        Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
    let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
    backend.cache_override = Some(cache.clone());
    let probe = finished(report(Presence::Unknown, false, Some("fp")));
    let key = crate::driver_probe::cache::InstallKey {
        device: "unreachable".into(),
        app: APP.into(),
        fingerprint: "fp".into(),
    };

    backend.learn_missed_handshake(APP, Some(7), Some(&probe));
    assert_eq!(cache.lookup(&key).unwrap().learned_misses(cache.now()), None);
    backend.learn_missed_handshake(APP, Some(7), Some(&probe));
    assert_eq!(cache.lookup(&key).unwrap().learned_misses(cache.now()), Some(2));

    // The next run's probe reads those misses back from the same cache.
    let report = crate::driver_probe::resolve("unreachable", APP, Some("fp".into()), Some(&cache), || {
        Verdict::unknown("unreadable")
    });
    assert_eq!(report.skip_reason(), Some(SkipReason::LearnedMisses { misses: 2 }));

    // Any handshake, in any `--input` mode, makes it stale.
    backend.record_handshake_success(APP);
    assert!(cache.lookup(&key).is_none());
    let report = crate::driver_probe::resolve("unreachable", APP, Some("fp".into()), Some(&cache), || {
        Verdict::unknown("unreadable")
    });
    assert_eq!(report.skip_reason(), None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_handshake_keeps_a_present_verdict_and_drops_a_contradicted_absent_one() {
    let (cache, dir) = temp_cache("success");
    let key = |fp: &str| crate::driver_probe::cache::InstallKey {
        device: "unreachable".into(),
        app: APP.into(),
        fingerprint: fp.into(),
    };
    cache.store_verdict(&key("a"), &Verdict { presence: Presence::Absent, dials: false, evidence: "x".into() });
    let mut dev = android();
    let mut drv = Driver::listen(0).unwrap();
    let opts = test_opts(InputMode::Flutter);
    let mut session =
        Session { dev: &mut dev, drv: &mut drv, opts: &opts, units_scale: 1.0, native: None, ios_native: None };
    let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
    backend.cache_override = Some(cache.clone());
    backend.record_handshake_success(APP);
    assert!(cache.lookup(&key("a")).is_none(), "--input flutter heals a wrong absent verdict by connecting");
    cache.store_verdict(&key("b"), &Verdict { presence: Presence::Present, dials: true, evidence: "y".into() });
    backend.record_handshake_success(APP);
    assert_eq!(cache.lookup(&key("b")).and_then(|e| e.known), Some(Known::Present));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_probe_starts_only_for_auto_with_a_native_lane_and_a_process_start() {
    let fake = FakeAdb::start(|_| ShellReply::ok(""));
    let probing_opts = |mode| Opts { driver_probe: true, ..test_opts(mode) };
    // (mode, probe enabled, restarting, native lane present, sticky black-box) -> probes?
    for (mode, enabled, restarting, lane, sticky, expect) in [
        (InputMode::Auto, true, true, true, false, true),
        (InputMode::Auto, false, true, true, false, false),
        (InputMode::Auto, true, false, true, false, false),
        (InputMode::Auto, true, true, false, false, false),
        (InputMode::Auto, true, true, true, true, false),
        (InputMode::Flutter, true, true, true, false, false),
        (InputMode::Native, true, true, true, false, false),
    ] {
        let (mut native, stop, srv) = serve_static_native(LAUNCHER);
        let (cache, dir) = temp_cache("gate");
        let mut dev = Device::Android(Android::with_adb_client("emulator-5554".into(), fake.client()));
        let mut drv = Driver::listen(0).unwrap();
        let opts = Opts { driver_probe: enabled, ..probing_opts(mode) };
        let mut session = Session {
            dev: &mut dev,
            drv: &mut drv,
            opts: &opts,
            units_scale: 1.0,
            native: lane.then_some(&mut native),
            ios_native: None,
        };
        let mut backend = DeviceFlutterBackend::new(&mut session, APP.into());
        backend.cache_override = Some(cache);
        if sticky {
            backend.native.as_deref_mut().unwrap().record_handshake_miss(APP);
            backend.native.as_deref_mut().unwrap().record_handshake_miss(APP);
        }
        let probe = backend.start_driver_probe(APP, restarting);
        assert_eq!(
            probe.is_some(),
            expect,
            "{mode:?} enabled={enabled} restarting={restarting} lane={lane} sticky={sticky}"
        );
        if let Some(probe) = probe {
            wait_for(&probe);
            // The fake device answers nothing useful: unknown, so the wait stays as it was.
            assert_eq!(probe.knowledge(), DriverKnowledge::Wait);
        }
        drop(backend);
        drop(native);
        let _ = stop.send(());
        srv.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
