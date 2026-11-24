//! Screenshot cropping, path resolution, and Maestro-exact pixel comparison
//! (`maestro-orchestra/.../ScreenshotMatch.kt`, `Orchestra.kt:681-773, 1262-1287`,
//! `Maestro.kt:629-682`).

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use image::{DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage};

/// Maestro's default Euclidean RGB pixel tolerance (`ScreenshotMatch.kt:22`).
pub const DEFAULT_PIXEL_TOLERANCE: f64 = 0.1;

/// Subdirectory under `--test-output-dir` where `takeScreenshot` writes PNGs
/// (matches Maestro's `BundleLayout.TAKE_SCREENSHOT_DIR`, `Orchestra.kt:702`).
pub const SCREENSHOTS_SUBDIR: &str = "screenshots";

/// Subdirectory under `--test-output-dir` where `assertScreenshot` writes diff
/// PNGs on mismatch (matches `BundleLayout.SCREENSHOT_DIFF_DIR`, `Orchestra.kt:752-755`).
pub const SCREENSHOT_DIFFS_SUBDIR: &str = "screenshot-diffs";

/// Image extensions recognized by `Orchestra.normalizeScreenshotPath` (`Orchestra.kt:682`).
const IMAGE_EXTENSIONS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tiff", ".wbmp", ".heic", ".heif"];

/// Appends `.png` unless `path` already ends with a known image extension (`Orchestra.kt:681-684`).
pub fn normalize_assert_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) { path.to_string() } else { format!("{path}.png") }
}

/// Output filename for `takeScreenshot`: Maestro ALWAYS appends `.png`
/// (`Orchestra.kt:1267`: `"${command.path}.png"`), so `"shot.png"` becomes
/// `"shot.png.png"`.
pub fn normalize_take_path(path: &str) -> String {
    format!("{path}.png")
}

/// Resolves the output path for `takeScreenshot` inside `test_output_dir/screenshots/`
/// (or directly when `raw_path` is absolute), matching `Orchestra.kt:1266-1268`.
pub fn resolve_take_path(test_output_dir: &Path, raw_path: &str) -> PathBuf {
    let norm = normalize_take_path(raw_path);
    let p = Path::new(&norm);
    if p.is_absolute() { p.to_path_buf() } else { test_output_dir.join(SCREENSHOTS_SUBDIR).join(p) }
}

/// Resolves the reference screenshot for `assertScreenshot` (`Orchestra.kt:697-716`):
/// searches relative to the flow directory first, then `test_output_dir/screenshots/`
/// (never CWD).
pub fn find_expected_file(flow_path: &Path, test_output_dir: &Path, raw_path: &str) -> Result<PathBuf> {
    let norm = normalize_assert_path(raw_path);
    let mut candidates = Vec::new();
    let p = Path::new(&norm);
    if p.is_absolute() {
        candidates.push(p.to_path_buf());
    } else {
        if let Some(flow_dir) = flow_path.parent().filter(|d| !d.as_os_str().is_empty()) {
            candidates.push(flow_dir.join(&norm));
        }
        candidates.push(test_output_dir.join(SCREENSHOTS_SUBDIR).join(&norm));
    }
    let mut seen = Vec::new();
    let mut unique = Vec::new();
    for c in candidates {
        let key = std::fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
        if !seen.contains(&key) {
            seen.push(key);
            unique.push(c);
        }
    }
    if let Some(found) = unique.iter().find(|c| c.exists()) {
        return Ok(found.clone());
    }
    let searched = unique.iter().map(|c| format!("  - {}", c.display())).collect::<Vec<_>>().join("\n");
    bail!("Screenshot file not found: {norm}. Searched in:\n{searched}");
}

/// Encodes a [`DynamicImage`] to PNG bytes.
pub fn encode_png(img: &DynamicImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).context("encoding PNG")?;
    Ok(out.into_inner())
}

/// Crops a decoded image to `[left, top, right, bottom]`, clamping to image extents
/// (`Maestro.kt:657-674`).
pub fn crop_image(img: &DynamicImage, bounds: [i64; 4]) -> Result<DynamicImage> {
    let [left, top, right, bottom] = bounds;
    let width = right - left;
    let height = bottom - top;
    if width <= 0 || height <= 0 {
        bail!("Cannot crop screenshot: invalid dimensions (width: {width}, height: {height})");
    }
    let (img_w, img_h) = img.dimensions();
    let start_x = left.clamp(0, i64::from(img_w)) as u32;
    let start_y = top.clamp(0, i64::from(img_h)) as u32;
    let crop_w = width.clamp(0, i64::from(img_w - start_x)) as u32;
    let crop_h = height.clamp(0, i64::from(img_h - start_y)) as u32;
    if crop_w == 0 || crop_h == 0 {
        bail!("Cannot crop screenshot: invalid dimensions (width: {crop_w}, height: {crop_h})");
    }
    Ok(img.crop_imm(start_x, start_y, crop_w, crop_h))
}

