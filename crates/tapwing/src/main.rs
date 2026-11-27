use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tapwing::app_health::{AppErrorsPolicy, AppHealthOpts};
use tapwing::device::{Device, Platform};
use tapwing::driver::Driver;
use tapwing::flow;
use tapwing::init::{self, InitArgs};
use tapwing::native_lane::{InputMode, deprecated_native_driver_warning};
use tapwing::report::ReportFormat;
use tapwing::run::suite::{DEVICE_PORT, SuiteRunConfig, run_test_suite};
use tapwing::run::{self, Opts};
use tapwing::trace::{TraceOpts, TraceScreenshots};
use tapwing::workspace;

/// `--maestro-compat` levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum MaestroCompat {
    /// Maestro semantics, except platform bugs Tapwing fixes with a warning.
    Default,
    /// Also emulate what Tapwing changes on purpose (iOS `hideKeyboard` failing on a focused
    /// field, iOS Simulator `clearState` reinstalling the app).
    Strict,
}

#[derive(Parser)]
#[command(
    name = "tapwing",
    version,
    about = "Flutter-first runner for Maestro YAML flows",
    disable_version_flag = true
)]
struct Cli {
    /// Print version.
    #[arg(short = 'v', visible_short_alias = 'V', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,
    /// adb serial, booted iOS simulator UDID/name, or comma-separated list for sharded runs.
    #[arg(long, visible_alias = "udid", global = true)]
    device: Option<String>,
    /// Force the platform (`android` or `ios`).
    #[arg(short = 'p', long, value_enum, global = true)]
    platform: Option<Platform>,
    /// Host port the app dials (0 = a free port per session/shard).
    #[arg(long, global = true)]
    port: Option<u16>,
    /// Maestro CLI compatibility no-op: disable ANSI output.
    #[arg(long, global = true, hide = true)]
    no_ansi: bool,
    /// Maestro CLI compatibility no-op: disable color output.
    #[arg(long, global = true, hide = true)]
    no_color: bool,
    /// Maestro CLI compatibility no-op: enable ANSI output.
    #[arg(long, global = true, hide = true)]
    ansi: bool,
    /// Maestro CLI compatibility no-op: enable color output.
    #[arg(long, global = true, hide = true)]
    color: bool,
    /// Maestro CLI compatibility no-op: verbose logging toggle.
    #[arg(long, global = true, hide = true)]
    verbose: bool,
    /// Maestro CLI compatibility no-op: remote driver host.
    #[arg(long, global = true, hide = true, value_name = "HOST")]
    host: Option<String>,
    /// Maestro CLI compatibility no-op: driver host port.
    #[arg(long, global = true, hide = true, value_name = "PORT")]
    driver_host_port: Option<u16>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct TestArgs {
    /// Flow files or workspace directories.
    #[arg(required = true)]
    flows: Vec<PathBuf>,
    /// Only run flows with at least one of these tags (comma-separated, merged with `config.yaml`).
    #[arg(long, value_delimiter = ',')]
    include_tags: Vec<String>,
    /// Skip flows with any of these tags (comma-separated, merged with `config.yaml`).
    #[arg(long, value_delimiter = ',')]
    exclude_tags: Vec<String>,
    /// Workspace config file (default: `config.yaml` / `config.yml` in the workspace directory).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Split `flows` evenly (round-robin) across N connected devices in parallel.
    #[arg(long)]
    shard_split: Option<usize>,
    /// Run all `flows` redundantly on N connected devices in parallel.
    #[arg(long)]
    shard_all: Option<usize>,
    /// Deprecated alias for `--shard-split <N>`.
    #[arg(short = 's', long)]
    shards: Option<usize>,
    /// Write a JSON report with per-command timings.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Test suite report format (`junit`, `html`, `html-detailed`, `noop`).
    #[arg(long, value_enum, ignore_case = true, default_value_t = ReportFormat::Noop)]
    format: ReportFormat,
    /// Output file path for `--format` (default: `report.xml` for junit, `report.html` for html).
    #[arg(long)]
    output: Option<PathBuf>,
    /// Custom `<testsuite name="...">` for JUnit XML and HTML report subtitle.
    #[arg(long)]
    test_suite_name: Option<String>,
    /// Directory for run artifacts (`screenshots/`, `screenshot-diffs/`).
    #[arg(long)]
    test_output_dir: Option<PathBuf>,
    /// Base directory for debug/run artifacts when `--test-output-dir` and `config.testOutputDir` are unset.
    #[arg(long)]
    debug_output: Option<PathBuf>,
    /// Write artifacts directly into the output root without timestamped `run-*` or `shard-N` subfolders.
    #[arg(long)]
    flatten_debug_output: bool,
    /// Env variables (`KEY=VALUE`), like Maestro's `-e`.
    #[arg(short = 'e', long = "env")]
    env: Vec<String>,
    /// How the driver decides the UI has settled (`v2`, `legacy`).
    #[arg(long, default_value = "v2")]
    settle_policy: String,
    /// After every action, verify no significant animation is still running.
    #[arg(long)]
    check_settle: bool,
    /// How long one in-flight HTTP request or pending image keeps a step waiting to settle, in ms
    /// (`0`: until it ends). Default: `config.yaml` `tapwing.settleLoadTimeoutMs`, else 5000.
    #[arg(long, value_name = "MS")]
    settle_load_timeout_ms: Option<u64>,
    /// Regex for request URLs that never keep a step waiting to settle, such as analytics or
    /// long-poll endpoints (repeatable; added to `tapwing.settleHttpIgnore`).
    #[arg(long, value_name = "REGEX")]
    settle_http_ignore: Vec<String>,
    /// Fail `inputText`/`eraseText` when no text field is focused.
    #[arg(long)]
    strict_input: bool,
    /// `strict` also reproduces Maestro behavior that Tapwing otherwise changes (a warning for bugs, a faster
    /// `clearState` on the iOS Simulator).
    #[arg(long, value_enum, default_value = "default")]
    maestro_compat: MaestroCompat,
    /// Which lane drives the UI (`auto`, `flutter`, `native`).
    #[arg(long, value_enum, default_value = "auto")]
    input: InputMode,
    /// Deprecated and ignored: Tapwing's own native driver is always used (Maestro's drivers were removed).
    #[arg(long, value_name = "DRIVER")]
    native_driver: Option<String>,
    /// App health issues (Flutter errors, uncaught Dart errors, overflows, crashes): `report` prints and
    /// reports them, `fail` also fails the flow, `off` disables capture. Default: `config.yaml` `tapwing.appErrors`, else `report`.
    #[arg(long, value_enum)]
    app_errors: Option<AppErrorsPolicy>,
    /// Regex for app issues to ignore (repeatable; added to `tapwing.appErrorsIgnore`).
    #[arg(long)]
    app_errors_ignore: Vec<String>,
    /// Screenshot the first occurrence of each app issue into `<test-output-dir>/app-issues/`.
    #[arg(long)]
    app_errors_screenshot: bool,
    /// Write an offline trace viewer (`trace.html` + `trace.json`) per shard into the test output dir:
    /// steps, lanes, timings, app health, failure screenshots, and the app's HTTP requests (redacted).
    #[arg(long)]
    trace: bool,
    /// With `--trace`: capture a screenshot and UI tree after every step (`steps`) or only on failures.
    #[arg(long, value_enum, default_value = "failures", requires = "trace")]
    trace_screenshots: TraceScreenshots,
    /// Maestro CLI compatibility no-op: continuous watch mode (not implemented).
    #[arg(short = 'c', long, hide = true)]
    continuous: bool,
    /// Maestro CLI compatibility no-op: headless web mode.
    #[arg(long, hide = true)]
    headless: bool,
    /// Maestro CLI compatibility no-op: headless web browser size.
    #[arg(long, hide = true, value_name = "WIDTHxHEIGHT")]
    screen_size: Option<String>,
    /// Maestro CLI compatibility no-op: AI insights analysis.
    #[arg(long, hide = true)]
    analyze: bool,
    /// Maestro CLI compatibility no-op: Maestro Cloud API base URL.
    #[arg(long, hide = true, value_name = "URL")]
    api_url: Option<String>,
    /// Maestro CLI compatibility no-op: Maestro Cloud API key.
    #[arg(long, hide = true, value_name = "KEY")]
    api_key: Option<String>,
    /// Maestro CLI compatibility no-op: force driver reinstall.
    #[arg(long, hide = true)]
    reinstall_driver: bool,
    /// Maestro CLI compatibility no-op: skip driver reinstall.
    #[arg(long, hide = true)]
    no_reinstall_driver: bool,
    /// Maestro CLI compatibility no-op: Apple Team ID for XCTest signing.
    #[arg(long, hide = true, value_name = "TEAM_ID")]
    apple_team_id: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run Maestro flows: flow files and/or workspace directories.
    Test(Box<TestArgs>),
    /// Check flow files without a device: parse them like Maestro 2.12.1 and report errors.
    Check {
        /// Flow files or directories (searched for *.yaml / *.yml).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Print the parsed commands (Maestro's IR) as one JSON document on stdout: an object for a single
        /// flow file, an array of objects for a directory or several paths.
        #[arg(long)]
        dump_ir: bool,
    },
    /// Dump the element tree of the running app (physical pixels on Android, points on iOS).
    Hierarchy {
        /// App to read: iOS bundle id (default: the only running user app on the
        /// simulator); Android package, whose PID the connecting app must have
        /// (default: the first instrumented app that connects).
        #[arg(long)]
        app_id: Option<String>,
    },
    /// Wire `tapwing_driver` and a `lib/main_tapwing.dart` entrypoint into a Flutter app.
    Init(InitArgs),
}

fn main() {
    let expanded_args = match tapwing::cli_args::expand_argfiles(std::env::args_os()) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(2);
        }
    };
    // `--version` also reports where the native-lane drivers come from, which
    // needs the filesystem, so it is answered before clap's static version.
    if matches!(expanded_args.get(1).and_then(|a| a.to_str()), Some("-V" | "-v" | "--version")) {
        print!("{}", tapwing::driver_bundle::version_report());
        return;
    }
    let cli = Cli::parse_from(expanded_args);
    tapwing::interrupt::install();
    let res = run(cli);
    // All RAII teardown guards owned by `run(cli)` have now dropped cleanly.
    if let Some(code) = tapwing::interrupt::exit_code() {
        std::process::exit(code);
    }
    let code = match res {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

/// Merges `-e KEY=VALUE` pairs with `MAESTRO_*` process environment variables,
/// matching Maestro 2.12.1 (`Env.withInjectedShellEnvVars`).
///
/// Explicit `-e` / `--env` entries take precedence. Process variables are
/// included only when the key starts with `MAESTRO_`, the key is not an
/// internal shard variable (`MAESTRO_SHARD_ID` / `MAESTRO_SHARD_INDEX`), and
/// the value is non-empty; the `MAESTRO_` prefix is preserved.
fn merge_flow_env<I, S, P>(cli_pairs: I, process_env: P) -> Result<HashMap<String, String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    P: IntoIterator<Item = (String, String)>,
{
    let mut merged: HashMap<String, String> = HashMap::new();
    for kv in cli_pairs {
        let kv = kv.as_ref();
        let (k, v) = kv.split_once('=').ok_or_else(|| anyhow!("invalid -e/--env value `{kv}` (expected KEY=VALUE)"))?;
        merged.insert(k.to_string(), v.to_string());
    }
    for (k, v) in process_env {
        if k.starts_with("MAESTRO_")
            && !v.is_empty()
            && !tapwing::orchestra::INTERNAL_ONLY_ENV_VARS.contains(&k.as_str())
        {
            merged.entry(k).or_insert(v);
        }
    }
    Ok(merged)
}

/// Returns stderr warnings for accepted Maestro CLI flags that Tapwing ignores.
fn compat_flag_warnings(cli: &Cli) -> Vec<String> {
    let mut warnings = Vec::new();
    let global_bools = [
        (cli.no_ansi, "--no-ansi", "Tapwing emits plain text output"),
        (cli.no_color, "--no-color", "Tapwing emits plain text output"),
        (cli.ansi, "--ansi", "Tapwing emits plain text output"),
        (cli.color, "--color", "Tapwing emits plain text output"),
        (cli.verbose, "--verbose", "Tapwing does not have a verbose logging toggle"),
    ];
    for (set, flag, reason) in global_bools {
        if set {
            warnings.push(format!("warning: {flag} is accepted for Maestro compatibility and ignored ({reason})"));
        }
    }
    if cli.host.is_some() {
        warnings.push(
            "warning: --host is accepted for Maestro compatibility and ignored (Tapwing manages driver connections locally)"
                .into(),
        );
    }
    if cli.driver_host_port.is_some() {
        warnings.push(
            "warning: --driver-host-port is accepted for Maestro compatibility and ignored (Tapwing allocates driver ports automatically)"
                .into(),
        );
    }
    if let Cmd::Test(args) = &cli.cmd {
        let test_bools = [
            (args.continuous, "-c/--continuous", "watch mode is not implemented; running once"),
            (args.headless, "--headless", "Web target is not supported"),
            (args.analyze, "--analyze", "Maestro Cloud AI insights are not supported"),
            (args.reinstall_driver, "--reinstall-driver", "Tapwing manages its native driver lifecycle automatically"),
            (
                args.no_reinstall_driver,
                "--no-reinstall-driver",
                "Tapwing manages its native driver lifecycle automatically",
            ),
        ];
        for (set, flag, reason) in test_bools {
            if set {
                warnings.push(format!("warning: {flag} is accepted for Maestro compatibility and ignored ({reason})"));
            }
        }
        let test_opts = [
            (args.screen_size.is_some(), "--screen-size", "Web target is not supported"),
            (args.api_url.is_some(), "--api-url", "Maestro Cloud is not supported"),
            (args.api_key.is_some(), "--api-key", "Maestro Cloud is not supported"),
            (args.apple_team_id.is_some(), "--apple-team-id", "Tapwing does not sign Xcode runner bundles"),
        ];
        for (set, flag, reason) in test_opts {
            if set {
                warnings.push(format!("warning: {flag} is accepted for Maestro compatibility and ignored ({reason})"));
            }
        }
    }
    warnings
}

fn run(cli: Cli) -> Result<bool> {
    for warning in compat_flag_warnings(&cli) {
        eprintln!("{warning}");
    }
    let _ = cli.version;
    let global_device = cli.device;
    let global_platform = cli.platform;
    let global_port = cli.port.unwrap_or(0);
    match cli.cmd {
        Cmd::Test(args) => {
            let cli_env = merge_flow_env(&args.env, std::env::vars())?;
            let plan = workspace::plan(&args.flows, &args.include_tags, &args.exclude_tags, args.config.as_deref())?;
            let tapwing_cfg = &plan.config.tapwing;
            for (origin, value) in [
                ("--native-driver", args.native_driver.as_deref()),
                ("config.yaml tapwing.nativeDriver", tapwing_cfg.native_driver.as_deref()),
            ] {
                if let Some(warning) = value.and_then(|v| deprecated_native_driver_warning(origin, v)) {
                    eprintln!("{warning}");
                }
            }
            let mut ignore = tapwing_cfg.app_errors_ignore.clone();
            ignore.extend(args.app_errors_ignore.iter().cloned());
            let app_health = AppHealthOpts::new(
                args.app_errors.or(tapwing_cfg.app_errors).unwrap_or_default(),
                &ignore,
                args.app_errors_screenshot || tapwing_cfg.app_errors_screenshot.unwrap_or(false),
            )?;
            let settle_load =
                run::SettleLoadOpts::resolve(args.settle_load_timeout_ms, &args.settle_http_ignore, tapwing_cfg)?;
            run_test_suite(SuiteRunConfig {
                plan,
                shard_split: args.shard_split,
                shard_all: args.shard_all,
                shards: args.shards,
                device: global_device,
                platform: global_platform,
                port: global_port,
                report_json: args.report,
                report_format: args.format,
                report_output: args.output,
                test_suite_name: args.test_suite_name,
                test_output_dir: args.test_output_dir,
                debug_output: args.debug_output,
                flatten_debug_output: args.flatten_debug_output,
                cli_env,
                settle_policy: args.settle_policy,
                check_settle: args.check_settle,
                settle_load,
                strict_input: args.strict_input,
                maestro_compat_strict: args.maestro_compat == MaestroCompat::Strict,
                input_mode: args.input,
                app_health,
                trace: args.trace.then_some(TraceOpts { screenshots: args.trace_screenshots }),
            })
        }
        Cmd::Check { paths, dump_ir } => check(&paths, dump_ir),
        Cmd::Hierarchy { app_id } => {
            let mut dev = Device::select(global_device.as_deref(), global_platform)?;
            let mut drv = Driver::listen(global_port)?;
            drv.expect_platform(dev.platform().wire_name());
            let host_port = drv.port()?;
            let mut _reverse = None;
            let mut app_name = None;
            let mut expected_pid = None;
            match &mut dev {
                Device::Android(a) => {
                    if let Some(app) = &app_id {
                        expected_pid = Some(
                            a.pid_of(app)?.ok_or_else(|| anyhow::anyhow!("{app} is not running on {}", a.serial))?,
                        );
                    }
                    _reverse = Some(a.reverse(DEVICE_PORT, host_port)?)
                }
                Device::Ios(sim) => {
                    let app = match app_id {
                        Some(a) => a,
                        None => match sim.running_apps()?.as_slice() {
                            [one] => one.clone(),
                            [] => anyhow::bail!("no user app is running on {}; pass --app-id", sim.name),
                            many => anyhow::bail!("several apps are running ({}); pass --app-id", many.join(", ")),
                        },
                    };
                    sim.write_port_file(&app, host_port)?;
                    app_name = sim.display_name(&app);
                }
            }
            drv.accept_with_pid(Duration::from_secs(5), expected_pid, &[])?;
            let opts = Opts {
                settle_policy: "v2".into(),
                check_settle: false,
                settle_load: Default::default(),
                cli_env: HashMap::new(),
                test_output_dir: PathBuf::new(),
                strict_input: false,
                soft_keyboard_hint: None,
                maestro_compat_strict: false,
                http_agent: None,
                input_mode: InputMode::Flutter,
                shard_index: None,
                log_prefix: String::new(),
                app_errors: tapwing::app_health::AppErrorsPolicy::Off,
                trace: false,
                driver_probe: false,
            };
            drv.call("configure", run::driver_config(&opts, dev.platform(), app_name.as_deref()))?;
            let snap = drv.call("snapshot", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&snap)?);
            Ok(true)
        }
        Cmd::Init(args) => {
            init::run(args)?;
            Ok(true)
        }
    }
}

