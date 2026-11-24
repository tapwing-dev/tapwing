use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use tapwing::init::{
    DEFAULT_DRIVER_SUBDIR, DEFAULT_DRIVER_URL, DriverSource, FilePlan, InitArgs, apply_plan, plan_init,
};
use yaml_rust2::{Yaml, YamlLoader};

static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tapwing_init_test_{}_{}_{}", std::process::id(), id, label));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, rel: &str, content: &str) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&full, content).expect("write fixture file");
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).expect("read fixture file")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn default_args(project_dir: &Path) -> InitArgs {
    InitArgs {
        project_dir: project_dir.to_path_buf(),
        app: None,
        target: PathBuf::from("lib/main.dart"),
        output: PathBuf::from("lib/main_tapwing.dart"),
        driver_path: None,
        driver_ref: None,
        driver_url: None,
        dry_run: false,
        force: false,
        pub_get: false,
    }
}

fn minimal_flutter_pubspec(name: &str) -> String {
    format!(
        "\
name: {name}
description: Fixture app.
publish_to: 'none'

environment:
  sdk: ^3.11.0

dependencies:
  flutter:
    sdk: flutter

dev_dependencies:
  flutter_test:
    sdk: flutter

flutter:
  uses-material-design: true
"
    )
}

#[test]
fn cli_and_driver_versions_stay_in_sync() {
    let cargo_version = env!("CARGO_PKG_VERSION");
    let driver_pubspec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/tapwing_driver/pubspec.yaml");
    let text = std::fs::read_to_string(&driver_pubspec).expect("read packages/tapwing_driver/pubspec.yaml");
    let docs = YamlLoader::load_from_str(&text).expect("parse driver pubspec.yaml");
    let root = docs[0].as_hash().expect("pubspec root mapping");
    let driver_version = root
        .get(&Yaml::String("version".to_string()))
        .and_then(Yaml::as_str)
        .expect("version field in packages/tapwing_driver/pubspec.yaml");
    let publish_to = root
        .get(&Yaml::String("publish_to".to_string()))
        .and_then(Yaml::as_str)
        .expect("publish_to field in packages/tapwing_driver/pubspec.yaml");

    assert_eq!(
        cargo_version, driver_version,
        "crates/tapwing/Cargo.toml version ({cargo_version}) must match \
         packages/tapwing_driver/pubspec.yaml version ({driver_version}) so release tags `v{cargo_version}` stay in sync"
    );
    assert_eq!(publish_to, "none", "tapwing_driver must keep publish_to: none");
}

#[test]
fn initializes_plain_flutter_app_with_default_git_dependency() {
    let tmp = TempDir::new("plain_app");
    tmp.write("pubspec.yaml", &minimal_flutter_pubspec("counter_app"));
    tmp.write("lib/main.dart", "void main() {\n  runApp(const MyApp());\n}\n");

    let args = default_args(tmp.path());
    let plan = plan_init(&args).expect("plan plain app");
    assert_eq!(
        plan.driver_source,
        DriverSource::Git {
            url: DEFAULT_DRIVER_URL.to_string(),
            path: DEFAULT_DRIVER_SUBDIR.to_string(),
            git_ref: format!("v{}", env!("CARGO_PKG_VERSION")),
        }
    );
    apply_plan(&plan, false).expect("apply plan");

    let pubspec = tmp.read("pubspec.yaml");
    let expected_ref = format!("v{}", env!("CARGO_PKG_VERSION"));
    assert!(pubspec.contains("  tapwing_driver:\n"));
    assert!(pubspec.contains(&format!("      url: {DEFAULT_DRIVER_URL}\n")));
    assert!(pubspec.contains(&format!("      path: {DEFAULT_DRIVER_SUBDIR}\n")));
    assert!(pubspec.contains(&format!("      ref: {expected_ref}\n")));

    let entry = tmp.read("lib/main_tapwing.dart");
    assert!(entry.contains("// ignore_for_file: depend_on_referenced_packages"));
    assert!(entry.contains("import 'package:tapwing_driver/tapwing_driver.dart';"));
    assert!(entry.contains("import 'main.dart' as app;"));
    assert!(entry.contains("void main() {\n  TapwingBinding.ensureInitialized();\n  app.main();\n}\n"));
}

#[test]
fn detects_single_flutter_app_in_pub_workspace() {
    let tmp = TempDir::new("workspace_single");
    tmp.write(
        "pubspec.yaml",
        "\
name: monorepo
environment:
  sdk: ^3.11.0
workspace:
  - apps/mobile_app
  - packages/design_system
",
    );
    tmp.write("apps/mobile_app/pubspec.yaml", &minimal_flutter_pubspec("mobile_app"));
    tmp.write("apps/mobile_app/lib/main.dart", "void main() => runApp(const App());\n");
    // Flutter UI library package (depends on flutter SDK, but has no lib/main.dart entrypoint).
    tmp.write("packages/design_system/pubspec.yaml", &minimal_flutter_pubspec("design_system"));
    tmp.write("packages/design_system/lib/design_system.dart", "library design_system;\n");

    let args = default_args(tmp.path());
    let plan = plan_init(&args).expect("plan workspace single app");
    assert_eq!(plan.app.package_name, "mobile_app");
    assert_eq!(plan.app.app_rel_display.as_deref(), Some("apps/mobile_app"));
    apply_plan(&plan, false).expect("apply workspace single app");

    assert!(tmp.path().join("apps/mobile_app/lib/main_tapwing.dart").is_file());
    assert!(tmp.read("apps/mobile_app/pubspec.yaml").contains("tapwing_driver:"));
    assert!(!tmp.read("packages/design_system/pubspec.yaml").contains("tapwing_driver:"));
}

