//! What earlier runs learned about an installed build, kept in
//! `~/.tapwing/driver-probe.json`.
//!
//! Two kinds of knowledge share an entry, keyed by device, app id and a
//! fingerprint of the installed build (a different install is a different key,
//! so a rebuilt app is never judged by its predecessor):
//!
//! * the probe's verdict, so a later run neither rescans nor, on iOS, waits to
//!   start the XCTest runner;
//! * handshake misses: consecutive `launchApp` waits that timed out while the
//!   app process was alive, which later runs may act on for builds the probe
//!   could not read (see [`Entry::learned_misses`]).
//!
//! The file is advisory. A missing, unreadable, oversized or corrupt file reads
//! as empty and is rewritten by the next update, writes are atomic, concurrent
//! shards and processes at worst lose each other's update, and the file is
//! bounded in entries, age and bytes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[cfg(test)]
use std::sync::{Arc, atomic::AtomicU64};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Presence, Verdict};
use crate::native_lane::STICKY_BLACKBOX_THRESHOLD;

/// Entries kept; the least recently updated go first.
pub(crate) const MAX_ENTRIES: usize = 256;
/// A file larger than this is not read (it cannot be ours, or it is corrupt).
const MAX_FILE_BYTES: u64 = 1 << 20;
/// Entries untouched for this long are dropped.
const MAX_AGE_SECS: u64 = 90 * 24 * 3600;
/// Learned misses stop counting this long after the last one, so an environment
/// glitch (two runs with a broken `adb reverse`) cannot demote a build for good:
/// a skipped wait never sees a handshake that would clear the entry.
pub(crate) const LEARNED_MISS_TTL_SECS: u64 = 7 * 24 * 3600;

const FILE_VERSION: u32 = 1;

