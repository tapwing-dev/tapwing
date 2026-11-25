//! Where Tapwing's own Android driver APK (`dev.tapwing.driver`,
//! `drivers/android`) comes from.
//!
//! Lookup order ([`select_source`]): `$TAPWING_DRIVER_APK`, the APK bundled
//! with a release install (`<prefix>/share/tapwing/drivers/android/`, checked
//! against its `.sha256` sidecar), then the output of
//! `scripts/build-android-driver.sh` in the source checkout this binary was
//! built from. Installing it reuses the hash-cached installer of
//! [`super::apk`], with its own device marker.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};

use super::apk::InstallPlan;
use crate::driver_bundle::{self, DriverPlatform, FileStamp, sidecar_path, sidecar_sha256, stream_file_sha256};

/// Package of the driver APK; its instrumentation targets itself.
pub const PACKAGE: &str = "dev.tapwing.driver";
/// `am instrument` component.
pub const INSTRUMENTATION: &str = "dev.tapwing.driver/.TapwingDriverInstrumentation";
/// File name in a release bundle and in the build output.
pub const APK_NAME: &str = "tapwing-driver.apk";
/// Marker on the device recording which driver APK is installed.
pub const DEVICE_MARKER: &str = "/data/local/tmp/tapwing_driver_apk.sha256";
/// Overrides the APK (a path to the `.apk` file).
pub const APK_ENV: &str = "TAPWING_DRIVER_APK";
/// `scripts/build-android-driver.sh` output, relative to this crate.
const CHECKOUT_APK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../drivers/android/build/tapwing-driver.apk");

/// Where the APK was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApkSource {
    Env(PathBuf),
    Bundled(PathBuf),
    /// Built in the source checkout of this binary.
    Checkout(PathBuf),
}

impl ApkSource {
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

impl fmt::Display for ApkSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.label(), self.path().display())
    }
}

/// The located APK and the SHA-256 of its contents.
#[derive(Clone, Debug)]
pub struct TapwingApk {
    pub source: ApkSource,
    pub sha256: String,
}

impl TapwingApk {
    pub fn install_plan(&self) -> InstallPlan {
        InstallPlan {
            packages: vec![(PACKAGE, self.source.path().to_path_buf())],
            marker: DEVICE_MARKER,
            sha256: self.sha256.clone(),
        }
    }
}

/// The env override always wins (even when the file is missing, which is then
/// an error); a bundled or checkout APK only counts when the file exists.
pub fn select_source(env: Option<PathBuf>, bundled: Option<PathBuf>, checkout: Option<PathBuf>) -> Option<ApkSource> {
    if let Some(p) = env {
        return Some(ApkSource::Env(p));
    }
    if let Some(p) = bundled.filter(|p| p.is_file()) {
        return Some(ApkSource::Bundled(p));
    }
    checkout.filter(|p| p.is_file()).map(ApkSource::Checkout)
}

