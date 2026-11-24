//! `launchApp` and `setPermissions` against a fake ADB server: the exact
//! scripts sent, and what they do once a shell has parsed them.
//!
//! The device is a [`ShDevice`]: the generated script runs under the host's
//! `sh` with `am`, `pm`, `pidof`, ... as functions that record their
//! arguments. A quoting mistake therefore shows up as the wrong arguments to
//! `am start` (or as a call to a command that was only ever data), not as a
//! string that merely looks right. Every script is also checked with `sh -n`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::Android;
use crate::device::adb_client::fake::{FakeAdb, ShDevice, sh_available};

const APP: &str = "com.example.app";
const SERIAL: &str = "fake-serial";
const COMPONENT: &str = "com.example.app/.MainActivity";

/// A device with `com.example.app` installed. It declares three runtime
/// permissions and `MANAGE_EXTERNAL_STORAGE`; `BLUETOOTH_SCAN` cannot be
/// changed (as on a device where it is not a runtime permission). The app's
/// process is `111` while it runs and `4242` after an `am start`.
const DEVICE: &str = r#"
date() { echo 1700000000000; }
sleep() { :; }
reboot() { rec reboot "$@"; }
id() { rec id "$@"; }
running() { [ -e "$TAPWING_FAKE_DIR/running" ]; }
pidof() {
  if [ -e "$TAPWING_FAKE_DIR/started" ]; then echo 4242; elif running; then echo 111; fi
}
am() {
  rec am "$@"
  case "$1" in
    force-stop) rm -f "$TAPWING_FAKE_DIR/running" ;;
    start)
      if [ -e "$TAPWING_FAKE_DIR/am-start-fails" ]; then
        echo "Starting: Intent { act=android.intent.action.MAIN cmp=com.example.app/.Nope }"
        echo "Error type 3"
        echo "Error: Activity class {com.example.app/com.example.app.Nope} does not exist."
      else
        : > "$TAPWING_FAKE_DIR/started"
        echo "Starting: Intent { act=android.intent.action.MAIN cat=[android.intent.category.LAUNCHER] cmp=$(cat "$TAPWING_FAKE_DIR/component" 2>/dev/null) }"
      fi ;;
  esac
}
pm() {
  rec pm "$@"
  case "$1" in
    clear)
      if [ -e "$TAPWING_FAKE_DIR/pm-clear-fails" ]; then echo "Error: java.lang.SecurityException"; echo "  at android.os.Parcel"; return 1; fi
      rm -f "$TAPWING_FAKE_DIR/running"; echo Success ;;
    grant|revoke)
      case "$3" in *.BLUETOOTH_SCAN) echo "not a changeable permission type" >&2; return 1 ;; esac ;;
  esac
}
appops() { rec appops "$@"; }
cmd() {
  rec cmd "$@"
  if [ -e "$TAPWING_FAKE_DIR/no-launcher" ]; then echo "No activity found"; return; fi
  printf 'priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=false\r\n%s\r\n' "$(cat "$TAPWING_FAKE_DIR/component")"
}
dumpsys() {
  rec dumpsys "$@"
  cat <<'DUMP'
Packages:
  Package [com.example.app] (abc123):
    requested permissions:
      android.permission.INTERNET
      android.permission.CAMERA
      android.permission.MANAGE_EXTERNAL_STORAGE
    install permissions:
      android.permission.INTERNET: granted=true
    User 0: ceDataInode=1 installed=true hidden=false
      runtime permissions:
        android.permission.CAMERA: granted=false, flags=[ USER_SET]
        android.permission.RECORD_AUDIO: granted=true
        android.permission.POST_NOTIFICATIONS: granted=false
    User 10: installed=false
DUMP
}
"#;

struct Rig {
    // Declared first, dropped last: the device answers until the fake server is gone.
    device: Arc<ShDevice>,
    fake: FakeAdb,
    android: Android,
}

impl Rig {
    fn new() -> Self {
        Self::with_component(COMPONENT)
    }

