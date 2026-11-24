//! Android device control over adb (F0: shells out; F4 moves to a
//! persistent adb protocol connection).

use anyhow::{Context, Result, bail};
use std::process::Command;

pub struct Android {
    pub serial: String,
    launcher_cache: std::collections::HashMap<String, String>,
}

impl Android {
    pub fn connect(serial: Option<String>) -> Result<Self> {
        let serial = match serial {
            Some(s) => s,
            None => {
                let out = run("adb", &["devices"])?;
                let devices: Vec<String> =
                    out.lines().skip(1).filter_map(|l| l.strip_suffix("\tdevice").map(str::to_string)).collect();
                match devices.as_slice() {
                    [one] => one.clone(),
                    [] => bail!("no Android device connected"),
                    _ => bail!("multiple devices connected, pass --device ({})", devices.join(", ")),
                }
            }
        };
        Ok(Self { serial, launcher_cache: Default::default() })
    }

    pub fn shell(&self, cmd: &str) -> Result<String> {
        run("adb", &["-s", &self.serial, "shell", cmd])
    }

    /// Keep the display on for the session and wake it if it is asleep.
    /// Input injected inside the app is not user activity for Android, so
    /// the screen timeout would otherwise turn the display off mid-run and
    /// the app would stop rendering frames. The previous setting is restored
    /// when the returned guard drops.
    pub fn keep_awake(&self) -> Result<AwakeGuard> {
        const KEYGUARD: &str = "dumpsys window 2>/dev/null | grep -m1 -o 'isKeyguardShowing=[a-z]*'";
        let out = self.shell(&format!(
            // 15 = stay on while plugged into AC, USB, wireless or dock (what
            // `svc power stayon true` writes, without its ~1 s JVM start).
            "settings get global stay_on_while_plugged_in; settings put global stay_on_while_plugged_in 15; \
             dumpsys power 2>/dev/null | grep -m1 mWakefulness=; {KEYGUARD}"
        ))?;
        // Created first so the setting is restored even if we bail below.
        let guard =
            AwakeGuard { serial: self.serial.clone(), previous: out.lines().next().unwrap_or("").trim().to_string() };
        let mut locked = out.contains("isKeyguardShowing=true");
        if !out.contains("mWakefulness=Awake") {
            self.shell("input keyevent KEYCODE_WAKEUP")?;
            locked = self.shell(KEYGUARD)?.contains("=true");
        }
        if locked {
            // Unlocks a swipe-only lock screen; a secure one stays locked.
            self.shell("wm dismiss-keyguard")?;
            std::thread::sleep(std::time::Duration::from_millis(300));
            if self.shell(KEYGUARD)?.contains("=true") {
                bail!(
                    "device {} is locked; unlock it (test devices are best left without a secure lock screen)",
                    self.serial
                );
            }
        }
        Ok(guard)
    }

    /// Forward the app's dial-out port to this session's host port. The rule
    /// is removed when the returned guard drops, so an app left running on
    /// this device cannot dial into a later session meant for another device.
    pub fn reverse(&self, device_port: u16, host_port: u16) -> Result<ReverseGuard> {
        run("adb", &["-s", &self.serial, "reverse", &format!("tcp:{device_port}"), &format!("tcp:{host_port}")])?;
        Ok(ReverseGuard { serial: self.serial.clone(), device_port })
    }

    pub fn stop_app(&self, app_id: &str) -> Result<()> {
        self.shell(&format!("am force-stop {app_id}"))?;
        Ok(())
    }

    pub fn clear_state(&self, app_id: &str) -> Result<()> {
        let out = self.shell(&format!("pm clear {app_id}"))?;
        if !out.contains("Success") {
            bail!("pm clear {app_id} failed: {out}");
        }
        Ok(())
    }

    pub fn launch(&mut self, app_id: &str) -> Result<()> {
        let component = match self.launcher_cache.get(app_id) {
            Some(c) => c.clone(),
            None => {
                let out = self.shell(&format!(
                    "cmd package resolve-activity --brief -c android.intent.category.LAUNCHER {app_id}"
                ))?;
                let c = out
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && l.contains('/'))
                    .with_context(|| format!("no launcher activity for {app_id}: {out}"))?
                    .to_string();
                self.launcher_cache.insert(app_id.to_string(), c.clone());
                c
            }
        };
        let out = self.shell_combined(&format!(
            "am start -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -n {component}"
        ))?;
        if out.contains("Error") {
            bail!("am start failed: {out}");
        }
        Ok(())
    }

    fn shell_combined(&self, cmd: &str) -> Result<String> {
        let out = Command::new("adb")
            .args(["-s", &self.serial, "shell", cmd])
            .output()
            .with_context(|| format!("running adb shell {cmd}"))?;
        let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if !out.status.success() {
            bail!("adb shell {cmd} failed: {combined}");
        }
        Ok(combined)
    }

    pub fn screenshot(&self, path: &str) -> Result<()> {
        let out = Command::new("adb").args(["-s", &self.serial, "exec-out", "screencap", "-p"]).output()?;
        if !out.status.success() {
            bail!("screencap failed");
        }
        std::fs::write(path, out.stdout)?;
        Ok(())
    }
}