/// Serializes read-modify-write cycles of this process's threads (parallel shards).
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Which installed build an entry is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstallKey {
    /// adb serial or simulator UDID.
    pub device: String,
    pub app: String,
    /// Changes whenever a different build is installed (see `android.rs`, `ios.rs`).
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Known {
    Present,
    Absent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub device: String,
    pub app: String,
    pub fingerprint: String,
    /// The probe's verdict; `None` for an entry that only counts misses.
    #[serde(default)]
    pub known: Option<Known>,
    /// Whether a present verdict rested on a dial marker.
    #[serde(default)]
    pub dials: bool,
    #[serde(default)]
    pub evidence: String,
    /// Consecutive handshake timeouts with the app alive.
    #[serde(default)]
    pub misses: u32,
    /// Unix seconds of the last update.
    pub updated: u64,
}

impl Entry {
    fn matches(&self, key: &InstallKey) -> bool {
        self.device == key.device && self.app == key.app && self.fingerprint == key.fingerprint
    }

    /// The verdict this entry remembers, if it holds one.
    pub(crate) fn verdict(&self) -> Option<Verdict> {
        let presence = match self.known? {
            Known::Present => Presence::Present,
            Known::Absent => Presence::Absent,
        };
        Some(Verdict { presence, dials: self.dials, evidence: self.evidence.clone() })
    }

    /// The miss count a later run may act on: at the sticky threshold and not
    /// older than [`LEARNED_MISS_TTL_SECS`].
    pub(crate) fn learned_misses(&self, now: u64) -> Option<u32> {
        (self.misses >= STICKY_BLACKBOX_THRESHOLD && now.saturating_sub(self.updated) <= LEARNED_MISS_TTL_SECS)
            .then_some(self.misses)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Clone, Debug)]
enum Clock {
    System,
    /// A test's own time, so concurrent tests cannot age each other's entries.
    #[cfg(test)]
    Fake(Arc<AtomicU64>),
}

/// The persistent cache. Cheap to create; every call goes to the file.
#[derive(Clone, Debug)]
pub(crate) struct DriverCache {
    path: PathBuf,
    clock: Clock,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

impl DriverCache {
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path, clock: Clock::System }
    }

    /// `~/.tapwing/driver-probe.json`, next to the iOS native driver cache.
    pub(crate) fn default_location() -> Option<Self> {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        Some(Self::at(PathBuf::from(home).join(".tapwing").join("driver-probe.json")))
    }

    #[cfg(test)]
    pub(crate) fn with_fake_clock(mut self, now: Arc<AtomicU64>) -> Self {
        self.clock = Clock::Fake(now);
        self
    }

    pub(crate) fn now(&self) -> u64 {
        match &self.clock {
            Clock::System => unix_now(),
            #[cfg(test)]
            Clock::Fake(now) => now.load(std::sync::atomic::Ordering::SeqCst),
        }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The file's store, or an empty one when it cannot be used.
    fn load(&self) -> Store {
        read_store(&self.path).unwrap_or_default()
    }

    /// The entry for exactly this install.
    pub(crate) fn lookup(&self, key: &InstallKey) -> Option<Entry> {
        self.load().entries.into_iter().find(|e| e.matches(key))
    }

    /// Remembers the probe's verdict for this install. An unknown verdict is
    /// not worth remembering; misses already counted for the same install stay.
    pub(crate) fn store_verdict(&self, key: &InstallKey, verdict: &Verdict) {
        let known = match verdict.presence {
            Presence::Present => Known::Present,
            Presence::Absent => Known::Absent,
            Presence::Unknown => return,
        };
        self.update(key, |e| {
            e.known = Some(known);
            e.dials = verdict.dials;
            e.evidence = verdict.evidence.clone();
        });
    }

    /// Counts one more consecutive handshake timeout for this install and
    /// returns the new count (`None` when the file cannot be written).
    pub(crate) fn record_miss(&self, key: &InstallKey) -> Option<u32> {
        let mut count = None;
        let wrote = self.update(key, |e| {
            e.misses = e.misses.saturating_add(1);
            count = Some(e.misses);
        });
        wrote.then_some(count).flatten()
    }

    /// A handshake arrived: whatever was learned against this app on this
    /// device is stale. Misses go; a recorded "absent" is contradicted, so it
    /// goes too; a recorded "present" stays.
    pub(crate) fn clear_after_handshake(&self, device: &str, app: &str) {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = self.load();
        let before = store.entries.clone();
        store.entries.retain_mut(|e| {
            if e.device != device || e.app != app {
                return true;
            }
            e.misses = 0;
            e.known == Some(Known::Present)
        });
        if store.entries != before {
            self.write(&mut store);
        }
    }

    /// Applies `change` to the entry of `key`, creating it, or replacing the
    /// entry of an earlier install of the same app, then saves. `false` when
    /// the file could not be written.
    fn update(&self, key: &InstallKey, change: impl FnOnce(&mut Entry)) -> bool {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = self.load();
        let at = match store.entries.iter().position(|e| e.matches(key)) {
            Some(at) => at,
            None => {
                // A new install supersedes what was known about the previous one.
                store.entries.retain(|e| !(e.device == key.device && e.app == key.app));
                store.entries.push(Entry {
                    device: key.device.clone(),
                    app: key.app.clone(),
                    fingerprint: key.fingerprint.clone(),
                    known: None,
                    dials: false,
                    evidence: String::new(),
                    misses: 0,
                    updated: 0,
                });
                store.entries.len() - 1
            }
        };
        change(&mut store.entries[at]);
        store.entries[at].updated = self.now();
        self.write(&mut store)
    }

    fn write(&self, store: &mut Store) -> bool {
        let now = self.now();
        store.version = FILE_VERSION;
        store.entries.retain(|e| now.saturating_sub(e.updated) <= MAX_AGE_SECS);
        if store.entries.len() > MAX_ENTRIES {
            store.entries.sort_by_key(|e| std::cmp::Reverse(e.updated));
            store.entries.truncate(MAX_ENTRIES);
        }
        let Ok(json) = serde_json::to_vec_pretty(store) else { return false };
        if let Some(dir) = self.path.parent()
            && std::fs::create_dir_all(dir).is_err()
        {
            return false;
        }
        crate::atomic_write::write(&self.path, json).is_ok()
    }
}

/// Parses the cache file; `None` when it is missing, too large, not UTF-8,
/// not JSON of this shape, or written by another version.
fn read_store(path: &Path) -> Option<Store> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
    let store: Store = serde_json::from_str(&text).ok()?;
    (store.version == FILE_VERSION).then_some(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// A cache in its own directory with its own clock, starting at 1 000 000 s.
    fn temp_cache(tag: &str) -> (DriverCache, PathBuf, Arc<AtomicU64>) {
        let dir = std::env::temp_dir().join(format!("tapwing-driver-cache-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = Arc::new(AtomicU64::new(1_000_000));
        let cache = DriverCache::at(dir.join("nested").join("driver-probe.json")).with_fake_clock(now.clone());
        (cache, dir, now)
    }

    fn key(fp: &str) -> InstallKey {
        InstallKey { device: "emulator-5554".into(), app: "dev.example.app".into(), fingerprint: fp.into() }
    }

    fn absent() -> Verdict {
        Verdict { presence: Presence::Absent, dials: false, evidence: "read libapp.so completely".into() }
    }

    #[test]
    fn a_verdict_round_trips_and_is_keyed_by_device_app_and_fingerprint() {
        let (cache, dir, _now) = temp_cache("roundtrip");
        assert!(cache.lookup(&key("a")).is_none(), "no file yet");
        cache.store_verdict(&key("a"), &absent());
        let hit = cache.lookup(&key("a")).unwrap();
        assert_eq!(hit.verdict(), Some(absent()));

        // Another device, another app and another install each miss.
        assert!(cache.lookup(&InstallKey { device: "emulator-5556".into(), ..key("a") }).is_none());
        assert!(cache.lookup(&InstallKey { app: "dev.example.other".into(), ..key("a") }).is_none());
        assert!(cache.lookup(&key("b")).is_none(), "a different install is a different key");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_install_replaces_the_old_one_and_its_misses() {
        let (cache, dir, _now) = temp_cache("reinstall");
        cache.store_verdict(&key("old"), &absent());
        assert_eq!(cache.record_miss(&key("old")), Some(1));
        assert_eq!(cache.record_miss(&key("old")), Some(2));
        // Installing a different build starts from nothing, and drops the old entry.
        assert_eq!(cache.record_miss(&key("new")), Some(1));
        assert!(cache.lookup(&key("old")).is_none());
        let fresh = cache.lookup(&key("new")).unwrap();
        assert_eq!((fresh.misses, fresh.known), (1, None));
        assert_eq!(cache.load().entries.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_verdicts_are_not_stored_and_misses_survive_a_verdict() {
        let (cache, dir, _now) = temp_cache("unknown");
        cache.store_verdict(&key("a"), &Verdict::unknown("no payload"));
        assert!(cache.lookup(&key("a")).is_none());
        assert_eq!(cache.record_miss(&key("a")), Some(1));
        cache.store_verdict(&key("a"), &Verdict { presence: Presence::Present, dials: false, evidence: "x".into() });
        let e = cache.lookup(&key("a")).unwrap();
        assert_eq!((e.misses, e.known), (1, Some(Known::Present)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn misses_become_learned_only_at_the_threshold_and_expire() {
        let (cache, dir, now) = temp_cache("learned");
        now.store(2_000_000, Ordering::SeqCst);
        cache.record_miss(&key("a"));
        assert_eq!(cache.lookup(&key("a")).unwrap().learned_misses(cache.now()), None, "one miss is not learned");
        cache.record_miss(&key("a"));
        let e = cache.lookup(&key("a")).unwrap();
        assert_eq!(e.learned_misses(cache.now()), Some(STICKY_BLACKBOX_THRESHOLD));
        assert_eq!(e.learned_misses(cache.now() + LEARNED_MISS_TTL_SECS), Some(STICKY_BLACKBOX_THRESHOLD));
        assert_eq!(e.learned_misses(cache.now() + LEARNED_MISS_TTL_SECS + 1), None, "stale misses are not acted on");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_handshake_clears_misses_and_a_contradicted_absent_but_keeps_present() {
        let (cache, dir, _now) = temp_cache("clear");
        let other = InstallKey { app: "dev.example.other".into(), ..key("x") };
        cache.record_miss(&key("a"));
        cache.record_miss(&key("a"));
        cache.store_verdict(&key("a"), &absent());
        cache.store_verdict(&other, &absent());
        cache.clear_after_handshake("emulator-5554", "dev.example.app");
        assert!(cache.lookup(&key("a")).is_none(), "an absent verdict the app just contradicted is gone");
        assert!(cache.lookup(&other).is_some(), "other apps are untouched");

        let present = Verdict { presence: Presence::Present, dials: true, evidence: "p".into() };
        cache.store_verdict(&key("b"), &present);
        cache.record_miss(&key("b"));
        cache.clear_after_handshake("emulator-5554", "dev.example.app");
        let kept = cache.lookup(&key("b")).unwrap();
        assert_eq!((kept.misses, kept.known), (0, Some(Known::Present)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clearing_with_nothing_recorded_never_creates_the_file() {
        let (cache, dir, _now) = temp_cache("clear-empty");
        cache.clear_after_handshake("d", "a");
        assert!(!cache.path().exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_oversized_or_foreign_files_read_as_empty_and_are_rewritten() {
        let (cache, dir, _now) = temp_cache("corrupt");
        std::fs::create_dir_all(cache.path().parent().unwrap()).unwrap();
        for bad in [
            b"{ not json".to_vec(),
            vec![0xff, 0xfe, 0x00],
            b"[]".to_vec(),
            br#"{"version": 99, "entries": []}"#.to_vec(),
            br#"{"version": 1, "entries": [{"device": 7}]}"#.to_vec(),
            vec![b' '; (MAX_FILE_BYTES + 1) as usize],
            Vec::new(),
        ] {
            std::fs::write(cache.path(), &bad).unwrap();
            assert!(cache.lookup(&key("a")).is_none());
            cache.store_verdict(&key("a"), &absent());
            assert_eq!(
                cache.lookup(&key("a")).unwrap().verdict(),
                Some(absent()),
                "rewritten after {} bytes",
                bad.len()
            );
        }
        // A directory where the file should be is a failed write, never a panic.
        std::fs::remove_file(cache.path()).unwrap();
        std::fs::create_dir(cache.path()).unwrap();
        assert_eq!(cache.record_miss(&key("a")), None);
        assert!(cache.lookup(&key("a")).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn size_and_age_are_bounded() {
        let (cache, dir, now) = temp_cache("bounded");
        now.store(10_000_000, Ordering::SeqCst);
        for i in 0..(MAX_ENTRIES + 20) {
            now.fetch_add(1, Ordering::SeqCst);
            cache.store_verdict(&InstallKey { app: format!("app{i}"), ..key("f") }, &absent());
        }
        let store = cache.load();
        assert_eq!(store.entries.len(), MAX_ENTRIES);
        assert!(store.entries.iter().all(|e| e.app != "app0"), "the least recently updated went first");
        assert!(store.entries.iter().any(|e| e.app == format!("app{}", MAX_ENTRIES + 19)));

        now.fetch_add(MAX_AGE_SECS + 10, Ordering::SeqCst);
        cache.store_verdict(&InstallKey { app: "fresh".into(), ..key("f") }, &absent());
        assert_eq!(cache.load().entries.len(), 1, "entries past the age limit are dropped");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_writers_never_corrupt_the_file_and_their_counts_add_up() {
        let (cache, dir, _now) = temp_cache("concurrent");
        const THREADS: usize = 8;
        const EACH: usize = 12;
        std::thread::scope(|s| {
            for t in 0..THREADS {
                let cache = cache.clone();
                s.spawn(move || {
                    for _ in 0..EACH {
                        // Shards of one run count misses of the same install...
                        cache.record_miss(&key("shared"));
                        // ...and every shard also writes its own entries.
                        cache.store_verdict(&InstallKey { device: format!("dev{t}"), ..key("own") }, &absent());
                    }
                });
            }
            // A reader never sees a half-written file: the strict parse always works once it exists.
            let path = cache.path().to_path_buf();
            s.spawn(move || {
                for _ in 0..200 {
                    if path.exists() {
                        let text = std::fs::read_to_string(&path).expect("readable");
                        serde_json::from_str::<serde_json::Value>(&text).expect("a complete JSON file");
                    }
                }
            });
        });
        assert_eq!(cache.lookup(&key("shared")).unwrap().misses as usize, THREADS * EACH);
        assert_eq!(cache.load().entries.len(), THREADS + 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_read_only_location_is_a_failed_write_not_a_crash() {
        let cache = DriverCache::at(PathBuf::from("/proc/tapwing-cannot-write-here/driver-probe.json"));
        assert_eq!(cache.record_miss(&key("a")), None);
        cache.store_verdict(&key("a"), &absent());
        cache.clear_after_handshake("emulator-5554", "dev.example.app");
        assert!(cache.lookup(&key("a")).is_none());
    }
}
