### Why the alert `tapOn` is not faster in `--input native`

Measured. In `--input native` the `touch` call drops from about 335 ms to about 12 to 15 ms, and the step is still 878 ms (`f5_cs`) or 1012 ms (`f5_cs_lazy`) against 900 and 915 ms with the flag off. The step is bounded by two animations, not by call cost.

The frame events come from the settle loop of `native_core/ops.rs` (`perform_tap`, which calls `surface.settle` before and after the tap) running on the runner-backed surface in `ios_native_lane/ops.rs` (`settle`, loop at line 134). With the flag on, `IosNativeClient::is_screen_static` answers from `HidSession::screen_static`, which takes two framebuffer frames of about 25 to 30 ms each. A poll is two frames plus the 50 ms `STATIC_POLL` sleep, so about 110 ms. Each pair of `frame` events in the log is one poll. The runner path does the same loop with one `isScreenStatic` HTTP call per poll, 120 to 160 ms each, and the call has no extra sleep.

The two animations are the alert appearing and the alert closing.

- Before the touch, the runner path needs one poll (180 ms) and the HID path needs four polls (456 ms, 8 frame events). With the flag on the earlier steps finish sooner, so the alert is still animating when the step starts. The touch can only go out once two frames match. The time from the Request touch to the alert touch is 578 ms with HID and 1087 ms with the runner (table below), so HID taps within roughly one poll (110 ms) of the earliest possible moment. There is nothing left to save before the touch.
- After the touch, the runner needs the 335 ms touch call (which blocks while the alert closes) plus two polls, 717 ms in total. HID needs 425 ms (`f5_cs`) or 569 ms (`f5_cs_lazy`), 4 to 5 polls. The closing animation and the Flutter rebuild bound this.

The saving shows up in the neighbouring steps, because those steps lose the runner call overhead. `f5_cs_lazy` native, medians off to on: `tapOn playground_button` 1068 to 241 ms, `tapOn "Request Location Permission"` 1062 to 632 ms, alert `tapOn` 915 to 1012 ms.

Net gain of the client routing (point (a)), measured and per flow (5 passed runs per cell, 6 for `f5_cs_lazy` native):

- `f5_cs` auto: 9889 to 9580 ms, 310 ms or 3.1 percent. The alert `tapOn` gains 200 ms (932 to 733). The alert `assertVisible` gains 37 ms. `assertVisible "Permission: granted.*"` loses 23 ms (see below). All other steps are unchanged because in `auto` Flutter taps do not use the runner.
- `f5_cs` native: 8234 to 6954 ms, 1279 ms or 15.5 percent. Gains come from Flutter element taps (830 ms and 411 ms), `setOrientation` (83 ms). The alert `tapOn` gains 22 ms. `assertVisible "Permission: granted.*"` loses 76 ms.
- `f5_cs_lazy` native: 6441 to 5165 ms, 1276 ms or 19.8 percent. The alert `tapOn` loses 97 ms.
- Per step: a Flutter tap gains 400 to 830 ms, the alert tap gains nothing in native mode, a `setOrientation` gains at most 140 ms.

Inferred. `--input native` is not the default, so the routing only helps users who ask for it or flows that already warm the runner (any flow with `setOrientation`). In `auto` it does little, and it adds one AX read per focus probe (below).

Timeline of the permission block in `--input native` (event clock):