fn run(bin: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(bin).args(args).output().with_context(|| format!("running {bin}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        bail!("{bin} {} failed: {}{}", args.join(" "), stdout, String::from_utf8_lossy(&out.stderr));
    }
    Ok(stdout)
}

/// Removes an `adb reverse` rule on drop (best effort).
pub struct ReverseGuard {
    serial: String,
    device_port: u16,
}

impl Drop for ReverseGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("adb")
            .args(["-s", &self.serial, "reverse", "--remove", &format!("tcp:{}", self.device_port)])
            .output();
    }
}

/// Restores `stay_on_while_plugged_in` on drop (best effort).
pub struct AwakeGuard {
    serial: String,
    previous: String,
}

impl Drop for AwakeGuard {
    fn drop(&mut self) {
        let restore = if self.previous.parse::<u32>().is_ok() {
            format!("settings put global stay_on_while_plugged_in {}", self.previous)
        } else {
            "svc power stayon false".to_string()
        };
        let _ = Command::new("adb").args(["-s", &self.serial, "shell", &restore]).output();
    }
}


/// An iOS simulator, driven with `xcrun simctl`.
pub struct IosSim {
    pub udid: String,
}

impl IosSim {
    /// `id` is a simulator UDID or name; None picks the booted one.
    pub fn find(id: Option<&str>) -> Result<Option<Self>> {
        let out = run("xcrun", &["simctl", "list", "devices", "booted"])?;
        for line in out.lines() {
            let line = line.trim();
            let Some(open) = line.rfind(" (Booted)") else { continue };
            let head = &line[..open];
            let Some(start) = head.rfind('(') else { continue };
            let udid = &head[start + 1..head.len() - 1];
            let name = head[..start].trim();
            if id.is_none_or(|i| i == udid || i == name) {
                return Ok(Some(IosSim { udid: udid.to_string() }));
            }
        }
        Ok(None)
    }

    pub fn stop_app(&self, bundle: &str) -> Result<()> {
        let _ = Command::new("xcrun").args(["simctl", "terminate", &self.udid, bundle]).output();
        Ok(())
    }

    /// Maestro (`LocalSimulatorUtils.clearAppState`): terminate, then reinstall.
    pub fn clear_state(&self, bundle: &str) -> Result<()> {
        self.stop_app(bundle)?;
        let app = run("xcrun", &["simctl", "get_app_container", &self.udid, bundle, "app"])?.trim().to_string();
        let tmp = std::env::temp_dir().join(format!("tapwing-reinstall-{bundle}.app"));
        let _ = std::fs::remove_dir_all(&tmp);
        run("cp", &["-R", &app, tmp.to_str().unwrap()])?;
        run("xcrun", &["simctl", "uninstall", &self.udid, bundle])?;
        run("xcrun", &["simctl", "install", &self.udid, tmp.to_str().unwrap()])?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(())
    }

    /// Launch with the session's host port; returns the new process id.
    /// Dart on iOS sees no environment, so the port also goes to a file in
    /// the app's tmp directory (its data container), read by the driver.
    pub fn launch(&self, bundle: &str, port: u16) -> Result<i64> {
        let data = run("xcrun", &["simctl", "get_app_container", &self.udid, bundle, "data"])?.trim().to_string();
        let tmp = std::path::Path::new(&data).join("tmp");
        std::fs::create_dir_all(&tmp)?;
        std::fs::write(tmp.join("tapwing_port"), port.to_string())?;
        let out = Command::new("xcrun")
            .args(["simctl", "launch", &self.udid, bundle])
            .env("SIMCTL_CHILD_TAPWING_PORT", port.to_string())
            .output()
            .context("running simctl launch")?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            bail!("simctl launch {bundle} failed: {}{}", text, String::from_utf8_lossy(&out.stderr));
        }
        text.trim().rsplit(':').next().and_then(|p| p.trim().parse().ok()).with_context(|| format!("no pid in `{text}`"))
    }

    pub fn screenshot(&self, path: &str) -> Result<()> {
        run("xcrun", &["simctl", "io", &self.udid, "screenshot", path])?;
        Ok(())
    }
}

/// The device a session runs on.
pub enum Device {
    Android(Android),
    Ios(IosSim),
}

impl Device {
    pub fn serial(&self) -> &str {
        match self {
            Device::Android(a) => &a.serial,
            Device::Ios(i) => &i.udid,
        }
    }

    pub fn is_ios(&self) -> bool {
        matches!(self, Device::Ios(_))
    }

    pub fn stop_app(&self, app: &str) -> Result<()> {
        match self {
            Device::Android(a) => a.stop_app(app),
            Device::Ios(i) => i.stop_app(app),
        }
    }

    pub fn clear_state(&self, app: &str) -> Result<()> {
        match self {
            Device::Android(a) => a.clear_state(app),
            Device::Ios(i) => i.clear_state(app),
        }
    }

    /// Launch; on iOS also returns the pid so only that process is accepted.
    pub fn launch(&mut self, app: &str, port: u16) -> Result<Option<i64>> {
        match self {
            Device::Android(a) => a.launch(app).map(|_| None),
            Device::Ios(i) => i.launch(app, port).map(Some),
        }
    }

    pub fn screenshot(&self, path: &str) -> Result<()> {
        match self {
            Device::Android(a) => a.screenshot(path),
            Device::Ios(i) => i.screenshot(path),
        }
    }
}