    fn with_component(component: &str) -> Self {
        let device = ShDevice::new(DEVICE);
        let fake = FakeAdb::with_device(&device);
        let android = Android::with_adb_client(SERIAL.into(), fake.client());
        let rig = Self { device, fake, android };
        rig.flag("component", component);
        rig
    }

    /// Writes a state file the stubs look at.
    fn flag(&self, name: &str, content: &str) {
        std::fs::write(self.device.dir().join(name), content).expect("writing a fake device flag");
    }

    fn launch(
        &mut self,
        clear_state: bool,
        stop_app: bool,
        permissions: &[(&str, &str)],
        arguments: &[(&str, Value)],
    ) -> anyhow::Result<crate::device::LaunchMetrics> {
        let arguments: Vec<(String, Value)> = arguments.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.android.prepare_and_launch(
            APP,
            clear_state,
            stop_app,
            &perms(permissions),
            arguments.iter().map(|(k, v)| (k, v)),
        )
    }

    /// The arguments `am start` got after the fixed intent and component.
    fn start_extras(&self) -> Vec<String> {
        let starts: Vec<Vec<String>> = self
            .device
            .calls()
            .into_iter()
            .filter(|c| c.starts_with(&["am".to_string(), "start".to_string()]))
            .collect();
        assert_eq!(starts.len(), 1, "exactly one `am start`: {starts:?}");
        let fixed = ["am", "start", "-a", "android.intent.action.MAIN", "-c", "android.intent.category.LAUNCHER", "-n"];
        assert_eq!(starts[0][..fixed.len()], fixed, "{:?}", starts[0]);
        starts[0][fixed.len() + 1..].to_vec()
    }
}

