# F11a results: Tapwing Android driver vs Maestro 2.12.1's

Setup: AVD `tapwing-f11a` (`emulator-5620`), Pixel 8 profile 1080×2400 @ 420 dpi,
Android 15 (API 35, `google_apis` arm64, userdebug), `-gpu host`, Mac mini host.
Sample app: profile build of `lib/main_tapwing.dart`. Tapwing driver APK
SHA-256 `d4360201…5ea12` (drivers/android at the F11a commits); Maestro APKs
from Maestro 2.12.1 (`maestro-client.jar`, pinned SHA-256). Tools:
[bench/f11a](../../f11a/README.md). Date: 2026-11-21.

## Hierarchy parity (11 screens)

`bench/f11a/run_parity.sh emulator-5620`. Each screen is dumped through
Maestro's driver, Tapwing's, then Maestro's again, 5 `viewHierarchy` calls each.

| Screen | Tapwing vs Maestro | XML bytes | `viewHierarchy` p50 Maestro / Tapwing (ms) | Warm start Maestro / Tapwing (ms) |
|---|---|---|---|---|
| `launcher` (Pixel launcher + status bar) | identical | 28 574 | 57 / 4 | 559 / 134 |
| `settings` (Settings home; IME window open) | identical | 102 268 | 77 / 25 | 516 / 141 |
| `settings_display` | identical¹ | 38 557 | 58 / 13 | 554 / 144 |
| `chrome_page` (example.com) | identical | 31 365 | 61 / 10 | 520 / 147 |
| `chrome_web_input` (web `<input>` + native URL bar) | identical | 27 980 | 52 / 9 | 515 / 139 |
| `notification_shade` | identical | 40 206 | 59 / 10 | 521 / 125 |
| `quick_settings` | identical | 53 726 | 717² / 23 | 522 / 142 |
| `sample_login` (Flutter) | identical | 19 578 | 53 / 5 | 521 / 149 |
| `sample_playground` (Flutter) | identical | 26 386 | 54 / 10 | 506 / 146 |
| `sample_text_keyboard` (Flutter field + Gboard window) | identical up to window order³ | 102 255 | 80 / 27 | 513 / 135 |
| `permission_dialog` (permission controller over the app) | identical | 21 999 | 59 / 8 | 553 / 151 |
| Earlier run (SwiftShader GPU): `chrome_first_run` | identical | | | |

"Identical" = byte-identical XML. ¹ Equal to the second Maestro dump; the
first differed in the status bar's mobile-signal node (the screen changed).
² Maestro's first dumps there waited out `waitForIdle(500)` while the panel
still animated. ³ The two windows (app, keyboard) came in the other order.
`UiDevice.getWindowRoots` collects the roots in a `HashSet`; Maestro's own
consecutive dumps of an unchanged launcher showed both orders, and Tapwing
calls the same UiAutomator 2.3.0 method. The first `viewHierarchy` of a
session takes ~400–500 ms with both drivers (Maestro's `waitForIdle(500)`
before every dump, kept for parity).

**Accepted difference: none in the XML.** The one deliberate behavioural
difference is Unicode input (below).

## Differential flows

Every Android flow in `examples/sample_app/.maestro` (top level, 11 flows),
`tapwing test --native-driver {tapwing,maestro} --input {auto,native}`.
Logs and JSON reports: `differential/suite-<driver>-<input>.{log,json}`.

| Flow | auto: tapwing / maestro | native: tapwing / maestro |
|---|---|---|
| F11 Native Unicode Input (new) | ✓ / ✓ | ✓ / ✓ |
| F2 Endless Animation | ✓ / ✓ | ✓ / ✓ |
| F2 Text Clipboard and Keys | ✓ / ✓ | ✓ / ✓ |
| F4 Android device commands | ✓ / ✓ | ✓ / ✓ |
| F5 Permission Dialog | ✓ / ✓ | ✓ / ✓ |
| F7 App Health | ✓ / ✓ | ✓ / ✓ |
| Login and browse | ✓ / ✓ | ✓ / ✓ |
| F2 Selectors and Taps | ✓ / ✓ | ✗ / ✗ (`"Traits Ready"`, see below) |
| F2 Swipe and Scroll | ✓ / ✓ | ✗ / ✗ (`scrollUntilVisible "Snap Page 3"`, 3/3 each) |
| F2 Dark Mode | ✗ / ✗ | ✗ / ✗ |
| F2 Screenshots and Light Mode | ✗ / ✗ | ✗ / ✗ |

Same outcome for both drivers in every cell. Default IME unchanged after
every run. The shared failures are not driver-related: Dark Mode also fails
in Maestro 2.12.1 itself on this AVD (`cmd uimode night` not reflected), the
screenshot references are 560 dpi images (700×308) vs 525×231 here, and the
two native-only failures are Tapwing native-lane gaps that exist with
Maestro's driver too (Selectors and Taps passes in Maestro 2.12.1; native
swipes go through `adb shell input swipe`, not the driver).

Fresh install (no Tapwing driver on the device, release tarball from
`scripts/dist.sh`, bundled APK): `f5_permission_dialog` + `f11_native_unicode_input`
pass in `auto` and `native`.

Extra flow `bench/f11a/flows/chrome_web_unicode.yaml` (Czech text into a web
`<input>` in Chrome): **Maestro 2.12.1 fails** (its IME commit does not reach
Chrome's field), Tapwing with Maestro's driver fails the same way, Tapwing's
driver passes 8/8 (`auto` 4/4, `native` 4/4) through `ACTION_SET_TEXT`. This
is the accepted difference: Tapwing types the text Maestro means to type.

## Timings

| | Maestro driver | Tapwing driver |
|---|---|---|
| Install + start (fresh device, 3 runs) | 616 / 653 / 705 ms | 425 / 404 / 428 ms |
| Start, already installed (median of 11 screens) | 521 ms | 142 ms |
| `viewHierarchy` p50 (median of 11 screens; settings screen, 20 calls) | 59 ms; 62 ms | 10 ms; 10 ms |
| Unicode `inputText` "Příliš žluťoučký kůň", Flutter field (3 runs) | 454 / 441 / 461 ms | 4 / 7 / 4 ms |
| Same, native `EditText` (Settings search, 2 runs) | 451 / 466 ms | 11 / 11 ms |
| `setLocation` through mock providers (first call incl. enabling) | 36 ms | 11 ms |
| APK size | 11.7 + 0.9 MB | 4.0 MB |

Unicode timings come from `native_hierarchy_diff type`; both drivers left the
same text in the field and the default IME unchanged. Maestro's path is
`ime enable/set` + status poll + commit broadcast + 250 ms settle + IME
restore; Tapwing's is one RPC. Tapwing's IME fallback (password field) takes
~400 ms, about the same as Maestro's path. `setLocation`: gps, network and
fused providers all report the mock location for both drivers.

## Environment notes

- With `-gpu swiftshader_indirect` this AVD's adb transport dropped 4 times
  (adbd `connection terminated: write failed`, emulator log full of
  SwiftShader `bad color buffer handle`), always while a soft keyboard
  appeared, with either driver. With `-gpu host` there were no drops. After a
  drop the native lane does not reconnect (pre-existing, both drivers).
- The open-source-licenses WebView (`android.settings.LICENSE`) was dropped
  from the screen list: one dump exceeds the 30 s RPC deadline with either
  driver.
