//! The Android half of the probe: the payload is read on the device, where the
//! APK already is, through the adb client the host already holds open.
//!
//! Pulling a 25 MB APK to the host costs more than the 20–50 ms `unzip -p | grep`
//! takes on the device. Two scripts run, both under the probe's own deadline:
//!
//! 1. *Listing* (one round trip): `pm path` (every split, not just the base),
//!    each APK's size and mtime (the install fingerprint) and the entries under
//!    `lib/` plus the debug kernel blob, read from the zip's central directory
//!    only, so a large non-Flutter APK is not read. It also lists native
//!    libraries extracted next to the APK (system images ship those).
//! 2. *Scan*: only when no cached verdict answers, `unzip -p` of the Dart
//!    payloads into `grep -a -o -F`. The byte count of that stream must equal
//!    the size the listing promised, or the read counts as incomplete.
//!
//! Anything the device does not answer as expected is a gap, never a guess.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::cache::DriverCache;
use super::markers::{self, MARKERS};
use super::{Engine, Inspection, Payload, Report, resolve};
use crate::device::adb_client::{AdbClient, ShellOutput};
use crate::device::android::{sh_quote, validate_package_name};

/// Nothing a build ships comes near this; past it the scan is not worth its cost.
const MAX_SCAN_BYTES: u64 = 128 * 1024 * 1024;

const PM_OK: &str = "TW_PM:";
const APK: &str = "TW_APK:";
const STAT: &str = "TW_STAT:";
const UNZIP_RC: &str = "TW_UNZIP_RC:";
const LIB: &str = "TW_LIB:";
const END: &str = "TW_END";
const SCAN: &str = "TW_SCAN:";
const BYTES: &str = "TW_BYTES:";
const GREP_RC: &str = "TW_GREP_RC:";