fn perms(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Skips a test that needs to run scripts where there is no `sh` (non-unix).
macro_rules! require_sh {
    () => {
        if !sh_available() {
            eprintln!("skipped: no sh on this platform");
            return;
        }
    };
}

// --- launchApp ---------------------------------------------------------

#[test]
fn a_plain_launch_sends_this_exact_script() {
    require_sh!();
    let mut rig = Rig::new();
    let metrics = rig.launch(false, false, &[], &[]).unwrap();

    assert_eq!(
        rig.fake.shells(),
        vec![concat!(
            "T0=$(date +%s%3N); OLD=; T1=$(date +%s%3N); GRANTED=0; ",
            "DUMP=$(dumpsys package com.example.app 2>/dev/null); ",
            "RPERMS=$(printf '%s\\n' \"$DUMP\" | sed -n '/runtime permissions:/,/User /p' ",
            "| grep -oE '[A-Za-z0-9._]+: granted=(true|false)' | sed 's/: granted=.*//'); ",
            "for P in $RPERMS; do echo \"TAPWING_RPERM:$P\"; done; ",
            "case \"$DUMP\" in *android.permission.MANAGE_EXTERNAL_STORAGE*) echo 'TAPWING_MANAGE_STORAGE:1'; ;; esac; ",
            "T2=$(date +%s%3N); ",
            "COMP=$(cmd package resolve-activity --brief -c android.intent.category.LAUNCHER com.example.app 2>&1 ",
            "| grep '/' | tail -n 1 | tr -d '\\r'); ",
            "if [ -z \"$COMP\" ]; then echo 'TAPWING_ERR:no launcher activity'; exit 12; fi; ",
            "echo \"TAPWING_COMP:$COMP\"; ",
            "START_OUT=$(am start -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -n \"$COMP\" 2>&1); ",
            "START_RC=$?; ",
            "if [ \"$START_RC\" -ne 0 ] || printf '%s\\n' \"$START_OUT\" | tr -d '\\r' | grep -qE '^[ \\t]*Error($|[: \\t])'; then ",
            "echo \"TAPWING_ERR:am start failed: $(printf '%s' \"$START_OUT\" | tr '\\n' ' ')\"; exit 13; fi; ",
            "T3=$(date +%s%3N); ",
            "PID=; N=0; while [ -z \"$PID\" ] && [ $N -lt 150 ]; do ",
            "for P in $(pidof com.example.app 2>/dev/null); do ",
            "case \" $OLD \" in *\" $P \"*) ;; *) PID=$P ;; esac; done; ",
            "[ -z \"$PID\" ] && sleep 0.01; N=$((N+1)); done; ",
            "echo \"TAPWING_PID:$PID\"; echo \"TAPWING_GRANTED:$GRANTED\"; echo \"TAPWING_T:$T0:$T1:$T2:$T3\""
        )
        .to_string()]
    );
    rig.fake.assert_scripts_parse();

    // What the device made of it: the launcher was resolved and started, nothing else touched.
    assert_eq!((metrics.pid, metrics.old_pids.as_slice(), metrics.granted_permissions), (Some(4242), &[][..], 0));
    assert_eq!(rig.device.lines("dumpsys"), vec!["dumpsys package com.example.app"]);
    assert_eq!(
        rig.device.lines("cmd"),
        vec!["cmd package resolve-activity --brief -c android.intent.category.LAUNCHER com.example.app"]
    );
    assert_eq!(
        rig.device.lines("am"),
        vec![format!("am start -a android.intent.action.MAIN -c android.intent.category.LAUNCHER -n {COMPONENT}")]
    );
    assert!(rig.device.lines("pm").is_empty() && rig.device.lines("appops").is_empty());
}

#[test]
fn clear_state_and_permissions_run_in_order_before_the_start() {
    require_sh!();
    let mut rig = Rig::new();
    rig.flag("running", "");
    let metrics =
        rig.launch(true, false, &[("all", "allow"), ("notifications", "deny"), ("bluetooth", "allow")], &[]).unwrap();
    rig.fake.assert_scripts_parse();

    let script = &rig.fake.shells()[0];
    for statement in [
        "OLD=$(pidof com.example.app 2>/dev/null); echo \"TAPWING_OLDPIDS:$OLD\"; ",
        "CLR=$(pm clear com.example.app 2>&1); case \"$CLR\" in *Success*) ;; \
         *) echo \"TAPWING_ERR:pm clear failed: $(printf '%s' \"$CLR\" | tr '\\n' ' ')\"; exit 11 ;; esac; ",
        "for P in $RPERMS; do echo \"TAPWING_RPERM:$P\"; \
         pm grant com.example.app \"$P\" 2>/dev/null && GRANTED=$((GRANTED+1)) || true; done; ",
        "echo 'TAPWING_MANAGE_STORAGE:1'; \
         appops set --uid com.example.app MANAGE_EXTERNAL_STORAGE allow 2>/dev/null || true; ;; esac; ",
        "pm grant com.example.app android.permission.BLUETOOTH_SCAN 2>/dev/null && GRANTED=$((GRANTED+1)) || true; ",
        "pm revoke com.example.app android.permission.POST_NOTIFICATIONS 2>/dev/null && GRANTED=$((GRANTED+1)) || true; ",
    ] {
        assert!(script.contains(statement), "missing `{statement}` in\n{script}");
    }
    assert!(!script.contains("am force-stop"), "clearState already stops the app: {script}");

    // `all` first (what the app declares), then the explicit entries in name order.
    assert_eq!(
        rig.device.lines("pm"),
        vec![
            "pm clear com.example.app",
            "pm grant com.example.app android.permission.CAMERA",
            "pm grant com.example.app android.permission.RECORD_AUDIO",
            "pm grant com.example.app android.permission.POST_NOTIFICATIONS",
            "pm grant com.example.app android.permission.BLUETOOTH_CONNECT",
            "pm grant com.example.app android.permission.BLUETOOTH_SCAN",
            "pm revoke com.example.app android.permission.POST_NOTIFICATIONS",
        ]
    );
    assert_eq!(rig.device.lines("appops"), vec!["appops set --uid com.example.app MANAGE_EXTERNAL_STORAGE allow"]);
    let order: Vec<String> = rig.device.calls().into_iter().map(|c| c[..2.min(c.len())].join(" ")).collect();
    let position =
        |call: &str| order.iter().position(|c| c == call).unwrap_or_else(|| panic!("no `{call}`: {order:?}"));
    assert!(position("pm clear") < position("pm grant") && position("pm revoke") < position("am start"), "{order:?}");

    // The one permission the device refused is not counted; the old process is told from the new.
    assert_eq!(metrics.granted_permissions, 5);
    assert_eq!((metrics.pid, metrics.old_pids), (Some(4242), vec![111]));
}

#[test]
fn stop_app_force_stops_without_clearing() {
    require_sh!();
    let mut rig = Rig::new();
    rig.flag("running", "");
    let metrics = rig.launch(false, true, &[], &[]).unwrap();
    rig.fake.assert_scripts_parse();
    assert!(rig.fake.shells()[0].contains("echo \"TAPWING_OLDPIDS:$OLD\"; am force-stop com.example.app; T1="));
    assert_eq!(rig.device.lines("am")[0], "am force-stop com.example.app");
    assert!(rig.device.lines("pm").is_empty(), "stopApp must not clear the app's data");
    assert_eq!((metrics.pid, metrics.old_pids), (Some(4242), vec![111]));
}

#[test]
fn launch_arguments_reach_am_start_exactly_as_written() {
    require_sh!();
    let hostile = [
        ("name", json!("O'Brien")),
        ("spaced key", json!("two  words ")),
        ("dollar", json!("$HOME $(reboot) `id` ${PATH} $((1+1))")),
        ("multi\nline", json!("first\nsecond 'single' \"double\"\n")),
        ("chain", json!("a; reboot; b && id | id > /sdcard/x &")),
        ("-n", json!("--es injected value")),
        ("empty", json!("")),
        ("glob", json!("* ? [a-z] ~ #comment \\ \\n \\' !")),
        ("quotes'\"'", json!("'\"'\"'")),
        ("unicode", json!("žluťoučký 🚀")),
    ];
    let typed = [
        ("flag", json!(true)),
        ("count", json!(-42)),
        ("big", json!(3_000_000_000_i64)),
        ("ratio", json!(0.5)),
        ("list", json!(["a b", "c'd"])),
        ("nothing", json!(null)),
    ];
    let mut rig = Rig::new();
    let arguments: Vec<(&str, Value)> = hostile.iter().chain(&typed).cloned().collect();
    let metrics = rig.launch(false, false, &[], &arguments).unwrap();
    rig.fake.assert_scripts_parse();
    assert_eq!(metrics.pid, Some(4242));

    let mut expected = Vec::new();
    for (key, value) in &hostile {
        expected.extend(strs(&["--es", key, value.as_str().unwrap()]));
    }
    expected.extend(strs(&[
        "--ez",
        "flag",
        "true",
        "--ei",
        "count",
        "-42",
        "--el",
        "big",
        "3000000000",
        "--ed",
        "ratio",
        "0.5",
        "--es",
        "list",
        "[\"a b\",\"c'd\"]",
        "--es",
        "nothing",
        "null",
    ]));
    assert_eq!(rig.start_extras(), expected);
    // Nothing that was data ran as a command.
    assert!(rig.device.lines("reboot").is_empty() && rig.device.lines("id").is_empty(), "{:?}", rig.device.calls());
}

#[test]
fn a_second_launch_uses_what_the_first_one_learned() {
    require_sh!();
    let mut rig = Rig::new();
    rig.launch(false, false, &[], &[]).unwrap();
    rig.launch(true, false, &[("all", "deny"), ("camera", "allow")], &[("k", json!("v w"))]).unwrap();
    rig.fake.assert_scripts_parse();

    let script = &rig.fake.shells()[1];
    assert!(!script.contains("dumpsys") && !script.contains("resolve-activity"), "{script}");
    for statement in [
        "GRANTED=0; pm revoke com.example.app android.permission.CAMERA 2>/dev/null && GRANTED=$((GRANTED+1)) || true; ",
        "pm revoke com.example.app android.permission.RECORD_AUDIO 2>/dev/null && GRANTED=$((GRANTED+1)) || true; ",
        "appops set --uid com.example.app MANAGE_EXTERNAL_STORAGE deny 2>/dev/null || true; \
         pm grant com.example.app android.permission.CAMERA 2>/dev/null && GRANTED=$((GRANTED+1)) || true; T2=",
        "COMP='com.example.app/.MainActivity'; START_OUT=$(am start -a android.intent.action.MAIN \
         -c android.intent.category.LAUNCHER -n \"$COMP\" --es 'k' 'v w' 2>&1); ",
    ] {
        assert!(script.contains(statement), "missing `{statement}` in\n{script}");
    }
    // One lookup of each kind for the whole session, and the same component both times.
    assert_eq!((rig.device.lines("dumpsys").len(), rig.device.lines("cmd").len()), (1, 1));
    let starts: Vec<String> = rig.device.lines("am").into_iter().filter(|l| l.starts_with("am start")).collect();
    assert_eq!(starts.len(), 2);
    assert!(starts[1].ends_with(&format!("-n {COMPONENT} --es k v w")), "{starts:?}");
}

#[test]
fn a_component_the_device_reported_is_data_on_the_next_launch() {
    require_sh!();
    let odd = "com.example.app/.Main'; reboot; echo '";
    let mut rig = Rig::with_component(odd);
    rig.launch(false, false, &[], &[]).unwrap();
    rig.launch(false, false, &[], &[]).unwrap();
    rig.fake.assert_scripts_parse();
    let starts: Vec<Vec<String>> = rig.device.calls().into_iter().filter(|c| c[..2] == ["am", "start"]).collect();
    assert_eq!(starts.len(), 2);
    assert_eq!((starts[0].last().unwrap().as_str(), starts[1].last().unwrap().as_str()), (odd, odd));
    assert!(rig.device.lines("reboot").is_empty(), "{:?}", rig.device.calls());
}

#[test]
fn an_activity_named_error_still_launches() {
    require_sh!();
    // `am start` echoes the intent it starts; only a line that begins with `Error` is a failure.
    let mut rig = Rig::with_component("com.example.app/.ErrorReportActivity");
    let metrics = rig.launch(false, false, &[], &[]).unwrap();
    assert_eq!(metrics.pid, Some(4242));
}

#[test]
fn launch_failures_carry_the_device_s_own_words() {
    require_sh!();
    /// Launches on a device with `flag` set and returns the error and the `am` calls made.
    fn failed_launch(flag: &str, clear_state: bool) -> (String, Vec<String>) {
        let mut rig = Rig::new();
        rig.flag(flag, "");
        let err = rig.launch(clear_state, false, &[], &[]).unwrap_err().to_string();
        rig.fake.assert_scripts_parse();
        (err, rig.device.lines("am"))
    }

    // `am start` names the cause on its last line, after the intent it tried.
    let (err, _) = failed_launch("am-start-fails", false);
    assert!(err.starts_with("am start failed: Starting: Intent"), "{err}");
    assert!(err.contains("Error: Activity class {com.example.app/com.example.app.Nope} does not exist."), "{err}");

    let (err, am) = failed_launch("pm-clear-fails", true);
    assert_eq!(err, "pm clear failed: Error: java.lang.SecurityException   at android.os.Parcel");
    assert!(am.is_empty(), "nothing is started after a failed clear: {am:?}");

    let (err, am) = failed_launch("no-launcher", false);
    assert_eq!(err, "no launcher activity");
    assert!(am.is_empty(), "{am:?}");
}

#[test]
fn a_package_id_that_is_not_one_never_reaches_the_device() {
    let fake = FakeAdb::start(|_| panic!("nothing may be sent"));
    let mut android = Android::with_adb_client(SERIAL.into(), fake.client());
    for bad in ["", "com.x; reboot", "com.x$(id)", "com.x'", "com x", "com.x\nreboot"] {
        let none: [(&String, &Value); 0] = [];
        let err = android.prepare_and_launch(bad, true, true, &perms(&[("all", "allow")]), none).unwrap_err();
        assert!(err.to_string().contains("invalid Android package id"), "{bad:?}: {err}");
        assert!(android.set_permissions(bad, &perms(&[("all", "allow")])).is_err(), "{bad:?}");
    }
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
}

// --- setPermissions ----------------------------------------------------

#[test]
fn set_permissions_sends_this_exact_script_for_all_with_an_override() {
    require_sh!();
    let mut rig = Rig::new();
    let outcome = rig.android.set_permissions(APP, &perms(&[("all", "deny"), ("camera", "allow")])).unwrap();
    assert_eq!(
        rig.fake.shells(),
        vec![
            // What `all` covers is read from the device first.
            "dumpsys package com.example.app".to_string(),
            concat!(
                "OK=0; REV=0; ",
                "pm revoke com.example.app android.permission.CAMERA >/dev/null 2>&1 && { OK=$((OK+1)); REV=$((REV+1)); }; ",
                "pm revoke com.example.app android.permission.RECORD_AUDIO >/dev/null 2>&1 && { OK=$((OK+1)); REV=$((REV+1)); }; ",
                "pm revoke com.example.app android.permission.POST_NOTIFICATIONS >/dev/null 2>&1 && { OK=$((OK+1)); REV=$((REV+1)); }; ",
                "appops set --uid com.example.app MANAGE_EXTERNAL_STORAGE deny >/dev/null 2>&1 && OK=$((OK+1)); ",
                "pm grant com.example.app android.permission.CAMERA >/dev/null 2>&1 && { OK=$((OK+1)); }; ",
                "echo TAPWING_PERM_OK:$OK; echo TAPWING_PERM_REVOKED:$REV"
            )
            .to_string(),
        ]
    );
    rig.fake.assert_scripts_parse();
    assert_eq!((outcome.applied, outcome.revoked), (5, 3));
    assert_eq!(rig.device.lines("pm").last().unwrap(), "pm grant com.example.app android.permission.CAMERA");
}

/// Every Maestro shorthand and what it stands for on Android, plus a full
/// permission name and the one permission that is an app-op.
const KINDS: &[(&str, &[&str])] = &[
    ("android.permission.ACTIVITY_RECOGNITION", &["android.permission.ACTIVITY_RECOGNITION"]),
    ("android.permission.MANAGE_EXTERNAL_STORAGE", &[]),
    ("bluetooth", &["android.permission.BLUETOOTH_CONNECT", "android.permission.BLUETOOTH_SCAN"]),
    ("calendar", &["android.permission.WRITE_CALENDAR", "android.permission.READ_CALENDAR"]),
    ("camera", &["android.permission.CAMERA"]),
    ("contacts", &["android.permission.READ_CONTACTS", "android.permission.WRITE_CONTACTS"]),
    ("location", &["android.permission.ACCESS_FINE_LOCATION", "android.permission.ACCESS_COARSE_LOCATION"]),
    (
        "medialibrary",
        &[
            "android.permission.WRITE_EXTERNAL_STORAGE",
            "android.permission.READ_EXTERNAL_STORAGE",
            "android.permission.READ_MEDIA_AUDIO",
            "android.permission.READ_MEDIA_IMAGES",
            "android.permission.READ_MEDIA_VIDEO",
        ],
    ),
    ("microphone", &["android.permission.RECORD_AUDIO"]),
    ("notifications", &["android.permission.POST_NOTIFICATIONS"]),
    ("phone", &["android.permission.CALL_PHONE", "android.permission.ANSWER_PHONE_CALLS"]),
    ("sms", &["android.permission.READ_SMS", "android.permission.RECEIVE_SMS", "android.permission.SEND_SMS"]),
    ("storage", &["android.permission.WRITE_EXTERNAL_STORAGE", "android.permission.READ_EXTERNAL_STORAGE"]),
];

#[test]
fn every_permission_kind_is_granted_revoked_and_unset_with_the_right_command() {
    require_sh!();
    // (value in the flow, `pm` verb, `appops` mode)
    for (value, verb, mode) in [("allow", "grant", "allow"), ("deny", "revoke", "deny"), ("unset", "revoke", "default")]
    {
        let mut rig = Rig::new();
        let wanted: Vec<(&str, &str)> = KINDS.iter().map(|(kind, _)| (*kind, value)).collect();
        let outcome = rig.android.set_permissions(APP, &perms(&wanted)).unwrap();
        rig.fake.assert_scripts_parse();
        assert_eq!(rig.fake.shells().len(), 1, "{value}: without `all` nothing is looked up first");

        let expected: Vec<String> = KINDS
            .iter()
            .flat_map(|(_, permissions)| permissions.iter())
            .map(|permission| format!("pm {verb} com.example.app {permission}"))
            .collect();
        assert_eq!(rig.device.lines("pm"), expected, "{value}");
        assert_eq!(
            rig.device.lines("appops"),
            vec![format!("appops set --uid com.example.app MANAGE_EXTERNAL_STORAGE {mode}")],
            "{value}"
        );
        // All but the one the device refuses (`BLUETOOTH_SCAN`), plus the app-op.
        assert_eq!(outcome.applied, expected.len(), "{value}");
        assert_eq!(outcome.revoked, if verb == "revoke" { expected.len() - 1 } else { 0 }, "{value}");
    }
}

#[test]
fn all_uses_the_permissions_a_launch_already_found() {
    require_sh!();
    let mut rig = Rig::new();
    rig.launch(false, false, &[], &[]).unwrap();
    let outcome = rig.android.set_permissions(APP, &perms(&[("all", "allow")])).unwrap();
    rig.fake.assert_scripts_parse();
    assert_eq!(rig.fake.shells().len(), 2, "the launch and one permission script, no second dumpsys");
    assert_eq!(
        rig.device.lines("pm"),
        vec![
            "pm grant com.example.app android.permission.CAMERA",
            "pm grant com.example.app android.permission.RECORD_AUDIO",
            "pm grant com.example.app android.permission.POST_NOTIFICATIONS",
        ]
    );
    assert_eq!(rig.device.lines("appops"), vec!["appops set --uid com.example.app MANAGE_EXTERNAL_STORAGE allow"]);
    assert_eq!((outcome.applied, outcome.revoked), (4, 0));
}

#[test]
fn hostile_permission_names_and_values_cannot_add_commands() {
    require_sh!();
    let mut rig = Rig::new();
    let outcome = rig
        .android
        .set_permissions(
            APP,
            &perms(&[
                ("camera; reboot", "allow"),
                ("$(reboot)", "deny"),
                ("a b'c\n`id`", "allow"),
                (";;; ", "allow"),
                ("microphone", "allow; reboot"),
                ("location", "$(reboot)"),
            ]),
        )
        .unwrap();
    rig.fake.assert_scripts_parse();
    // Names keep only what a permission name can contain; an unknown value means revoke.
    assert_eq!(
        rig.device.lines("pm"),
        vec![
            "pm revoke com.example.app reboot",
            "pm grant com.example.app abcid",
            "pm grant com.example.app camerareboot",
            "pm revoke com.example.app android.permission.ACCESS_FINE_LOCATION",
            "pm revoke com.example.app android.permission.ACCESS_COARSE_LOCATION",
            "pm revoke com.example.app android.permission.RECORD_AUDIO",
        ]
    );
    assert!(rig.device.lines("reboot").is_empty() && rig.device.lines("id").is_empty(), "{:?}", rig.device.calls());
    assert_eq!((outcome.applied, outcome.revoked), (6, 4));
}

#[test]
fn nothing_to_set_sends_nothing() {
    let fake = FakeAdb::start(|_| panic!("nothing may be sent"));
    let mut android = Android::with_adb_client(SERIAL.into(), fake.client());
    for nothing in [perms(&[]), perms(&[(";;; ", "allow")])] {
        assert_eq!(android.set_permissions(APP, &nothing).unwrap(), super::PermissionOutcome::default());
    }
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
}
