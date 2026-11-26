//! Does the installed app contain `tapwing_driver`? Decided before `launchApp`
//! waits for a handshake that a driver-free build can never send.
//!
//! `--input auto` waits [`crate::run`]'s `HANDSHAKE_FALLBACK_TIMEOUT` (15 s) for
//! the in-app driver and only then drives the app through the native lane. A
//! build made from the plain entrypoint has no driver, so that wait is pure
//! loss. The verdict here is one of three:
//!
//! * [`Presence::Absent`] needs positive proof: the Dart payload was found and
//!   read completely and holds no marker of any driver generation, or the app is
//!   not a Flutter app at all.
//! * [`Presence::Present`]: a marker was found. A wrong "present" only costs the
//!   old wait.
//! * [`Presence::Unknown`]: anything else (nothing found to read, a read cut
//!   short, an uninspected split, a missing tool, a deadline, any error). It
//!   behaves exactly as before the probe existed.
//!
//! A wrong "absent" would silently demote a working instrumented app to the
//! slower, less exact lane, so every doubt resolves to unknown.

pub(crate) mod android;
pub(crate) mod cache;
pub(crate) mod ios;
pub(crate) mod markers;
pub(crate) mod scan;

use std::collections::BTreeSet;

use cache::{DriverCache, InstallKey};

/// What the inspection concluded about the driver in the installed build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Present,
    Absent,
    Unknown,
}

impl Presence {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Unknown => "unknown",
        }
    }
}

/// A [`Presence`] with what it rests on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub presence: Presence,
    /// A marker that only exists where the driver dials the host was found.
    /// Without one, a "present" only says the package is linked in (a release
    /// build without the opt-in compiles the dial path out).
    pub dials: bool,
    /// A short, user-readable statement of what was inspected.
    pub evidence: String,
}

impl Verdict {
    pub(crate) fn unknown(why: impl Into<String>) -> Self {
        Self { presence: Presence::Unknown, dials: false, evidence: why.into() }
    }
}

/// Where a [`Report`]'s verdict came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerdictSource {
    /// The payload was read by this run.
    Scan,
    /// An earlier run read the same install (see [`cache`]).
    Cache,
}

/// A probe's whole answer: the verdict, how it was reached and what the run
/// needs to record about the install afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub verdict: Verdict,
    pub source: VerdictSource,
    /// The installed build's identity; `None` when it could not be established
    /// (then nothing is cached or learned about it).
    pub fingerprint: Option<String>,
    /// Consecutive handshake timeouts earlier runs saw against this install,
    /// once they reached the sticky threshold and are still fresh.
    pub learned_misses: Option<u32>,
}

impl Report {
    /// An unknown verdict with no install identity: behaves like no probe at all.
    pub(crate) fn unknown(why: impl Into<String>) -> Self {
        Self { verdict: Verdict::unknown(why), source: VerdictSource::Scan, fingerprint: None, learned_misses: None }
    }

    /// Whether a run may skip the handshake wait on this report, and why.
    pub fn skip_reason(&self) -> Option<SkipReason> {
        match self.verdict.presence {
            Presence::Absent => Some(SkipReason::NoDriver { cached: self.source == VerdictSource::Cache }),
            // A build the probe proved to dial is never second-guessed by misses;
            // one it could not read (or that links the driver without a dial
            // path) may be, once earlier runs timed out on it repeatedly.
            Presence::Present if self.verdict.dials => None,
            Presence::Present | Presence::Unknown => {
                self.learned_misses.map(|misses| SkipReason::LearnedMisses { misses })
            }
        }
    }
}

/// Why a launch does not wait for the handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The installed build holds no driver (read now, or remembered).
    NoDriver { cached: bool },
    /// Earlier runs waited the full time on this install and got no handshake.
    LearnedMisses { misses: u32 },
}