/// Result of comparing an actual screenshot against a reference (`ScreenshotMatch.Result`).
#[derive(Debug, PartialEq)]
pub enum CompareResult {
    Match { match_percent: f64 },
    Mismatch { match_percent: f64 },
    SizeMismatch { expected_w: u32, expected_h: u32, actual_w: u32, actual_h: u32 },
}

/// Exact port of `ScreenshotMatch.matchPercentage` (`ScreenshotMatch.kt:78-112`).
///
/// Counts pixels whose Euclidean RGB distance exceeds `pixel_tolerance * sqrt(255^2 * 3)`
/// and returns `100.0 - (differing / total) * 100.0`.
pub fn match_percentage(expected: &RgbaImage, actual: &RgbaImage, pixel_tolerance: f64) -> f64 {
    let (w, h) = expected.dimensions();
    assert_eq!((w, h), actual.dimensions(), "images must have the same dimensions");
    let total = u64::from(w) * u64::from(h);
    if total == 0 {
        return 100.0;
    }
    let max_dist = (255.0_f64 * 255.0 * 3.0).sqrt();
    let diff_const = (pixel_tolerance * max_dist).powi(2);
    let mut differing: u64 = 0;
    for (e, a) in expected.pixels().zip(actual.pixels()) {
        if e == a {
            continue;
        }
        if pixel_tolerance == 0.0 {
            differing += 1;
            continue;
        }
        let dr = i32::from(a[0]) - i32::from(e[0]);
        let dg = i32::from(a[1]) - i32::from(e[1]);
        let db = i32::from(a[2]) - i32::from(e[2]);
        let sq_dist = f64::from(dr * dr + dg * dg + db * db);
        if sq_dist > diff_const {
            differing += 1;
        }
    }
    100.0 - (differing as f64 / total as f64) * 100.0
}

/// Compares `actual` against `expected` at `threshold_percentage` (`ScreenshotMatch.kt:39-71`).
/// On mismatch, writes a visual diff PNG to `diff_path`.
pub fn compare(
    expected: &DynamicImage,
    actual: &DynamicImage,
    threshold_percentage: f64,
    diff_path: &Path,
    pixel_tolerance: f64,
) -> Result<CompareResult> {
    let (ew, eh) = expected.dimensions();
    let (aw, ah) = actual.dimensions();
    if ew != aw || eh != ah {
        return Ok(CompareResult::SizeMismatch { expected_w: ew, expected_h: eh, actual_w: aw, actual_h: ah });
    }
    let exp_rgba = expected.to_rgba8();
    let act_rgba = actual.to_rgba8();
    let match_percent = match_percentage(&exp_rgba, &act_rgba, pixel_tolerance);
    if match_percent >= threshold_percentage {
        return Ok(CompareResult::Match { match_percent });
    }
    write_diff_image(&exp_rgba, &act_rgba, pixel_tolerance, diff_path)?;
    Ok(CompareResult::Mismatch { match_percent })
}

/// Writes a diff PNG highlighting pixels exceeding `pixel_tolerance` in bright red
/// over the actual screenshot.
fn write_diff_image(expected: &RgbaImage, actual: &RgbaImage, pixel_tolerance: f64, diff_path: &Path) -> Result<()> {
    let (w, h) = actual.dimensions();
    let max_dist = (255.0_f64 * 255.0 * 3.0).sqrt();
    let diff_const = (pixel_tolerance * max_dist).powi(2);
    let mut out = RgbaImage::new(w, h);
    for (x, y, a) in actual.enumerate_pixels() {
        let e = expected.get_pixel(x, y);
        let differs = if e == a {
            false
        } else if pixel_tolerance == 0.0 {
            true
        } else {
            let dr = i32::from(a[0]) - i32::from(e[0]);
            let dg = i32::from(a[1]) - i32::from(e[1]);
            let db = i32::from(a[2]) - i32::from(e[2]);
            f64::from(dr * dr + dg * dg + db * db) > diff_const
        };
        if differs {
            out.put_pixel(x, y, Rgba([255, 0, 0, 255]));
        } else {
            out.put_pixel(x, y, *a);
        }
    }
    if let Some(parent) = diff_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    out.save_with_format(diff_path, ImageFormat::Png)
        .with_context(|| format!("writing diff image to {}", diff_path.display()))?;
    Ok(())
}

