//! End-to-end tests of the compiled `tapwing` binary: argument parsing, exit codes and the
//! messages users see. Nothing here needs a device, an emulator or a Flutter SDK; every `test`
//! invocation below fails (by design) before any device is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const VALID_FLOW: &str = "appId: com.example.app\n---\n- launchApp\n- tapOn: Login\n- assertVisible: Welcome\n";

static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tapwing_cli_e2e_{}_{}_{}", std::process::id(), id, label));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&full, content).expect("write file");
        full
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The compiled CLI, run in an isolated environment so a developer's own `TAPWING_*` settings
/// never change what the tests see.
fn tapwing(args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tapwing"));
    cmd.args(args).env_remove("TAPWING_DRIVER_APK").env_remove("TAPWING_IOS_DRIVER_DIR").env("NO_COLOR", "1");
    cmd.output().expect("run the tapwing binary")
}

fn tapwing_in(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tapwing"));
    cmd.current_dir(dir).args(args).env("NO_COLOR", "1");
    cmd.output().expect("run the tapwing binary")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exit code (not killed by a signal)")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn minimal_flutter_pubspec(name: &str) -> String {
    format!(
        "name: {name}\npublish_to: 'none'\n\nenvironment:\n  sdk: ^3.11.0\n\ndependencies:\n  flutter:\n    sdk: flutter\n\n\
         dev_dependencies:\n  flutter_test:\n    sdk: flutter\n\nflutter:\n  uses-material-design: true\n"
    )
}

// ---------------------------------------------------------------- --version / --help

#[test]
fn version_prints_the_crate_version_and_the_driver_report_and_exits_zero() {
    for flag in ["--version", "-V"] {
        let out = tapwing(&[flag]);
        assert_eq!(code(&out), 0, "{flag}: {}", stderr(&out));
        let text = stdout(&out);
        assert!(text.starts_with(&format!("tapwing {}\n", env!("CARGO_PKG_VERSION"))), "{flag}: {text}");
        assert!(text.contains("android driver:"), "{text}");
        assert!(text.contains("ios driver:"), "{text}");
    }
}

#[test]
fn help_lists_every_subcommand_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let out = tapwing(&[flag]);
        assert_eq!(code(&out), 0);
        let text = stdout(&out);
        assert!(text.contains("Flutter-first runner for Maestro YAML flows"), "{text}");
        for sub in ["test", "check", "hierarchy", "init"] {
            assert!(text.contains(&format!("\n  {sub} ")), "`{sub}` missing from help:\n{text}");
        }
    }
}

#[test]
fn every_subcommand_has_help_and_documents_its_key_flags() {
    let expectations: &[(&str, &[&str])] = &[
        ("test", &["--shard-split", "--shard-all", "--trace", "--trace-screenshots", "--format", "--input"]),
        ("check", &["--dump-ir"]),
        ("hierarchy", &["--device", "--app-id", "--port"]),
        ("init", &["--dry-run", "--force", "--driver-path", "--pub-get"]),
    ];
    for (sub, flags) in expectations {
        let out = tapwing(&[sub, "--help"]);
        assert_eq!(code(&out), 0, "{sub} --help");
        let text = stdout(&out);
        for flag in *flags {
            assert!(text.contains(flag), "`tapwing {sub} --help` must document {flag}:\n{text}");
        }
    }
}

#[test]
fn no_arguments_and_unknown_subcommands_are_usage_errors() {
    let out = tapwing(&[]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("Usage: tapwing"), "{}", stderr(&out));

    let out = tapwing(&["frobnicate"]);
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.contains("unrecognized subcommand 'frobnicate'"), "{err}");

    let out = tapwing(&["check", "--no-such-flag", "x.yaml"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("--no-such-flag"), "{}", stderr(&out));
}

// ---------------------------------------------------------------- check