/// Gives the report for an install: the cached verdict when this exact build
/// was judged before, otherwise whatever `inspect` concludes (which is then
/// cached). `fingerprint` of `None` skips the cache entirely.
pub(crate) fn resolve(
    device: &str,
    app: &str,
    fingerprint: Option<String>,
    cache: Option<&DriverCache>,
    inspect: impl FnOnce() -> Verdict,
) -> Report {
    let key = fingerprint.clone().map(|fingerprint| InstallKey { device: device.into(), app: app.into(), fingerprint });
    let entry = key.as_ref().zip(cache).and_then(|(key, cache)| cache.lookup(key));
    let (verdict, source) = match entry.as_ref().and_then(cache::Entry::verdict) {
        Some(verdict) => (verdict, VerdictSource::Cache),
        None => {
            let verdict = inspect();
            if let (Some(key), Some(cache)) = (&key, cache) {
                cache.store_verdict(key, &verdict);
            }
            (verdict, VerdictSource::Scan)
        }
    };
    let learned_misses = entry.zip(cache).and_then(|(e, c)| e.learned_misses(c.now()));
    Report { verdict, source, fingerprint, learned_misses }
}

/// Whether the Flutter engine library was among what was inspected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    Found,
    NotFound,
}

/// A Dart payload that was read to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Payload {
    /// Where it was read, for the evidence (`base.apk: lib/arm64-v8a/libapp.so`).
    pub name: String,
    pub bytes: u64,
    pub found: BTreeSet<&'static str>,
}

/// Everything one inspection learned, before it is judged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Inspection {
    /// Where the engine was looked for (`base.apk`, the `.app` bundle), for the evidence.
    pub scope: String,
    pub engine: Engine,
    /// Only payloads read completely belong here.
    pub payloads: Vec<Payload>,
    /// Why the inspection is incomplete, one entry per thing that could not be
    /// looked at as it should have been. Any entry rules out "absent".
    pub gaps: Vec<String>,
}

/// Judges an inspection. Pure, so every case is a table row in the tests.
pub(crate) fn classify(inspection: &Inspection) -> Verdict {
    let found: BTreeSet<&'static str> = inspection.payloads.iter().flat_map(|p| p.found.iter().copied()).collect();
    if !found.is_empty() {
        // A marker is proof enough, whatever else could not be read.
        let dials = found.iter().any(|t| markers::by_text(t).is_some_and(|m| m.dials));
        let where_ = inspection.payloads.iter().filter(|p| !p.found.is_empty()).map(|p| p.name.as_str());
        let list = found.iter().copied().collect::<Vec<_>>().join(", ");
        let note = if dials { "" } else { " (no dial marker: the driver may be disabled in this build)" };
        return Verdict {
            presence: Presence::Present,
            dials,
            evidence: format!("found {list} in {}{note}", where_.collect::<Vec<_>>().join(", ")),
        };
    }
    if let Some(gap) = inspection.gaps.first() {
        let more = inspection.gaps.len().saturating_sub(1);
        let tail = if more > 0 { format!(" (+{more} more)") } else { String::new() };
        return Verdict::unknown(format!("{gap}{tail}"));
    }
    match (inspection.engine, inspection.payloads.is_empty()) {
        (Engine::NotFound, true) => Verdict {
            presence: Presence::Absent,
            dials: false,
            evidence: format!("{} has no Flutter engine library, so it is not a Flutter app", inspection.scope),
        },
        (Engine::NotFound, false) => Verdict::unknown(format!(
            "{} has a Dart payload but no Flutter engine library; cannot tell what it runs",
            inspection.scope
        )),
        (Engine::Found, true) => Verdict::unknown(format!(
            "{} has the Flutter engine but no Dart payload I know how to read",
            inspection.scope
        )),
        (Engine::Found, false) => {
            let read: Vec<String> =
                inspection.payloads.iter().map(|p| format!("{} ({})", p.name, human_bytes(p.bytes))).collect();
            Verdict {
                presence: Presence::Absent,
                dials: false,
                evidence: format!("read {} completely: no tapwing_driver marker", read.join(", ")),
            }
        }
    }
}

