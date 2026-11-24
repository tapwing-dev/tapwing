//! Pure shell builders, validators, media planning, and output parsers for
//! Android host commands.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::process::run_with_timeout;

/// Deadline of `adb start-server` and of `adb devices` (which starts the
/// server too): the daemon needs a few seconds to come up and scan USB.
pub(in crate::device) const ADB_SERVER_START_TIMEOUT: Duration = Duration::from_secs(20);

/// Polls `child` until it exits or `timeout` passes.
pub(in crate::device) fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Single-quotes `s` for the device's `sh`.
pub(in crate::device) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Formats `launchArguments` (`BTreeMap<String, Value>` or `serde_json::Map<String, Value>`)
/// as typed `am start` intent extra flags matching Maestro's `AndroidLaunchArguments` /
/// `MaestroDriverService.launchApp`:
/// - `Value::Bool(b)` -> `--ez <quoted_key> <true|false>`
/// - `Value::Number(n)` -> `--ei` (fits in `i32`), `--el` (`i64`), or `--ed` (`f64`)
/// - `Value::String(s)` -> `--es <quoted_key> <quoted_val>`
/// - Other JSON values -> `--es <quoted_key> <quoted_json>`
pub fn format_launch_arguments<'a, I>(arguments: I) -> String
where
    I: IntoIterator<Item = (&'a String, &'a serde_json::Value)>,
{
    let mut parts = Vec::new();
    for (key, val) in arguments {
        let qkey = sh_quote(key);
        let flag = match val {
            serde_json::Value::Bool(b) => format!("--ez {qkey} {b}"),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    if i32::try_from(i).is_ok() { format!("--ei {qkey} {i}") } else { format!("--el {qkey} {i}") }
                } else if let Some(u) = n.as_u64() {
                    if i32::try_from(u).is_ok() {
                        format!("--ei {qkey} {u}")
                    } else if i64::try_from(u).is_ok() {
                        format!("--el {qkey} {u}")
                    } else {
                        format!("--ed {qkey} {u}")
                    }
                } else if let Some(f) = n.as_f64() {
                    format!("--ed {qkey} {f}")
                } else {
                    format!("--es {qkey} {}", sh_quote(&n.to_string()))
                }
            }
            serde_json::Value::String(s) => format!("--es {qkey} {}", sh_quote(s)),
            other => format!("--es {qkey} {}", sh_quote(&other.to_string())),
        };
        parts.push(flag);
    }
    parts.join(" ")
}

/// Package ids are interpolated into shell scripts, so only the characters
/// Android allows in them are accepted.
pub(super) fn validate_package_name(app_id: &str) -> Result<()> {
    if app_id.is_empty() || !app_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
        bail!("invalid Android package id `{app_id}`");
    }
    Ok(())
}

/// The `Package` Maestro pins `openLink` to when `browser: true`
/// (`AndroidDriver.openBrowser`): Chrome, then Firefox.
pub fn browser_package(pm_list_packages: &str) -> Option<&'static str> {
    let installed: Vec<&str> = pm_list_packages.lines().filter_map(|l| l.trim().strip_prefix("package:")).collect();
    ["com.android.chrome", "org.mozilla.firefox"].into_iter().find(|p| installed.contains(p))
}

/// `am start` command for `openLink`; the link is single-quoted for `sh`.
pub fn open_link_command(link: &str, package: Option<&str>) -> String {
    let mut cmd = format!("am start -a android.intent.action.VIEW -d {}", sh_quote(link));
    if let Some(p) = package {
        cmd.push(' ');
        cmd.push_str(p);
    }
    cmd
}

/// Whether `serial` is an Android emulator (`emulator-<port>`).
pub fn is_emulator(serial: &str) -> bool {
    serial.starts_with("emulator-")
}

/// `adb emu` arguments for `setLocation`; the console wants longitude first.
pub fn geo_fix_args(latitude: f64, longitude: f64) -> Vec<String> {
    vec!["geo".into(), "fix".into(), longitude.to_string(), latitude.to_string()]
}

