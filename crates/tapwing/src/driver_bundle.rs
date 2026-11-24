//! Where the native-lane drivers bundled with a release install live.
//!
//! Release tarballs ship Tapwing's own drivers next to the binary:
//! `<prefix>/bin/tapwing` + `<prefix>/share/tapwing/drivers/{android,ios}/`.
//! Each driver is checked against the `.sha256` sidecar it ships with (see
//! [`crate::native_lane::tapwing_apk`] and
//! [`crate::ios_native_lane::tapwing_driver`]), which also own the lookup order
//! (env override, bundle, checkout build).

use std::fs;
use std::path::{Path, PathBuf};

/// The bundle below the install prefix.
pub const BUNDLE_DIR: &str = "share/tapwing/drivers";
/// Any value but empty or `0` logs which driver source each lane uses.
pub const DEBUG_ENV: &str = "TAPWING_DEBUG";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverPlatform {
    Android,
    Ios,
}

impl DriverPlatform {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Android => "android",
            Self::Ios => "ios",
        }
    }
}

/// `<prefix>/share/tapwing/drivers` for an executable at `<prefix>/bin/tapwing`.
/// Symlinks are resolved first, so `/opt/homebrew/bin/tapwing -> ../Cellar/…`
/// or `~/.local/bin/tapwing -> ~/.tapwing/current/bin/tapwing` find the real
/// prefix.
pub fn bundle_root_for_exe(exe: &Path) -> Option<PathBuf> {
    let real = fs::canonicalize(exe).ok()?;
    Some(real.parent()?.parent()?.join(BUNDLE_DIR))
}

/// [`bundle_root_for_exe`] of the running binary.
pub fn bundle_root() -> Option<PathBuf> {
    bundle_root_for_exe(&std::env::current_exe().ok()?)
}

/// The platform directory of the bundle, when the bundle ships that platform.
pub fn bundled_dir(root: Option<&Path>, platform: DriverPlatform) -> Option<PathBuf> {
    root.map(|r| r.join(platform.dir_name())).filter(|d| d.is_dir())
}

/// True when `$TAPWING_DEBUG` asks for driver-source logging.
pub fn debug_enabled() -> bool {
    std::env::var_os(DEBUG_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// `tapwing --version`: the CLI version plus the source of each native-lane driver.
pub fn version_report() -> String {
    let mut out = format!("tapwing {}\n", env!("CARGO_PKG_VERSION"));
    out.push_str(&crate::native_lane::tapwing_apk::version_line());
    out.push('\n');
    if cfg!(target_os = "macos") {
        out.push_str(&crate::ios_native_lane::tapwing_driver::version_line());
    } else {
        out.push_str("tapwing ios driver: n/a (the iOS Simulator needs macOS)");
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tapwing-drivers-test-{}-{tag}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            // Canonical, so comparisons survive macOS's /var -> /private/var.
            Self(fs::canonicalize(&dir).unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bundle_root_follows_symlinks_to_the_real_prefix() {
        let s = Scratch::new("exe");
        let prefix = s.0.join("versions/0.1.0");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::create_dir_all(prefix.join(BUNDLE_DIR).join("android")).unwrap();
        let exe = prefix.join("bin/tapwing");
        fs::write(&exe, "").unwrap();
        assert_eq!(bundle_root_for_exe(&exe).unwrap(), prefix.join(BUNDLE_DIR));

        // Homebrew-style chain: <bin dir>/tapwing -> <prefix>/current/bin/tapwing, current -> versions/0.1.0.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("versions/0.1.0", s.0.join("current")).unwrap();
            fs::create_dir_all(s.0.join("local/bin")).unwrap();
            let link = s.0.join("local/bin/tapwing");
            symlink(s.0.join("current/bin/tapwing"), &link).unwrap();
            let root = bundle_root_for_exe(&link).unwrap();
            assert_eq!(root, prefix.join(BUNDLE_DIR), "not <bin dir>/../share");
            assert_eq!(bundled_dir(Some(&root), DriverPlatform::Android), Some(root.join("android")));
            assert_eq!(bundled_dir(Some(&root), DriverPlatform::Ios), None, "platform not bundled");
        }
        assert_eq!(bundle_root_for_exe(&s.0.join("missing/bin/tapwing")), None);
        assert_eq!(bundled_dir(None, DriverPlatform::Android), None);
    }

    #[test]
    fn bundled_dir_requires_a_real_directory_per_platform() {
        let s = Scratch::new("bundled_dir");
        let root = s.0.join(BUNDLE_DIR);
        fs::create_dir_all(root.join("android")).unwrap();
        // A plain file named like a platform directory is not a bundle.
        fs::write(root.join("ios"), "not a dir").unwrap();
        assert_eq!(bundled_dir(Some(&root), DriverPlatform::Android), Some(root.join("android")));
        assert_eq!(bundled_dir(Some(&root), DriverPlatform::Ios), None);
        assert_eq!(bundled_dir(Some(&s.0.join("nope")), DriverPlatform::Android), None);
    }

    #[test]
    fn bundle_root_is_relative_to_the_executable_prefix_even_without_a_bundle() {
        let s = Scratch::new("no_bundle");
        fs::create_dir_all(s.0.join("bin")).unwrap();
        let exe = s.0.join("bin/tapwing");
        fs::write(&exe, "").unwrap();
        // The root is computed, not checked: `bundled_dir` is what filters on existence.
        let root = bundle_root_for_exe(&exe).unwrap();
        assert_eq!(root, s.0.join(BUNDLE_DIR));
        assert!(!root.exists());
        assert_eq!(bundled_dir(Some(&root), DriverPlatform::Android), None);
    }

    #[test]
    fn a_broken_symlink_has_no_bundle_root() {
        #[cfg(unix)]
        {
            let s = Scratch::new("dangling");
            let link = s.0.join("tapwing");
            std::os::unix::fs::symlink(s.0.join("gone/bin/tapwing"), &link).unwrap();
            assert_eq!(bundle_root_for_exe(&link), None);
        }
    }

    #[test]
    fn the_bundle_layout_constants_match_the_release_tarball() {
        assert_eq!(BUNDLE_DIR, "share/tapwing/drivers");
        assert_eq!(DriverPlatform::Android.dir_name(), "android");
        assert_eq!(DriverPlatform::Ios.dir_name(), "ios");
        assert_eq!(DEBUG_ENV, "TAPWING_DEBUG");
    }

    #[test]
    fn version_report_names_only_tapwing_drivers() {
        let report = version_report();
        assert!(report.starts_with(&format!("tapwing {}\n", env!("CARGO_PKG_VERSION"))), "{report}");
        assert!(report.contains("tapwing android driver:") && report.contains("tapwing ios driver:"), "{report}");
        // The checkout path may itself contain the word (e.g. a clone named after Maestro).
        let without_paths = report.replace(env!("CARGO_MANIFEST_DIR"), "<manifest>");
        assert!(!without_paths.to_ascii_lowercase().contains("maestro"), "{report}");
    }
}
