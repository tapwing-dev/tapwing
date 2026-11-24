use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use std::collections::HashMap;
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
    /// Also emulate Maestro bugs (iOS `hideKeyboard` failing on a focused field).
    Strict,
}

#[derive(Parser)]
#[command(name = "tapwing", version, about = "Flutter-first runner for Maestro YAML flows")]
struct Cli {
    /// adb serial, booted iOS simulator UDID/name, or comma-separated list for sharded runs.
    #[arg(long, visible_alias = "udid", global = true)]
    device: Option<String>,
    /// Force the platform (`android` or `ios`).
    #[arg(short = 'p', long, value_enum, global = true)]
    platform: Option<Platform>,
    /// Host port the app dials (0 = a free port per session/shard).
    #[arg(long, global = true)]
    port: Option<u16>,
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
    /// Fail `inputText`/`eraseText` when no text field is focused.
    #[arg(long)]
    strict_input: bool,
    /// `strict` also reproduces Maestro bugs that Tapwing otherwise fixes with a warning.
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
        /// Print the parsed commands (Maestro's IR) as JSON.
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
    if matches!(expanded_args.get(1).and_then(|a| a.to_str()), Some("-V" | "--version")) {
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

fn run(cli: Cli) -> Result<bool> {
    let global_device = cli.device;
    let global_platform = cli.platform;
    let global_port = cli.port.unwrap_or(0);
    match cli.cmd {
        Cmd::Test(args) => {
            let cli_env: HashMap<String, String> = args
                .env
                .iter()
                .map(|kv| {
                    kv.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .ok_or_else(|| anyhow!("invalid -e/--env value `{kv}` (expected KEY=VALUE)"))
                })
                .collect::<Result<_>>()?;
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
                strict_input: args.strict_input,
                maestro_compat_strict: args.maestro_compat == MaestroCompat::Strict,
                input_mode: args.input,
                app_health,
                trace: args.trace.then_some(TraceOpts { screenshots: args.trace_screenshots }),
            })
        }
        Cmd::Check { paths, dump_ir } => {
            let mut files = Vec::new();
            let mut config_errors = 0;
            for p in &paths {
                if p.is_dir()
                    && let Some(cfg) = workspace::find_config_file(p)
                {
                    match workspace::config::read(&cfg) {
                        Ok(_) => println!("✓ {} (workspace config)", cfg.display()),
                        Err(e) => {
                            config_errors += 1;
                            eprint!("✗ {}", e.render());
                        }
                    }
                }
                collect_yaml(p, &mut files)?;
            }
            let t0 = Instant::now();
            let (mut ok, mut failed) = (0, 0);
            for file in &files {
                match flow::parse_file(file) {
                    Ok(f) => {
                        ok += 1;
                        if dump_ir {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&json!({ "path": f.path, "commands": f.commands }))?
                            );
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
            eprintln!(
                "{ok} valid, {failed} invalid ({} files in {:.1} ms)",
                files.len(),
                t0.elapsed().as_secs_f64() * 1000.0
            );
            Ok(failed == 0 && config_errors == 0)
        }
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

/// Files `tapwing check` validates: explicit paths as given; inside
/// directories every flow file, including sub-flows in sub-folders, but not
/// workspace configs or other non-flow YAML (`workspace::is_flow_file`).
fn collect_yaml(p: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if p.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(p)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            if e.is_dir() {
                collect_yaml(&e, out)?;
            } else if workspace::is_flow_file(&e, None) {
                out.push(e);
            }
        }
    } else {
        out.push(p.to_path_buf());
    }
    Ok(())
}
