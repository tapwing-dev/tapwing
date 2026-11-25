//! `adb` access and the hash-cached device install of the native-lane driver APKs.
//!
//! On the device a marker file holds the hash of the installed APK set, so
//! later runs install nothing (see [`ensure_installed`]). Where the APK comes
//! from is the business of [`super::tapwing_apk`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::device::adb_client::AdbClient;
use crate::process::{Output, run_cleanup_with_timeout, run_with_timeout, run_with_timeout_or_cancel};

/// Deadline of a short `adb` CLI call (`forward`, `uninstall`, a `shell`
/// fallback while the smartsocket is down). It includes the few seconds the
/// CLI needs to start the ADB server when none is running.
const ADB_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Deadline of one `adb install`: the driver APKs are small, but a wireless
/// or busy device verifies and optimizes them slowly.
const ADB_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Thin ADB wrapper for a single device `serial`. Shell and port-forward
/// commands go directly over the ADB smartsocket ([`AdbClient`]) to avoid
/// per-call subprocess overhead; `install` and long-lived `am instrument` still
/// use the `adb` CLI via [`Self::command`] / [`Self::run`].
#[derive(Clone, Debug)]
pub struct Adb {
    pub serial: String,
    client: AdbClient,
}

impl Adb {
    pub fn new(serial: impl Into<String>) -> Self {
        Self { serial: serial.into(), client: AdbClient::new() }
    }

    #[cfg(test)]
    pub(crate) fn with_client(serial: impl Into<String>, client: AdbClient) -> Self {
        Self { serial: serial.into(), client }
    }

    pub fn client(&self) -> &AdbClient {
        &self.client
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("adb");
        c.arg("-s").arg(&self.serial).args(args);
        c
    }

    /// Runs `adb -s <serial> <args>` for at most [`ADB_COMMAND_TIMEOUT`];
    /// returns stdout, errors carry stderr.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        checked_stdout(args, run_with_timeout(&mut self.command(args), ADB_COMMAND_TIMEOUT)?)
    }

    /// Runs a bounded `adb` teardown command from `Drop`, even when a signal is
    /// already pending.
    pub fn run_cleanup(&self, args: &[&str]) -> Result<String> {
        checked_stdout(args, run_cleanup_with_timeout(&mut self.command(args), ADB_COMMAND_TIMEOUT)?)
    }

    /// `adb install` of `apk` (`-r`: replace, `-t`: allow test-only packages,
    /// `-d`: allow a version downgrade), killed after [`ADB_INSTALL_TIMEOUT`]
    /// or as soon as `cancel` is set.
    fn install(&self, apk: &Path, cancel: &AtomicBool) -> Result<()> {
        let path = apk.to_string_lossy();
        let args = ["install", "-r", "-t", "-d", &path];
        let out = run_with_timeout_or_cancel(&mut self.command(&args), ADB_INSTALL_TIMEOUT, cancel)?;
        checked_stdout(&args, out).map(|_| ())
    }

    pub fn shell(&self, cmd: &str) -> Result<String> {
        let out = match self.client.shell(&self.serial, cmd) {
            Ok(out) => out,
            Err(_) if !self.client.is_available() => return self.run(&["shell", cmd]),
            Err(e) => return Err(e),
        };
        if !out.success() {
            bail!("adb shell {cmd} failed (exit {}): {}", out.exit_code, out.combined().trim());
        }
        Ok(out.stdout)
    }
}

/// Stdout of a finished `adb <args>`, or an error with its output when it failed.
fn checked_stdout(args: &[&str], out: Output) -> Result<String> {
    if !out.status.success() {
        bail!("adb {} failed: {}{}", args.join(" "), out.stderr_lossy().trim(), out.stdout_lossy().trim());
    }
    Ok(out.stdout_lossy())
}

