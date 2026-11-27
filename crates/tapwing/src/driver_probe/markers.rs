//! The literals that tell whether a build contains `tapwing_driver`.
//!
//! Dart never obfuscates string literals, and a build only contains what its
//! entrypoint can reach, so these survive `--obfuscate` and profile, release
//! and debug alike (verified on the sample app, see `docs/native-lanes.md`).
//! A marker is judged by what it proves:
//!
//! * `dials`: the literal sits on the path that only runs once the driver has
//!   started dialing the host (the `hello` event, the port resolution of
//!   `TapwingServer`). A release build of the Tapwing entrypoint without
//!   `TAPWING_ALLOW_RELEASE` compiles that path out, so it lacks them.
//! * the others only prove the driver *package* is linked in.
//!
//! The list covers every generation of the driver. Removing an entry would
//! make builds of that generation look driver-free, so entries are only ever
//! added; `package:tapwing_driver/` is the one no generation can drop.

/// One literal to look for in the Dart payload of an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Marker {
    pub text: &'static str,
    /// Whether the literal exists only where the driver dials the host.
    pub dials: bool,
}

/// The driver's own `hello` literal (`driverProbeMarker` in `rpc_envelope.dart`).
pub(crate) const HELLO_V1: &str = "tapwing_driver_hello_v1";

/// Every marker, newest generation first. Each contains [`ANCHOR`].
pub(crate) const MARKERS: &[Marker] = &[
    Marker { text: HELLO_V1, dials: true },
    // 0.1.1: the port handoff file and environment variable of `TapwingServer.resolvePort`.
    Marker { text: "tapwing_port", dials: true },
    Marker { text: "TAPWING_PORT", dials: true },
    // 0.1.1: the release opt-in of `isDriverEnabled`, which stays in the binary
    // when the guard disables the driver.
    Marker { text: "TAPWING_ALLOW_RELEASE", dials: false },
    // The library URI of any generation, and the bare package name for payloads
    // that keep only that.
    Marker { text: "package:tapwing_driver/", dials: false },
    Marker { text: "tapwing_driver", dials: false },
];

/// A string every marker contains in either letter case, which lets a scanner
/// look for one short needle instead of all of them.
pub(crate) const ANCHOR: &str = "apwing";

/// Longest marker, in bytes: how much of a chunk a scan carries into the next.
pub(crate) fn longest() -> usize {
    MARKERS.iter().map(|m| m.text.len()).max().unwrap_or(0)
}

/// The marker with exactly this text.
pub(crate) fn by_text(text: &str) -> Option<&'static Marker> {
    MARKERS.iter().find(|m| m.text == text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn every_marker_contains_the_anchor_in_one_letter_case() {
        for m in MARKERS {
            let lower = m.text.to_ascii_lowercase();
            assert!(lower.contains(ANCHOR), "{} lacks the scan anchor", m.text);
            assert!(m.text == lower || m.text == m.text.to_ascii_uppercase() || m.text.starts_with("tapwing"));
        }
    }

    #[test]
    fn markers_are_unique_and_the_hello_literal_dials() {
        for (i, a) in MARKERS.iter().enumerate() {
            assert!(MARKERS[i + 1..].iter().all(|b| b.text != a.text), "{} listed twice", a.text);
        }
        assert!(by_text(HELLO_V1).is_some_and(|m| m.dials));
    }

    /// The Dart driver is the source of these literals; a rename there must not
    /// silently turn every instrumented build into one the host calls driver-free.
    #[test]
    fn dialing_markers_still_exist_in_the_dart_driver() {
        let lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/tapwing_driver/lib");
        let Ok(files) = walk(&lib) else {
            return; // a packaged crate has no sibling Dart package to check
        };
        let all: String = files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
        for m in MARKERS.iter().filter(|m| m.dials) {
            // The environment variable is a `fromEnvironment` name and the file a
            // string literal; both appear verbatim in the source.
            assert!(all.contains(m.text), "marker `{}` is gone from packages/tapwing_driver/lib", m.text);
        }
        assert!(all.contains("package:tapwing_driver/") || all.contains("tapwing_driver"));
    }

    fn walk(dir: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                out.extend(walk(&path)?);
            } else if path.extension().is_some_and(|e| e == "dart") {
                out.push(path);
            }
        }
        Ok(out)
    }
}
