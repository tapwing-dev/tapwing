//! Android animation scale suppression (`platform.android.disableAnimations`
//! in `config.yaml`) over the pure-Rust ADB smartsocket ([`AdbClient`]).
//!
//! Reads the device's previous `window_animation_scale`,
//! `transition_animation_scale`, and `animator_duration_scale` and sets all
//! three to `0` in a single `adb shell` round-trip, restoring them via RAII in
//! [`AnimationScaleGuard::drop`].

use crate::device::adb_client::AdbClient;

/// Captured Android global animation scales prior to disabling them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnimationScales {
    pub window: Option<f64>,
    pub transition: Option<f64>,
    pub animator_duration: Option<f64>,
}

/// Single-round-trip shell script that prints current animation scales with
/// `TAPWING_ANIM_*` sentinels and sets all three global scales to `0`.
pub fn disable_animations_script() -> &'static str {
    "printf 'TAPWING_ANIM_WIN=%s\\n' \"$(settings get global window_animation_scale 2>/dev/null)\"; \
     printf 'TAPWING_ANIM_TRANS=%s\\n' \"$(settings get global transition_animation_scale 2>/dev/null)\"; \
     printf 'TAPWING_ANIM_DUR=%s\\n' \"$(settings get global animator_duration_scale 2>/dev/null)\"; \
     settings put global window_animation_scale 0 && \
     settings put global transition_animation_scale 0 && \
     settings put global animator_duration_scale 0"
}

fn parse_scale(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// Parses the sentinel output of [`disable_animations_script`].
pub fn parse_animation_scales(out: &str) -> AnimationScales {
    let mut scales = AnimationScales::default();
    for line in out.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("TAPWING_ANIM_WIN=") {
            scales.window = parse_scale(v);
        } else if let Some(v) = line.strip_prefix("TAPWING_ANIM_TRANS=") {
            scales.transition = parse_scale(v);
        } else if let Some(v) = line.strip_prefix("TAPWING_ANIM_DUR=") {
            scales.animator_duration = parse_scale(v);
        }
    }
    scales
}

fn format_scale(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:.1}") } else { format!("{v}") }
}

/// Builds the single-round-trip shell command restoring `prev` animation scales.
pub fn restore_animations_script(prev: &AnimationScales) -> String {
    let mut cmds = Vec::with_capacity(3);
    for (key, val) in [
        ("window_animation_scale", prev.window),
        ("transition_animation_scale", prev.transition),
        ("animator_duration_scale", prev.animator_duration),
    ] {
        match val.filter(|v| v.is_finite() && *v >= 0.0) {
            Some(v) => cmds.push(format!("settings put global {key} {}", format_scale(v))),
            None => cmds.push(format!("(settings delete global {key} 2>/dev/null || settings put global {key} 1.0)")),
        }
    }
    cmds.join("; ")
}

/// RAII guard that restores Android global animation scales on drop.
pub struct AnimationScaleGuard {
    pub(super) serial: String,
    pub(super) adb: AdbClient,
    pub(super) previous: AnimationScales,
    pub(super) disarmed: bool,
}

impl AnimationScaleGuard {
    pub fn previous(&self) -> &AnimationScales {
        &self.previous
    }

    pub(super) fn cleanup(&mut self) {
        if self.disarmed {
            return;
        }
        self.disarmed = true;
        let script = restore_animations_script(&self.previous);
        let _ = self.adb.shell(&self.serial, &script);
    }
}