#[test]
fn requires_app_flag_in_pub_workspace_with_two_apps() {
    let tmp = TempDir::new("workspace_two");
    tmp.write(
        "pubspec.yaml",
        "\
name: monorepo
environment:
  sdk: ^3.11.0
workspace:
  - apps/alpha
  - apps/beta
",
    );
    tmp.write("apps/alpha/pubspec.yaml", &minimal_flutter_pubspec("alpha"));
    tmp.write("apps/alpha/lib/main.dart", "void main() {}\n");
    tmp.write("apps/beta/pubspec.yaml", &minimal_flutter_pubspec("beta"));
    tmp.write("apps/beta/lib/main.dart", "void main() {}\n");

    let err = plan_init(&default_args(tmp.path())).expect_err("must require --app");
    let msg = format!("{err:#}");
    assert!(msg.contains("--app"), "expected --app hint, got: {msg}");
    assert!(msg.contains("apps/alpha"), "expected apps/alpha in candidates: {msg}");
    assert!(msg.contains("apps/beta"), "expected apps/beta in candidates: {msg}");

    let mut args = default_args(tmp.path());
    args.app = Some(PathBuf::from("apps/beta"));
    let plan = plan_init(&args).expect("plan with --app");
    assert_eq!(plan.app.package_name, "beta");
    apply_plan(&plan, false).expect("apply with --app");

    assert!(tmp.path().join("apps/beta/lib/main_tapwing.dart").is_file());
    assert!(!tmp.path().join("apps/alpha/lib/main_tapwing.dart").exists());
}

#[test]
fn handles_async_main_with_args_and_custom_target_and_output() {
    let tmp = TempDir::new("async_args");
    tmp.write("pubspec.yaml", &minimal_flutter_pubspec("flavored_app"));
    tmp.write(
        "lib/main_dev.dart",
        "\
import 'package:flutter/widgets.dart';

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(MyApp(args: args));
}
",
    );
    tmp.write(
        "android/app/build.gradle.kts",
        "\
android {
    productFlavors {
        create(\"dev\") { dimension = \"env\" }
        create(\"prod\") { dimension = \"env\" }
    }
}
",
    );

    let mut args = default_args(tmp.path());
    args.target = PathBuf::from("lib/main_dev.dart");
    args.output = PathBuf::from("lib/main_tapwing_dev.dart");

    let plan = plan_init(&args).expect("plan async+args app");
    assert!(plan.main_signature.returns_future);
    assert!(plan.main_signature.takes_args);
    assert_eq!(plan.android_flavors, Some(vec!["dev".to_string(), "prod".to_string()]));

    let steps = plan.next_steps(false);
    assert!(steps.contains("--flavor dev -t lib/main_tapwing_dev.dart"));
    assert!(steps.contains("dev, prod"));

    apply_plan(&plan, false).expect("apply async+args app");
    let entry = tmp.read("lib/main_tapwing_dev.dart");
    assert!(entry.contains("import 'main_dev.dart' as app;"));
    assert!(entry.contains("Future<void> main(List<String> args) async {"));
    assert!(entry.contains("  TapwingBinding.ensureInitialized();\n  await app.main(args);\n}"));
}

#[test]
fn creates_dev_dependencies_and_preserves_comments_and_formatting() {
    let tmp = TempDir::new("comments_no_dev_deps");
    let original = "\
# Header comment must survive
name: commented_app
version: 1.2.3+4

environment:
  sdk: ^3.11.0

# Runtime dependencies
dependencies:
  flutter:
    sdk: flutter
  # Inline section note
  cupertino_icons: ^1.0.8

# Flutter assets & fonts section
flutter:
  uses-material-design: true
";
    tmp.write("pubspec.yaml", original);
    tmp.write("lib/main.dart", "void main() {}\n");

    let mut args = default_args(tmp.path());
    args.driver_ref = Some("main".to_string());
    let plan = plan_init(&args).expect("plan commented pubspec");
    apply_plan(&plan, false).expect("apply commented pubspec");

    let updated = tmp.read("pubspec.yaml");
    assert!(updated.starts_with("# Header comment must survive\nname: commented_app\n"));
    assert!(updated.contains("# Runtime dependencies\ndependencies:\n"));
    assert!(updated.contains("  # Inline section note\n  cupertino_icons: ^1.0.8\n\ndev_dependencies:\n"));
    assert!(updated.contains("      ref: main\n\n# Flutter assets & fonts section\nflutter:\n"));
}

