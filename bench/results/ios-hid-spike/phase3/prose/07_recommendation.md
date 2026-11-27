Recommendation: keep it opt-in. Do not make it the default.

Reasons.

- The one large win is real and measured. `f5_cs_lazy` auto goes from 11079 ms to 6147 ms (5/5 passed on both sides, 0 of 5 runs spawned `xcodebuild`), and 20 of 20 further runs passed with exactly one delivered touch each. It is a runner-less alert path, and it is what the issue asked for.
- Outside that case the gain is small or zero. `f5_cs` auto gains 310 ms (3 percent). In `--input native` the Flutter taps gain 400 to 830 ms each, but the alert tap does not, and that mode is rare. The flag adds about 25 ms per focus probe when the runner is up (finding 1).
- The delivery risk from the research did not show up in 380 taps, but my test covered one device, one runtime, one gesture and warm boots, and the shim cannot detect a lost input. The default needs more than that.
- The review has real defects that make a default unsafe. Finding 2 (a miss on the HID tree is final, so a flow that passes today can fail), finding 4 (shared accessibility state when shards run at once), and finding 5 (waits of 6 to 18 s) each need a fix and a test before the flag becomes the default.
- It also relies on private Apple API that idb says is on the way out for buttons and keyboard (Q4), and the shim now compiles on Intel where nobody tested it.

Before widening it, I would fix findings 1, 2 and 5, gate the shim out of `x86_64-apple-darwin`, ship `THIRD_PARTY_NOTICES.md` in the tarball, and repeat the reliability test on a busy host and with swipes.

Not verified. Home screen, Control Center, lock screen and app switcher were not exercised. Rotation and landscape were not exercised beyond `f5_cs`. Intel and non-macOS builds were not compiled. The flag-on path under `--shard` was not run. Xcode 26 behaviour is unknown.