fn current_source() -> Option<ApkSource> {
    let root = driver_bundle::bundle_root();
    select_source(
        std::env::var_os(APK_ENV).map(PathBuf::from),
        driver_bundle::bundled_dir(root.as_deref(), DriverPlatform::Android).map(|d| d.join(APK_NAME)),
        Some(PathBuf::from(CHECKOUT_APK)),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ApkSourceKind {
    Env,
    Bundled,
    Checkout,
}

impl ApkSource {
    fn kind(&self) -> ApkSourceKind {
        match self {
            Self::Env(_) => ApkSourceKind::Env,
            Self::Bundled(_) => ApkSourceKind::Bundled,
            Self::Checkout(_) => ApkSourceKind::Checkout,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ApkCacheKey {
    kind: ApkSourceKind,
    canonical_path: PathBuf,
    apk_stamp: FileStamp,
    sidecar_stamp: Option<FileStamp>,
}

static APK_HASH_CACHE: OnceLock<Mutex<HashMap<ApkCacheKey, String>>> = OnceLock::new();

fn apk_cache() -> &'static Mutex<HashMap<ApkCacheKey, String>> {
    APK_HASH_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(source: &ApkSource) -> Option<ApkCacheKey> {
    let canonical_path = fs::canonicalize(source.path()).ok()?;
    let apk_stamp = FileStamp::from_path(&canonical_path)?;
    let sidecar_stamp = match source {
        ApkSource::Bundled(_) => FileStamp::from_path(&sidecar_path(&canonical_path)),
        ApkSource::Env(_) | ApkSource::Checkout(_) => None,
    };
    Some(ApkCacheKey { kind: source.kind(), canonical_path, apk_stamp, sidecar_stamp })
}

/// Hashes the APK in 64 KiB chunks; a bundled APK must match its sidecar (a
/// broken release install fails loudly instead of installing something
/// unexpected).
pub fn resolve(source: Option<ApkSource>) -> Result<TapwingApk> {
    let Some(source) = source else {
        bail!(
            "Tapwing's Android driver APK ({APK_NAME}) not found: build it with scripts/build-android-driver.sh, \
             install a Tapwing release (it bundles it), or set ${APK_ENV}"
        );
    };
    let path = source.path();
    let sha256 =
        stream_file_sha256(path, None).with_context(|| format!("reading the Tapwing driver APK {}", path.display()))?;
    if let ApkSource::Bundled(_) = source
        && let Some(expected) = sidecar_sha256(path)
        && expected != sha256
    {
        bail!(
            "the bundled Tapwing driver {} is corrupt (SHA-256 {sha256}, expected {expected}); \
             reinstall Tapwing or set ${APK_ENV}",
            path.display()
        );
    }
    Ok(TapwingApk { source, sha256 })
}

fn locate_with_cache(
    source: Option<ApkSource>,
    cache: &Mutex<HashMap<ApkCacheKey, String>>,
    resolve_fn: impl FnOnce(Option<ApkSource>) -> Result<TapwingApk>,
) -> Result<TapwingApk> {
    let key = source.as_ref().and_then(cache_key);
    if let (Some(src), Some(k)) = (&source, &key) {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sha256) = guard.get(k) {
            return Ok(TapwingApk { source: src.clone(), sha256: sha256.clone() });
        }
    }
    let apk = resolve_fn(source)?;
    if let Some(k) = key {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(k, apk.sha256.clone());
    }
    Ok(apk)
}

/// Finds and hashes the Tapwing driver APK, memoizing verified digests by
/// canonical path and file/sidecar metadata stamps across shards and restarts.
pub fn locate() -> Result<TapwingApk> {
    let apk = locate_with_cache(current_source(), apk_cache(), resolve)?;
    if std::env::var_os(driver_bundle::DEBUG_ENV).is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("debug: Tapwing Android driver: {} (sha256 {})", apk.source, apk.sha256);
    }
    Ok(apk)
}

/// `tapwing --version` line.
pub fn version_line() -> String {
    match current_source() {
        Some(source) => format!(
            "tapwing android driver: {} ({}) {}",
            env!("CARGO_PKG_VERSION"),
            source.label(),
            source.path().display()
        ),
        None => "tapwing android driver: missing (scripts/build-android-driver.sh, or set $TAPWING_DRIVER_APK)".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::apk::sha256_hex;
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tapwing-apk-src-{}-{tag}", std::process::id()));
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

    #[test]
    fn lookup_order_is_env_then_bundle_then_checkout() {
        let s = Scratch::new("order");
        let (bundled, checkout) = (s.0.join("bundled.apk"), s.0.join("checkout.apk"));
        let env = s.0.join("missing-env.apk");
        assert_eq!(select_source(None, Some(bundled.clone()), Some(checkout.clone())), None, "no file exists");
        fs::write(&checkout, "c").unwrap();
        assert_eq!(
            select_source(None, Some(bundled.clone()), Some(checkout.clone())),
            Some(ApkSource::Checkout(checkout.clone()))
        );
        fs::write(&bundled, "b").unwrap();
        assert_eq!(
            select_source(None, Some(bundled.clone()), Some(checkout.clone())),
            Some(ApkSource::Bundled(bundled.clone()))
        );
        // An explicit override wins even when it points nowhere (reported by `resolve`).
        assert_eq!(select_source(Some(env.clone()), Some(bundled), Some(checkout)), Some(ApkSource::Env(env.clone())));
        let err = format!("{:#}", resolve(Some(ApkSource::Env(env))).unwrap_err());
        assert!(err.contains("missing-env.apk"), "{err}");
    }

    #[test]
    fn bundled_apk_must_match_its_sidecar() {
        let s = Scratch::new("sidecar");
        let apk = s.0.join(APK_NAME);
        fs::write(&apk, b"driver").unwrap();
        let sum = sha256_hex(&[b"driver"]);
        let ok = resolve(Some(ApkSource::Bundled(apk.clone()))).unwrap();
        assert_eq!(ok.sha256, sum, "no sidecar: hash only");

        fs::write(s.0.join("tapwing-driver.apk.sha256"), format!("{sum}  {APK_NAME}\n")).unwrap();
        assert_eq!(resolve(Some(ApkSource::Bundled(apk.clone()))).unwrap().sha256, sum);

        fs::write(&apk, b"tampered").unwrap();
        let err = format!("{:#}", resolve(Some(ApkSource::Bundled(apk.clone()))).unwrap_err());
        assert!(err.contains("corrupt") && err.contains(&sum), "{err}");
        // A checkout build is the developer's own: the sidecar is not enforced.
        assert!(resolve(Some(ApkSource::Checkout(apk))).is_ok());
    }

    #[test]
    fn missing_apk_explains_how_to_get_one() {
        let err = resolve(None).unwrap_err().to_string();
        assert!(err.contains("build-android-driver.sh") && err.contains(APK_ENV) && !err.contains("maestro"));
    }

    #[test]
    fn sidecar_parsing_accepts_shasum_format_and_rejects_malformed_sums() {
        let s = Scratch::new("sidecar_parse");
        let apk = s.0.join(APK_NAME);
        let sidecar = s.0.join(format!("{APK_NAME}.sha256"));
        let sum = sha256_hex(&[b"x"]);
        // No sidecar at all.
        assert_eq!(sidecar_sha256(&apk), None);
        // `shasum -a 256` output, upper-case digits are normalised, trailing newline is fine.
        fs::write(&sidecar, format!("{}  {APK_NAME}\n", sum.to_ascii_uppercase())).unwrap();
        assert_eq!(sidecar_sha256(&apk), Some(sum.clone()));
        // The bare digest without a file name works too.
        fs::write(&sidecar, &sum).unwrap();
        assert_eq!(sidecar_sha256(&apk), Some(sum.clone()));
        // Too short, too long, non-hex and empty sidecars are ignored, never trusted.
        for bad in ["", "   \n", &sum[..63], &format!("{sum}0"), &format!("{}g", &sum[..63])] {
            fs::write(&sidecar, bad).unwrap();
            assert_eq!(sidecar_sha256(&apk), None, "{bad:?}");
        }
    }

    #[test]
    fn a_corrupt_bundled_apk_is_rejected_even_when_the_sidecar_is_uppercase() {
        let s = Scratch::new("sidecar_upper");
        let apk = s.0.join(APK_NAME);
        fs::write(&apk, b"driver").unwrap();
        let sum = sha256_hex(&[b"driver"]);
        fs::write(s.0.join(format!("{APK_NAME}.sha256")), sum.to_ascii_uppercase()).unwrap();
        assert_eq!(resolve(Some(ApkSource::Bundled(apk.clone()))).unwrap().sha256, sum);

        // A truncated download (empty file) is a mismatch, not a silent install.
        fs::write(&apk, b"").unwrap();
        let err = format!("{:#}", resolve(Some(ApkSource::Bundled(apk))).unwrap_err());
        assert!(err.contains("corrupt") && err.contains("reinstall Tapwing"), "{err}");
    }

    #[test]
    fn the_env_override_is_hashed_but_never_checked_against_a_sidecar() {
        let s = Scratch::new("env_override");
        let apk = s.0.join("custom.apk");
        fs::write(&apk, b"my build").unwrap();
        // A stale sidecar next to a developer's own APK must not block it.
        fs::write(s.0.join("custom.apk.sha256"), "0".repeat(64)).unwrap();
        let resolved = resolve(Some(ApkSource::Env(apk))).unwrap();
        assert_eq!(resolved.sha256, sha256_hex(&[b"my build"]));
        assert_eq!(resolved.source.label(), "env");
    }

    #[test]
    fn an_unreadable_apk_names_the_path_in_the_error() {
        let s = Scratch::new("unreadable");
        // A directory where a file is expected cannot be read as an APK.
        let dir = s.0.join("dir.apk");
        fs::create_dir_all(&dir).unwrap();
        for source in [ApkSource::Bundled(dir.clone()), ApkSource::Checkout(dir.clone()), ApkSource::Env(dir)] {
            let err = format!("{:#}", resolve(Some(source)).unwrap_err());
            assert!(err.contains("reading the Tapwing driver APK") && err.contains("dir.apk"), "{err}");
        }
    }

    #[test]
    fn sources_render_their_label_and_path() {
        let p = PathBuf::from("/x/tapwing-driver.apk");
        assert_eq!(ApkSource::Env(p.clone()).to_string(), "env /x/tapwing-driver.apk");
        assert_eq!(ApkSource::Bundled(p.clone()).to_string(), "bundled /x/tapwing-driver.apk");
        assert_eq!(ApkSource::Checkout(p.clone()).to_string(), "checkout /x/tapwing-driver.apk");
        assert_eq!(ApkSource::Checkout(p.clone()).path(), p.as_path());
    }

    #[test]
    fn a_directory_is_not_a_bundled_or_checkout_apk() {
        let s = Scratch::new("dir_not_file");
        let dir = s.0.join("fake.apk");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(select_source(None, Some(dir.clone()), Some(dir)), None);
    }

    #[test]
    fn install_plan_uses_the_tapwing_marker() {
        let apk = TapwingApk { source: ApkSource::Env(PathBuf::from("/x/tapwing-driver.apk")), sha256: "ab".into() };
        let plan = apk.install_plan();
        assert_eq!(plan.packages, vec![(PACKAGE, PathBuf::from("/x/tapwing-driver.apk"))]);
        assert_eq!(plan.marker, DEVICE_MARKER);
        assert_eq!(plan.sha256, "ab");
    }

    #[test]
    fn locate_memoizes_unchanged_apk_and_invalidates_on_apk_or_sidecar_change() {
        let s = Scratch::new("memoize");
        let apk = s.0.join(APK_NAME);
        let initial_bytes = b"initial-apk-payload";
        fs::write(&apk, initial_bytes).unwrap();
        let initial_sum = sha256_hex(&[initial_bytes]);
        let sidecar = s.0.join(format!("{APK_NAME}.sha256"));
        fs::write(&sidecar, format!("{initial_sum}  {APK_NAME}\n")).unwrap();

        let cache = Mutex::new(HashMap::new());
        let resolves = AtomicUsize::new(0);
        let run = |src: ApkSource| {
            locate_with_cache(Some(src), &cache, |opt| {
                resolves.fetch_add(1, Ordering::Relaxed);
                resolve(opt)
            })
        };

        // First call hashes the APK; second call with unchanged file/sidecar is a cache hit.
        let first = run(ApkSource::Bundled(apk.clone())).unwrap();
        let second = run(ApkSource::Bundled(apk.clone())).unwrap();
        assert_eq!(first.sha256, initial_sum);
        assert_eq!(second.sha256, initial_sum);
        assert_eq!(resolves.load(Ordering::Relaxed), 1, "second locate must hit cache");

        // Replacing the APK bytes invalidates the cache and detects corruption against the sidecar.
        fs::write(&apk, b"tampered-apk-payload").unwrap();
        let err = format!("{:#}", run(ApkSource::Bundled(apk.clone())).unwrap_err());
        assert!(err.contains("corrupt") && err.contains(&initial_sum), "{err}");
        assert_eq!(resolves.load(Ordering::Relaxed), 2);

        // Updating the APK back with matching sidecar succeeds and caches again.
        let updated_bytes = b"updated-apk-payload-v2";
        let updated_sum = sha256_hex(&[updated_bytes]);
        fs::write(&apk, updated_bytes).unwrap();
        fs::write(&sidecar, format!("{updated_sum}  {APK_NAME}\n")).unwrap();
        let third = run(ApkSource::Bundled(apk.clone())).unwrap();
        assert_eq!(third.sha256, updated_sum);
        assert_eq!(resolves.load(Ordering::Relaxed), 3);
        let fourth = run(ApkSource::Bundled(apk.clone())).unwrap();
        assert_eq!(fourth.sha256, updated_sum);
        assert_eq!(resolves.load(Ordering::Relaxed), 3, "unchanged updated APK must hit cache");

        // Tampering with only the `.sha256` sidecar immediately invalidates the cache.
        fs::write(&sidecar, format!("{}  {APK_NAME}\n", "0".repeat(64))).unwrap();
        let err = format!("{:#}", run(ApkSource::Bundled(apk)).unwrap_err());
        assert!(err.contains("corrupt"), "{err}");
        assert_eq!(resolves.load(Ordering::Relaxed), 4);
    }
}
