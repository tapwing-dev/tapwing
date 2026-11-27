//! In-process search for the driver markers in a payload, chunk by chunk.
//!
//! Spawning `grep` on macOS costs 240–680 ms on a 44 MB kernel blob; reading it
//! here takes tens of milliseconds. All markers contain [`markers::ANCHOR`], so
//! the scan looks for the byte `w` (one cheap comparison per byte), and only
//! around a hit checks the anchor and then every marker that could end there.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::markers::{self, ANCHOR, MARKERS};

/// Bytes read per step; also how often the deadline and cancel flag are checked.
const CHUNK: usize = 1 << 20;

/// What a full read of one file found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Scanned {
    pub bytes: u64,
    /// Texts of the markers found, see [`markers::MARKERS`].
    pub found: BTreeSet<&'static str>,
}

/// Adds to `found` every marker that occurs in `buf`.
pub(crate) fn find_markers(buf: &[u8], found: &mut BTreeSet<&'static str>) {
    let anchor = ANCHOR.as_bytes();
    // `w` is the third letter of the anchor.
    let third = 2;
    for (i, b) in buf.iter().enumerate() {
        if *b | 0x20 != b'w' || i < third {
            continue;
        }
        let start = i - third;
        let Some(window) = buf.get(start..start + anchor.len()) else { continue };
        if !window.eq_ignore_ascii_case(anchor) {
            continue;
        }
        for m in MARKERS {
            if found.contains(m.text) {
                continue;
            }
            // Where the anchor sits inside this marker, so the marker's own start follows.
            let Some(offset) = m.text.to_ascii_lowercase().find(ANCHOR) else { continue };
            let Some(from) = start.checked_sub(offset) else { continue };
            if buf.get(from..from + m.text.len()) == Some(m.text.as_bytes()) {
                found.insert(m.text);
            }
        }
    }
}

/// Reads `reader` to its end and reports the markers in it. Fails (never
/// guesses) when the deadline passes, the scan is cancelled or a read fails.
pub(crate) fn scan_reader(reader: &mut impl Read, deadline: Instant, cancel: &AtomicBool) -> Result<Scanned> {
    let carry_len = markers::longest().saturating_sub(1);
    let mut buf = vec![0u8; carry_len + CHUNK];
    let mut kept = 0usize;
    let mut out = Scanned::default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        if Instant::now() >= deadline {
            bail!("the deadline passed after {} bytes", out.bytes);
        }
        let n = read_full(reader, &mut buf[kept..]).context("reading the payload")?;
        if n == 0 {
            return Ok(out);
        }
        out.bytes += n as u64;
        let end = kept + n;
        find_markers(&buf[..end], &mut out.found);
        // A marker can straddle two chunks: the tail is scanned again with the next one.
        kept = carry_len.min(end);
        buf.copy_within(end - kept..end, 0);
    }
}

/// Fills as much of `buf` as the reader still has.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// [`scan_reader`] over a file, which must also have been read to the length
/// the file system reports: a file that changed or shrank while it was read
/// proves nothing.
pub(crate) fn scan_file(path: &Path, deadline: Instant, cancel: &AtomicBool) -> Result<Scanned> {
    let mut file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let expected = file.metadata().with_context(|| format!("reading the size of {}", path.display()))?.len();
    let scanned = scan_reader(&mut file, deadline, cancel).with_context(|| format!("scanning {}", path.display()))?;
    if scanned.bytes != expected {
        bail!("{} is {expected} bytes but {} were read", path.display(), scanned.bytes);
    }
    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn scan(bytes: &[u8]) -> Scanned {
        scan_reader(&mut Cursor::new(bytes.to_vec()), far(), &AtomicBool::new(false)).unwrap()
    }

    fn found(bytes: &[u8]) -> Vec<&'static str> {
        scan(bytes).found.into_iter().collect()
    }

    #[test]
    fn finds_each_marker_in_either_case_and_nothing_else() {
        assert_eq!(found(b"\0\0tapwing_port\0"), vec!["tapwing_port"]);
        assert_eq!(found(b"x TAPWING_PORT y"), vec!["TAPWING_PORT"]);
        assert_eq!(found(b"TAPWING_ALLOW_RELEASE"), vec!["TAPWING_ALLOW_RELEASE"]);
        assert_eq!(found(b"..package:tapwing_driver/src/x.dart"), vec!["package:tapwing_driver/", "tapwing_driver"]);
        assert_eq!(found(b"tapwing_driver_hello_v1"), vec!["tapwing_driver", "tapwing_driver_hello_v1"]);
        assert!(found(b"tapwing, Tapwing F7 flood, dev.tapwing.sample_app, apwing, TAPWING_PORTS").len() == 1);
        assert!(found(b"nothing to see: tapwin tapwing_por apwing_port").is_empty());
        assert!(found(b"").is_empty());
    }

    #[test]
    fn a_marker_at_the_very_start_or_end_is_found() {
        assert_eq!(found(b"tapwing_port"), vec!["tapwing_port"]);
        let mut tail = vec![b'a'; 100];
        tail.extend_from_slice(b"tapwing_port");
        assert_eq!(found(&tail), vec!["tapwing_port"]);
    }

    #[test]
    fn markers_straddling_chunk_boundaries_are_found() {
        // The needle is cut at every offset around the 1 MiB boundary.
        for cut in 1.."tapwing_driver_hello_v1".len() {
            let mut data = vec![b'.'; CHUNK - cut];
            data.extend_from_slice(b"tapwing_driver_hello_v1");
            data.extend(std::iter::repeat_n(b'.', 4096));
            let scanned = scan(&data);
            assert!(scanned.found.contains("tapwing_driver_hello_v1"), "cut at {cut}: {:?}", scanned.found);
            assert_eq!(scanned.bytes, data.len() as u64);
        }
    }

    #[test]
    fn a_short_read_reader_is_still_read_completely() {
        struct Dribble(Cursor<Vec<u8>>);
        impl Read for Dribble {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(3);
                self.0.read(&mut buf[..n])
            }
        }
        let mut r = Dribble(Cursor::new(b"xxxxxxxxxxxxtapwing_portxxxxxxxxx".to_vec()));
        let s = scan_reader(&mut r, far(), &AtomicBool::new(false)).unwrap();
        assert_eq!(s.found.into_iter().collect::<Vec<_>>(), vec!["tapwing_port"]);
        assert_eq!(s.bytes, 33);
    }

    #[test]
    fn deadline_cancel_and_read_errors_fail_instead_of_guessing() {
        let past = Instant::now() - Duration::from_millis(1);
        let err = scan_reader(&mut Cursor::new(vec![0u8; 8]), past, &AtomicBool::new(false)).unwrap_err();
        assert!(format!("{err:#}").contains("deadline"), "{err:#}");
        let err = scan_reader(&mut Cursor::new(vec![0u8; 8]), far(), &AtomicBool::new(true)).unwrap_err();
        assert!(format!("{err:#}").contains("cancelled"), "{err:#}");

        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk gone"))
            }
        }
        let err = scan_reader(&mut Broken, far(), &AtomicBool::new(false)).unwrap_err();
        assert!(format!("{err:#}").contains("disk gone"), "{err:#}");
    }

    #[test]
    fn scan_file_reads_the_whole_file_and_reports_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("tapwing-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("payload.bin");
        std::fs::write(&path, b"....TAPWING_PORT....").unwrap();
        let s = scan_file(&path, far(), &AtomicBool::new(false)).unwrap();
        assert_eq!((s.bytes, s.found.len()), (20, 1));
        assert!(scan_file(&dir.join("missing"), far(), &AtomicBool::new(false)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
