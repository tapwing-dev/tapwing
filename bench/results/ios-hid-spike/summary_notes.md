## Findings

Measured unless marked inferred.

1. The 3.0 s step of issue #30 is still there on this branch. In `auto` mode `tapOn "Request Location Permission"` takes 3010 to 3018 ms in all 10 measured `auto` runs. The step report shows `settle: "timeout"` with `settleUs` about 3,003,000 and `actUs` about 9,000. The tap itself is fast and the Dart driver waits out its full 3 s settle budget. PR #29 (settle exit when the app pauses) did not remove this stall in the iOS 27 simulator with Flutter 3.50.0. In `--input native` the same step takes about 1.04 s, because the Dart driver is not involved. This stall is not runner time.
2. The runner starts in 2.6 to 2.8 s (`xcodebuild` spawn until `/status` answers with our session id) in every configuration, plus about 0.1 s before the spawn (locate driver, orphan sweep, port).
3. With a static native command (`f5_cs`, `setOrientation` at step 1) the warm-up runs from flow start. `launchApp` hides about 2.3 s of it. `setOrientation` blocks in `get_or_wait` for about 0.5 s. Total startup cost paid by the flow is about 0.5 s.
4. Without a static native command (`f5_cs_lazy`, `auto`) the runner is started when the alert step first misses in the Flutter tree. That step (`assertVisible` of the alert label) takes about 4.96 s. About 2.0 s are the lazy Flutter probe before the spawn (`IOS_LAZY_PROBE_MS`), about 2.7 s are the runner start, and only about 0.2 s are runner calls and other work. `get_or_wait` blocks for 0 ms here, because the step waits inside `wait_on_flutter_while_warming`, not in `get_or_wait`. The startup paid in this step is derived from the event clock (see below).
5. In `--input native` the whole flow goes through the runner, including `launchApp`. The runner start (about 2.0 s blocked in `launchApp`) is the only startup cost. After that, the three alert steps cost about 0.96 s in total.
6. Per alert step once the runner is up (`auto` and `native` agree):
   - `assertVisible` label is 1 to 3 `viewHierarchy` calls of 20 to 65 ms each (median 23 ms over all runs) (plus one `deviceInfo` call, median 41 ms over all runs, on the first native call). In `auto` the step also spends about 413 ms before the first runner call (inferred: `FLUTTER_PROBE_MS` is 400, the Flutter lookup that misses first).
   - `tapOn` label is about 0.9 to 1.0 s: one `touch` of 328 to 349 ms (one run 445 ms), three `isScreenStatic` calls of about 115 to 170 ms each (median 125 ms over all runs) (about 410 ms, inferred to be the post-tap settle wait), and 3 to 4 `viewHierarchy` calls of 21 to 40 ms. About 60 ms is host time between calls.
   - `assertNotVisible` label is one `viewHierarchy` (about 22 ms) plus one `runningApp` call in `auto`.
7. On the alert steps the largest runner cost is `touch` plus `isScreenStatic`, not `viewHierarchy`. `viewHierarchy` is 5 to 14% of the alert steps once the runner is up.

## Method and caveats

- Event lines come from `TAPWING_IOS_NATIVE_TIMING=1` (commit "add TAPWING_IOS_NATIVE_TIMING event lines"). Events are printed before the `✓` line of the step they belong to, which is how calls are assigned to steps. Failed startup `/status` probes are not logged.
- "Startup paid" is measured when it is time blocked in `get_or_wait` (eager warm-up). For the on-demand start it is derived: `t_ms` of the `runner_start` event minus the start of `start_session` (`total_ms`), for the step in which the spawn happened. Step start on the event clock is derived as the `t_ms` of the step's last runner event minus the step duration from the report, so it can be off by a few ms.
- "rest" is the step time that no runner event covers. It contains the Flutter lookup, the Dart settle and sleeps. It is not measured separately.
- Each configuration is 1 discarded warm-up run (`warmup.*`) and 5 measured runs (`run-1` to `run-5`), run one after the other on the same simulator. Wall time includes process start and exit.
- Reports have the home directory rewritten to `/Users/me`.
