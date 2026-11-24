mod device;
mod driver;
mod run;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use device::{Android, Device, IosSim};
use driver::Driver;
use tapwing::flow::{self, ParsedFlow};
use run::{Opts, Session};

/// Port the app dials on the device (compiled into the driver).
const DEVICE_PORT: u16 = 7781;

#[derive(Parser)]
#[command(name = "tapwing", version, about = "Flutter-first runner for Maestro YAML flows")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run Maestro flows.
    Test {
        /// Flow files.
        #[arg(required = true)]
        flows: Vec<PathBuf>,
        /// adb serial.
        #[arg(long)]
        device: Option<String>,
        /// Host port the device port is reversed to. 0 = a free port per
        /// session, so several devices never reach each other's sessions.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Write a JSON report with per-command timings.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Env variables (KEY=VALUE), like Maestro's -e.
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// How the driver decides the UI has settled (v2, legacy).
        #[arg(long, default_value = "v2")]
        settle_policy: String,
        /// After every action, verify no significant animation is still
        /// running (diagnostics for the settle detector).
        #[arg(long)]
        check_settle: bool,
    },
    /// Check flow files without a device: parse them exactly like Maestro
    /// 2.12.1 and report errors with their position.
    Check {
        /// Flow files or directories (searched for *.yaml / *.yml).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Print the parsed commands (Maestro's IR) as JSON.
        #[arg(long)]
        dump_ir: bool,
    },
    /// Dump the element tree of the running app (Maestro attribute names).
    Hierarchy {
        #[arg(long)]
        device: Option<String>,
        /// Host port (0 = pick a free one).
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
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
    match cli.cmd {
        Cmd::Test { flows, device, port, report, env, settle_policy, check_settle } => {
            let cli_env: HashMap<String, String> = env
                .iter()
                .map(|kv| {
                    kv.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .ok_or_else(|| anyhow!("invalid -e/--env value `{kv}` (expected KEY=VALUE)"))
                })
                .collect::<Result<_>>()?;
            let parsed = parse_all(&flows)?;
            let opts = Opts { settle_policy, check_settle, cli_env };
            let t0 = Instant::now();
            let mut drv = Driver::listen(port)?;
            // A booted simulator matching --device (UDID or name) means iOS;
            // it shares the host loopback, so no forwarding or keep-awake.
            let (mut dev, _guards) = match IosSim::find(device.as_deref())? {
                Some(sim) if device.is_some() => (Device::Ios(sim), None),
                _ => {
                    let a = Android::connect(device)?;
                    let guards = (a.reverse(DEVICE_PORT, drv.port()?)?, a.keep_awake()?);
                    (Device::Android(a), Some(guards))
                }
            };
            let setup_ms = t0.elapsed().as_secs_f64() * 1000.0;
            println!("device {} · setup {:.0} ms", dev.serial(), setup_ms);
            let mut all_ok = true;
            let mut reports = Vec::new();
            let mut session = Session { dev: &mut dev, drv: &mut drv, opts: &opts };
            for f in &parsed {
                let (ok, r) = run::run_flow(f, &mut session);
                all_ok &= ok;
                reports.push(r);
            }
            if let Some(path) = report {
                std::fs::write(
                    &path,
                    serde_json::to_string_pretty(&json!({ "setupMs": setup_ms, "flows": reports }))?,
                )?;
            }
            Ok(all_ok)
        }
        Cmd::Check { paths, dump_ir } => {
            let mut files = Vec::new();
            for p in &paths {
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
            Ok(failed == 0)
        }
        Cmd::Hierarchy { device, port } => {
            let dev = Android::connect(device)?;
            let mut drv = Driver::listen(port)?;
            let _reverse = dev.reverse(DEVICE_PORT, drv.port()?)?;
            drv.accept(Duration::from_secs(5))?;
            let snap = drv.call("snapshot", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&snap)?);
            Ok(true)
        }
    }
}

/// Parse every flow before touching the device, so a typo fails fast.
fn parse_all(paths: &[PathBuf]) -> Result<Vec<ParsedFlow>> {
    paths.iter().map(|p| flow::parse_file(p).map_err(|e| anyhow!("{}", e.render().trim_end()))).collect()
}

fn collect_yaml(p: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if p.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(p)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            if e.is_dir() || matches!(e.extension().and_then(|x| x.to_str()), Some("yaml" | "yml")) {
                collect_yaml(&e, out)?;
            }
        }
    } else {
        out.push(p.to_path_buf());
    }
    Ok(())
}