/// Derives `<test_output_dir>/screenshot-diffs/<stem>_diff.png` (`Orchestra.kt:752-755`).
pub fn diff_output_path(test_output_dir: &Path, expected_file: &Path) -> PathBuf {
    let stem = expected_file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "diff".into());
    test_output_dir.join(SCREENSHOT_DIFFS_SUBDIR).join(format!("{stem}_diff.png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba(rgba)))
    }

    #[test]
    fn path_normalization_matches_maestro() {
        assert_eq!(normalize_assert_path("login"), "login.png");
        assert_eq!(normalize_assert_path("login.png"), "login.png");
        assert_eq!(normalize_assert_path("login.JPEG"), "login.JPEG");
        // Orchestra.kt:1267: takeScreenshot ALWAYS appends `.png`.
        assert_eq!(normalize_take_path("shot"), "shot.png");
        assert_eq!(normalize_take_path("shot.png"), "shot.png.png");
    }

    #[test]
    fn find_expected_searches_flow_dir_and_artifacts_not_cwd() {
        let base = std::env::temp_dir().join(format!("tapwing_shot_paths_{}", std::process::id()));
        let flow_dir = base.join("flows");
        let out_dir = base.join("out");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&flow_dir).unwrap();
        std::fs::create_dir_all(out_dir.join(SCREENSHOTS_SUBDIR)).unwrap();

        let flow_path = flow_dir.join("test.yaml");
        let art_file = resolve_take_path(&out_dir, "from_take");
        std::fs::write(&art_file, b"png").unwrap();

        // Found in artifacts dir when absent from flow dir.
        let found = find_expected_file(&flow_path, &out_dir, "from_take").unwrap();
        assert_eq!(found, art_file);

        // Flow dir takes precedence over artifacts dir.
        let flow_ref = flow_dir.join("from_take.png");
        std::fs::write(&flow_ref, b"png2").unwrap();
        let found2 = find_expected_file(&flow_path, &out_dir, "from_take").unwrap();
        assert_eq!(found2, flow_ref);

        // Missing file errors cleanly without falling back to CWD.
        assert!(find_expected_file(&flow_path, &out_dir, "Cargo.toml").is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn match_percentage_tolerance_and_mismatch() {
        let a = solid(10, 10, [100, 100, 100, 255]);
        let b = solid(10, 10, [105, 100, 100, 255]); // distance 5 < 0.1 * 441.67 (~44.17)
        assert_eq!(match_percentage(&a.to_rgba8(), &b.to_rgba8(), DEFAULT_PIXEL_TOLERANCE), 100.0);
        assert_eq!(match_percentage(&a.to_rgba8(), &b.to_rgba8(), 0.0), 0.0);

        let mut half = a.to_rgba8();
        for y in 0..5 {
            for x in 0..10 {
                half.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let pct = match_percentage(&a.to_rgba8(), &half, DEFAULT_PIXEL_TOLERANCE);
        assert!((pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn compare_handles_match_mismatch_diff_file_and_size_mismatch() {
        let dir = std::env::temp_dir().join(format!("tapwing_shot_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let diff = dir.join("out_diff.png");

        let img1 = solid(20, 20, [10, 20, 30, 255]);
        let img2 = solid(20, 20, [200, 200, 200, 255]);
        let img3 = solid(10, 10, [10, 20, 30, 255]);

        assert_eq!(
            compare(&img1, &img1, 95.0, &diff, DEFAULT_PIXEL_TOLERANCE).unwrap(),
            CompareResult::Match { match_percent: 100.0 }
        );
        assert!(!diff.exists());

        assert_eq!(
            compare(&img1, &img2, 95.0, &diff, DEFAULT_PIXEL_TOLERANCE).unwrap(),
            CompareResult::Mismatch { match_percent: 0.0 }
        );
        assert!(diff.exists());

        assert_eq!(
            compare(&img1, &img3, 95.0, &diff, DEFAULT_PIXEL_TOLERANCE).unwrap(),
            CompareResult::SizeMismatch { expected_w: 20, expected_h: 20, actual_w: 10, actual_h: 10 }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crop_clamps_to_image_bounds() {
        let img = solid(100, 80, [1, 2, 3, 255]);
        let cropped = crop_image(&img, [-10, 10, 50, 120]).unwrap();
        assert_eq!(cropped.dimensions(), (60, 70));
        assert!(crop_image(&img, [10, 10, 10, 20]).is_err());
    }

    #[test]
    fn matches_maestro_reference_fixtures() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/fixtures/screenshot");
        assert!(dir.exists(), "committed Maestro screenshot fixtures missing at {}", dir.display());
        let expected = image::open(dir.join("expected.png")).unwrap().to_rgba8();
        let actual = image::open(dir.join("actual.png")).unwrap().to_rgba8();
        let saved = image::open(dir.join("saved_without_search.png")).unwrap().to_rgba8();

        // Pinned numbers from Maestro AssertScreenshotMatchTest.kt:42, 77, 130.
        let p1 = match_percentage(&expected, &actual, DEFAULT_PIXEL_TOLERANCE);
        assert!((p1 - 99.9511).abs() < 0.01, "expected ~99.9511, got {p1}");

        let p2 = match_percentage(&actual, &saved, DEFAULT_PIXEL_TOLERANCE);
        assert!((p2 - 91.4724).abs() < 0.01, "expected ~91.4724, got {p2}");

        let p3 = match_percentage(&expected, &actual, 0.0);
        assert!((p3 - 99.4251).abs() < 0.01, "expected ~99.4251, got {p3}");
    }
}