impl Drop for AnimationScaleGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::device::Android;

    #[test]
    fn parses_finite_non_negative_scales_and_maps_null_or_garbage_to_none() {
        let out = "TAPWING_ANIM_WIN=1.0\nTAPWING_ANIM_TRANS=0.5\nTAPWING_ANIM_DUR=null\n";
        let scales = parse_animation_scales(out);
        assert_eq!(scales, AnimationScales { window: Some(1.0), transition: Some(0.5), animator_duration: None });

        let bad = "TAPWING_ANIM_WIN=-1.0\nTAPWING_ANIM_TRANS=NaN\nTAPWING_ANIM_DUR=garbage\n";
        assert_eq!(parse_animation_scales(bad), AnimationScales::default());
    }

    #[test]
    fn restore_script_puts_saved_values_and_deletes_or_defaults_missing_ones() {
        let prev = AnimationScales { window: Some(1.0), transition: Some(0.5), animator_duration: None };
        assert_eq!(
            restore_animations_script(&prev),
            "settings put global window_animation_scale 1.0; \
             settings put global transition_animation_scale 0.5; \
             (settings delete global animator_duration_scale 2>/dev/null || settings put global animator_duration_scale 1.0)"
        );

        let non_finite =
            AnimationScales { window: Some(f64::NAN), transition: Some(-1.0), animator_duration: Some(f64::INFINITY) };
        let sanitized = restore_animations_script(&non_finite);
        assert!(!sanitized.contains("NaN") && !sanitized.contains("-1") && !sanitized.contains("inf"));
        assert_eq!(sanitized.matches("settings delete global").count(), 3);
        assert!(disable_animations_script().contains("window_animation_scale 0 && "));
    }

    fn read_adb_cmd(stream: &mut TcpStream) -> String {
        let mut len_hex = [0u8; 4];
        stream.read_exact(&mut len_hex).unwrap();
        let len = usize::from_str_radix(std::str::from_utf8(&len_hex).unwrap(), 16).unwrap();
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn write_shell_v2_exit(stream: &mut TcpStream, stdout: &[u8]) {
        if !stdout.is_empty() {
            let mut h = [0u8; 5];
            h[0] = 1; // STDOUT
            h[1..].copy_from_slice(&(stdout.len() as u32).to_le_bytes());
            stream.write_all(&h).unwrap();
            stream.write_all(stdout).unwrap();
        }
        let mut h = [0u8; 5];
        h[0] = 3; // EXIT
        h[1..].copy_from_slice(&1u32.to_le_bytes());
        stream.write_all(&h).unwrap();
        stream.write_all(&[0]).unwrap();
        stream.flush().unwrap();
    }

    #[test]
    fn disable_animations_and_guard_drop_round_trip_over_mock_loopback_adb() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_cmds = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_clone = Arc::clone(&seen_cmds);

        let server = thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                assert_eq!(read_adb_cmd(&mut stream), "host:transport:emulator-5554");
                stream.write_all(b"OKAY").unwrap();
                let cmd = read_adb_cmd(&mut stream);
                seen_clone.lock().unwrap().push(cmd);
                stream.write_all(b"OKAY").unwrap();
                if idx == 0 {
                    write_shell_v2_exit(
                        &mut stream,
                        b"TAPWING_ANIM_WIN=1.0\nTAPWING_ANIM_TRANS=0.25\nTAPWING_ANIM_DUR=null\n",
                    );
                } else {
                    write_shell_v2_exit(&mut stream, b"");
                }
            }
        });

        let client = AdbClient::with_addr(addr).with_timeouts(Duration::from_secs(2), Duration::from_secs(5));
        let android = Android::with_adb_client("emulator-5554".into(), client);
        {
            let guard = android.disable_animations().unwrap();
            assert_eq!(
                guard.previous(),
                &AnimationScales { window: Some(1.0), transition: Some(0.25), animator_duration: None }
            );
        }
        server.join().unwrap();

        let cmds = seen_cmds.lock().unwrap();
        assert_eq!(cmds.len(), 2);
        assert!(cmds[0].contains("settings put global window_animation_scale 0"));
        assert!(cmds[1].contains("settings put global window_animation_scale 1.0"));
        assert!(cmds[1].contains("settings put global transition_animation_scale 0.25"));
        assert!(cmds[1].contains("settings delete global animator_duration_scale"));
    }
}