/// Script 1: where the installed app is and what its APKs hold.
pub(crate) fn listing_script(package: &str) -> String {
    format!(
        r#"P=$(pm path {pkg} 2>/dev/null); echo "{PM_OK}$?"
for l in $P; do
  A=${{l#package:}}
  echo "{APK}$A"
  stat -c '{STAT}%s:%Y' "$A" 2>/dev/null || echo "{STAT}?"
  unzip -l "$A" 'lib/*' 'assets/flutter_assets/kernel_blob.bin' 2>&1
  echo "{UNZIP_RC}$?"
  D=${{A%/*}}
  for f in "$D"/lib/*/libflutter.so "$D"/lib/*/libapp.so; do
    [ -f "$f" ] && echo "{LIB}$(wc -c < "$f"):$f"
  done
done
echo {END}
"#,
        pkg = sh_quote(package)
    )
}

/// What one APK's central directory lists under the paths asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ApkListing {
    pub path: String,
    pub size: Option<u64>,
    pub mtime: Option<u64>,
    pub entries: Vec<(String, u64)>,
    /// The file count of `unzip -l`'s summary line.
    pub promised: Option<usize>,
    /// Why this APK's listing cannot be trusted, if so.
    pub problem: Option<String>,
}

/// A native library that sits next to the APK instead of inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiskLib {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Listing {
    pub apks: Vec<ApkListing>,
    pub libs: Vec<DiskLib>,
}

impl Listing {
    /// Changes whenever a different build is installed: the paths (random per
    /// install since Android 11), sizes and mtimes of every APK. `None` when
    /// any of them is missing, since such a key could repeat across installs.
    pub(crate) fn fingerprint(&self) -> Option<String> {
        let mut parts = Vec::new();
        for apk in &self.apks {
            parts.push(format!("{}:{}:{}", apk.path, apk.size?, apk.mtime?));
        }
        (!parts.is_empty()).then(|| {
            let digest = Sha256::digest(parts.join("|").as_bytes());
            digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
        })
    }
}

/// Parses the output of [`listing_script`]. `Err` means the device did not run
/// it to the end or `pm path` failed: nothing can be concluded.
pub(crate) fn parse_listing(out: &ShellOutput) -> Result<Listing, String> {
    let mut saw_pm = false;
    let mut ended = false;
    let mut listing = Listing::default();
    for line in out.stdout.lines().map(str::trim_end) {
        if let Some(rc) = line.strip_prefix(PM_OK) {
            if rc != "0" {
                return Err(format!(
                    "`pm path` failed (exit {rc}): the app is not installed or the package manager is down"
                ));
            }
            saw_pm = true;
        } else if let Some(path) = line.strip_prefix(APK) {
            listing.apks.push(ApkListing { path: path.to_string(), ..ApkListing::default() });
        } else if let Some(stat) = line.strip_prefix(STAT) {
            if let Some(apk) = listing.apks.last_mut() {
                let mut parts = stat.split(':');
                apk.size = parts.next().and_then(|s| s.trim().parse().ok());
                apk.mtime = parts.next().and_then(|s| s.trim().parse().ok());
            }
        } else if let Some(rc) = line.strip_prefix(UNZIP_RC) {
            if let Some(apk) = listing.apks.last_mut() {
                apk.problem = unzip_problem(apk, rc);
            }
        } else if let Some(lib) = line.strip_prefix(LIB) {
            let Some((bytes, path)) = lib.split_once(':') else { continue };
            if let Ok(bytes) = bytes.trim().parse() {
                listing.libs.push(DiskLib { path: path.to_string(), bytes });
            }
        } else if line == END {
            ended = true;
        } else if let Some(apk) = listing.apks.last_mut() {
            parse_unzip_line(line, apk);
        }
    }
    if !saw_pm || !ended {
        return Err("the listing script did not run to its end".into());
    }
    if listing.apks.is_empty() {
        return Err("`pm path` printed no APK".into());
    }
    Ok(listing)
}

/// Whether the listing of one APK, complete up to its exit status, can be trusted:
/// `unzip` succeeded and its summary line counts the rows that were read.
fn unzip_problem(apk: &ApkListing, rc: &str) -> Option<String> {
    // Android's toybox prints an empty table and exits 0 when no entry matches the
    // patterns; Info-ZIP prints the same table and exits 11.
    if rc != "0" && !(rc == "11" && apk.promised == Some(0)) {
        return Some(format!("`unzip -l` failed (exit {rc})"));
    }
    match apk.promised {
        None => Some("`unzip -l` printed no summary line".into()),
        Some(n) if n != apk.entries.len() => {
            Some(format!("`unzip -l` promised {n} entries but {} were read", apk.entries.len()))
        }
        Some(_) => None,
    }
}

/// One line of `unzip -l`: a row (`  <len>  <date> <time>   <name>`) or the
/// summary (`  <total>   <n> files`). Headers, rules and noise are ignored.
fn parse_unzip_line(line: &str, apk: &mut ApkListing) {
    let trimmed = line.trim_start();
    if let Some(count) = footer_count(trimmed) {
        apk.promised = Some(count);
        return;
    }
    let Some((len, rest)) = trimmed.split_once(' ') else { return };
    let Ok(len) = len.parse::<u64>() else { return };
    let Some((date, rest)) = rest.trim_start().split_once(' ') else { return };
    let Some((time, name)) = rest.trim_start().split_once(' ') else { return };
    // 1981-01-01 (toybox) or 01-01-1981 (Info-ZIP), and 01:01.
    if date.len() == 10 && date.contains('-') && time.contains(':') {
        apk.entries.push((name.trim().to_string(), len));
    }
}

fn footer_count(line: &str) -> Option<usize> {
    let mut words = line.split_whitespace();
    words.next()?.parse::<u64>().ok()?;
    let count = words.next()?.parse().ok()?;
    let unit = words.next()?;
    (words.next().is_none() && matches!(unit, "file" | "files")).then_some(count)
}

/// One Dart payload to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// `base.apk: lib/arm64-v8a/libapp.so`, or the on-disk path.
    pub name: String,
    pub bytes: u64,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Entry { apk: String, entry: String },
    File(String),
}

/// What the listing says is there to read, and what keeps it from being the whole story.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ScanPlan {
    pub engine: Engine,
    pub targets: Vec<Target>,
    pub gaps: Vec<String>,
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `lib/<abi>/<file>` entries.
fn lib_entry(name: &str, wanted: &str) -> bool {
    let mut parts = name.split('/');
    matches!((parts.next(), parts.next(), parts.next(), parts.next()), (Some("lib"), Some(abi), Some(f), None) if !abi.is_empty() && f == wanted)
}

pub(crate) fn plan_scan(listing: &Listing) -> ScanPlan {
    let mut plan = ScanPlan { engine: Engine::NotFound, targets: Vec::new(), gaps: Vec::new() };
    for apk in &listing.apks {
        let apk_name = file_name(&apk.path);
        if let Some(why) = &apk.problem {
            plan.gaps.push(format!("{apk_name}: {why}"));
            continue;
        }
        for (name, len) in &apk.entries {
            if lib_entry(name, "libflutter.so") {
                plan.engine = Engine::Found;
            } else if lib_entry(name, "libapp.so") || name == "assets/flutter_assets/kernel_blob.bin" {
                if *len == 0 {
                    plan.gaps.push(format!("{apk_name}: {name} is empty"));
                } else {
                    plan.targets.push(Target {
                        name: format!("{apk_name}: {name}"),
                        bytes: *len,
                        source: Source::Entry { apk: apk.path.clone(), entry: name.clone() },
                    });
                }
            } else if name.starts_with("lib/") && name.contains("/app.so-") && name.ends_with(".part.so") {
                plan.gaps.push(format!("{apk_name}: {name} is a deferred component of the Dart code"));
            }
        }
    }
    for lib in &listing.libs {
        let name = file_name(&lib.path);
        if name == "libflutter.so" {
            plan.engine = Engine::Found;
        } else if name == "libapp.so" {
            if lib.bytes == 0 {
                plan.gaps.push(format!("{} is empty", lib.path));
            } else {
                plan.targets.push(Target {
                    name: lib.path.clone(),
                    bytes: lib.bytes,
                    source: Source::File(lib.path.clone()),
                });
            }
        }
    }
    let total: u64 = plan.targets.iter().map(|t| t.bytes).sum();
    if total > MAX_SCAN_BYTES {
        plan.gaps.push(format!("{} of Dart payload is too much to scan on the device", super::human_bytes(total)));
        plan.targets.clear();
    }
    plan
}

/// Script 2: reads every target, printing its byte count and the markers in it.
pub(crate) fn scan_script(targets: &[Target]) -> String {
    let patterns: String = MARKERS.iter().map(|m| format!(" -e {}", sh_quote(m.text))).collect();
    let mut script = String::new();
    for (i, t) in targets.iter().enumerate() {
        script.push_str(&format!("echo \"{SCAN}{i}\"\n"));
        match &t.source {
            Source::Entry { apk, entry } => {
                let (apk, entry) = (sh_quote(apk), sh_quote(entry));
                script.push_str(&format!(
                    "n=$(unzip -p {apk} {entry} 2>/dev/null | wc -c); echo \"{BYTES}$n\"\n\
                     unzip -p {apk} {entry} 2>/dev/null | grep -a -o -F{patterns}; echo \"{GREP_RC}$?\"\n"
                ));
            }
            Source::File(path) => {
                let path = sh_quote(path);
                script.push_str(&format!(
                    "n=$(wc -c < {path}); echo \"{BYTES}$n\"\n\
                     grep -a -o -F{patterns} {path}; echo \"{GREP_RC}$?\"\n"
                ));
            }
        }
    }
    script.push_str(&format!("echo {END}\n"));
    script
}

/// What the scan script printed for one target.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ScanOutcome {
    pub(crate) bytes: Option<u64>,
    pub(crate) grep_rc: Option<i32>,
    pub(crate) found: BTreeSet<&'static str>,
}

pub(crate) fn parse_scan(stdout: &str, count: usize) -> Result<Vec<ScanOutcome>, String> {
    let mut outcomes: Vec<ScanOutcome> = Vec::new();
    let mut ended = false;
    for line in stdout.lines().map(str::trim_end) {
        if line.starts_with(SCAN) {
            outcomes.push(ScanOutcome::default());
        } else if line == END {
            ended = true;
        } else if let Some(current) = outcomes.last_mut() {
            if let Some(n) = line.strip_prefix(BYTES) {
                current.bytes = n.trim().parse().ok();
            } else if let Some(rc) = line.strip_prefix(GREP_RC) {
                current.grep_rc = rc.trim().parse().ok();
            } else if let Some(marker) = markers::by_text(line) {
                current.found.insert(marker.text);
            }
        }
    }
    if !ended || outcomes.len() != count {
        return Err("the scan script did not run to its end".into());
    }
    Ok(outcomes)
}

/// Joins the plan and the scan output into the inspection to classify.
pub(crate) fn inspection_from_scan(
    plan: ScanPlan,
    scope: String,
    scan: Result<Vec<ScanOutcome>, String>,
) -> Inspection {
    let mut inspection = Inspection { scope, engine: plan.engine, payloads: Vec::new(), gaps: plan.gaps };
    let outcomes = match scan {
        Ok(o) => o,
        Err(why) => {
            inspection.gaps.push(why);
            return inspection;
        }
    };
    for (target, outcome) in plan.targets.iter().zip(outcomes) {
        match (outcome.bytes, outcome.grep_rc) {
            (None, _) | (_, None) => inspection.gaps.push(format!("{}: no scan result", target.name)),
            (Some(bytes), Some(0 | 1)) if bytes == target.bytes => {
                // `grep` exits 0 exactly when it printed a match.
                if outcome.grep_rc == Some(0) && outcome.found.is_empty() {
                    inspection.gaps.push(format!("{}: grep matched something that is not a marker", target.name));
                } else {
                    inspection.payloads.push(Payload { name: target.name.clone(), bytes, found: outcome.found });
                }
            }
            (Some(bytes), Some(0 | 1)) => {
                inspection.gaps.push(format!("{}: read {bytes} of {} bytes", target.name, target.bytes))
            }
            (Some(_), Some(rc)) => inspection.gaps.push(format!("{}: `grep` failed (exit {rc})", target.name)),
        }
    }
    inspection
}

/// Judges the installed app of `package` on `serial`, answering from `cache`
/// when this exact install was judged before. Never fails: every problem is an
/// unknown verdict in the returned report.
pub(crate) fn probe(
    client: &AdbClient,
    serial: &str,
    package: &str,
    deadline: Instant,
    cancel: &AtomicBool,
    cache: Option<&DriverCache>,
) -> Report {
    if validate_package_name(package).is_err() {
        return Report::unknown(format!("`{package}` is not a valid Android package id"));
    }
    let run = |script: &str| -> Result<ShellOutput, String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the probe deadline passed".into());
        }
        let bounded = client.clone().with_timeouts(left.min(Duration::from_secs(2)), left);
        bounded.shell(serial, script).map_err(|e| format!("adb: {e:#}"))
    };
    let listing = match run(&listing_script(package)).and_then(|out| parse_listing(&out)) {
        Ok(l) => l,
        Err(why) => return Report::unknown(why),
    };
    let fingerprint = listing.fingerprint();
    resolve(serial, package, fingerprint, cache, || {
        let scope = match listing.apks.as_slice() {
            [one] => file_name(&one.path).to_string(),
            many => format!("{} APKs", many.len()),
        };
        let plan = plan_scan(&listing);
        let scan = if plan.targets.is_empty() {
            Ok(Vec::new())
        } else {
            run(&scan_script(&plan.targets)).and_then(|out| parse_scan(&out.stdout, plan.targets.len()))
        };
        super::classify(&inspection_from_scan(plan, scope, scan))
    })
}

#[cfg(test)]
mod tests;
