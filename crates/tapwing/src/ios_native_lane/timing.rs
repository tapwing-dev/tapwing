//! Temporary spike instrumentation (issue #30): one stderr line per event when
//! `TAPWING_IOS_NATIVE_TIMING=1`, silent otherwise.
//!
//! Format: `ios-native-timing event=<name> t_ms=<ms since process start> ms=<duration> key=value...`

use std::sync::OnceLock;
use std::time::{Duration, Instant};

const ENV: &str = "TAPWING_IOS_NATIVE_TIMING";

static PROCESS_START: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Marks process start. Call first thing in `main`.
pub fn init() {
    PROCESS_START.get_or_init(Instant::now);
}

fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var(ENV).is_ok_and(|v| v == "1"))
}

/// Prints one event line. `extra` is a preformatted `key=value` list (may be empty).
pub(crate) fn emit(event: &str, took: Duration, extra: &str) {
    if !enabled() {
        return;
    }
    let t = PROCESS_START.get_or_init(Instant::now).elapsed().as_millis();
    eprintln!("ios-native-timing event={event} t_ms={t} ms={} {extra}", took.as_millis());
}