pub fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What a driver needs on the device and how to recognise it is already there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallPlan {
    /// `(package, apk)` in install order.
    pub packages: Vec<(&'static str, PathBuf)>,
    /// Device file holding [`Self::sha256`] once the packages are installed.
    pub marker: &'static str,
    /// Lowercase hex SHA-256 identifying the APK set.
    pub sha256: String,
}

/// What one probe round-trip learned about the device.
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceProbe {
    pub marker: Option<String>,
    /// The plan's packages that are installed (exact names, in plan order).
    pub installed: Vec<&'static str>,
    pub api_level: u32,
}

impl DeviceProbe {
    /// True when the device already has exactly this APK set installed.
    pub fn is_current(&self, plan: &InstallPlan) -> bool {
        self.installed.len() == plan.packages.len() && self.marker.as_deref() == Some(plan.sha256.as_str())
    }
}

/// One `adb shell` script answering everything the install decision needs.
/// `pm list packages <p>` matches substrings, so the plan's first package also
/// lists packages that merely start with its name; [`parse_probe`] keeps exact names.
pub fn probe_script(plan: &InstallPlan) -> String {
    let filter = plan.packages.first().map_or("", |(p, _)| p);
    format!(
        "echo TAPWING_MARKER=$(cat {} 2>/dev/null); \
         pm list packages --user 0 {filter} 2>/dev/null; \
         echo TAPWING_SDK=$(getprop ro.build.version.sdk)",
        plan.marker
    )
}

pub fn parse_probe(out: &str, plan: &InstallPlan) -> DeviceProbe {
    let mut probe = DeviceProbe { marker: None, installed: Vec::new(), api_level: 0 };
    for line in out.lines().map(str::trim) {
        if let Some(m) = line.strip_prefix("TAPWING_MARKER=") {
            probe.marker = Some(m.trim().to_string()).filter(|m| !m.is_empty());
        } else if let Some(sdk) = line.strip_prefix("TAPWING_SDK=") {
            probe.api_level = sdk.trim().parse().unwrap_or(0);
        } else if let Some(pkg) = line.strip_prefix("package:")
            && let Some((name, _)) = plan.packages.iter().find(|(p, _)| *p == pkg.trim())
            && !probe.installed.contains(name)
        {
            probe.installed.push(name);
        }
    }
    probe.installed.sort_by_key(|p| plan.packages.iter().position(|(q, _)| q == p));
    probe
}

/// Outcome of [`ensure_installed`].
#[derive(Debug)]
pub struct InstallReport {
    /// `true` when APKs were pushed (first run, new driver version, or removed).
    pub installed: bool,
    pub api_level: u32,
}

/// `adb install` failures that an uninstall fixes: another signing key, or an
/// older driver than the one on the device.
fn needs_uninstall(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") || msg.contains("INSTALL_FAILED_VERSION_DOWNGRADE")
}

/// Installs the plan's APKs only when missing or when their hash differs from
/// the device marker. A warm device costs one `adb shell` round-trip.
pub fn ensure_installed(adb: &Adb, plan: &InstallPlan, cancel: &AtomicBool) -> Result<InstallReport> {
    use std::sync::atomic::Ordering;

    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    let probe = parse_probe(&adb.shell(&probe_script(plan))?, plan);
    if probe.is_current(plan) {
        return Ok(InstallReport { installed: false, api_level: probe.api_level });
    }
    for (pkg, apk) in &plan.packages {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        match adb.install(apk, cancel) {
            Ok(()) => {}
            Err(e) if needs_uninstall(&e) => {
                let _ = adb.run(&["uninstall", pkg]);
                adb.install(apk, cancel).with_context(|| format!("installing {}", apk.display()))?;
            }
            Err(e) => return Err(e).with_context(|| format!("installing {}", apk.display())),
        }
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    adb.shell(&format!("echo {} > {}", plan.sha256, plan.marker))?;
    Ok(InstallReport { installed: true, api_level: probe.api_level })
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn sha256_is_hex_and_order_sensitive() {
        // Known vector: SHA-256("abc").
        assert_eq!(sha256_hex(&[b"abc"]), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex(&[b"a", b"bc"]), sha256_hex(&[b"abc"]));
        assert_ne!(sha256_hex(&[b"abc"]), sha256_hex(&[b"acb"]));
    }

    fn plan_for(hash: &str) -> InstallPlan {
        InstallPlan {
            packages: vec![("dev.tapwing.driver", PathBuf::from("/k.apk"))],
            marker: "/data/local/tmp/tapwing_driver_apk.sha256",
            sha256: hash.into(),
        }
    }

    #[test]
    fn probe_parses_current_install() {
        let hash = "a".repeat(64);
        let plan = plan_for(&hash);
        assert!(probe_script(&plan).contains("cat /data/local/tmp/tapwing_driver_apk.sha256"));
        assert!(probe_script(&plan).contains("pm list packages --user 0 dev.tapwing.driver"));
        let out = format!("TAPWING_MARKER={hash}\npackage:dev.tapwing.driver\nTAPWING_SDK=35\n");
        let p = parse_probe(&out, &plan);
        assert_eq!(p, DeviceProbe { marker: Some(hash.clone()), installed: vec!["dev.tapwing.driver"], api_level: 35 });
        assert!(p.is_current(&plan));
        assert!(!p.is_current(&plan_for(&"b".repeat(64))), "different hash forces a reinstall");
    }

    #[test]
    fn probe_detects_missing_packages_and_marker() {
        let plan = plan_for("x");
        let p = parse_probe("TAPWING_MARKER=\nTAPWING_SDK=29\n", &plan);
        assert_eq!(p, DeviceProbe { marker: None, installed: vec![], api_level: 29 });
        assert!(!p.is_current(&plan));
        // The marker exists but the APK was uninstalled.
        let hash = "c".repeat(64);
        let plan = plan_for(&hash);
        let p = parse_probe(&format!("TAPWING_MARKER={hash}\nTAPWING_SDK=33\n"), &plan);
        assert!(p.installed.is_empty());
        assert!(!p.is_current(&plan));
        // Prefix matches of unrelated packages are ignored.
        let p = parse_probe("package:dev.tapwing.driver.other\nTAPWING_SDK=x\n", &plan);
        assert!(p.installed.is_empty());
        assert_eq!(p.api_level, 0);
    }

    #[test]
    fn a_marker_without_the_package_is_not_current() {
        let plan = plan_for(&"d".repeat(64));
        let out = format!("TAPWING_MARKER={}\npackage:dev.tapwing.driver\nTAPWING_SDK=35\n", "d".repeat(64));
        assert!(parse_probe(&out, &plan).is_current(&plan));
        assert!(!parse_probe("TAPWING_MARKER=\npackage:dev.tapwing.driver\nTAPWING_SDK=35\n", &plan).is_current(&plan));
    }

    #[test]
    fn signature_and_downgrade_conflicts_trigger_an_uninstall() {
        assert!(needs_uninstall(&anyhow!("adb install failed: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: ...]")));
        assert!(needs_uninstall(&anyhow!("Failure [INSTALL_FAILED_VERSION_DOWNGRADE]")));
        assert!(!needs_uninstall(&anyhow!("Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]")));
    }
}
