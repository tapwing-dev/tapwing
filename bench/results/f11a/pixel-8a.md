# F11a on a physical device: Pixel 8a, Tapwing driver vs Maestro 2.12.1's

Device: Pixel 8a (`akita`), Android 16 (API 36), 1080×2400 @ 420 dpi, Gboard
(`com.google.android.inputmethod.latin`) as the default IME, developer options on,
en-US. A personal device, so the runs avoided anything that changes user
settings or accounts (see "Not run"). Tapwing driver APK SHA-256 `d4360201…5ea12`;
Maestro APKs from Maestro 2.12.1 (`maestro-client.jar`). Sample app: profile build
of `lib/main_tapwing.dart`. Date: 2026-11-22.

Raw hierarchy dumps are not committed: the launcher, notification shade and
Settings screens of a personal phone contain personal data.

## Hierarchy parity (11 screens)

`bench/f11a/run_parity.sh 4C101JEKB02816` (output redirected with `PARITY_OUT`).

| Screen | Tapwing vs Maestro | XML bytes | `viewHierarchy` p50 Maestro / Tapwing (ms) | Start Maestro / Tapwing (ms) |
|---|---|---|---|---|
| `launcher` (Pixel launcher) | identical | 33 625 | 241 / 163 | 3044 / 3283 (first start) |
| `settings` | identical up to window order | 49 983 | 424 / 309 | 2057 / 1140 |
| `settings_display` | identical | 50 235 | 336 / 253 | 1987 / 1038 |
| `chrome_page` | identical¹ | 20 501 | 153 / 87 | 2077 / 1020 |
| `chrome_web_input` | identical¹ | 20 501 | 195 / 101 | 2027 / 1035 |
| `notification_shade` | identical | 60 905 | 400 / 282 | 1957 / 906 |
| `quick_settings` | identical | 51 810 | 488 / 185 | 1830 / 947 |
| `sample_login` | identical | 20 960 | 175 / 97 | 2183 / 1018 |
| `sample_playground` | identical | 27 768 | 242 / 126 | 2052 / 933 |
| `sample_text_keyboard` (Gboard open) | identical up to window order | 103 251 | 560 / 367 | 2125 / 973 |
| `permission_dialog` | identical | 23 382 | 181 / 98 | 2121 / 1114 |

No screen differs. The two "up to window order" cases are UiAutomator's
`HashSet` of window roots (Maestro's own second dump varies the same way).
Tapwing's `viewHierarchy` is 1.4–2.6× faster at p50 and starts about 2× faster
on this device. Absolute numbers are higher than on the emulator because the
real device has larger hierarchies (Settings 50 KB, keyboard 103 KB).

¹ Chrome showed a "Sign in to managed account" dialog (the phone's Chrome is
enrolled in a work organization), so both Chrome screens are that dialog over a
"This site can't be reached" page, not the web page. The dialog was not
dismissed (it would sign in to an account), so Chrome's web-input parity and
`bench/f11a/flows/chrome_web_unicode.yaml` were **not** verified on this device.

## Unicode input and default IME

`native_hierarchy_diff type --serial 4C101JEKB02816 --text "Příliš žluťoučký kůň"`
into the sample app's text field (soft keyboard open):

| Driver | `inputText` | Field | Default IME |
|---|---|---|---|
| Maestro | 1095 ms | `Příliš žluťoučký kůň` | unchanged (Gboard) |
| Tapwing | 80 ms | `Příliš žluťoučký kůň` | unchanged (Gboard) |

## Mock location (physical device)

`native_hierarchy_diff location --serial 4C101JEKB02816` (50.0755, 14.4378):

- Both drivers register `gps`, `network` and `fused` test providers and push
  the location; `dumpsys location` shows `last location=Location[gps 50.075500,14.437800 … mock]`
  for all three providers afterwards. `setLocation` took 369 ms (Maestro) vs 243 ms (Tapwing).
- **Developer options → "Select mock location app" is not needed and is not
  touched.** `settings get secure mock_location` is `0` and no app is selected.
  Maestro's host-side path grants `android:mock_location` to `dev.mobile.maestro`
  with `appops set … allow`, and that grant is enough on API 36; Tapwing's driver
  grants it to itself, so the host does nothing. The only prerequisite is that
  developer options are enabled on the device.
- The providers stay registered as test providers after the lane ends (also with
  Maestro's driver) until something removes them, and the cached last location
  of `gps` and `fused` stays the mocked one until the next real fix. Cleanup
  used: `appops set 2000 android:mock_location allow`,
  `cmd location providers remove-test-provider gps|network|fused`, then
  `appops set 2000 android:mock_location default`.
- Play-services behaviour (fused location) was observed only through
  `dumpsys location`, not through an app consuming it.

## Differential flows

`tapwing test --device 4C101JEKB02816 --input <mode> --native-driver <driver> <flow>`:

| Flow | auto Tapwing / Maestro | native Tapwing / Maestro |
|---|---|---|
| `f5_permission_dialog` | PASS 5.0 s / PASS 5.3 s | PASS 7.2 s / PASS 9.4 s |
| `f11_native_unicode_input` | PASS 4.3 s / PASS 4.4 s | PASS 22.6 s / PASS 27.6 s |
| `login_and_browse` | PASS 8.2 s / PASS 8.2 s | PASS 40.3 s / PASS 47.5 s |
| `f2_text_clipboard_keys` | PASS 9.4 s / PASS 9.2 s | PASS 60.9 s / PASS 64.7 s |

16/16 pass, and the default IME was Gboard after every run. Native-mode `inputText`
in `f11_native_unicode_input`: Tapwing 49 / 45 / 933 / 36 / 905 ms vs Maestro
1206 / 880 / 975 / 964 / 900 ms. Tapwing's ~0.9 s steps are the pure-ASCII
`Tapwing` (typed key by key, 75 ms per character, as in Maestro) and the
password field `heslo-ř` (the fallback IME path).
`f5_permission_dialog` `tapOn` on the system dialog in `auto`: 2417 ms (Tapwing)
vs 1680 ms (Maestro), within run-to-run noise (the dialog animates in).

Cold install (`dump --fresh`, driver uninstalled first): ready in 2227 ms
(Tapwing) vs 2438 ms (Maestro).

## Not run

- `f4_device_commands`: it toggles dark mode and airplane mode, adds a media
  file, and opens a link in Chrome (which would hit the managed-account dialog),
  on a personal phone. It is host-side adb and does not exercise the native
  drivers; `setLocation` is covered above.
- Chrome web-input parity and `chrome_web_unicode.yaml` (see ¹).
- OEM IMEs other than Gboard, API levels other than 35/36, Compose apps, and
  real-project flows are still unverified.

## After the removal of Maestro's drivers

Same Pixel 8a, binary built from the removal commit, `dev.tapwing.driver` and every
`dev.mobile.maestro*` package uninstalled first (fresh driver install):

| Flow | auto | native |
|---|---|---|
| `f5_permission_dialog` | PASS 4.2 s (fresh install) | PASS 6.1 s |
| `f11_native_unicode_input` | PASS 3.6 s | PASS 21.3 s |
| `login_and_browse` | PASS 7.5 s | not run |

Default IME unchanged after every run, `dev.mobile.*` never reinstalled. The
`--native-driver maestro` flag prints a deprecation warning and runs with Tapwing's
driver. The unpacked release tarball from `scripts/dist.sh` (bundled APK, no
checkout build) also passes `f5_permission_dialog` in `native` after a fresh install.
