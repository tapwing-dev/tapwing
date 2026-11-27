
### Why `assertVisible "Permission: granted.*"` took 275 ms in the smoke run

Measured. The smoke log has one `hid route=snapshot` event of 246 ms right before that step. `ios_focus_probe` in `run/hybrid.rs` (line 544) asks `hid_focus()` first, on every focus probe, even when the runner is already running. That is one accessibility snapshot of depth 1 added to the step. In the 6 flag-on `f5_cs` auto runs of this benchmark (warm-up plus 5) the snapshot took 23 to 27 ms in that step and the step took 50 to 53 ms against 27 ms off. Over all flag-on benchmark runs (excluding warm-ups) the successful snapshot events range from 3 to 42 ms. The 246 ms read was a one-off and is not reproducible: 0 of 6 runs at that position, 0 of 45 successful snapshot events in the flag-on benchmark runs above 42 ms.

So the steady cost is about 25 ms per focus probe while the runner is up. It buys nothing there, because the runner already answers the same question in 1 ms (`runningApp`). In `f5_cs` native the same step is 22 ms off and 98 ms on, from an extra `viewHierarchy` call in one run, which is run noise (min 21 ms, one outlier of 101 ms).