#[test]
fn idempotent_re_run_reports_already_initialized_and_force_overwrites() {
    let tmp = TempDir::new("idempotent_and_force");
    tmp.write("pubspec.yaml", &minimal_flutter_pubspec("idempotent_app"));
    tmp.write("lib/main.dart", "void main() {}\n");

    let args = default_args(tmp.path());
    let first = plan_init(&args).expect("first plan");
    apply_plan(&first, false).expect("first apply");

    // Customize generated entrypoint and verify a non-force re-run leaves both untouched.
    tmp.write("lib/main_tapwing.dart", "// custom manual edit\nvoid main() {}\n");
    let pubspec_before = tmp.read("pubspec.yaml");

    let second = plan_init(&args).expect("second plan");
    assert!(second.is_already_initialized());
    assert!(matches!(second.pubspec, FilePlan::SkipAlreadyPresent { .. }));
    assert!(matches!(second.entrypoint, FilePlan::SkipAlreadyPresent { .. }));
    apply_plan(&second, false).expect("second apply no-op");

    assert_eq!(tmp.read("pubspec.yaml"), pubspec_before);
    assert_eq!(tmp.read("lib/main_tapwing.dart"), "// custom manual edit\nvoid main() {}\n");

    // Re-run with --force and a new --driver-ref: both files are updated.
    let mut forced_args = default_args(tmp.path());
    forced_args.force = true;
    // Never the default ref (v<CARGO_PKG_VERSION>), whatever version the CLI is at.
    let forced_ref = format!("v{}-forced", env!("CARGO_PKG_VERSION"));
    forced_args.driver_ref = Some(forced_ref.clone());

    let forced = plan_init(&forced_args).expect("forced plan");
    assert!(!forced.is_already_initialized());
    assert!(matches!(forced.pubspec, FilePlan::Update { .. }));
    assert!(matches!(forced.entrypoint, FilePlan::Update { .. }));
    apply_plan(&forced, false).expect("forced apply");

    let pubspec_after = tmp.read("pubspec.yaml");
    assert!(pubspec_after.contains(&format!("ref: {forced_ref}\n")));
    assert!(!pubspec_after.contains(&format!("ref: v{}\n", env!("CARGO_PKG_VERSION"))));
    assert!(tmp.read("lib/main_tapwing.dart").contains("TapwingBinding.ensureInitialized();"));
}

#[test]
fn dry_run_prints_unified_diff_and_writes_nothing() {
    let tmp = TempDir::new("dry_run");
    let pubspec_before = minimal_flutter_pubspec("dry_run_app");
    tmp.write("pubspec.yaml", &pubspec_before);
    tmp.write("lib/main.dart", "void main() {}\n");

    let mut args = default_args(tmp.path());
    args.dry_run = true;

    let plan = plan_init(&args).expect("dry run plan");
    let diff = plan.render_unified_diff();
    assert!(diff.contains("--- a/pubspec.yaml\n+++ b/pubspec.yaml\n"));
    assert!(diff.contains("+  tapwing_driver:\n"));
    assert!(diff.contains("--- /dev/null\n+++ b/lib/main_tapwing.dart\n"));
    assert!(diff.contains("+  TapwingBinding.ensureInitialized();\n"));

    // Also run the compiled binary end-to-end with `--dry-run` and verify zero writes.
    let output = Command::new(env!("CARGO_BIN_EXE_tapwing"))
        .arg("init")
        .arg(tmp.path())
        .arg("--dry-run")
        .output()
        .expect("run tapwing init --dry-run");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--- a/pubspec.yaml"));
    assert!(stdout.contains("+++ b/lib/main_tapwing.dart"));

    assert_eq!(tmp.read("pubspec.yaml"), pubspec_before);
    assert!(!tmp.path().join("lib/main_tapwing.dart").exists());
}

#[test]
fn driver_path_writes_relative_path_dependency() {
    let tmp = TempDir::new("driver_path");
    tmp.write("app/pubspec.yaml", &minimal_flutter_pubspec("local_app"));
    tmp.write("app/lib/main.dart", "void main() {}\n");
    tmp.write("vendor/tapwing_driver/pubspec.yaml", "name: tapwing_driver\n");

    let mut args = default_args(&tmp.path().join("app"));
    args.driver_path = Some(tmp.path().join("vendor/tapwing_driver"));

    let plan = plan_init(&args).expect("plan driver path");
    assert_eq!(plan.driver_source, DriverSource::Path("../vendor/tapwing_driver".to_string()));
    apply_plan(&plan, false).expect("apply driver path");

    let pubspec = tmp.read("app/pubspec.yaml");
    assert!(pubspec.contains("  tapwing_driver:\n    path: ../vendor/tapwing_driver\n"));
    assert!(!pubspec.contains("git:"));
}
