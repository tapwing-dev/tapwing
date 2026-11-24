# Parser Benchmark: Tapwing vs Maestro

YAML-to-IR parsing only, no device involved. Measured 2026-11-20 on an Apple M1 Pro, Maestro 2.12.1, OpenJDK 17. Background work was running concurrently (system load ~7–9), so absolute timings are slightly higher than on an idle machine, while relative ratios hold.

Run command: `bench/parse/run_parse_bench.sh [iterations] [warmup]`

Both parsers received the same 463 files from `compat/maestro-fixtures`. Both parse 399 files and reject the exact same 64 files. Timings include reading files from disk and resolving nested subflows via `runFlow`.

| | Maestro | Tapwing | Speedup |
|---|---:|---:|---:|
| Cold process, single flow (`login_and_browse.yaml`) | 805–868 ms | 3.5–4.0 ms (~2–3 ms process spawn) | **~220×** |
| First pass over 463 files in a new process | 690–800 ms | 16–45 ms | **~20–40×** |
| Warm, 463 files per pass | 36–40 ms | 14.5 ms | ~2.6× |
| Warm, median per file | 50–56 µs | 23 µs | ~2.3× |
| Warm, p99 per file | 390–420 µs | 121 µs | ~3.4× |

Ranges cover 4 in-process runs and 2 cold-process runs.

## Takeaways

- **A CLI parses once per invocation, so the first row is what matters.** Maestro spends almost the entire second starting the JVM and loading Jackson/Kotlin classes; actual parsing is a tiny fraction.
- **Once warmed up, Jackson is fast.** Tapwing's lead narrows to 2–3×. Most of the real-world speedup comes from eliminating the JVM rather than algorithmic tricks.
- **In Tapwing, parsing is negligible.** A single flow takes ~1 ms out of a ~6.6 s run (see [F0_RESULTS.md](F0_RESULTS.md)).
