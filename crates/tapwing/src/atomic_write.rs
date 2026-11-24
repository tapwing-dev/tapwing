//! Crash-safe file writes for user-facing files (`tapwing init` outputs,
//! `run.json`, JUnit, HTML, trace files).
//!
//! `std::fs::write` truncates the target first, so a crash, `SIGKILL` or full
//! disk mid-write leaves a truncated file: a half `pubspec.yaml` or a JUnit
//! report CI cannot parse. [`write`] instead writes a temp file in the **same
//! directory** (same filesystem, so the rename is atomic), flushes it to disk
//! and renames it over the target. Readers and a later run see either the old
//! file or the complete new one, never a partial one.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Attempts at finding an unused temp name before giving up. A collision needs
/// a stale temp file left by a killed process that had the same PID.
const TEMP_NAME_ATTEMPTS: u32 = 16;

/// Distinguishes temp files of concurrent writers (parallel shards) in one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically replaces `path` with `contents`.
///
/// - An existing file keeps its permissions; a new file gets the same default
///   mode as `std::fs::write`.
/// - A symlink is followed, so the link stays and its target is replaced.
/// - A target that exists but is not a regular file (`/dev/stdout`, a FIFO) is
///   written in place: it cannot be renamed over and has no content to protect.
/// - An existing file in a directory that refuses new files is written in
///   place, since no temp file can be created next to it.
/// - On any other failure the temp file is removed and `path` is left untouched.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    write_with(path, |file| file.write_all(contents.as_ref()))
}

/// [`write`] with the content produced by `fill`, which receives the open
/// temp file. An error from `fill` aborts the write and keeps the old file.
pub fn write_with(path: &Path, fill: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    // Rename replaces the directory entry, so resolve a symlink to the file it
    // points at instead of replacing the link itself.
    let target = match std::fs::symlink_metadata(path) {
        // A dangling link has no target to protect: replace the link.
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    };
    let existing = std::fs::metadata(&target).ok();
    if existing.as_ref().is_some_and(|meta| !meta.is_file()) {
        let mut file = OpenOptions::new().write(true).open(&target)?;
        fill(&mut file)?;
        return file.flush();
    }

    let (mut file, mut temp) = match TempFile::create_next_to(&target, &TEMP_COUNTER) {
        Ok(created) => created,
        // A writable file in a directory we may not create files in (or a
        // redirected `/dev/stdout`) cannot be replaced atomically; writing it
        // in place is what `std::fs::write` did.
        Err(e) if existing.is_some() && e.kind() == io::ErrorKind::PermissionDenied => {
            let mut file = OpenOptions::new().write(true).truncate(true).open(&target)?;
            fill(&mut file)?;
            return file.sync_all();
        }
        Err(e) => return Err(e),
    };
    if let Some(meta) = &existing {
        file.set_permissions(meta.permissions())?;
    }
    fill(&mut file)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp.path, &target)?;
    temp.committed = true;
    sync_parent_dir(&target);
    Ok(())
}

/// Removes the temp file on drop unless it was renamed over the target.
struct TempFile {
    path: PathBuf,
    committed: bool,
}