/// `tapwing check`: validates flows offline; `Ok(false)` if anything is invalid.
///
/// With `--dump-ir` stdout carries exactly one JSON document and every status
/// line goes to stderr, so the output can be piped into a JSON tool.
fn check(paths: &[PathBuf], dump_ir: bool) -> Result<bool> {
    let loader = flow::Loader::new();
    let mut files = Vec::new();
    let mut path_errors = 0;
    for p in paths {
        if p.is_dir()
            && let Some(cfg) = workspace::find_config_file(p)
        {
            match workspace::config::read(&cfg) {
                Ok(_) if dump_ir => eprintln!("✓ {} (workspace config)", cfg.display()),
                Ok(_) => println!("✓ {} (workspace config)", cfg.display()),
                Err(e) => {
                    path_errors += 1;
                    eprint!("✗ {}", e.render());
                }
            }
        }
        let before = files.len();
        collect_yaml(p, &mut files, &mut HashSet::new(), &loader)?;
        if files.len() == before {
            // Only a directory can contribute nothing; passing CI on it would
            // hide a wrong path or an emptied flow folder.
            path_errors += 1;
            eprintln!("✗ {}: no flow files found", p.display());
        }
    }
    let t0 = Instant::now();
    let (mut ok, mut failed) = (0, 0);
    let mut ir = Vec::new();
    for file in &files {
        match loader.parse_file(file) {
            Ok(f) => {
                ok += 1;
                if dump_ir {
                    ir.push(json!({ "path": f.path, "commands": f.commands }));
                } else {
                    println!("✓ {} ({} commands)", file.display(), f.commands.len());
                }
            }
            Err(e) => {
                failed += 1;
                eprint!("✗ {}", e.render());
            }
        }
    }
    if dump_ir {
        // One flow file argument keeps the original `{path, commands}` object;
        // a directory or several paths give an array of those objects, so the
        // shape follows from the command line, not from what a folder holds.
        let single_file = matches!(paths, [only] if !only.is_dir());
        match ir.as_slice() {
            _ if !single_file => println!("{}", serde_json::to_string_pretty(&ir)?),
            [flow] => println!("{}", serde_json::to_string_pretty(flow)?),
            [] => println!("null"),
            _ => println!("{}", serde_json::to_string_pretty(&ir)?),
        }
    }
    eprintln!("{ok} valid, {failed} invalid ({} files in {:.1} ms)", files.len(), t0.elapsed().as_secs_f64() * 1000.0);
    Ok(failed == 0 && path_errors == 0)
}

