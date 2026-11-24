## Unreleased

* Settle no longer waits without limit for a single HTTP request or pending image: each counts as busy for at most 5 s (`configure` `settleLoadTimeoutMs`, `0` = no bound), and requests whose URL matches `configure` `settleHttpIgnore` never count. Both stay in the HTTP request log.

## 0.1.1

* The driver stays off in release builds unless the app is built with `--dart-define=TAPWING_ALLOW_RELEASE=true`. Profile and debug builds are unchanged.
* HTTP request log: URLs inside header values (`Location`, `Referer`, `Content-Location`, `Link`, `Refresh`) are redacted like the request URL.
* The connection to the host rejects request lines longer than 4 MiB, and an app left running with no host for a minute dials once a second instead of every 150 ms.

## 0.1.0

* Initial release of the in-app Flutter driver, version-locked with the `tapwing` CLI (`v0.1.0`).
* Semantics tree snapshot, Maestro 2.12.1 selector engine, reactive waits, engine-boundary input injection, F7 app health diagnostics, and Android + iOS Simulator support.