impl TempFile {
    /// Creates a new, uniquely named file in the directory of `target`.
    fn create_next_to(target: &Path, counter: &AtomicU64) -> io::Result<(File, Self)> {
        let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let name = target
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
            .to_string_lossy();
        let pid = std::process::id();
        let mut last_err = None;
        for _ in 0..TEMP_NAME_ATTEMPTS {
            let seq = counter.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(".{name}.{pid}.{seq}.tmp"));
            // `create_new` (O_EXCL) never opens a file another writer owns.
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((file, Self { path, committed: false })),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_err = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::other("no free temp file name")))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Persists the rename itself across a power loss. Best effort: the data is
/// already safe from a process crash, and directories cannot be opened for
/// syncing on every platform.
fn sync_parent_dir(target: &Path) {
    #[cfg(unix)]
    {
        let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if let Ok(handle) = File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = target;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("tapwing-atomic-{tag}-{}-{seq}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn entries(&self) -> Vec<String> {
            let mut names: Vec<String> =
                std::fs::read_dir(&self.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_and_overwrites_without_leaving_temp_files() {
        let scratch = Scratch::new("basic");
        let path = scratch.0.join("report.json");

        write(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        write(&path, b"second, longer").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second, longer");
        write(&path, "").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        assert_eq!(scratch.entries(), ["report.json"]);
    }

    #[test]
    fn failed_write_keeps_the_old_file_and_removes_the_temp_file() {
        let scratch = Scratch::new("fail");
        let path = scratch.0.join("pubspec.yaml");
        std::fs::write(&path, "old content").unwrap();

        let err = write_with(&path, |file| {
            file.write_all(b"half of the new cont")?;
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();

        assert_eq!(err.to_string(), "disk full");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old content");
        assert_eq!(scratch.entries(), ["pubspec.yaml"]);
    }

    #[test]
    fn failed_first_write_leaves_no_file_at_all() {
        let scratch = Scratch::new("fail-new");
        let path = scratch.0.join("report.xml");

        write_with(&path, |_| Err(io::Error::other("boom"))).unwrap_err();

        assert!(scratch.entries().is_empty(), "{:?}", scratch.entries());
    }

    #[test]
    fn missing_parent_directory_is_an_error_not_a_panic() {
        let scratch = Scratch::new("no-parent");
        let err = write(&scratch.0.join("missing/report.xml"), "x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(scratch.entries().is_empty());
    }

    #[test]
    fn stale_temp_file_of_another_writer_is_never_reused() {
        let scratch = Scratch::new("stale");
        let target = scratch.0.join("run.json");
        let temp_name = |seq: u64| scratch.0.join(format!(".run.json.{}.{seq}.tmp", std::process::id()));
        for seq in 0..3 {
            std::fs::write(temp_name(seq), "other writer").unwrap();
        }

        let counter = AtomicU64::new(0);
        let (_file, temp) = TempFile::create_next_to(&target, &counter).unwrap();
        assert_eq!(temp.path, temp_name(3));
        drop(temp);

        assert!(!temp_name(3).exists(), "an uncommitted temp file is removed");
        for seq in 0..3 {
            assert_eq!(std::fs::read_to_string(temp_name(seq)).unwrap(), "other writer");
        }

        // Every candidate name taken: a clean error, nothing overwritten.
        let counter = AtomicU64::new(0);
        for seq in 0..u64::from(TEMP_NAME_ATTEMPTS) {
            std::fs::write(temp_name(seq), "other writer").unwrap();
        }
        let err = TempFile::create_next_to(&target, &counter).map(|_| ()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn concurrent_writers_to_one_directory_never_collide() {
        let scratch = Scratch::new("parallel");
        let dir = &scratch.0;
        std::thread::scope(|scope| {
            for shard in 0..8 {
                scope.spawn(move || {
                    for round in 0..20 {
                        // Same target for every writer plus one target per writer.
                        write(&dir.join("shared.json"), format!("shard {shard} round {round}")).unwrap();
                        write(&dir.join(format!("trace-shard-{shard}.json")), format!("{round}")).unwrap();
                    }
                });
            }
        });

        let shared = std::fs::read_to_string(dir.join("shared.json")).unwrap();
        assert!(shared.starts_with("shard ") && shared.ends_with("round 19"), "{shared}");
        let mut expected: Vec<String> = (0..8).map(|s| format!("trace-shard-{s}.json")).collect();
        expected.push("shared.json".into());
        expected.sort();
        assert_eq!(scratch.entries(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_preserves_permissions_and_follows_symlinks() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("perms");
        let path = scratch.0.join("main_tapwing.dart");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        write(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o640);

        let link = scratch.0.join("link.dart");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        write(&link, "through link").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link must survive");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "through link");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o640);

        assert_eq!(scratch.entries(), ["link.dart", "main_tapwing.dart"]);
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_targets_are_written_in_place() {
        write(Path::new("/dev/null"), "discarded").unwrap();
        assert!(!std::fs::metadata("/dev/null").unwrap().is_file(), "/dev/null must not be replaced by a file");
    }
}
