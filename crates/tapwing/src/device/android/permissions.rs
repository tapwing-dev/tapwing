//! Android permission shorthand mapping, `dumpsys package` parsing, and
//! batched `pm`/`appops` script generation for `setPermissions` & `launchApp`.

use anyhow::Result;
use std::collections::BTreeMap;

use super::Android;
use super::helpers::validate_package_name;

const MANAGE_EXTERNAL_STORAGE: &str = "android.permission.MANAGE_EXTERNAL_STORAGE";

/// Result of [`Android::set_permissions`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PermissionOutcome {
    /// `pm`/`appops` calls that succeeded (non-changeable permissions fail and
    /// are ignored, like Maestro).
    pub applied: usize,
    /// How many of those revoked a permission; Android kills the app process
    /// when a runtime permission is revoked.
    pub revoked: usize,
}

impl Android {
    /// `setPermissions` (`AndroidDriver.setPermissions`, `AndroidDriver.kt:764-777`):
    /// `all` first (every runtime permission the app declares, plus the
    /// `MANAGE_EXTERNAL_STORAGE` app-op), then the explicit entries, which
    /// override it. One `adb shell` applies everything; calls that fail
    /// (non-changeable permissions) are ignored like in Maestro.
    pub fn set_permissions(
        &mut self,
        app_id: &str,
        permissions: &BTreeMap<String, String>,
    ) -> Result<PermissionOutcome> {
        validate_package_name(app_id)?;
        let all_targets =
            if permissions.contains_key("all") { self.all_permission_targets(app_id)? } else { Vec::new() };
        let script = build_permissions_script(app_id, &all_targets, permissions);
        if script.is_empty() {
            return Ok(PermissionOutcome::default());
        }
        let out = self.shell(&script)?;
        let count = |marker: &str| {
            out.lines()
                .find_map(|l| l.trim().strip_prefix(marker))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0)
        };
        Ok(PermissionOutcome { applied: count("TAPWING_PERM_OK:"), revoked: count("TAPWING_PERM_REVOKED:") })
    }

    /// Permissions `all` applies to: the app's runtime permissions plus
    /// `MANAGE_EXTERNAL_STORAGE` when requested. Uses the metadata cached by
    /// `launchApp` when present, one `dumpsys package` otherwise.
    fn all_permission_targets(&self, app_id: &str) -> Result<Vec<String>> {
        if let Some(info) = self.app_cache.get(app_id) {
            let mut targets = info.runtime_permissions.clone();
            if info.has_manage_external_storage {
                targets.push(MANAGE_EXTERNAL_STORAGE.to_string());
            }
            return Ok(targets);
        }
        let dump = self.shell(&format!("dumpsys package {app_id}"))?;
        Ok(all_permission_targets_from_dump(&dump))
    }
}

