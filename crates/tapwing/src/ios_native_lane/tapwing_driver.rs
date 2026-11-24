//! Where Tapwing's own iOS Simulator XCUITest driver (`dev.tapwing.ios-runner`,
//! `drivers/ios`) comes from.
//!
//! Lookup order ([`select_source`]): `$TAPWING_IOS_DRIVER_DIR` (unpacked
//! directory), the archives bundled with a release install
//! (`<prefix>/share/tapwing/drivers/ios/`, checked against their `.sha256`
//! sidecars and unpacked once into `~/.tapwing/ios-native-driver/tapwing-<hash>/`),
//! then the output of `scripts/build-ios-driver.sh` in the source checkout this
//! binary was built from.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};

use super::{
    PRODUCTS_DIR, RunnerLayout, cache_root, extract_driver_with, is_complete_layout, sha256_hex,
    validate_driver_dir_layout,
};
use crate::driver_bundle::{self, DriverPlatform};

/// Bundle id of Tapwing's iOS Simulator UITest runner app.
pub const RUNNER_BUNDLE_ID: &str = "dev.tapwing.ios-runnerUITests.xctrunner";
/// Overrides the driver location: an unpacked directory holding
/// `tapwing-ios-runner.xctestrun` and `Debug-iphonesimulator/TapwingRunnerUITests-Runner.app`.
pub const DRIVER_DIR_ENV: &str = "TAPWING_IOS_DRIVER_DIR";

pub const XCTESTRUN: &str = "tapwing-ios-runner.xctestrun";
pub const RUNNER_ZIP: &str = "tapwing-ios-runnerUITests-Runner.zip";
pub const APP_ZIP: &str = "tapwing-ios-runner.zip";
pub const RUNNER_APP: &str = "TapwingRunnerUITests-Runner.app";

pub(crate) const LAYOUT: RunnerLayout =
    RunnerLayout { xctestrun: XCTESTRUN, runner_zip: RUNNER_ZIP, app_zip: APP_ZIP, runner_app: RUNNER_APP };

/// `scripts/build-ios-driver.sh` output directory, relative to this crate.
const CHECKOUT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../drivers/ios/build");

const BUNDLED_FILES: [&str; 3] = [XCTESTRUN, RUNNER_ZIP, APP_ZIP];

/// Where Tapwing's iOS driver was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TapwingIosSource {
    /// Explicit `$TAPWING_IOS_DRIVER_DIR` (unpacked directory).
    Env(PathBuf),
    /// `<prefix>/share/tapwing/drivers/ios/` of a release install (zips + xctestrun).
    Bundled(PathBuf),
    /// Output of `scripts/build-ios-driver.sh` in the source checkout.
    Checkout(PathBuf),
}

impl TapwingIosSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Env(_) => "env",
            Self::Bundled(_) => "bundled",
            Self::Checkout(_) => "checkout",
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Env(p) | Self::Bundled(p) | Self::Checkout(p) => p,
        }
    }
}

impl fmt::Display for TapwingIosSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.label(), self.path().display())
    }
}

/// The resolved, runnable Tapwing iOS driver.
#[derive(Clone, Debug)]
pub struct TapwingIosFiles {
    pub xctestrun: PathBuf,
    pub source: TapwingIosSource,
}

fn has_bundled_archives(dir: &Path) -> bool {
    BUNDLED_FILES.iter().all(|name| dir.join(name).is_file())
}

fn has_unpacked_checkout(dir: &Path) -> bool {
    validate_driver_dir_layout(dir, LAYOUT).is_ok()
}

