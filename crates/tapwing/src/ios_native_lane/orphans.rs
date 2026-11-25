//! Sweep of `xcodebuild` runners a hard-killed earlier run left behind.
//!
//! `SIGKILL` (or a crash) skips `RunnerGuard::drop`, so its
//! `xcodebuild test-without-building` keeps running: it holds the test session
//! of the simulator, its runner port and a scratch directory. A new session
//! removes such leftovers before it starts its own runner.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::process::run_with_timeout;

/// `ps` answers in milliseconds; the deadline only guards a wedged host.
const PS_TIMEOUT: Duration = Duration::from_secs(5);

/// A leftover `xcodebuild` and the scratch directory of its dead session.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct OrphanRunner {
    pub pid: u32,
    pub session_dir: PathBuf,
}

/// The owning Tapwing pid of a session scratch directory name
/// (`tapwing-ios-native-<pid>-<seq>-<port>`, see `session_dir_name`).
fn session_owner(dir_name: &str) -> Option<u32> {
    let mut parts = dir_name.strip_prefix("tapwing-ios-native-")?.split('-');
    let (pid, seq, port) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if parts.next().is_some() || !digits(pid) || !digits(seq) || !digits(port) {
        return None;
    }
    pid.parse().ok()
}

/// Picks the processes of a `ps -axww -o pid=,command=` listing that may be killed.
///
/// The rule: a process is an orphan only when all of this holds, so nothing
/// that is not provably a dead Tapwing session's runner is ever touched:
///
/// * it is `xcodebuild test-without-building` (a build or a test run somebody
///   started by hand has other arguments);
/// * its `-destination` is `id=<udid>`, the simulator this session drives;
/// * its `-derivedDataPath` is `<scratch_root>/tapwing-ios-native-<pid>-<seq>-<port>/derived`,
///   the layout only `start_session` produces, directly under this host's
///   temp directory;
/// * no process with that `<pid>` is alive, so neither this process (another
///   shard or a restarted runner of it) nor a concurrent `tapwing` owns it. A
///   recycled pid only delays the cleanup.
pub(super) fn orphan_runners(
    ps: &str,
    udid: &str,
    scratch_root: &Path,
    pid_alive: impl Fn(u32) -> bool,
) -> Vec<OrphanRunner> {
    let destination = format!("id={udid}");
    ps.lines()
        .filter_map(|line| {
            let mut args = line.split_whitespace();
            let pid: u32 = args.next()?.parse().ok()?;
            let args: Vec<&str> = args.collect();
            if Path::new(args.first()?).file_name()?.to_str()? != "xcodebuild" {
                return None;
            }
            let value_of = |flag: &str| args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).copied();
            if !args.contains(&"test-without-building") || value_of("-destination")? != destination {
                return None;
            }
            let derived = Path::new(value_of("-derivedDataPath")?);
            let session_dir = derived.parent()?;
            if derived.file_name()?.to_str()? != "derived" || session_dir.parent()? != scratch_root {
                return None;
            }
            let owner = session_owner(session_dir.file_name()?.to_str()?)?;
            (!pid_alive(owner)).then(|| OrphanRunner { pid, session_dir: session_dir.to_path_buf() })
        })
        .collect()
}

