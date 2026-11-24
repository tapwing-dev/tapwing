//! Warns when the iOS runner was built with another major Xcode than the one
//! that will run it.
//!
//! `scripts/build-ios-driver.sh` writes [`BUILD_INFO`] next to the runner
//! (shipped in release bundles): the Xcode version and build and the iOS
//! Simulator SDK it was built with. `xcodebuild test-without-building` then
//! runs that prebuilt runner with whatever Xcode the user has selected. Whether
//! and how a mismatch breaks startup has not been reproduced, so this never
//! fails or blocks anything: it compares the two once per process and prints
//! one `warning:` line. Without the file (older artifacts), or when the local
//! Xcode cannot be asked in time, it stays silent.
//!
//! A mismatch is a different **major** version. The runner depends on private
//! XCTest API and on the test-runner protocol between `xcodebuild`, the
//! simulator and `XCTest.framework`, which change with major Xcode releases;
//! minor and patch releases keep them. Comparing exact builds would warn nearly
//! every user (anyone not on the release machine's exact Xcode) and teach them
//! to ignore the line.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Written by `scripts/build-ios-driver.sh` next to the runner artifacts.
pub(crate) const BUILD_INFO: &str = "tapwing-ios-runner.build-info.json";
/// `xcodebuild -version` answers in well under a second; a broken toolchain must not stall the lane.
const XCODE_VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
/// The file is a few short strings; anything larger is not ours.
const MAX_BUILD_INFO_BYTES: u64 = 16 * 1024;

/// The toolchain a runner artifact was built with.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BuildInfo {
    pub xcode_version: String,
    pub xcode_build: String,
    pub sdk_version: String,
}

/// `xcodebuild -version` of the local machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalXcode {
    pub version: String,
    pub build: String,
}

/// Reads [`BUILD_INFO`] from the driver directory. `None` when it is absent,
/// unreadable or names no Xcode version: older artifacts have no such file.
pub(crate) fn read_build_info(driver_dir: &Path) -> Option<BuildInfo> {
    let file = fs::File::open(driver_dir.join(BUILD_INFO)).ok()?;
    let mut text = String::new();
    file.take(MAX_BUILD_INFO_BYTES).read_to_string(&mut text).ok()?;
    let info: BuildInfo = serde_json::from_str(&text).ok()?;
    (!info.xcode_version.trim().is_empty()).then_some(info)
}

/// Parses `Xcode 16.2\nBuild version 16C5032a`.
pub(crate) fn parse_xcodebuild_version(output: &str) -> Option<LocalXcode> {
    let mut version = None;
    let mut build = String::new();
    for line in output.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("Xcode ") {
            version = Some(v.trim().to_string());
        } else if let Some(b) = line.strip_prefix("Build version ") {
            build = b.trim().to_string();
        }
    }
    version.filter(|v| !v.is_empty()).map(|version| LocalXcode { version, build })
}

/// `16` of `16.2`.
fn major(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// The warning for a runner built with another major Xcode, if it is one.
/// Versions that cannot be read as numbers never warn.
pub(crate) fn mismatch_warning(built: &BuildInfo, local: &LocalXcode) -> Option<String> {
    let (built_major, local_major) = (major(&built.xcode_version)?, major(&local.version)?);
    if built_major == local_major {
        return None;
    }
    let paren = |parts: &[&str]| {
        let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.trim().is_empty()).collect();
        if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(", ")) }
    };
    let sdk = if built.sdk_version.trim().is_empty() {
        String::new()
    } else {
        format!("iOS Simulator SDK {}", built.sdk_version.trim())
    };
    Some(format!(
        "the Tapwing iOS driver was built with Xcode {}{} but this machine runs Xcode {}{}; if the iOS native lane \
         fails to start, select a matching Xcode (`xcode-select`) or rebuild the driver with \
         scripts/build-ios-driver.sh and point ${} at drivers/ios/build",
        built.xcode_version.trim(),
        paren(&[&built.xcode_build, &sdk]),
        local.version,
        paren(&[&local.build]),
        super::tapwing_driver::DRIVER_DIR_ENV,
    ))
}

/// Runs `cmd` and returns its stdout if it exits successfully within
/// `timeout`; kills it on the timeout or when `cancel` is set. Meant for
/// commands with a few lines of output (the pipe is read after the exit).
fn stdout_within(mut cmd: Command, timeout: Duration, cancel: &AtomicBool) -> Option<String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))),
            Err(_) => break None,
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status.success().then_some(out)
}

fn local_xcode(cancel: &AtomicBool) -> Option<LocalXcode> {
    let mut cmd = Command::new("xcodebuild");
    cmd.arg("-version");
    parse_xcodebuild_version(&stdout_within(cmd, XCODE_VERSION_TIMEOUT, cancel)?)
}

/// The whole check with the local Xcode supplied by `local` (asked only when
/// the driver has build info).
fn check(driver_dir: &Path, local: impl FnOnce() -> Option<LocalXcode>) -> Option<String> {
    let built = read_build_info(driver_dir)?;
    mismatch_warning(&built, &local()?)
}