/// `Settings.System.USER_ROTATION` for a Maestro `DeviceOrientation` name
/// (`AndroidDriver.setOrientation`).
pub fn user_rotation(orientation: &str) -> Result<u8> {
    match crate::flow::ir::orientation_by_name(orientation) {
        Some("PORTRAIT") => Ok(0),
        Some("LANDSCAPE_LEFT") => Ok(1),
        Some("UPSIDE_DOWN") => Ok(2),
        Some("LANDSCAPE_RIGHT") => Ok(3),
        _ => bail!("Unknown orientation: {orientation}"),
    }
}

/// Reading of `cmd connectivity airplane-mode`.
#[derive(Debug, PartialEq, Eq)]
pub enum AirplaneRead {
    State(bool),
    /// Old releases print nothing / "No shell command implementation."; read the setting instead.
    Fallback,
    Invalid(String),
}

pub fn parse_airplane_cmd(out: &str) -> AirplaneRead {
    match out.trim() {
        "enabled" => AirplaneRead::State(true),
        "disabled" => AirplaneRead::State(false),
        "" | "No shell command implementation." => AirplaneRead::Fallback,
        other => AirplaneRead::Invalid(other.to_string()),
    }
}

/// Parses `settings get global airplane_mode_on` (`0` / `1`).
pub fn parse_airplane_setting(out: &str) -> Result<bool> {
    match out.trim() {
        "0" => Ok(false),
        "1" => Ok(true),
        other => bail!("Received invalid response from while trying to read airplane mode state: {other}"),
    }
}

/// Where and how `addMedia` copies one local file.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct MediaPlan {
    pub(super) local: PathBuf,
    pub(super) remote_dir: &'static str,
    pub(super) remote_path: String,
}

/// Media types Maestro accepts (`MediaExt`) mapped to their MediaStore directory.
fn media_dir(extension: &str) -> Option<&'static str> {
    match extension {
        "png" | "jpeg" | "jpg" | "gif" => Some("/sdcard/Pictures"),
        "mp4" => Some("/sdcard/Movies"),
        _ => None,
    }
}

/// Device-safe file name: anything outside `[A-Za-z0-9._-]` becomes `_`.
pub fn sanitize_remote_name(name: &str) -> String {
    let clean: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
    if clean.is_empty() || clean.chars().all(|c| c == '.') { "media".to_string() } else { clean }
}