/// Applies the lookup order: `$TAPWING_IOS_DRIVER_DIR` always wins (validated
/// by [`resolve`]), then a release bundle containing the Tapwing iOS archives,
/// then a local checkout build in `drivers/ios/build/`.
pub fn select_source(
    env: Option<PathBuf>,
    bundled: Option<PathBuf>,
    checkout: Option<PathBuf>,
) -> Option<TapwingIosSource> {
    if let Some(dir) = env {
        return Some(TapwingIosSource::Env(dir));
    }
    if let Some(dir) = bundled.filter(|d| has_bundled_archives(d)) {
        return Some(TapwingIosSource::Bundled(dir));
    }
    checkout.filter(|d| has_unpacked_checkout(d)).map(TapwingIosSource::Checkout)
}

fn current_source() -> Option<TapwingIosSource> {
    let root = driver_bundle::bundle_root();
    select_source(
        std::env::var_os(DRIVER_DIR_ENV).map(PathBuf::from),
        driver_bundle::bundled_dir(root.as_deref(), DriverPlatform::Ios),
        Some(PathBuf::from(CHECKOUT_DIR)),
    )
}

/// `<file>.sha256` in `shasum -a 256` format, written next to each archive by
/// `scripts/build-ios-driver.sh` and shipped in release bundles.
fn sidecar_sha256(path: &Path) -> Option<String> {
    let mut name = path.as_os_str().to_owned();
    name.push(".sha256");
    let text = fs::read_to_string(PathBuf::from(name)).ok()?;
    let sum = text.split_whitespace().next()?.to_ascii_lowercase();
    (sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sum)
}

/// Reads and verifies the three bundled Tapwing iOS driver files against their
/// `.sha256` sidecars (if present).
fn read_bundled_files(dir: &Path) -> Result<Vec<(&'static str, Vec<u8>)>> {
    BUNDLED_FILES
        .iter()
        .map(|&name| {
            let path = dir.join(name);
            let bytes =
                fs::read(&path).with_context(|| format!("reading bundled Tapwing iOS driver {}", path.display()))?;
            let actual = sha256_hex(&[&bytes]);
            if let Some(expected) = sidecar_sha256(&path)
                && expected != actual
            {
                bail!(
                    "the bundled Tapwing iOS driver {} is corrupt (SHA-256 {actual}, expected {expected}); \
                     reinstall Tapwing or set ${DRIVER_DIR_ENV}",
                    path.display()
                );
            }
            Ok((name, bytes))
        })
        .collect()
}

/// Resolves `source` into a runnable unpacked `.xctestrun` path.
pub fn resolve(
    source: Option<TapwingIosSource>,
    cache: impl FnOnce() -> Result<PathBuf>,
    cancel: &AtomicBool,
) -> Result<TapwingIosFiles> {
    let Some(source) = source else {
        bail!(
            "Tapwing's iOS Simulator driver ({XCTESTRUN}) not found: build it with scripts/build-ios-driver.sh, \
             install a macOS Tapwing release (it bundles it), set ${DRIVER_DIR_ENV} to a directory with \
             {XCTESTRUN} and {PRODUCTS_DIR}/{RUNNER_APP}"
        );
    };
    match &source {
        TapwingIosSource::Env(dir) => {
            let xctestrun = validate_driver_dir_layout(dir, LAYOUT).with_context(|| format!("${DRIVER_DIR_ENV}"))?;
            Ok(TapwingIosFiles { xctestrun, source })
        }
        TapwingIosSource::Checkout(dir) => {
            let xctestrun = validate_driver_dir_layout(dir, LAYOUT)?;
            Ok(TapwingIosFiles { xctestrun, source })
        }
        TapwingIosSource::Bundled(dir) => {
            let files = read_bundled_files(dir)?;
            let slices: Vec<&[u8]> = files.iter().map(|(_, b)| b.as_slice()).collect();
            let dest = cache()?.join(format!("tapwing-{}", &sha256_hex(&slices)[..16]));
            if !is_complete_layout(&dest, LAYOUT) {
                extract_driver_with(&dest, LAYOUT, cancel, |name| {
                    files
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, b)| b.clone())
                        .with_context(|| format!("{name} is missing from bundled Tapwing iOS driver"))
                })?;
            }
            let xctestrun = validate_driver_dir_layout(&dest, LAYOUT)?;
            Ok(TapwingIosFiles { xctestrun, source })
        }
    }
}

