//! The `am` commands that start and stop Tapwing's Android driver.

use super::tapwing_apk;

/// The `am instrument` command; `-m` (protobuf status output) from API 26,
/// as Maestro 2.12.1's `AndroidDriver.kt` does.
pub fn instrument_command(api_level: u32, device_port: u16) -> String {
    let m = if api_level >= 26 { "-m " } else { "" };
    format!("am instrument -w {m}-e port {device_port} {}", tapwing_apk::INSTRUMENTATION)
}

/// The driver's input method (`TapwingInputMethodService.IME_ID`), which a
/// non-ASCII `inputText` selects for the moment it types.
pub const IME_ID: &str = "dev.tapwing.driver/.input.TapwingInputMethodService";

/// `settings` (global) key under which the driver records, before it selects
/// its own input method, what to put back: `<previous default IME or ->,<1 when
/// the Tapwing IME was already enabled, else 0>` (`UnicodeTextInput.kt`).
pub const IME_RESTORE_KEY: &str = "tapwing_ime_restore";

/// Stops every process of the driver, then finishes what a driver killed in
/// the middle of an IME `inputText` could not. Android deselects a keyboard
/// whose package is force-stopped, but it falls back to the system default
/// rather than to the keyboard the user had chosen, and Tapwing's stays
/// enabled. With a record present, this selects the recorded keyboard again
/// and disables Tapwing's. Runs before every start and at every teardown, so
/// an interrupted run is put right at the latest by the next one; without a
/// record (the usual case) it costs one `settings get`.
pub fn force_stop_command() -> String {
    format!(
        "am force-stop {pkg}; \
         r=$(settings get global {key} 2>/dev/null); \
         case \"$r\" in ''|null) ;; *) {{ \
         o=${{r%%,*}}; [ \"$o\" = - ] || ime set \"$o\"; \
         case \"$r\" in *,1) ;; *) ime disable '{ime}';; esac; \
         settings delete global {key}; \
         }} >/dev/null 2>&1;; esac",
        pkg = tapwing_apk::PACKAGE,
        key = IME_RESTORE_KEY,
        ime = IME_ID,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_command_requests_protobuf_status_from_api_26() {
        assert_eq!(
            instrument_command(35, 7001),
            "am instrument -w -m -e port 7001 dev.tapwing.driver/.TapwingDriverInstrumentation"
        );
        assert_eq!(
            instrument_command(24, 7002),
            "am instrument -w -e port 7002 dev.tapwing.driver/.TapwingDriverInstrumentation"
        );
    }

    #[test]
    fn force_stop_targets_the_driver_package() {
        assert!(force_stop_command().starts_with("am force-stop dev.tapwing.driver; "));
    }

    /// Runs the stop command under the host `sh` with `am`, `settings` and
    /// `ime` replaced by functions over a fake device, and returns the `ime`
    /// and `settings delete` calls it made.
    #[cfg(unix)]
    fn stop_calls(record: &str) -> Vec<String> {
        let script = format!(
            "am() {{ :; }}\n\
             settings() {{ case \"$1 $2 $3\" in \
             'get global {key}') echo \"$RECORD\";; \
             'delete global {key}') echo \"settings delete\" >&3;; esac; }}\n\
             ime() {{ echo \"ime $*\" >&3; }}\n\
             {{ {stop}; }} 3>&1 >/dev/null",
            key = IME_RESTORE_KEY,
            stop = force_stop_command(),
        );
        let out =
            std::process::Command::new("sh").args(["-c", &script]).env("RECORD", record).output().expect("running sh");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }

    #[cfg(unix)]
    #[test]
    fn stop_restores_the_keyboard_recorded_by_a_killed_driver() {
        let gboard = "com.google.android.inputmethod.latin/com.android.inputmethod.latin.LatinIME";
        let disable = format!("ime disable {IME_ID}");

        // The usual case: nothing was interrupted, no keyboard setting is touched.
        assert!(stop_calls("null").is_empty());
        assert!(stop_calls("").is_empty());

        // Killed mid-input: the recorded keyboard comes back, ours is disabled.
        assert_eq!(
            stop_calls(&format!("{gboard},0")),
            vec![format!("ime set {gboard}"), disable.clone(), "settings delete".into()]
        );
        // Ours was enabled before the driver used it: it stays enabled.
        assert_eq!(stop_calls(&format!("{gboard},1")), vec![format!("ime set {gboard}"), "settings delete".into()]);
        // No default keyboard was set when the driver took over: none to select.
        assert_eq!(stop_calls("-,0"), vec![disable, "settings delete".into()]);
        assert_eq!(stop_calls("-,1"), vec!["settings delete".to_string()]);
    }
}
