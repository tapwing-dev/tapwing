//! Diagnostics for Tapwing's Android driver on a real device or emulator.
//!
//! ```sh
//! # Hierarchy of the current screen (several dumps; the first is saved) and a stability verdict:
//! cargo run --example native_hierarchy_diff -- dump --serial emulator-5554 --out /tmp/dumps --label settings
//! # Diff two saved dumps (optionally ignoring an attribute, e.g. on an animating screen):
//! cargo run --example native_hierarchy_diff -- diff a.xml b.xml [--ignore bounds]
//! # Type into the focused field (timed), read the result back, check the default IME:
//! cargo run --example native_hierarchy_diff -- type --serial emulator-5554 --text "Příliš žluťoučký kůň"
//! # Mock location through the driver's mock providers (the physical-device path):
//! cargo run --example native_hierarchy_diff -- location --serial emulator-5554 --lat 50.0755 --lon 14.4378
//! ```
//!
//! `--fresh` uninstalls the driver first to time a cold install. Every
//! session goes through `NativeLaneHandle`, the code path `tapwing test` uses
//! (APK lookup, hash-cached install, `am instrument`, readiness probe).
//!
//! The F11a comparison with Maestro's driver (before it was removed) is in
//! `bench/results/f11a/`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tapwing::native_lane::{Adb, NativeLaneHandle, NativeOps};

const READY_TIMEOUT: Duration = Duration::from_secs(60);

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// A started lane and how long the start took.
struct Started {
    handle: NativeLaneHandle,
    adb: Adb,
    ready: Duration,
}

fn start(serial: &str, fresh: bool) -> Result<Started> {
    let adb = Adb::new(serial);
    if fresh {
        let _ = adb.run(&["uninstall", "dev.tapwing.driver"]);
    }
    let t = Instant::now();
    let mut handle = NativeLaneHandle::spawn(serial);
    handle.get_or_wait(READY_TIMEOUT)?;
    Ok(Started { handle, adb, ready: t.elapsed() })
}

fn default_ime(adb: &Adb) -> String {
    adb.shell("settings get secure default_input_method").map(|s| s.trim().to_string()).unwrap_or_default()
}

/// p50 / max of `samples` in ms.
fn summary(samples: &[Duration]) -> (f64, f64) {
    let mut ms: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    let p50 = ms.get(ms.len() / 2).copied().unwrap_or(0.0);
    (p50, ms.last().copied().unwrap_or(0.0))
}

fn dump(args: &[String]) -> Result<()> {
    let serial = arg(args, "--serial").context("--serial")?;
    let out = PathBuf::from(arg(args, "--out").context("--out")?);
    let label = arg(args, "--label").context("--label")?;
    let rounds: usize = arg(args, "--rounds").map_or(Ok(5), |r| r.parse())?;
    std::fs::create_dir_all(&out)?;

    let mut s = start(&serial, flag(args, "--fresh"))?;
    let client = s.handle.get_or_wait(READY_TIMEOUT)?;
    let mut latencies = Vec::with_capacity(rounds);
    let mut dumps = Vec::with_capacity(rounds);
    for _ in 0..rounds.max(1) {
        let t = Instant::now();
        let xml = client.view_hierarchy()?;
        latencies.push(t.elapsed());
        dumps.push(xml);
    }
    let first = dumps.first().cloned().unwrap_or_default();
    std::fs::write(out.join(format!("{label}.tapwing.xml")), &first)?;
    let (p50, max) = summary(&latencies);
    println!(
        "{label} tapwing ready {:>6} ms  viewHierarchy p50 {p50:.0} ms max {max:.0} ms  {} bytes",
        s.ready.as_millis(),
        first.len()
    );
    // Consecutive dumps of a screen that holds still are equal (up to window order).
    let verdict = if dumps.iter().all(|d| *d == first) {
        "stable"
    } else if dumps.iter().all(|d| same_windows(d, &first)) {
        "stable up to window order"
    } else {
        "unstable screen (dumps differ)"
    };
    println!("{label}: {verdict}");
    let report = serde_json::json!({
        "readyMs": s.ready.as_millis() as u64,
        "hierarchyP50Ms": p50,
        "hierarchyMaxMs": max,
        "bytes": first.len(),
        "verdict": verdict,
    });
    std::fs::write(out.join(format!("{label}.json")), serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

/// The top-level `<node>` subtrees of a dump (one per window, plus a toast).
/// UiAutomator's `getWindowRoots` collects the roots in a `HashSet`, so their
/// order varies between dumps in Maestro itself; their content does not.
fn windows(xml: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in xml.lines() {
        if line.starts_with("  <node") {
            out.push(String::new());
        }
        if line.starts_with("  ")
            && let Some(w) = out.last_mut()
        {
            w.push_str(line);
            w.push('\n');
        }
    }
    out.sort();
    out
}

fn same_windows(a: &str, b: &str) -> bool {
    a == b || windows(a) == windows(b)
}

/// Line-level LCS diff of two pretty-printed dumps (same serializer, so lines
/// are whole nodes), printing at most `limit` changed lines.
fn print_diff(a: &str, b: &str, limit: usize) {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut shown, mut changed) = (0, 0, 0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            changed += 1;
            if shown < limit {
                println!("  + {}", b[j].trim());
                shown += 1;
            }
            j += 1;
        } else {
            changed += 1;
            if shown < limit {
                println!("  - {}", a[i].trim());
                shown += 1;
            }
            i += 1;
        }
    }
    println!("  {changed} changed line(s) ({n} vs {m} lines)");
}