/// `7.6 MB` style size for evidence strings.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 { format!("{:.1} MB", bytes as f64 / MB) } else { format!("{} KB", bytes.div_ceil(1024)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(name: &str, found: &[&'static str]) -> Payload {
        Payload { name: name.into(), bytes: 7_996_376, found: found.iter().copied().collect() }
    }

    fn inspection(engine: Engine, payloads: Vec<Payload>, gaps: &[&str]) -> Inspection {
        Inspection { scope: "base.apk".into(), engine, payloads, gaps: gaps.iter().map(|g| g.to_string()).collect() }
    }

    #[test]
    fn classify_table() {
        struct Case {
            name: &'static str,
            input: Inspection,
            presence: Presence,
            dials: bool,
            evidence_has: &'static str,
        }
        let cases = [
            Case {
                name: "a profile build with the driver dials",
                input: inspection(
                    Engine::Found,
                    vec![payload(
                        "lib/arm64-v8a/libapp.so",
                        &["tapwing_port", "TAPWING_PORT", "package:tapwing_driver/"],
                    )],
                    &[],
                ),
                presence: Presence::Present,
                dials: true,
                evidence_has: "TAPWING_PORT",
            },
            Case {
                name: "the hello literal alone proves the dial path",
                input: inspection(Engine::Found, vec![payload("libapp.so", &["tapwing_driver_hello_v1"])], &[]),
                presence: Presence::Present,
                dials: true,
                evidence_has: "tapwing_driver_hello_v1",
            },
            Case {
                name: "a release build without the opt-in links the package but never dials",
                input: inspection(
                    Engine::Found,
                    vec![payload("libapp.so", &["TAPWING_ALLOW_RELEASE", "package:tapwing_driver/"])],
                    &[],
                ),
                presence: Presence::Present,
                dials: false,
                evidence_has: "may be disabled",
            },
            Case {
                name: "a marker wins over a gap elsewhere (a wrong present is cheap)",
                input: inspection(
                    Engine::Found,
                    vec![payload("libapp.so", &["tapwing_port"])],
                    &["split_config.x.apk unreadable"],
                ),
                presence: Presence::Present,
                dials: true,
                evidence_has: "tapwing_port",
            },
            Case {
                name: "a plain build: payload read completely, no marker",
                input: inspection(Engine::Found, vec![payload("lib/arm64-v8a/libapp.so", &[])], &[]),
                presence: Presence::Absent,
                dials: false,
                evidence_has: "read lib/arm64-v8a/libapp.so (7.6 MB) completely",
            },
            Case {
                name: "a plain debug build: the kernel blob was read",
                input: inspection(Engine::Found, vec![payload("assets/flutter_assets/kernel_blob.bin", &[])], &[]),
                presence: Presence::Absent,
                dials: false,
                evidence_has: "kernel_blob.bin",
            },
            Case {
                name: "not a Flutter app at all",
                input: inspection(Engine::NotFound, vec![], &[]),
                presence: Presence::Absent,
                dials: false,
                evidence_has: "not a Flutter app",
            },
            Case {
                name: "engine but no payload to read",
                input: inspection(Engine::Found, vec![], &[]),
                presence: Presence::Unknown,
                dials: false,
                evidence_has: "no Dart payload",
            },
            Case {
                name: "a payload without an engine is inconsistent",
                input: inspection(Engine::NotFound, vec![payload("libapp.so", &[])], &[]),
                presence: Presence::Unknown,
                dials: false,
                evidence_has: "no Flutter engine library",
            },
            Case {
                name: "a payload that could not be read completely",
                input: inspection(Engine::Found, vec![], &["libapp.so: read 4096 of 7996376 bytes"]),
                presence: Presence::Unknown,
                dials: false,
                evidence_has: "read 4096 of 7996376",
            },
            Case {
                name: "one payload clean but another unreadable stays unknown",
                input: inspection(
                    Engine::Found,
                    vec![payload("libapp.so", &[])],
                    &["kernel_blob.bin: deadline passed"],
                ),
                presence: Presence::Unknown,
                dials: false,
                evidence_has: "deadline passed",
            },
            Case {
                name: "a non-Flutter verdict with an uninspected split stays unknown",
                input: inspection(Engine::NotFound, vec![], &["split_config.arm64_v8a.apk: listing failed", "second"]),
                presence: Presence::Unknown,
                dials: false,
                evidence_has: "(+1 more)",
            },
        ];
        for c in cases {
            let v = classify(&c.input);
            assert_eq!((v.presence, v.dials), (c.presence, c.dials), "{}: {v:?}", c.name);
            assert!(v.evidence.contains(c.evidence_has), "{}: `{}` lacks `{}`", c.name, v.evidence, c.evidence_has);
        }
    }

    #[test]
    fn human_bytes_rounds_sensibly() {
        assert_eq!(human_bytes(49_568), "49 KB");
        assert_eq!(human_bytes(7_996_376), "7.6 MB");
        assert_eq!(human_bytes(0), "0 KB");
    }
}
