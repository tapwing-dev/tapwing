---
name: rust-systems-reliability
description: >-
  Enforces speed, zero-flake stability, protocol safety, subprocess hygiene, and
  deterministic testing in Rust device/network code (`adb_client`, `device/`,
  `native_lane/`, `ios_native_lane/`, `driver.rs`). Use when writing or
  reviewing I/O, TCP/HTTP/gRPC clients, subprocess wrappers, background threads,
  or RAII guards.
---

# Rust Systems Reliability, Speed & Zero-Flake Engineering

Speed and stability are Tapwing's primary goals. Apply these rules to every I/O, process, and concurrency boundary.

## 1. Hot-Path Latency Rules

- **No subprocess spawns on hot paths:** never call `Command::new("adb")` for shell/exec/reverse/push/pull/emu when `AdbClient` loopback TCP (`~0.1 ms` vs `~50–75 ms`) can serve the call. Keep `Command::new` only for streaming long-lived children (`screenrecord`, `am instrument`, `xcodebuild`) and one-time `adb start-server` / `adb install`.
- **Reuse HTTP/gRPC transports:** keep persistent `ureq::Agent` (HTTP/1.1 keep-alive) and tonic `Channel` per session; never build a new client or Tokio runtime per request.
- **No busy-loops:** every poll/settle loop (`settle`, `poll_find`, `waitForAnimationToEnd`) must sleep a bounded interval (`POLL_INTERVAL`, e.g. 50 ms) clamped to the remaining deadline, and return immediately on `timeout == 0` where semantics allow.
- **Stream large payloads:** never buffer multi-MB files (screen recordings, screenshots, APK pulls) into an unbounded `Vec<u8>` when writing directly to a `BufWriter<File>` is possible.

## 2. Wire Protocol & Memory Safety

- **Bound every wire-length allocation:** before `vec![0u8; len]` or `buf.resize(...)` from network input (`shell,v2`, `sync: RECV`, length-prefixed frames), reject lengths exceeding an explicit `MAX_*_PACKET` constant to prevent OOM aborts on corrupted streams.
- **Require explicit completion markers:** on framed protocols like ADB `shell,v2`, track `exit_code: Option<i32>` and return an error on EOF without an `EXIT` packet — never default to `0` on premature disconnect.
- **Scope fallbacks narrowly:** fall back (e.g. `shell,v2` → legacy `shell:`) only on explicit protocol rejection (`FAIL`), never on transport disconnects or timeouts.
- **Recover server errors on `BrokenPipe`:** when streaming upload chunks (`sync: SEND`), if a write hits `BrokenPipe`/`ConnectionReset`, try reading a pending `FAIL` status from the socket before returning the raw I/O error.

## 3. Subprocess, Shell & `Drop` Safety

- **Prevent SIGPIPE on long-running children:** never `take()` and drop `child.stdout` or `child.stderr` while the child is still writing. Redirect device-side output to a temp log file or drain the pipe explicitly, and bound initial `read_line` handshakes with a timeout.
- **Non-blocking, cancellable background warm-up:**
  - Check `cancel: &AtomicBool` between every step (install, force-stop, forward, spawn, readiness probe).
  - Use short per-attempt readiness timeouts (e.g. `<= 500 ms`), never a 30 s default RPC timeout inside a startup loop, so `Drop` joins quickly and pure-Flutter flows never stall on exit.
  - Clean up **all** device-side processes in `Drop` (e.g. `am force-stop dev.tapwing.driver` on Android, or `xctrunner` on iOS).
- **Shell injection & exit-code hygiene:**
  - Validate package/bundle IDs before shell interpolation (`validate_package_name`).
  - Single-quote (`sh_quote`) every dynamic argument (`openLink`, `addMedia`, `launchArguments`).
  - Under `shell,v2`, non-zero exit codes fail immediately: append `|| true` to optional `grep` probes (e.g. `KEYGUARD`), and inspect sentinel markers (`TAPWING_ERR:`) before bailing on `!out.success()`.

## 4. Deterministic Tests

- **No ephemeral port TOCTOU races:** never bind `127.0.0.1:0`, drop the `TcpListener`, and rebind the port in a background thread. Bind once and pass the live `TcpListener` (e.g. `tonic::transport::server::TcpIncoming::from(listener)`) directly to the server, sending the bound port back via a channel.
- **Idle `Drop` must be side-effect free:** dropping an unused handle/device in tests must never spawn processes or touch the network.
- **Verify every new protocol/error path with mock loopback servers** (`TcpListener`), including premature EOF, `FAIL` packets, timeouts, and cancellation.

## 5. Review Checklist Command

```sh
cd crates/tapwing && cargo test --lib && cargo clippy --all-targets -- -D warnings
```