/// Prints one `warning:` line if the driver in `driver_dir` was built with
/// another major Xcode than the local one. At most once per process (later
/// sessions, shards and restarts skip it), and never an error.
pub(crate) fn warn_once(driver_dir: &Path, cancel: &AtomicBool) {
    // A flag, not a `Once`: concurrent sessions (shards) must not wait for the first one's check.
    static CHECKED: AtomicBool = AtomicBool::new(false);
    if CHECKED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Some(warning) = check(driver_dir, || local_xcode(cancel)) {
        eprintln!("warning: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tapwing-xcode-compat-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn with_info(body: &str) -> Self {
            let s = Self::new();
            fs::write(s.0.join(BUILD_INFO), body).unwrap();
            s
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn built(version: &str, build: &str, sdk: &str) -> BuildInfo {
        BuildInfo { xcode_version: version.into(), xcode_build: build.into(), sdk_version: sdk.into() }
    }

    fn local(version: &str, build: &str) -> LocalXcode {
        LocalXcode { version: version.into(), build: build.into() }
    }

    #[test]
    fn parses_xcodebuild_version_output() {
        assert_eq!(parse_xcodebuild_version("Xcode 16.2\nBuild version 16C5032a\n"), Some(local("16.2", "16C5032a")));
        assert_eq!(parse_xcodebuild_version("  Xcode 27.0  \r\n"), Some(local("27.0", "")));
        for bad in ["", "Build version 16C5032a", "xcode-select: error: tool 'xcodebuild' requires Xcode", "Xcode "] {
            assert_eq!(parse_xcodebuild_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn reads_the_file_the_build_script_writes_and_ignores_anything_else() {
        // The exact shape `scripts/build-ios-driver.sh` writes (extra keys are fine).
        let s = Scratch::with_info(
            "{\n  \"sdkBuild\": \"23B77\",\n  \"sdkVersion\": \"26.1\",\n  \"xcodeBuild\": \"17B55\",\n  \
             \"xcodeVersion\": \"26.1\"\n}\n",
        );
        assert_eq!(read_build_info(&s.0), Some(built("26.1", "17B55", "26.1")));

        assert_eq!(read_build_info(&Scratch::new().0), None, "older artifacts have no file");
        for bad in ["", "not json", "[]", "{}", r#"{"xcodeVersion":"  "}"#, r#"{"xcodeVersion":16}"#] {
            assert_eq!(read_build_info(&Scratch::with_info(bad).0), None, "{bad:?}");
        }
        let partial = Scratch::with_info(r#"{"xcodeVersion":"16.2"}"#);
        assert_eq!(read_build_info(&partial.0), Some(built("16.2", "", "")));
    }

    #[test]
    fn only_a_different_major_version_is_a_mismatch() {
        let b = built("26.1", "17B55", "26.1");
        assert_eq!(mismatch_warning(&b, &local("26.1", "17B55")), None);
        assert_eq!(mismatch_warning(&b, &local("26.4.1", "17E200")), None, "minor and patch releases are compatible");
        assert_eq!(mismatch_warning(&built("26", "", ""), &local("26.0", "x")), None);

        let newer = mismatch_warning(&b, &local("27.2", "27B52")).unwrap();
        assert!(
            newer.starts_with(
                "the Tapwing iOS driver was built with Xcode 26.1 (17B55, iOS Simulator SDK 26.1) but this machine \
                 runs Xcode 27.2 (27B52);"
            ),
            "{newer}"
        );
        assert!(newer.contains("xcode-select") && newer.contains("build-ios-driver.sh"), "{newer}");
        assert!(newer.contains("$TAPWING_IOS_DRIVER_DIR") && !newer.contains('\n'), "one line: {newer}");
        assert!(mismatch_warning(&b, &local("16.4", "16F6")).is_some(), "an older local Xcode warns too");

        // Missing details leave no empty brackets.
        let bare = mismatch_warning(&built("26.1", "", ""), &local("27.0", "")).unwrap();
        assert!(bare.contains("built with Xcode 26.1 but this machine runs Xcode 27.0;"), "{bare}");

        // Versions that are not numbers never warn.
        assert_eq!(mismatch_warning(&built("beta", "", ""), &local("27.2", "")), None);
        assert_eq!(mismatch_warning(&b, &local("unknown", "")), None);
    }

    #[test]
    fn the_check_is_silent_without_build_info_and_does_not_even_ask_xcode() {
        let asked = AtomicBool::new(false);
        let ask = || {
            asked.store(true, Ordering::Relaxed);
            Some(local("27.2", "27B52"))
        };
        assert_eq!(check(&Scratch::new().0, ask), None);
        assert!(!asked.load(Ordering::Relaxed), "an artifact without build info costs no subprocess");

        let s = Scratch::with_info(r#"{"xcodeVersion":"26.1","xcodeBuild":"17B55","sdkVersion":"26.1"}"#);
        assert!(check(&s.0, || Some(local("27.2", "27B52"))).unwrap().contains("Xcode 27.2"));
        assert_eq!(check(&s.0, || Some(local("26.2", "17C1"))), None);
        assert_eq!(check(&s.0, || None), None, "an Xcode that cannot be asked is not a mismatch");
    }

    #[cfg(unix)]
    #[test]
    fn the_version_command_is_bounded_cancellable_and_tolerates_failure() {
        let sh = |script: &str| {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", script]);
            c
        };
        let live = AtomicBool::new(false);
        let out = stdout_within(sh("printf 'Xcode 16.2\\nBuild version 16C5032a\\n'"), Duration::from_secs(10), &live);
        assert_eq!(parse_xcodebuild_version(&out.unwrap()), Some(local("16.2", "16C5032a")));

        assert_eq!(stdout_within(sh("echo nope; exit 1"), Duration::from_secs(10), &live), None);
        assert_eq!(stdout_within(Command::new("/nonexistent/xcodebuild"), Duration::from_secs(10), &live), None);

        // A hung toolchain is killed at the timeout, and at once when cancelled.
        let t = Instant::now();
        assert_eq!(stdout_within(sh("exec sleep 30"), Duration::from_millis(100), &live), None);
        assert_eq!(stdout_within(sh("exec sleep 30"), Duration::from_secs(30), &AtomicBool::new(true)), None);
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    }
}
