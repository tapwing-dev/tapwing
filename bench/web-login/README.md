# Browser-login benchmark

Measures the part of a flow that every real app starts with: a login in a system browser window. The sample app's "Browser login" screen opens a login page in a Chrome Custom Tab (Android) or an `ASWebAuthenticationSession` (iOS), the page redirects to `tapwingsample://callback`, and the app shows `Signed in as <email>`. Everything in between is outside Flutter and runs through the native lane.

Recorded results: [bench/results/browser-login/summary.md](../results/browser-login/summary.md).

## Pieces

| What | Where |
|---|---|
| Login pages and server (Python 3 standard library, binds `127.0.0.1` only) | `server.py`, `pages/` |
| App screen | `BrowserLoginScreen` in `examples/sample_app/lib/main.dart` |
| Flows, Android | `examples/sample_app/.maestro/web/web_login_{plain,two_step,controlled}.yaml` |
| Flows, iOS Simulator | `examples/sample_app/.maestro/ios/web/web_login_{plain,two_step,controlled}.yaml` |
| Runner | `run_bench.sh` |
| Per-step medians | `summarize.py` |

The flows are plain Maestro syntax and select everything in the browser by visible text. They sit in sub-folders, so `tapwing test .maestro` and `maestro test .maestro` (top-level flows only) do not run them without a server, while `tapwing check examples/sample_app/.maestro` validates them.

## Page variants

| Path | Page |
|---|---|
| `/plain/login` | Email and password inputs with labels and a submit button on one page |
| `/two-step/login` | Email first, then `/two-step/password` with the password, like Auth0 and most identity providers |
| `/controlled/login` | The visible inputs have no `name`. JavaScript copies their value into its own state on each `input` event, writes that state back into the inputs, and posts the state. The submit button is disabled until the state holds an email and at least 12 password characters. The line `Events: keydown N, input N, change N` counts the events the two inputs received |

The `controlled` variant exists so that a faster typing method can be shown to reach forms a framework controls. A method that writes the value without firing `input` leaves the button disabled, and the login fails.

Every variant takes `redirect_uri` and `state` in the query and answers a correct login with `302` to `<redirect_uri>?email=...&state=...&variant=...`. The server refuses any `redirect_uri` outside the `tapwingsample` scheme. The one account is `qa.pilot@tapwing.dev` with the password `Tw!ng-Pass2026` (20 and 14 characters). A wrong login shows `Wrong email or password.` and the server logs what it received, so a dropped character is visible in `server.log`. For the `controlled` variant it also logs the event counts the page posted.

## Running

```sh
# Once: the driver build of the sample app (see CONTRIBUTING.md) and the CLI.
cd crates/tapwing && cargo build --release && cd ../..

bench/web-login/run_bench.sh android <adb-serial>          # 1 warm-up + 5 runs per variant
bench/web-login/run_bench.sh ios <simulator-udid> 5
RUNNERS="tapwing maestro" bench/web-login/run_bench.sh android <adb-serial>   # interleaved
```

The runner starts the server on port 8765, sets up `adb reverse tcp:8765 tcp:8765` on Android (the iOS Simulator shares the Mac's loopback), runs each flow with `--input auto --report`, and prints the tables of `summarize.py`. The app reads the server address from `--dart-define=SAMPLE_WEB_LOGIN_URL`, default `http://127.0.0.1:8765`.

To try a page by hand, start `python3 -I bench/web-login/server.py` and open `http://127.0.0.1:8765/`.

## Device state the flows expect

- **Chrome first run (Android).** On a fresh emulator Chrome shows its welcome and sign-in screens inside the Custom Tab before the page. The flows do not dismiss them. Open Chrome once and finish the first run, or disable it the way Maestro's own device setup does (`compat/maestro-fixtures/e2e/demo_app/android_device_configuration/disable_chrome_first_run.yaml`). The emulator used for the recorded baseline already had Chrome set up, so neither path was exercised here.
- **Consent alert (iOS).** `ASWebAuthenticationSession` first shows a SpringBoard alert, `"sample_app" Wants to Use "127.0.0.1" to Sign In`. The iOS flows tap `Continue`. The label is localised, so the flows need an English simulator.
- **`com.apple.SafariViewService` (iOS).** The sign-in sheet's web content lives in that process. On the iOS 27.2 Simulator its accessibility server stopped answering XCTest after about four sign-in sheets (`kAXErrorServerNotFound` in the runner log), and from then on neither Tapwing nor Maestro saw the page. The runner therefore terminates the process before each run. Set `IOS_KEEP_SAFARI_VIEW_SERVICE=1` to leave it alone.
- **One UiAutomation client (Android).** Android gives the UiAutomation connection to one instrumentation at a time. Another tool that keeps one open (an accessibility-snapshot helper of a device-control tool, for example) makes Tapwing's native lane fail with `the driver did not start in time`. Close that tool's session before a run.
- **Maestro and an `unauthorized` device.** Maestro 2.12.1 lists no Android device while any attached device is `unauthorized`. `MAESTRO_DEVICE_ARGS="--host localhost --port <console port + 1>"` makes the runner connect Maestro to one emulator directly.