/// Permission names listed under `header` in `dumpsys package` (entries are
/// indented deeper than the header; `name` or `name: flags`).
fn section_permissions(dump: &str, header: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut lines = dump.lines();
    while let Some(line) = lines.next() {
        if line.trim() != header {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        for entry in lines.by_ref() {
            if entry.trim().is_empty() || entry.len() - entry.trim_start().len() <= indent {
                break;
            }
            let name = entry.trim().split(':').next().unwrap_or("").trim();
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
                && !out.iter().any(|n| n == name)
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// What `all` covers for an app, from its `dumpsys package` output: runtime
/// permissions plus the `MANAGE_EXTERNAL_STORAGE` app-op when requested.
pub fn all_permission_targets_from_dump(dump: &str) -> Vec<String> {
    let mut targets = section_permissions(dump, "runtime permissions:");
    if section_permissions(dump, "requested permissions:").iter().any(|p| p == MANAGE_EXTERNAL_STORAGE) {
        targets.push(MANAGE_EXTERNAL_STORAGE.to_string());
    }
    targets
}

/// One `pm grant|revoke` (or `appops set` for app-op permissions) statement.
fn permission_op(app_id: &str, permission: &str, value: &str) -> String {
    if permission == MANAGE_EXTERNAL_STORAGE {
        let op = translate_appops_value(value);
        return format!("appops set --uid {app_id} MANAGE_EXTERNAL_STORAGE {op} >/dev/null 2>&1 && OK=$((OK+1)); ");
    }
    let verb = translate_permission_value(value);
    let revoked = if verb == "revoke" { " REV=$((REV+1));" } else { "" };
    format!("pm {verb} {app_id} {permission} >/dev/null 2>&1 && {{ OK=$((OK+1));{revoked} }}; ")
}

/// Builds the single shell script behind `setPermissions`: `all` first, then
/// each explicit entry (which therefore wins). Empty when there is nothing to do.
fn build_permissions_script(app_id: &str, all_targets: &[String], permissions: &BTreeMap<String, String>) -> String {
    let mut ops = String::new();
    if let Some(value) = permissions.get("all") {
        for permission in all_targets {
            ops.push_str(&permission_op(app_id, permission, value));
        }
    }
    for (name, value) in permissions.iter().filter(|(name, _)| name.as_str() != "all") {
        for permission in translate_permission_name(name) {
            ops.push_str(&permission_op(app_id, &permission, value));
        }
    }
    if ops.is_empty() {
        return ops;
    }
    format!("OK=0; REV=0; {ops}echo TAPWING_PERM_OK:$OK; echo TAPWING_PERM_REVOKED:$REV")
}

/// Translates Maestro permission shorthands to Android permission names
/// (`AndroidDriver.kt:1079-1137`).
pub fn translate_permission_name(name: &str) -> Vec<String> {
    match name {
        "bluetooth" => {
            vec!["android.permission.BLUETOOTH_CONNECT".into(), "android.permission.BLUETOOTH_SCAN".into()]
        }
        "calendar" => {
            vec!["android.permission.WRITE_CALENDAR".into(), "android.permission.READ_CALENDAR".into()]
        }
        "camera" => vec!["android.permission.CAMERA".into()],
        "contacts" => {
            vec!["android.permission.READ_CONTACTS".into(), "android.permission.WRITE_CONTACTS".into()]
        }
        "location" => {
            vec!["android.permission.ACCESS_FINE_LOCATION".into(), "android.permission.ACCESS_COARSE_LOCATION".into()]
        }
        "medialibrary" => vec![
            "android.permission.WRITE_EXTERNAL_STORAGE".into(),
            "android.permission.READ_EXTERNAL_STORAGE".into(),
            "android.permission.READ_MEDIA_AUDIO".into(),
            "android.permission.READ_MEDIA_IMAGES".into(),
            "android.permission.READ_MEDIA_VIDEO".into(),
        ],
        "microphone" => vec!["android.permission.RECORD_AUDIO".into()],
        "notifications" => vec!["android.permission.POST_NOTIFICATIONS".into()],
        "phone" => vec!["android.permission.CALL_PHONE".into(), "android.permission.ANSWER_PHONE_CALLS".into()],
        "sms" => vec![
            "android.permission.READ_SMS".into(),
            "android.permission.RECEIVE_SMS".into(),
            "android.permission.SEND_SMS".into(),
        ],
        "storage" => {
            vec!["android.permission.WRITE_EXTERNAL_STORAGE".into(), "android.permission.READ_EXTERNAL_STORAGE".into()]
        }
        other => {
            let cleaned: String =
                other.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_').collect();
            if cleaned.is_empty() { vec![] } else { vec![cleaned] }
        }
    }
}

/// Translates Maestro permission values (`allow`/`deny`/`unset`) to `pm` subcommands
/// (`AndroidDriver.kt:1139-1146`).
pub(super) fn translate_permission_value(value: &str) -> &'static str {
    match value {
        "allow" => "grant",
        _ => "revoke",
    }
}

/// Translates Maestro permission values for `appops set` (`AndroidDriver.kt:1070-1074`).
pub(super) fn translate_appops_value(value: &str) -> &'static str {
    match value {
        "allow" => "allow",
        "deny" => "deny",
        _ => "default",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn translates_every_maestro_permission_shorthand() {
        assert_eq!(translate_permission_name("camera"), strs(&["android.permission.CAMERA"]));
        assert_eq!(
            translate_permission_name("location"),
            strs(&["android.permission.ACCESS_FINE_LOCATION", "android.permission.ACCESS_COARSE_LOCATION"])
        );
        assert_eq!(
            translate_permission_name("bluetooth"),
            strs(&["android.permission.BLUETOOTH_CONNECT", "android.permission.BLUETOOTH_SCAN"])
        );
        assert_eq!(translate_permission_name("medialibrary").len(), 5);
        assert_eq!(translate_permission_name("storage").len(), 2);
        assert_eq!(translate_permission_name("sms").len(), 3);
        assert_eq!(translate_permission_name("phone").len(), 2);
        assert_eq!(translate_permission_name("calendar").len(), 2);
        assert_eq!(translate_permission_name("contacts").len(), 2);
        assert_eq!(translate_permission_name("notifications"), strs(&["android.permission.POST_NOTIFICATIONS"]));
        assert_eq!(translate_permission_name("microphone"), strs(&["android.permission.RECORD_AUDIO"]));
        assert_eq!(
            translate_permission_name("android.permission.ACTIVITY_RECOGNITION"),
            strs(&["android.permission.ACTIVITY_RECOGNITION"])
        );
        assert_eq!(translate_permission_name("a b;rm -rf /"), strs(&["abrmrf"]));
        assert!(translate_permission_name(";;; ").is_empty());
    }

    #[test]
    fn permission_values_map_to_pm_verbs_and_appops() {
        assert_eq!(translate_permission_value("allow"), "grant");
        assert_eq!(translate_permission_value("deny"), "revoke");
        assert_eq!(translate_permission_value("unset"), "revoke");
        assert_eq!(translate_appops_value("allow"), "allow");
        assert_eq!(translate_appops_value("deny"), "deny");
        assert_eq!(translate_appops_value("unset"), "default");
    }

    #[test]
    fn permissions_script_applies_all_then_explicit_overrides() {
        let targets = strs(&["android.permission.CAMERA", "android.permission.RECORD_AUDIO", MANAGE_EXTERNAL_STORAGE]);
        let script = build_permissions_script("dev.x", &targets, &perms(&[("all", "deny"), ("camera", "allow")]));
        let revoke_cam = script.find("pm revoke dev.x android.permission.CAMERA").unwrap();
        let grant_cam = script.find("pm grant dev.x android.permission.CAMERA").unwrap();
        assert!(revoke_cam < grant_cam, "explicit entries must come after `all`: {script}");
        assert!(script.contains("pm revoke dev.x android.permission.RECORD_AUDIO"));
        assert!(script.contains("appops set --uid dev.x MANAGE_EXTERNAL_STORAGE deny"));
        assert!(script.ends_with("echo TAPWING_PERM_OK:$OK; echo TAPWING_PERM_REVOKED:$REV"));

        let allow = build_permissions_script("dev.x", &targets, &perms(&[("all", "allow")]));
        assert!(allow.contains("pm grant dev.x android.permission.CAMERA"));
        assert!(allow.contains("appops set --uid dev.x MANAGE_EXTERNAL_STORAGE allow"));
        assert!(!allow.contains("REV=$((REV+1))"));

        let unset = build_permissions_script("dev.x", &targets, &perms(&[("all", "unset")]));
        assert!(unset.contains("pm revoke dev.x android.permission.CAMERA"));
        assert!(unset.contains("appops set --uid dev.x MANAGE_EXTERNAL_STORAGE default"));

        let loc = build_permissions_script("dev.x", &[], &perms(&[("location", "allow")]));
        assert!(loc.contains("pm grant dev.x android.permission.ACCESS_FINE_LOCATION"));
        assert!(loc.contains("pm grant dev.x android.permission.ACCESS_COARSE_LOCATION"));
        let mes = build_permissions_script("dev.x", &[], &perms(&[(MANAGE_EXTERNAL_STORAGE, "allow")]));
        assert!(mes.contains("appops set --uid dev.x MANAGE_EXTERNAL_STORAGE allow") && !mes.contains("pm grant"));

        assert_eq!(build_permissions_script("dev.x", &targets, &BTreeMap::new()), "");
    }

    #[test]
    fn all_targets_come_from_runtime_section_and_manage_storage_request() {
        let dump = "\
Packages:
  Package [dev.x] (abc):
    requested permissions:
      android.permission.INTERNET
      android.permission.CAMERA: restricted=true
      android.permission.MANAGE_EXTERNAL_STORAGE
    install permissions:
      android.permission.INTERNET: granted=true
    User 0: ceDataInode=1 installed=true
      runtime permissions:
        android.permission.CAMERA: granted=false, flags=[ USER_SET]
        android.permission.RECORD_AUDIO: granted=true
    User 10: installed=false
";
        assert_eq!(
            all_permission_targets_from_dump(dump),
            strs(&["android.permission.CAMERA", "android.permission.RECORD_AUDIO", MANAGE_EXTERNAL_STORAGE])
        );
        assert_eq!(
            section_permissions(dump, "requested permissions:"),
            strs(&["android.permission.INTERNET", "android.permission.CAMERA", MANAGE_EXTERNAL_STORAGE])
        );
        assert!(all_permission_targets_from_dump("Unable to find package: dev.x").is_empty());
    }
}