/// Kills the orphaned runners of the simulator `udid` and removes their
/// scratch directories; returns how many were killed. Best effort: the caller
/// at most warns on an error.
pub(super) fn sweep(udid: &str) -> Result<usize> {
    let out = run_with_timeout(Command::new("ps").args(["-axww", "-o", "pid=,command="]), PS_TIMEOUT)?;
    if !out.status.success() {
        bail!("ps failed: {}", out.stderr_lossy().trim());
    }
    // `ps` prints the path as `xcodebuild` got it, which is the uncanonicalized temp directory.
    let orphans = orphan_runners(&out.stdout_lossy(), udid, &std::env::temp_dir(), crate::process::pid_alive);
    let mut killed = 0;
    for orphan in orphans {
        #[cfg(unix)]
        if crate::process::kill_pid(orphan.pid) {
            killed += 1;
        }
        let _ = std::fs::remove_dir_all(&orphan.session_dir);
    }
    Ok(killed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UDID: &str = "47862A4D-744F-4125-8D9E-20D8B51AD7DF";
    const ROOT: &str = "/var/folders/ab/T";

    fn runner(pid: u32, udid: &str, derived: &str) -> String {
        format!(
            "{pid} /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild test-without-building \
             -xctestrun /Users/me/.tapwing/ios-native-driver/abc/TapwingRunner.xctestrun \
             -destination id={udid} -destination-timeout 1 -derivedDataPath {derived}\n"
        )
    }

    fn orphans(ps: &str, alive: &[u32]) -> Vec<OrphanRunner> {
        orphan_runners(ps, UDID, Path::new(ROOT), |pid| alive.contains(&pid))
    }

    #[test]
    fn a_runner_of_a_dead_session_on_the_same_simulator_is_an_orphan() {
        let ps = format!(
            "  501 /sbin/launchd\n{}{}",
            runner(7001, UDID, "/var/folders/ab/T/tapwing-ios-native-100-0-50123/derived"),
            runner(7002, UDID, "/var/folders/ab/T/tapwing-ios-native-200-3-50124/derived"),
        );
        // Session 200 is alive (this process or a concurrent run); 100 is dead.
        assert_eq!(
            orphans(&ps, &[200]),
            vec![OrphanRunner {
                pid: 7001,
                session_dir: PathBuf::from("/var/folders/ab/T/tapwing-ios-native-100-0-50123")
            }]
        );
        assert!(orphans(&ps, &[100, 200]).is_empty(), "nothing is killed while every owner lives");
        assert_eq!(orphans(&ps, &[]).len(), 2);
    }

    #[test]
    fn processes_that_are_not_provably_a_dead_tapwing_runner_are_left_alone() {
        let dead_session = "/var/folders/ab/T/tapwing-ios-native-100-0-50123/derived";
        let not_ours = [
            // Another simulator.
            runner(1, "OTHER-UDID", dead_session),
            // A derived-data path Tapwing does not produce: elsewhere, wrong name, wrong leaf.
            runner(2, UDID, "/Users/me/Library/Developer/Xcode/DerivedData/App-abc"),
            runner(3, UDID, "/elsewhere/tapwing-ios-native-100-0-50123/derived"),
            runner(4, UDID, "/var/folders/ab/T/nested/tapwing-ios-native-100-0-50123/derived"),
            runner(5, UDID, "/var/folders/ab/T/tapwing-ios-native-100-0/derived"),
            runner(6, UDID, "/var/folders/ab/T/tapwing-ios-native-100-0-50123-9/derived"),
            runner(7, UDID, "/var/folders/ab/T/tapwing-ios-native-x-0-50123/derived"),
            runner(8, UDID, "/var/folders/ab/T/tapwing-ios-native-100-0-50123/other"),
            runner(9, UDID, "/var/folders/ab/T/tapwing-ios-native-100-0-50123"),
            // Not `xcodebuild test-without-building`.
            format!("10 /usr/bin/xcodebuild build -destination id={UDID} -derivedDataPath {dead_session}\n"),
            format!(
                "11 /bin/zsh -c xcodebuild test-without-building -destination id={UDID} -derivedDataPath {dead_session}\n"
            ),
            format!("12 /usr/bin/grep xcodebuild test-without-building id={UDID} {dead_session}\n"),
            format!(
                "13 /usr/bin/xcodebuild-wrapper test-without-building -destination id={UDID} -derivedDataPath {dead_session}\n"
            ),
            // Truncated or malformed lines.
            format!("14 /usr/bin/xcodebuild test-without-building -destination id={UDID} -derivedDataPath\n"),
            format!(
                "pid /usr/bin/xcodebuild test-without-building -destination id={UDID} -derivedDataPath {dead_session}\n"
            ),
            "15\n".to_string(),
            "\n".to_string(),
        ];
        assert!(orphans(&not_ours.concat(), &[]).is_empty());
        // The same lines do match once they are a real runner invocation.
        assert_eq!(orphans(&runner(16, UDID, dead_session), &[]).len(), 1);
    }

    #[test]
    fn session_owner_reads_the_pid_of_session_dir_names() {
        assert_eq!(session_owner(&super::super::session_dir_name(1234, 0, 50000)), Some(1234));
        assert_eq!(session_owner("tapwing-ios-native-1234-0"), None);
        assert_eq!(session_owner("tapwing-ios-native--0-1"), None);
        assert_eq!(session_owner("tapwing-ios-native-99999999999-0-1"), None);
        assert_eq!(session_owner("tapwing-ios-lane-test-1-x-0"), None);
    }
}