/// Files `tapwing check` validates: explicit paths as given; inside
/// directories every flow file, including sub-flows in sub-folders, but not
/// workspace configs or other non-flow YAML (`workspace::is_flow_file`).
///
/// `visited` holds the canonical path of every directory walked so far: a
/// symlink that leads back into the tree is entered once instead of forever.
fn collect_yaml(p: &Path, out: &mut Vec<PathBuf>, visited: &mut HashSet<PathBuf>, loader: &flow::Loader) -> Result<()> {
    if p.is_dir() {
        let canonical = p.canonicalize().with_context(|| format!("resolving {}", p.display()))?;
        if !visited.insert(canonical) {
            return Ok(());
        }
        let listing = std::fs::read_dir(p).with_context(|| format!("reading {}", p.display()))?;
        let mut entries: Vec<PathBuf> = listing.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            if e.is_dir() {
                collect_yaml(&e, out, visited, loader)?;
            } else if workspace::is_flow_file_with_loader(&e, None, loader) {
                out.push(e);
            }
        }
    } else {
        out.push(p.to_path_buf());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_flow_env_injects_maestro_vars_and_cli_wins() {
        let proc_env = vec![
            ("MAESTRO_EXAMPLE".into(), "from_shell".into()),
            ("MAESTRO_API_TOKEN".into(), "secret-123".into()),
            ("MAESTRO_EMPTY".into(), "".into()),
            ("MAESTRO_SHARD_ID".into(), "99".into()),
            ("MAESTRO_SHARD_INDEX".into(), "98".into()),
            ("maestro_lowercase".into(), "ignored".into()),
            ("PATH".into(), "/usr/bin".into()),
            ("APP_ID".into(), "ignored_without_prefix".into()),
        ];
        let cli_pairs = ["MAESTRO_EXAMPLE=from_cli", "CUSTOM=a=b=c", "EMPTY_CLI="];

        let merged = merge_flow_env(cli_pairs, proc_env).unwrap();

        assert_eq!(merged.get("MAESTRO_EXAMPLE").map(String::as_str), Some("from_cli"));
        assert_eq!(merged.get("MAESTRO_API_TOKEN").map(String::as_str), Some("secret-123"));
        assert_eq!(merged.get("CUSTOM").map(String::as_str), Some("a=b=c"));
        assert_eq!(merged.get("EMPTY_CLI").map(String::as_str), Some(""));
        assert!(!merged.contains_key("EXAMPLE"));
        assert!(!merged.contains_key("MAESTRO_EMPTY"));
        assert!(!merged.contains_key("MAESTRO_SHARD_ID"));
        assert!(!merged.contains_key("MAESTRO_SHARD_INDEX"));
        assert!(!merged.contains_key("maestro_lowercase"));
        assert!(!merged.contains_key("PATH"));
        assert!(!merged.contains_key("APP_ID"));
    }

    #[test]
    fn merge_flow_env_rejects_malformed_cli_entry() {
        let err = merge_flow_env(["NO_EQUALS"], std::iter::empty()).unwrap_err();
        assert!(err.to_string().contains("invalid -e/--env value `NO_EQUALS`"));
    }

    #[test]
    fn cli_parses_representative_maestro_ci_invocations_and_warns() {
        let cli = Cli::try_parse_from([
            "tapwing",
            "--no-ansi",
            "--verbose",
            "--host",
            "127.0.0.1",
            "--driver-host-port",
            "7001",
            "-p",
            "ios",
            "--udid",
            "SIM-123",
            "test",
            "--no-reinstall-driver",
            "--apple-team-id",
            "ABCDE12345",
            "--flatten-debug-output",
            "--debug-output",
            "./artifacts",
            "--format",
            "junit",
            "--output",
            "junit.xml",
            "-e",
            "FOO=bar",
            "flow.yaml",
        ])
        .unwrap();

        let warnings = compat_flag_warnings(&cli);
        assert!(warnings.iter().any(|w| w.contains("--no-ansi")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("--verbose")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("--host")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("--driver-host-port")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("--no-reinstall-driver")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("--apple-team-id")), "{warnings:?}");
        assert!(!warnings.iter().any(|w| w.contains("--flatten-debug-output")), "{warnings:?}");
        assert!(!warnings.iter().any(|w| w.contains("--debug-output")), "{warnings:?}");
    }

    #[test]
    fn cli_parses_remaining_maestro_test_and_global_flags() {
        let cli = Cli::try_parse_from([
            "tapwing",
            "test",
            "--no-color",
            "--ansi",
            "--color",
            "-c",
            "--headless",
            "--screen-size",
            "1920x1080",
            "--analyze",
            "--api-url",
            "https://api.copilot.mobile.dev",
            "--api-key",
            "secret",
            "--reinstall-driver",
            "flows/",
        ])
        .unwrap();

        let warnings = compat_flag_warnings(&cli);
        for flag in [
            "--no-color",
            "--ansi",
            "--color",
            "-c/--continuous",
            "--headless",
            "--screen-size",
            "--analyze",
            "--api-url",
            "--api-key",
            "--reinstall-driver",
        ] {
            assert!(warnings.iter().any(|w| w.contains(flag)), "missing warning for {flag}: {warnings:?}");
        }

        let clean = Cli::try_parse_from(["tapwing", "test", "-p", "android", "flow.yaml"]).unwrap();
        assert!(compat_flag_warnings(&clean).is_empty());

        let version_err = Cli::try_parse_from(["tapwing", "-v"]).err().expect("-v triggers DisplayVersion");
        assert_eq!(version_err.kind(), clap::error::ErrorKind::DisplayVersion);

        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