pub(super) fn plan_media(path: &Path) -> Result<MediaPlan> {
    if !path.is_file() {
        bail!("Media file not found: {}", path.display());
    }
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let Some(remote_dir) = media_dir(extension) else {
        bail!("Extension .{extension} is not yet supported for add media");
    };
    let name = sanitize_remote_name(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    Ok(MediaPlan { local: path.to_path_buf(), remote_path: format!("{remote_dir}/{name}"), remote_dir })
}

/// Registers pushed files with the MediaStore (`scan_file` on API 29+, the
/// legacy scanner broadcast before; each is best effort).
pub(super) fn media_scan_script(remote_paths: &[String]) -> String {
    let mut script = String::new();
    for p in remote_paths {
        let q = sh_quote(p);
        script.push_str(&format!(
            "content call --uri content://media/external/file --method scan_file --arg {q} >/dev/null 2>&1; \
             am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d {} >/dev/null 2>&1; ",
            sh_quote(&format!("file://{p}"))
        ));
    }
    script.push_str("true");
    script
}

/// Runs a host tool for at most `timeout` (see [`run_with_timeout`]) and
/// returns its stdout; a non-zero exit is an error carrying both streams.
pub(in crate::device) fn run(bin: &str, args: &[&str], timeout: Duration) -> Result<String> {
    let out = run_with_timeout(Command::new(bin).args(args), timeout)?;
    let stdout = out.stdout_lossy();
    if !out.status.success() {
        bail!("{bin} {} failed: {}{}", args.join(" "), stdout, out.stderr_lossy());
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::device::Android;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn package_names_are_validated() {
        assert!(validate_package_name("dev.tapwing.sample_app").is_ok());
        assert!(validate_package_name("").is_err());
        assert!(validate_package_name("dev.x; reboot").is_err());
    }

    #[test]
    fn orientation_maps_to_user_rotation() {
        assert_eq!(user_rotation("PORTRAIT").unwrap(), 0);
        assert_eq!(user_rotation("LANDSCAPE_LEFT").unwrap(), 1);
        assert_eq!(user_rotation("UPSIDE_DOWN").unwrap(), 2);
        assert_eq!(user_rotation("LANDSCAPE_RIGHT").unwrap(), 3);
        assert_eq!(user_rotation("landscapeLeft").unwrap(), 1);
        assert_eq!(user_rotation("upsidedown").unwrap(), 2);
        assert_eq!(user_rotation("Landscape_Right").unwrap(), 3);
        let err = user_rotation("sideways").unwrap_err().to_string();
        assert_eq!(err, "Unknown orientation: sideways");
    }

    #[test]
    fn geo_fix_puts_longitude_before_latitude() {
        assert_eq!(geo_fix_args(50.0755, 14.4378), strs(&["geo", "fix", "14.4378", "50.0755"]));
        assert_eq!(geo_fix_args(-33.5, -70.25), strs(&["geo", "fix", "-70.25", "-33.5"]));
        assert_eq!(geo_fix_args(0.0, 1e-7)[2], "0.0000001");
        assert!(is_emulator("emulator-5554"));
        assert!(!is_emulator("R5CX1234ABC"));
        assert!(!is_emulator("192.168.1.5:5555"));
    }

    #[test]
    fn physical_devices_cannot_set_location_without_the_native_lane() {
        let err = Android::new("R5CX1234ABC".into()).set_location(50.0, 14.0).unwrap_err().to_string();
        assert!(err.contains("native lane") && err.contains("R5CX1234ABC"), "{err}");
    }

    #[test]
    fn airplane_mode_output_is_parsed_like_maestro() {
        assert_eq!(parse_airplane_cmd("enabled\n"), AirplaneRead::State(true));
        assert_eq!(parse_airplane_cmd("disabled"), AirplaneRead::State(false));
        assert_eq!(parse_airplane_cmd("No shell command implementation.\n"), AirplaneRead::Fallback);
        assert_eq!(parse_airplane_cmd(""), AirplaneRead::Fallback);
        assert_eq!(parse_airplane_cmd("weird"), AirplaneRead::Invalid("weird".into()));
        assert!(!parse_airplane_setting("0\n").unwrap());
        assert!(parse_airplane_setting("1").unwrap());
        assert!(parse_airplane_setting("null").is_err());
        assert!(parse_airplane_setting("").is_err());
    }

    #[test]
    fn open_link_quotes_the_url_for_the_device_shell() {
        assert_eq!(
            open_link_command("https://example.com/a?b=1&c=2", None),
            "am start -a android.intent.action.VIEW -d 'https://example.com/a?b=1&c=2'"
        );
        assert_eq!(
            open_link_command("myapp://x?name=O'Brien", None),
            "am start -a android.intent.action.VIEW -d 'myapp://x?name=O'\\''Brien'"
        );
        assert!(open_link_command("a$(reboot)`id`\"", None).contains("'a$(reboot)`id`\"'"));
        assert!(open_link_command("https://e.com", Some("com.android.chrome")).ends_with("' com.android.chrome"));
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn browser_prefers_chrome_then_firefox() {
        let both = "package:org.mozilla.firefox\npackage:com.android.chrome\npackage:com.x\n";
        assert_eq!(browser_package(both), Some("com.android.chrome"));
        assert_eq!(browser_package("package:org.mozilla.firefox\n"), Some("org.mozilla.firefox"));
        assert_eq!(browser_package("package:com.android.chrome.beta\n"), None);
        assert_eq!(browser_package(""), None);
    }

    #[test]
    fn add_media_validates_before_touching_the_device() {
        let dir = std::env::temp_dir().join(format!("tapwing-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("my photo (1).png");
        let mp4 = dir.join("clip.mp4");
        let txt = dir.join("notes.txt");
        for f in [&png, &mp4, &txt] {
            std::fs::write(f, b"x").unwrap();
        }

        let p = plan_media(&png).unwrap();
        assert_eq!(p.remote_dir, "/sdcard/Pictures");
        assert_eq!(p.remote_path, "/sdcard/Pictures/my_photo__1_.png");
        assert_eq!(plan_media(&mp4).unwrap().remote_path, "/sdcard/Movies/clip.mp4");
        assert_eq!(plan_media(&txt).unwrap_err().to_string(), "Extension .txt is not yet supported for add media");
        let missing = dir.join("missing.png");
        assert!(plan_media(&missing).unwrap_err().to_string().starts_with("Media file not found"));
        assert!(plan_media(&dir).is_err(), "a directory is not a media file");

        let android = Android::new("no-such-device".into());
        let paths = vec![png.display().to_string(), missing.display().to_string()];
        let err = android.add_media(&paths).unwrap_err().to_string();
        assert!(err.starts_with("Media file not found"), "{err}");
        assert!(android.add_media(&[]).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_names_are_sanitized() {
        assert_eq!(sanitize_remote_name("photo.png"), "photo.png");
        assert_eq!(sanitize_remote_name("a b'c;d$e.jpg"), "a_b_c_d_e.jpg");
        assert_eq!(sanitize_remote_name("café.gif"), "caf_.gif");
        assert_eq!(sanitize_remote_name(""), "media");
        assert_eq!(sanitize_remote_name(".."), "media");
    }

    #[test]
    fn media_scan_registers_each_file_with_quoted_paths() {
        let script = media_scan_script(&["/sdcard/Pictures/a.png".into(), "/sdcard/Movies/b.mp4".into()]);
        assert_eq!(script.matches("scan_file --arg '/sdcard/").count(), 2);
        assert!(script.contains("-d 'file:///sdcard/Pictures/a.png'"));
        assert!(script.ends_with("true"));
    }

    #[test]
    fn format_launch_arguments_builds_typed_and_quoted_intent_extras() {
        assert_eq!(format_launch_arguments(&BTreeMap::new()), "");

        let mut args = BTreeMap::new();
        args.insert("boolTrue".into(), serde_json::json!(true));
        args.insert("boolFalse".into(), serde_json::json!(false));
        args.insert("intSmall".into(), serde_json::json!(42));
        args.insert("intNeg".into(), serde_json::json!(-2_147_483_648_i64));
        args.insert("longBig".into(), serde_json::json!(3_000_000_000_i64));
        args.insert("longNeg".into(), serde_json::json!(-3_000_000_000_i64));
        args.insert("floatVal".into(), serde_json::json!(3.125));
        args.insert("plainStr".into(), serde_json::json!("cart"));
        args.insert("quote'key$1".into(), serde_json::json!("O'Brien $(reboot) `id` \"x\""));
        args.insert("jsonArr".into(), serde_json::json!([1, "two"]));

        let formatted = format_launch_arguments(&args);
        assert_eq!(
            formatted,
            "--ez 'boolFalse' false \
             --ez 'boolTrue' true \
             --ed 'floatVal' 3.125 \
             --ei 'intNeg' -2147483648 \
             --ei 'intSmall' 42 \
             --es 'jsonArr' '[1,\"two\"]' \
             --el 'longBig' 3000000000 \
             --el 'longNeg' -3000000000 \
             --es 'plainStr' 'cart' \
             --es 'quote'\\''key$1' 'O'\\''Brien $(reboot) `id` \"x\"'"
        );
    }
}