/// Blanks attribute `name` (e.g. `bounds` of an animating screen) in every node.
fn without_attr(xml: &str, name: &str) -> String {
    let key = format!(" {name}=\"");
    xml.lines()
        .map(|line| match line.find(&key) {
            Some(start) => {
                let value = start + key.len();
                let end = line[value..].find('"').map_or(line.len(), |e| value + e);
                format!("{}{}", &line[..value], &line[end..])
            }
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn diff(args: &[String]) -> Result<()> {
    let (a, b) = match args {
        [a, b, ..] => (Path::new(a), Path::new(b)),
        _ => bail!("diff <a.xml> <b.xml> [--ignore <attribute>]..."),
    };
    let (mut a, mut b) = (std::fs::read_to_string(a)?, std::fs::read_to_string(b)?);
    for (i, opt) in args.iter().enumerate() {
        if opt == "--ignore"
            && let Some(name) = args.get(i + 1)
        {
            (a, b) = (without_attr(&a, name), without_attr(&b, name));
        }
    }
    if a == b {
        println!("identical");
    } else if same_windows(&a, &b) {
        println!("identical up to window order");
    } else {
        print_diff(&a, &b, usize::MAX);
    }
    Ok(())
}

/// The `text` attribute of the focused editor of a hierarchy dump.
fn focused_text(xml: &str) -> Option<String> {
    let line =
        xml.lines().find(|l| l.contains(" focused=\"true\"") && l.contains("class=\"android.widget.EditText\""))?;
    let start = line.find(" text=\"")? + 7;
    let end = start + line[start..].find('"')?;
    Some(line[start..end].replace("&amp;", "&").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">"))
}

fn type_text(args: &[String]) -> Result<()> {
    let serial = arg(args, "--serial").context("--serial")?;
    let text = arg(args, "--text").context("--text")?;
    let erase: i32 = arg(args, "--erase").map_or(Ok(60), |r| r.parse())?;
    let mut s = start(&serial, false)?;
    let ime_before = default_ime(&s.adb);
    let adb = s.adb.clone();
    let client = s.handle.get_or_wait(READY_TIMEOUT)?;
    let mut ops = NativeOps::new(client, &adb);
    ops.erase_text(erase)?;
    let t = Instant::now();
    ops.input_text(&text)?;
    let typed = t.elapsed();
    let result = focused_text(&client_view(&mut s)?);
    drop(s);
    let ime_after = default_ime(&adb);
    println!(
        "tapwing inputText {:>5} ms  field={:?}  ime {}",
        typed.as_millis(),
        result.unwrap_or_default(),
        if ime_before == ime_after { "unchanged".to_string() } else { format!("CHANGED {ime_before} -> {ime_after}") }
    );
    Ok(())
}

/// `setLocation` through the driver's mock providers (what physical devices
/// use; emulators normally go through `geo fix`), read back from `dumpsys location`.
fn location(args: &[String]) -> Result<()> {
    let serial = arg(args, "--serial").context("--serial")?;
    let lat: f64 = arg(args, "--lat").map_or(Ok(50.0755), |v| v.parse())?;
    let lon: f64 = arg(args, "--lon").map_or(Ok(14.4378), |v| v.parse())?;
    let mut s = start(&serial, false)?;
    let adb = s.adb.clone();
    let t = Instant::now();
    NativeOps::new(s.handle.get_or_wait(READY_TIMEOUT)?, &adb).set_location(lat, lon)?;
    let took = t.elapsed();
    std::thread::sleep(Duration::from_millis(2500));
    let dump = adb.shell("dumpsys location")?;
    let needle = format!("{lat:.4}");
    let providers: Vec<&str> = dump
        .lines()
        .filter(|l| l.contains(&needle) && l.to_ascii_lowercase().contains("location"))
        .map(str::trim)
        .take(4)
        .collect();
    println!("tapwing setLocation {:>4} ms  {} matching location line(s)", took.as_millis(), providers.len());
    for p in providers {
        println!("    {}", p.chars().take(160).collect::<String>());
    }
    drop(s);
    Ok(())
}

fn client_view(s: &mut Started) -> Result<String> {
    s.handle.get_or_wait(READY_TIMEOUT)?.view_hierarchy()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("dump") => dump(&args[1..]),
        Some("diff") => diff(&args[1..]),
        Some("type") => type_text(&args[1..]),
        Some("location") => location(&args[1..]),
        _ => bail!("usage: native_hierarchy_diff dump|diff|type|location ... (see the file header)"),
    }
}