#[test]
fn check_accepts_a_valid_flow() {
    let tmp = TempDir::new("check_valid");
    let flow = tmp.write("login.yaml", VALID_FLOW);
    let out = tapwing(&["check", flow.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("✓"), "{}", stdout(&out));
    assert!(stdout(&out).contains("login.yaml (4 commands)"), "{}", stdout(&out));
    assert!(stderr(&out).contains("1 valid, 0 invalid"), "{}", stderr(&out));
}

#[test]
fn check_walks_a_directory_including_subfolders_and_skips_workspace_configs() {
    let tmp = TempDir::new("check_dir");
    tmp.write("a.yaml", VALID_FLOW);
    tmp.write("nested/b.yml", VALID_FLOW);
    tmp.write("config.yaml", "flows:\n  - '*'\n");
    tmp.write("README.md", "# not a flow\n");
    let out = tapwing(&["check", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("a.yaml"), "{text}");
    assert!(text.contains("b.yml"), "{text}");
    assert!(text.contains("(workspace config)"), "{text}");
    assert!(stderr(&out).contains("2 valid, 0 invalid"), "{}", stderr(&out));
}

#[test]
fn check_validates_the_sample_app_flows_and_the_bench_flows() {
    let root = repo_root();
    let sample = root.join("examples/sample_app/.maestro");
    let bench = root.join("bench/f11a/flows");
    let out = tapwing(&["check", sample.to_str().unwrap(), bench.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(", 0 invalid"), "{err}");
    // The Android and iOS flow sets and the negative flows are all covered.
    let text = stdout(&out);
    for expected in ["login_and_browse.yaml", "ios/login_and_browse.yaml", "negative/fail_missing_element.yaml"] {
        assert!(text.contains(expected), "{expected} not checked:\n{text}");
    }
}

#[test]
fn check_dump_ir_prints_valid_json_with_the_parsed_commands() {
    let tmp = TempDir::new("check_ir");
    let flow = tmp.write("flow.yaml", VALID_FLOW);
    let out = tapwing(&["check", "--dump-ir", flow.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let ir: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("--dump-ir is valid JSON");
    assert!(ir["path"].as_str().unwrap().ends_with("flow.yaml"));
    let commands = ir["commands"].as_array().expect("commands array");
    // The config section becomes `applyConfiguration`, then one entry per flow command.
    assert_eq!(commands.len(), 4, "{ir}");
    assert!(commands[0].get("applyConfigurationCommand").is_some(), "{ir}");
    assert_eq!(commands[0]["applyConfigurationCommand"]["config"]["appId"], "com.example.app");
    assert!(commands[1].get("launchAppCommand").is_some(), "{ir}");
    assert!(commands[2].get("tapOnElement").is_some(), "{ir}");
    assert!(commands[3].get("assertConditionCommand").is_some(), "{ir}");
}

#[test]
fn check_dump_ir_keeps_stdout_pure_json_and_sends_status_to_stderr() {
    let tmp = TempDir::new("check_ir_status");
    tmp.write("a.yaml", VALID_FLOW);
    tmp.write("config.yaml", "flows:\n  - '*'\n");
    let out = tapwing(&["check", "--dump-ir", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains('✓'), "status line on stdout:\n{text}");
    serde_json::from_str::<serde_json::Value>(&text).expect("stdout is exactly one JSON document");
    let err = stderr(&out);
    assert!(err.contains("(workspace config)"), "{err}");
    assert!(err.contains("1 valid, 0 invalid"), "{err}");
}

#[test]
fn check_dump_ir_of_several_flows_is_one_json_array() {
    let tmp = TempDir::new("check_ir_many");
    let a = tmp.write("a.yaml", VALID_FLOW);
    let b = tmp.write("nested/b.yaml", "appId: com.example.app\n---\n- back\n");

    // A directory: an array, one `{path, commands}` object per valid flow, in walk order.
    let out = tapwing(&["check", "--dump-ir", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let ir: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    let flows = ir.as_array().expect("an array for a directory");
    assert_eq!(flows.len(), 2, "{ir}");
    assert!(flows[0]["path"].as_str().unwrap().ends_with("a.yaml"), "{ir}");
    assert!(flows[1]["path"].as_str().unwrap().ends_with("b.yaml"), "{ir}");
    assert_eq!(flows[0]["commands"].as_array().unwrap().len(), 4, "{ir}");
    assert!(flows[1]["commands"][1].get("backPressCommand").is_some(), "{ir}");

    // Several file arguments: the same array, and an invalid flow fails the run without breaking the JSON.
    let bad = tmp.write("bad/c.yaml", "appId: x\n---\n- bogusCommand\n");
    let out = tapwing(&["check", "--dump-ir", a.to_str().unwrap(), bad.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let ir: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(ir.as_array().expect("an array for several paths").len(), 2, "{ir}");
    assert!(stderr(&out).contains("2 valid, 1 invalid"), "{}", stderr(&out));

    // A directory holding a single flow is still an array: the shape follows the arguments.
    let out = tapwing(&["check", "--dump-ir", tmp.path().join("nested").to_str().unwrap()]);
    let ir: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(ir.as_array().expect("an array for a directory").len(), 1, "{ir}");
}

#[test]
fn check_fails_on_a_directory_without_flows() {
    let tmp = TempDir::new("check_empty");
    tmp.write("empty/README.md", "# no flows here\n");
    tmp.write("empty/config.yaml", "flows:\n  - '*'\n");
    let empty = tmp.path().join("empty");
    let out = tapwing(&["check", empty.to_str().unwrap()]);
    assert_eq!(code(&out), 1, "{}{}", stdout(&out), stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(&format!("✗ {}: no flow files found", empty.display())), "{err}");
    assert!(err.contains("0 valid, 0 invalid"), "{err}");

    // An empty directory next to a good one still fails, and the good flows are still checked.
    let good = tmp.write("good/a.yaml", VALID_FLOW);
    let out = tapwing(&["check", good.parent().unwrap().to_str().unwrap(), empty.to_str().unwrap()]);
    assert_eq!(code(&out), 1, "{}{}", stdout(&out), stderr(&out));
    assert!(stdout(&out).contains("a.yaml (4 commands)"), "{}", stdout(&out));
    assert!(stderr(&out).contains("no flow files found"), "{}", stderr(&out));

    // With --dump-ir the document is still valid JSON.
    let out = tapwing(&["check", "--dump-ir", empty.to_str().unwrap()]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "[]");

    // A single invalid flow file with --dump-ir still prints one JSON document (`null`).
    let bad = tmp.write("bad.yaml", "not: [valid");
    let out = tapwing(&["check", "--dump-ir", bad.to_str().unwrap()]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "null");
}

#[cfg(unix)]
#[test]
fn check_walks_a_symlink_loop_once() {
    let tmp = TempDir::new("check_symlink_loop");
    tmp.write("a.yaml", VALID_FLOW);
    tmp.write("nested/b.yaml", VALID_FLOW);
    // `nested/up` leads back to the root and `self` to its own directory.
    std::os::unix::fs::symlink(tmp.path(), tmp.path().join("nested/up")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("nested"), tmp.path().join("nested/self")).unwrap();
    let out = tapwing(&["check", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
    let text = stdout(&out);
    assert_eq!(text.matches("a.yaml (4 commands)").count(), 1, "{text}");
    assert_eq!(text.matches("b.yaml (4 commands)").count(), 1, "{text}");
    assert!(stderr(&out).contains("2 valid, 0 invalid (2 files"), "{}", stderr(&out));
}

#[test]
fn check_rejects_an_unknown_command_with_an_actionable_message() {
    let tmp = TempDir::new("check_unknown");
    let flow = tmp.write("bad.yaml", "appId: x\n---\n- launchApp\n- bogusCommand\n");
    let out = tapwing(&["check", flow.to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains("bad.yaml:4:3"), "location (line:col) missing: {err}");
    assert!(err.contains("Invalid Command: bogusCommand"), "{err}");
    assert!(err.contains("`bogusCommand` is not a valid command."), "{err}");
    assert!(err.contains("0 valid, 1 invalid"), "{err}");
}

#[test]
fn check_rejects_broken_yaml_and_a_missing_config_section() {
    let tmp = TempDir::new("check_broken");
    let broken = tmp.write("broken.yaml", "appId: x\n---\n- tapOn: [\n");
    let out = tapwing(&["check", broken.to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("Parsing Failed"), "{}", stderr(&out));

    let no_config = tmp.write("no_config.yaml", "- launchApp\n");
    let out = tapwing(&["check", no_config.to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("Config Section Required"), "{}", stderr(&out));
}

#[test]
fn check_reports_every_invalid_file_and_still_validates_the_good_ones() {
    let tmp = TempDir::new("check_mixed");
    tmp.write("good.yaml", VALID_FLOW);
    tmp.write("bad1.yaml", "appId: x\n---\n- nope\n");
    tmp.write("bad2.yaml", "appId: x\n---\n- alsoNope\n");
    let out = tapwing(&["check", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    assert!(stdout(&out).contains("good.yaml"), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains("bad1.yaml") && err.contains("bad2.yaml"), "{err}");
    assert!(err.contains("1 valid, 2 invalid"), "{err}");
}

#[test]
fn check_fails_with_a_clear_message_for_a_missing_path() {
    let tmp = TempDir::new("check_missing");
    let missing = tmp.path().join("does_not_exist.yaml");
    let out = tapwing(&["check", missing.to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains("does_not_exist.yaml"), "{err}");
    assert!(err.contains("Failed to read file"), "{err}");
}

#[test]
fn check_without_a_path_is_a_usage_error() {
    let out = tapwing(&["check"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("<PATHS>"), "{}", stderr(&out));
}

// ---------------------------------------------------------------- init

fn flutter_project(tmp: &TempDir) {
    tmp.write("pubspec.yaml", &minimal_flutter_pubspec("demo_app"));
    tmp.write("lib/main.dart", "void main() {}\n");
}

#[test]
fn init_dry_run_prints_a_diff_and_writes_nothing() {
    let tmp = TempDir::new("init_dry");
    flutter_project(&tmp);
    let before = tmp.read("pubspec.yaml");
    let out = tapwing(&["init", tmp.path().to_str().unwrap(), "--dry-run"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Dry run for demo_app (no files written)"), "{text}");
    assert!(text.contains("+  tapwing_driver:"), "{text}");
    assert!(text.contains("+++ b/lib/main_tapwing.dart"), "{text}");
    assert_eq!(tmp.read("pubspec.yaml"), before, "--dry-run must not touch pubspec.yaml");
    assert!(!tmp.path().join("lib/main_tapwing.dart").exists(), "--dry-run must not create the entrypoint");
}

#[test]
fn init_wires_the_driver_and_is_idempotent_on_rerun() {
    let tmp = TempDir::new("init_apply");
    flutter_project(&tmp);
    let version = env!("CARGO_PKG_VERSION");

    let out = tapwing(&["init", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("Initialized Tapwing in demo_app"), "{}", stdout(&out));

    let pubspec = tmp.read("pubspec.yaml");
    assert!(pubspec.contains("tapwing_driver:"), "{pubspec}");
    assert!(pubspec.contains(&format!("ref: v{version}")), "{pubspec}");
    let entry = tmp.read("lib/main_tapwing.dart");
    assert!(entry.contains("TapwingBinding.ensureInitialized();"), "{entry}");
    assert!(entry.contains("import 'main.dart' as app;"), "{entry}");
    // Production code is untouched.
    assert_eq!(tmp.read("lib/main.dart"), "void main() {}\n");

    // Re-running changes nothing and still succeeds.
    let out = tapwing(&["init", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("Already initialized (demo_app)"), "{}", stdout(&out));
    assert_eq!(tmp.read("pubspec.yaml"), pubspec, "second run must not rewrite pubspec.yaml");
    assert_eq!(tmp.read("lib/main_tapwing.dart"), entry);
    assert_eq!(pubspec.matches("tapwing_driver:").count(), 1, "no duplicate dependency entry:\n{pubspec}");
}

#[test]
fn init_uses_the_current_directory_and_a_local_driver_path() {
    let tmp = TempDir::new("init_cwd");
    flutter_project(&tmp);
    let out = tapwing_in(tmp.path(), &["init", "--driver-path", "../tapwing/packages/tapwing_driver"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let pubspec = tmp.read("pubspec.yaml");
    assert!(pubspec.contains("path: ../tapwing/packages/tapwing_driver"), "{pubspec}");
    assert!(!pubspec.contains("git:"), "a path dependency replaces the git one:\n{pubspec}");
}

#[test]
fn init_supports_a_custom_target_and_output() {
    let tmp = TempDir::new("init_custom");
    tmp.write("pubspec.yaml", &minimal_flutter_pubspec("demo_app"));
    tmp.write("lib/main_dev.dart", "void main() {}\n");
    let out = tapwing(&[
        "init",
        tmp.path().to_str().unwrap(),
        "--target",
        "lib/main_dev.dart",
        "--output",
        "lib/main_e2e.dart",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let entry = tmp.read("lib/main_e2e.dart");
    assert!(entry.contains("import 'main_dev.dart' as app;"), "{entry}");
}

#[test]
fn init_fails_clearly_outside_a_flutter_project() {
    let tmp = TempDir::new("init_not_flutter");
    let out = tapwing(&["init", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("no pubspec.yaml found"), "{}", stderr(&out));

    // A Dart package without a Flutter dependency is not an app either.
    tmp.write("pubspec.yaml", "name: plain_dart\nenvironment:\n  sdk: ^3.11.0\n");
    let out = tapwing(&["init", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 2, "{}{}", stdout(&out), stderr(&out));
    assert!(stderr(&out).starts_with("error:"), "{}", stderr(&out));
}

#[test]
fn init_fails_when_the_target_entrypoint_is_missing() {
    let tmp = TempDir::new("init_no_main");
    tmp.write("pubspec.yaml", &minimal_flutter_pubspec("demo_app"));
    let out = tapwing(&["init", tmp.path().to_str().unwrap()]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("main.dart"), "{}", stderr(&out));
    assert!(!tmp.path().join("lib/main_tapwing.dart").exists());
}

// ---------------------------------------------------------------- test (argument validation)

fn flow_file(tmp: &TempDir) -> String {
    tmp.write("flow.yaml", VALID_FLOW).to_str().unwrap().to_string()
}

#[test]
fn test_without_flows_is_a_usage_error() {
    let out = tapwing(&["test"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("<FLOWS>"), "{}", stderr(&out));
}

#[test]
fn test_fails_clearly_for_a_missing_flow_file() {
    let tmp = TempDir::new("test_missing");
    let missing = tmp.path().join("missing.yaml");
    let out = tapwing(&["test", missing.to_str().unwrap()]);
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.starts_with("error: Flow path does not exist:"), "{err}");
    assert!(err.contains("missing.yaml"), "{err}");
}

#[test]
fn test_rejects_conflicting_shard_flags() {
    let tmp = TempDir::new("test_shards");
    let flow = flow_file(&tmp);
    let out = tapwing(&["test", &flow, "--shard-split", "2", "--shard-all", "2"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("--shard-split and --shard-all are mutually exclusive"), "{}", stderr(&out));

    let out = tapwing(&["test", &flow, "--shard-split", "0"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("Shard count must be at least 1"), "{}", stderr(&out));

    let out = tapwing(&["test", &flow, "--shard-split", "many"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("--shard-split"), "{}", stderr(&out));
}

#[test]
fn test_requires_trace_for_trace_screenshots() {
    let tmp = TempDir::new("test_trace");
    let flow = flow_file(&tmp);
    let out = tapwing(&["test", &flow, "--trace-screenshots", "steps"]);
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.contains("--trace"), "{err}");
    assert!(err.contains("required arguments were not provided"), "{err}");

    let out = tapwing(&["test", &flow, "--trace", "--trace-screenshots", "sometimes"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("invalid value 'sometimes'"), "{}", stderr(&out));
}

#[test]
fn test_rejects_invalid_enum_values_and_lists_the_valid_ones() {
    let tmp = TempDir::new("test_enums");
    let flow = flow_file(&tmp);
    for (flag, bad, valid) in [
        ("--input", "bogus", "auto, flutter, native"),
        ("--platform", "windows", "android, ios"),
        ("--maestro-compat", "loose", "default, strict"),
        ("--app-errors", "maybe", "report, fail, off"),
    ] {
        let out = tapwing(&["test", &flow, flag, bad]);
        assert_eq!(code(&out), 2, "{flag} {bad}");
        let err = stderr(&out);
        assert!(err.contains(&format!("invalid value '{bad}'")), "{flag}: {err}");
        assert!(err.contains(valid), "{flag} must list its valid values ({valid}): {err}");
    }
}

#[test]
fn test_rejects_malformed_env_pairs_before_doing_anything() {
    let tmp = TempDir::new("test_env");
    let flow = flow_file(&tmp);
    let out = tapwing(&["test", &flow, "-e", "NOEQUALS"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("invalid -e/--env value `NOEQUALS` (expected KEY=VALUE)"), "{}", stderr(&out));
}

#[test]
fn test_rejects_a_bad_app_errors_ignore_regex() {
    let tmp = TempDir::new("test_regex");
    let flow = flow_file(&tmp);
    let out = tapwing(&["test", &flow, "--app-errors-ignore", "(unclosed"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).starts_with("error:"), "{}", stderr(&out));
}

#[test]
fn test_rejects_a_bad_settle_load_timeout_and_documents_the_settle_load_flags() {
    let tmp = TempDir::new("test_settle_load");
    let flow = flow_file(&tmp);
    let out = tapwing(&["test", &flow, "--settle-load-timeout-ms", "soon"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("invalid value 'soon' for '--settle-load-timeout-ms <MS>'"), "{}", stderr(&out));
    // Negative bounds are not a thing (`0` removes the bound).
    assert_eq!(code(&tapwing(&["test", &flow, "--settle-load-timeout-ms=-1"])), 2);
    let help = stdout(&tapwing(&["test", "--help"]));
    assert!(help.contains("--settle-load-timeout-ms <MS>"), "{help}");
    assert!(help.contains("--settle-http-ignore <REGEX>"), "{help}");
}

#[test]
fn test_rejects_an_invalid_flow_before_touching_a_device() {
    let tmp = TempDir::new("test_invalid_flow");
    let flow = tmp.write("bad.yaml", "appId: x\n---\n- bogusCommand\n");
    let out = tapwing(&["test", flow.to_str().unwrap()]);
    assert_eq!(code(&out), 2, "{}{}", stdout(&out), stderr(&out));
    assert!(stderr(&out).contains("bogusCommand"), "{}", stderr(&out));
}

// ---------------------------------------------------------------- signal cleanup (SIGINT / SIGTERM)

#[cfg(unix)]
mod signal_cleanup {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    fn read_adb_frame(stream: &mut TcpStream) -> Option<String> {
        let mut len_hex = [0u8; 4];
        stream.read_exact(&mut len_hex).ok()?;
        let len = usize::from_str_radix(std::str::from_utf8(&len_hex).ok()?, 16).ok()?;
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).ok()?;
        String::from_utf8(buf).ok()
    }

    fn write_shell_v2(stream: &mut TcpStream, stdout: &[u8]) {
        if !stdout.is_empty() {
            let mut h = [0u8; 5];
            h[0] = 1; // STDOUT
            h[1..].copy_from_slice(&(stdout.len() as u32).to_le_bytes());
            let _ = stream.write_all(&h);
            let _ = stream.write_all(stdout);
        }
        let mut h = [0u8; 5];
        h[0] = 3; // EXIT 0
        h[1..].copy_from_slice(&1u32.to_le_bytes());
        let _ = stream.write_all(&h);
        let _ = stream.write_all(&[0]);
        let _ = stream.flush();
    }

    fn run_signal_cleanup_check(sig_flag: &str, expected_code: i32) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock adb");
        let adb_port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_server = Arc::clone(&seen);
        let (launched_tx, launched_rx) = mpsc::channel::<()>();

        let server = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let Some(first) = read_adb_frame(&mut stream) else { continue };
                let _ = stream.write_all(b"OKAY");
                if !first.starts_with("host:transport:") {
                    seen_server.lock().unwrap().push(first);
                    continue;
                }
                let Some(cmd) = read_adb_frame(&mut stream) else { continue };
                seen_server.lock().unwrap().push(cmd.clone());
                let _ = stream.write_all(b"OKAY");
                if cmd.starts_with("reverse:") {
                    let _ = stream.write_all(b"OKAY");
                    let _ = stream.flush();
                } else if cmd.contains("TAPWING_ANIM_WIN=") {
                    write_shell_v2(
                        &mut stream,
                        b"TAPWING_ANIM_WIN=1.0\nTAPWING_ANIM_TRANS=0.5\nTAPWING_ANIM_DUR=1.0\n",
                    );
                } else if cmd.contains("stay_on_while_plugged_in") && cmd.contains("TAPWING_IME_HW=") {
                    write_shell_v2(&mut stream, b"3\nmWakefulness=Awake\nisKeyguardShowing=false\nTAPWING_IME_HW=1\n");
                } else if cmd.contains("ro.product.model") {
                    write_shell_v2(&mut stream, b"MockPixel\n35\n");
                } else if cmd.contains("monkey") || cmd.contains("am start") {
                    write_shell_v2(&mut stream, b"TAPWING_PID:4242\n");
                    let _ = launched_tx.send(());
                } else {
                    write_shell_v2(&mut stream, b"");
                }
            }
        });

        let tmp = TempDir::new(&format!("sig_{expected_code}"));
        tmp.write("config.yaml", "flows:\n  - '*'\nplatform:\n  android:\n    disableAnimations: true\n");
        tmp.write("flow.yaml", VALID_FLOW);
        let report_path = tmp.path().join("report.json");
        let junit_path = tmp.path().join("report.xml");

        let child = Command::new(env!("CARGO_BIN_EXE_tapwing"))
            .args([
                "test",
                tmp.path().to_str().unwrap(),
                "--device",
                "emulator-5554",
                "--platform",
                "android",
                "--input",
                "flutter",
                "--report",
                report_path.to_str().unwrap(),
                "--format",
                "junit",
                "--output",
                junit_path.to_str().unwrap(),
            ])
            .env("ANDROID_ADB_SERVER_PORT", adb_port.to_string())
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tapwing");

        launched_rx.recv_timeout(Duration::from_secs(5)).expect("tapwing did not reach launchApp on mock ADB");
        // Give accept_with_pid a moment to enter its wait loop.
        std::thread::sleep(Duration::from_millis(40));

        let t0 = Instant::now();
        let kill_status =
            Command::new("kill").args([sig_flag, &child.id().to_string()]).status().expect("send signal via kill");
        assert!(kill_status.success());

        let out = child.wait_with_output().expect("wait for tapwing exit");
        let elapsed = t0.elapsed();
        drop(server);

        assert_eq!(out.status.code(), Some(expected_code), "stderr:\n{}", stderr(&out));
        assert!(elapsed < Duration::from_secs(2), "signal teardown took too long: {elapsed:?}");
        assert!(
            stderr(&out).contains("interrupted; cleaning up"),
            "missing cleanup notice on stderr:\n{}",
            stderr(&out)
        );

        // The interrupted run still writes its reports, marked as partial.
        let signal_name = if expected_code == 130 { "SIGINT" } else { "SIGTERM" };
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report_path).expect("partial JSON report"))
                .expect("valid JSON");
        assert_eq!(report["interrupted"], true, "{report}");
        assert_eq!(report["interruptSignal"], signal_name, "{report}");
        assert_eq!(report["flows"][0]["status"], "interrupted", "{report}");
        let junit = std::fs::read_to_string(&junit_path).expect("partial JUnit report");
        assert!(junit.contains("tests=\"1\" failures=\"1\"") && junit.contains("status=\"STOPPED\""), "{junit}");
        assert!(junit.trim_end().ends_with("</testsuites>"), "{junit}");
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "atomic report writes left temp files: {leftovers:?}");

        let cmds = seen.lock().unwrap().join("\n");
        assert!(cmds.contains("reverse:killforward:tcp:7781"), "adb reverse was not removed:\n{cmds}");
        assert!(
            cmds.contains("settings put global stay_on_while_plugged_in 3"),
            "stay_on_while_plugged_in was not restored:\n{cmds}"
        );
        assert!(
            cmds.contains("settings put global window_animation_scale 1.0")
                && cmds.contains("settings put global transition_animation_scale 0.5")
                && cmds.contains("settings put global animator_duration_scale 1.0"),
            "animation scales were not restored:\n{cmds}"
        );
    }

    #[test]
    fn sigint_and_sigterm_unwind_and_restore_device_guards_with_standard_exit_codes() {
        run_signal_cleanup_check("-INT", 130);
        run_signal_cleanup_check("-TERM", 143);
    }

    #[test]
    fn hierarchy_sigint_removes_adb_reverse_and_exits_130() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock adb");
        let adb_port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_server = Arc::clone(&seen);
        let (reversed_tx, reversed_rx) = mpsc::channel::<()>();

        let server = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let Some(first) = read_adb_frame(&mut stream) else { continue };
                let _ = stream.write_all(b"OKAY");
                if !first.starts_with("host:transport:") {
                    seen_server.lock().unwrap().push(first);
                    continue;
                }
                let Some(cmd) = read_adb_frame(&mut stream) else { continue };
                seen_server.lock().unwrap().push(cmd.clone());
                let _ = stream.write_all(b"OKAY");
                if cmd.starts_with("reverse:") {
                    let _ = stream.write_all(b"OKAY");
                    let _ = stream.flush();
                    if cmd.starts_with("reverse:forward:") {
                        let _ = reversed_tx.send(());
                    }
                } else {
                    write_shell_v2(&mut stream, b"");
                }
            }
        });

        let child = Command::new(env!("CARGO_BIN_EXE_tapwing"))
            .args(["hierarchy", "--device", "emulator-5554", "--platform", "android"])
            .env("ANDROID_ADB_SERVER_PORT", adb_port.to_string())
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tapwing hierarchy");

        reversed_rx.recv_timeout(Duration::from_secs(5)).expect("hierarchy did not establish adb reverse");
        std::thread::sleep(Duration::from_millis(30));

        let kill_status = Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
        assert!(kill_status.success());

        let out = child.wait_with_output().expect("wait for tapwing hierarchy exit");
        drop(server);

        assert_eq!(out.status.code(), Some(130), "stderr:\n{}", stderr(&out));
        let cmds = seen.lock().unwrap().join("\n");
        assert!(cmds.contains("reverse:killforward:tcp:7781"), "adb reverse was not removed:\n{cmds}");
    }
}
