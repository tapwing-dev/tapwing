# Tapwing test-driver signing key

`tapwing-test-driver.p12` signs `dev.tapwing.driver`. It is **intentionally
public**: store password, key alias and key password are all
`tapwing-test-driver`, and they are written into `driver/build.gradle.kts`.

| | |
|---|---|
| Subject | `CN=Tapwing Test Driver - public key - not a secret, O=Tapwing` |
| Certificate SHA-256 | `A7:D4:5C:D4:2B:DD:8C:65:F3:6B:54:E4:C5:FE:87:3C:92:E3:5D:99:16:7C:02:DA:83:7F:34:70:F3:5A:D8:39` |
| Algorithm, validity | RSA 2048, SHA256withRSA, 2026-11-24 to 2126-10-31 |

## Why a committed key

Android only installs an update over an existing app when both are signed
with the same certificate. A per-machine debug key (`~/.android/debug.keystore`)
would make every developer's or CI machine's driver incompatible with the one
already on a shared device (`INSTALL_FAILED_UPDATE_INCOMPATIBLE`, which then
needs an uninstall). One stable key lets any Tapwing build replace any older
driver in place. It also makes the build reproducible: the same sources give
byte-identical APKs and SHA-256s on every machine (see
`scripts/build-android-driver.sh`). Maestro does the same with the
`debug.keystore` in its repository.

## Why it is not a secret

The signature proves nothing about who built the APK and is not meant to:

- The driver is a test tool. It is installed over `adb` by someone who already
  controls the device; `adb` can install, instrument or remove any app anyway.
- It declares no signature-level permissions and exports nothing a
  same-signature app could use: its only component other apps can bind is the
  input method, which (like any IME) needs `BIND_INPUT_METHOD`, held only by
  the system.
- The instrumentation targets its own package, so the key grants
  instrumentation access to nothing else.

Consequences: anyone can sign an APK that installs over `dev.tapwing.driver`.
That is acceptable for a test driver, and the reason this key must **never**
sign anything else (an app, a library, a release of another package). Tapwing
itself never trusts the signature; it checks the APK's SHA-256 against the
marker it writes on the device.