/// Finds (and, if bundled, unpacks once) Tapwing's iOS Simulator driver.
pub fn locate(cancel: &AtomicBool) -> Result<TapwingIosFiles> {
    let files = resolve(current_source(), cache_root, cancel)?;
    if std::env::var_os(driver_bundle::DEBUG_ENV).is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("debug: Tapwing iOS driver: {}", files.source);
    }
    Ok(files)
}

/// `tapwing --version` line.
pub fn version_line() -> String {
    match current_source() {
        Some(source) => format!(
            "tapwing ios driver: {} ({}) {}",
            env!("CARGO_PKG_VERSION"),
            source.label(),
            source.path().display()
        ),
        None => "tapwing ios driver: missing (scripts/build-ios-driver.sh, or set $TAPWING_IOS_DRIVER_DIR)".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::sync::atomic::AtomicUsize;

    use super::*;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tapwing-ios-drv-{}-{tag}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn zip_bytes(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body, mode) in files {
            let opts = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(body).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn write_unpacked(dir: &Path) {
        fs::create_dir_all(dir.join(PRODUCTS_DIR).join(RUNNER_APP)).unwrap();
        fs::write(dir.join(XCTESTRUN), "<plist/>").unwrap();
    }

    fn write_bundle(dir: &Path, with_sidecars: bool) {
        fs::create_dir_all(dir).unwrap();
        let runner = zip_bytes(&[
            ("TapwingRunnerUITests-Runner.app/", b"", 0o755),
            (
                "TapwingRunnerUITests-Runner.app/PlugIns/TapwingRunnerUITests.xctest/TapwingRunnerUITests",
                b"macho",
                0o755,
            ),
        ]);
        let app = zip_bytes(&[("TapwingRunner.app/", b"", 0o755), ("TapwingRunner.app/TapwingRunner", b"app", 0o755)]);
        for (name, bytes) in [(XCTESTRUN, b"<plist/>".to_vec()), (RUNNER_ZIP, runner), (APP_ZIP, app)] {
            fs::write(dir.join(name), &bytes).unwrap();
            if with_sidecars {
                let sum = sha256_hex(&[&bytes]);
                fs::write(dir.join(format!("{name}.sha256")), format!("{sum}  {name}\n")).unwrap();
            }
        }
    }

    #[test]
    fn lookup_order_is_env_then_bundle_then_checkout() {
        let s = Scratch::new("order");
        let bundled = s.0.join("bundled");
        let checkout = s.0.join("checkout");
        let env = s.0.join("missing-env");

        assert_eq!(select_source(None, Some(bundled.clone()), Some(checkout.clone())), None);

        write_unpacked(&checkout);
        assert_eq!(
            select_source(None, Some(bundled.clone()), Some(checkout.clone())),
            Some(TapwingIosSource::Checkout(checkout.clone()))
        );

        write_bundle(&bundled, true);
        assert_eq!(
            select_source(None, Some(bundled.clone()), Some(checkout.clone())),
            Some(TapwingIosSource::Bundled(bundled.clone()))
        );

        assert_eq!(
            select_source(Some(env.clone()), Some(bundled), Some(checkout)),
            Some(TapwingIosSource::Env(env.clone()))
        );
        let err = format!(
            "{:#}",
            resolve(Some(TapwingIosSource::Env(env)), || Ok(s.0.join("c")), &AtomicBool::new(false)).unwrap_err()
        );
        assert!(err.contains(DRIVER_DIR_ENV) && err.contains(XCTESTRUN), "{err}");
    }

    #[test]
    fn bundled_archives_are_verified_unpacked_once_and_reject_tampering() {
        let s = Scratch::new("bundle");
        let bundle = s.0.join("share/tapwing/drivers/ios");
        let cache = s.0.join("cache");
        write_bundle(&bundle, true);

        let files =
            resolve(Some(TapwingIosSource::Bundled(bundle.clone())), || Ok(cache.clone()), &AtomicBool::new(false))
                .unwrap();
        let unpacked_dir = files.xctestrun.parent().unwrap().to_path_buf();
        assert!(unpacked_dir.starts_with(&cache));
        assert!(unpacked_dir.file_name().unwrap().to_str().unwrap().starts_with("tapwing-"));
        assert!(is_complete_layout(&unpacked_dir, LAYOUT));

        // Warm run reuses the cache.
        fs::write(unpacked_dir.join("sentinel"), "ok").unwrap();
        let again =
            resolve(Some(TapwingIosSource::Bundled(bundle.clone())), || Ok(cache.clone()), &AtomicBool::new(false))
                .unwrap();
        assert_eq!(again.xctestrun, files.xctestrun);
        assert!(unpacked_dir.join("sentinel").exists());

        // Tampering with a bundled archive fails loudly against its sidecar.
        fs::write(bundle.join(APP_ZIP), b"tampered").unwrap();
        let err = format!(
            "{:#}",
            resolve(Some(TapwingIosSource::Bundled(bundle)), || Ok(cache), &AtomicBool::new(false)).unwrap_err()
        );
        assert!(err.contains("corrupt") && err.contains(APP_ZIP) && err.contains(DRIVER_DIR_ENV), "{err}");
    }

    #[test]
    fn an_incomplete_bundle_is_not_selected() {
        let s = Scratch::new("incomplete");
        let bundled = s.0.join("bundled");
        write_bundle(&bundled, true);
        for missing in [XCTESTRUN, RUNNER_ZIP, APP_ZIP] {
            let copy = s.0.join(format!("without-{missing}"));
            fs::create_dir_all(&copy).unwrap();
            for name in [XCTESTRUN, RUNNER_ZIP, APP_ZIP] {
                if name != missing {
                    fs::copy(bundled.join(name), copy.join(name)).unwrap();
                }
            }
            assert_eq!(select_source(None, Some(copy), None), None, "bundle without {missing}");
        }
        // An unpacked checkout without the runner app is not usable either.
        let checkout = s.0.join("half-built");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join(XCTESTRUN), "<plist/>").unwrap();
        assert_eq!(select_source(None, None, Some(checkout)), None);
    }

    #[test]
    fn every_bundled_archive_is_checked_against_its_own_sidecar() {
        let s = Scratch::new("each_sidecar");
        for tampered in [XCTESTRUN, RUNNER_ZIP, APP_ZIP] {
            let bundle = s.0.join(format!("bundle-{tampered}"));
            write_bundle(&bundle, true);
            fs::write(bundle.join(tampered), b"tampered").unwrap();
            let err = format!(
                "{:#}",
                resolve(Some(TapwingIosSource::Bundled(bundle)), || Ok(s.0.join("cache")), &AtomicBool::new(false))
                    .unwrap_err()
            );
            assert!(err.contains("corrupt") && err.contains(tampered), "{tampered}: {err}");
        }
    }

    #[test]
    fn a_bundle_without_sidecars_is_still_unpacked_and_the_hash_names_the_cache_dir() {
        let s = Scratch::new("no_sidecars");
        let bundle = s.0.join("bundle");
        let cache = s.0.join("cache");
        write_bundle(&bundle, false);
        let a = resolve(Some(TapwingIosSource::Bundled(bundle.clone())), || Ok(cache.clone()), &AtomicBool::new(false))
            .unwrap();
        // Different archive contents unpack into a different cache directory.
        fs::write(bundle.join(XCTESTRUN), b"<plist>other</plist>").unwrap();
        let b = resolve(Some(TapwingIosSource::Bundled(bundle)), || Ok(cache), &AtomicBool::new(false)).unwrap();
        assert_ne!(a.xctestrun.parent(), b.xctestrun.parent());
        assert!(a.xctestrun.is_file() && b.xctestrun.is_file());
    }

    #[test]
    fn a_missing_bundled_archive_names_the_file() {
        let s = Scratch::new("missing_archive");
        let bundle = s.0.join("bundle");
        write_bundle(&bundle, true);
        fs::remove_file(bundle.join(RUNNER_ZIP)).unwrap();
        let err = format!(
            "{:#}",
            resolve(Some(TapwingIosSource::Bundled(bundle)), || Ok(s.0.join("cache")), &AtomicBool::new(false))
                .unwrap_err()
        );
        assert!(err.contains("reading bundled Tapwing iOS driver") && err.contains(RUNNER_ZIP), "{err}");
    }

    #[test]
    fn a_corrupt_zip_with_a_matching_sidecar_fails_to_unpack_with_an_error() {
        let s = Scratch::new("bad_zip");
        let bundle = s.0.join("bundle");
        write_bundle(&bundle, false);
        // Not a zip, but the sidecar agrees with the bytes: the hash check passes, unpacking must not.
        let junk = b"this is not a zip archive";
        fs::write(bundle.join(APP_ZIP), junk).unwrap();
        fs::write(bundle.join(format!("{APP_ZIP}.sha256")), format!("{}  {APP_ZIP}\n", sha256_hex(&[junk]))).unwrap();
        let res = resolve(Some(TapwingIosSource::Bundled(bundle)), || Ok(s.0.join("cache")), &AtomicBool::new(false));
        assert!(res.is_err(), "unpacking a non-zip archive must fail, not produce a half-written driver");
    }

    #[test]
    fn an_env_dir_without_the_runner_layout_is_rejected_with_the_env_name() {
        let s = Scratch::new("env_layout");
        let dir = s.0.join("empty");
        fs::create_dir_all(&dir).unwrap();
        let err = format!(
            "{:#}",
            resolve(Some(TapwingIosSource::Env(dir)), || Ok(s.0.join("c")), &AtomicBool::new(false)).unwrap_err()
        );
        assert!(err.contains(DRIVER_DIR_ENV), "{err}");
    }

    #[test]
    fn sidecars_with_malformed_digests_are_ignored() {
        let s = Scratch::new("sidecar_parse");
        let file = s.0.join(XCTESTRUN);
        let sidecar = s.0.join(format!("{XCTESTRUN}.sha256"));
        assert_eq!(sidecar_sha256(&file), None);
        let sum = sha256_hex(&[b"x"]);
        fs::write(&sidecar, format!("{}  {XCTESTRUN}\n", sum.to_ascii_uppercase())).unwrap();
        assert_eq!(sidecar_sha256(&file), Some(sum.clone()));
        for bad in ["", &sum[..10], &format!("{sum}ff"), &"z".repeat(64)] {
            fs::write(&sidecar, bad).unwrap();
            assert_eq!(sidecar_sha256(&file), None, "{bad:?}");
        }
    }

    #[test]
    fn sources_render_their_label_and_path() {
        let p = PathBuf::from("/x/ios");
        assert_eq!(TapwingIosSource::Env(p.clone()).to_string(), "env /x/ios");
        assert_eq!(TapwingIosSource::Bundled(p.clone()).to_string(), "bundled /x/ios");
        assert_eq!(TapwingIosSource::Checkout(p).to_string(), "checkout /x/ios");
    }

    #[test]
    fn missing_driver_explains_how_to_get_one() {
        let err = resolve(None, || Ok(PathBuf::from("/tmp")), &AtomicBool::new(false)).unwrap_err().to_string();
        assert!(
            err.contains("build-ios-driver.sh") && err.contains(DRIVER_DIR_ENV) && !err.contains("maestro"),
            "{err}"
        );
    }
}
