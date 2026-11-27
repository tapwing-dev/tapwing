# Install

Each Tapwing release tarball bundles the `tapwing` binary together with its native drivers (Android UiAutomator on all platforms, plus the iOS Simulator XCUITest runner on macOS). You do not need Maestro installed.

Supported targets:

- `aarch64-apple-darwin` (Apple Silicon macOS)
- `x86_64-apple-darwin` (Intel macOS)
- `x86_64-unknown-linux-gnu` (Linux x86_64, Android driver only)

## Homebrew

```sh
brew install tapwing-dev/tap/tapwing   # install
brew upgrade tapwing                   # upgrade to the latest release
tapwing --version
```

The formula installs the same release tarball as `install.sh` (into Homebrew's Cellar), so the bundled drivers are found through Homebrew's symlinks. `brew test tapwing` checks that the binary reports them as bundled.

## Install script

```sh
# Latest GitHub Release for this OS/arch, SHA-256 verified (works with bash or sh)
curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash

# A specific version (shorthand)
curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash -s 0.1.0

# Or pass flags explicitly (everything after `--` goes to install.sh)
curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | sh -s -- --version 0.1.0 --no-modify-path

# From a checkout: latest release, a pinned version, or a local tarball (or https:// URL)
scripts/install.sh --version latest
scripts/install.sh 0.1.0
scripts/install.sh dist/tapwing-0.1.0-aarch64-apple-darwin.tar.gz

# Verify the binary and bundled drivers
tapwing --version
```

Requires only `curl` (or `wget`), `tar`, and `sha256sum` (or `shasum`). By default, piping into `bash` or `sh` installs the latest GitHub Release via `/releases/latest` redirect (no API rate limits), detects Apple Silicon even from a Rosetta shell, always verifies the `.sha256` checksum, and adds `~/.local/bin` to your shell config (`~/.zshrc`, `~/.bashrc`, `~/.bash_profile`, `config.fish`, or `~/.profile`) if it is not already on `PATH`. Run the same command again to upgrade.

Example output on macOS:

```
tapwing 0.1.0
tapwing android driver: 0.1.0 (bundled) /Users/me/.tapwing/versions/0.1.0/share/tapwing/drivers/android/tapwing-driver.apk
tapwing ios driver: 0.1.0 (bundled) /Users/me/.tapwing/versions/0.1.0/share/tapwing/drivers/ios
```

On Linux, `tapwing ios driver` reports `n/a (the iOS Simulator needs macOS)`.

## Build from source

You need:

- Rust (stable) and `protoc` for the CLI
- JDK 17+ and Android SDK platform 37 for the Android driver
- Xcode for the iOS Simulator driver (macOS only)

```sh
git clone https://github.com/tapwing-dev/tapwing.git && cd tapwing
scripts/dist.sh                                         # builds dist/tapwing-<version>-<target>.tar.gz
scripts/install.sh dist/tapwing-0.1.0-<target>.tar.gz   # installs into ~/.tapwing and ~/.local/bin/tapwing
```

`scripts/dist.sh` builds `--release` for every installed supported Rust target (or a specific one via `--target <triple>` and `--out-dir <dir>`), builds the native drivers, verifies their SHA-256 checksums, and smoke-tests `tapwing --version`. On Apple Silicon, you can also build `x86_64-apple-darwin` after `rustup target add x86_64-apple-darwin`. Cross-compiling Linux from macOS requires a C cross-linker such as `cargo-zigbuild`.

To verify the tarball layout, checksums, and installer offline, run `scripts/test-dist.sh`.

## Installer layout and options

By default, `install.sh` unpacks into `~/.tapwing/versions/<version>`, updates `~/.tapwing/current`, symlinks `~/.local/bin/tapwing` to it, cleans up older versions, runs `tapwing --version`, and adds `~/.local/bin` to your shell config if it is not already on `PATH` (removed cleanly by `--uninstall`). Re-running it upgrades or reinstalls cleanly.

Every tarball is verified against `--sha256` or a sibling `.sha256` file (`--version`, including `--version latest` and positional versions, refuses to install if the checksum is missing or doesn't match).

| Flag | Purpose |
|---|---|
| `[SOURCE\|VERSION]` | Local `.tar.gz`, `https://` URL, unpacked directory, or version shorthand (`latest`, `0.1.0`, `v0.1.0`). Default: the unpacked release containing `install.sh`, otherwise the latest GitHub Release |
| `--version <VER>\|latest` | Download and verify a GitHub Release (`latest` resolves the newest one; env: `$TAPWING_VERSION`) |
| `--repo <OWNER/REPO>` | GitHub repository for `--version` (default: `$TAPWING_REPO` or `tapwing-dev/tapwing`) |
| `--target <TRIPLE>` | Override detected host target |
| `--prefix <DIR>` | Install root (default: `$TAPWING_PREFIX` or `~/.tapwing`) |
| `--bin-dir <DIR>` | Symlink directory for `tapwing` (default: `$TAPWING_BIN_DIR` or `~/.local/bin`) |
| `--sha256 <HEX>` | Expected tarball SHA-256 checksum |
| `--no-modify-path` | Do not edit shell rc files to add `--bin-dir` to `PATH` (env: `TAPWING_NO_MODIFY_PATH=1`) |
| `--print-target` | Print the detected target triple and exit |
| `--uninstall` | Remove the installation and its shell rc block (leaves driver caches in `~/.tapwing/` intact) |

## How drivers are found

For each platform, `tapwing` looks for its native driver in this order:

1. Environment variable override (`TAPWING_DRIVER_APK` for Android, `TAPWING_IOS_DRIVER_DIR` for iOS).
2. Bundled drivers next to the binary (`share/tapwing/drivers/{android,ios}/`, resolving symlinks so Homebrew-style installs work). Bundled drivers are checked against their `.sha256` file; a mismatch fails instead of silently falling back.
3. Local build output in a source checkout (`scripts/build-android-driver.sh` / `scripts/build-ios-driver.sh`).

Set `TAPWING_DEBUG=1` to log which driver source Tapwing picks.

## Releasing

`main` only takes pull requests, so a release is cut in two steps from an up-to-date, clean `main`:

```sh
scripts/release.sh prepare 0.2.0 --dry-run   # checks, then prints the version diff and the commands it would run
scripts/release.sh prepare 0.2.0             # bump on release/v0.2.0, verify, push, open the pull request
# merge the pull request once CI passes, then:
git switch main && git pull
scripts/release.sh tag 0.2.0                 # check CI Gate on that commit, tag v0.2.0, push the tag
```

`prepare` bumps the version in `Cargo.toml`, both `Cargo.lock` files (`crates/tapwing` and `bench/parse`), `packages/tapwing_driver/pubspec.yaml` and the `ref:` of the driver README's install snippet, turns the `## Unreleased` heading of both root `CHANGELOG.md` and `packages/tapwing_driver/CHANGELOG.md` into `## <version>`, verifies version sync and packaging tests, and commits `release: v<version>` on the branch `release/v<version>`. If a check fails, it restores the edited files and removes the branch. `tag` refuses to tag unless `main` carries that version and the `CI Gate` check passed on the commit (it asks GitHub through `gh`). Both accept `--no-push`.

Pushing `v<version>` triggers [.github/workflows/release.yml](../.github/workflows/release.yml), which builds and publishes `.tar.gz` and `.sha256` assets for all three targets (Linux builds on `ubuntu-22.04`, requiring glibc 2.35+) and generates `Formula/tapwing.rb`. You can also run the workflow manually via `workflow_dispatch` from a branch (with an empty `tag`) to test builds without publishing.

### Homebrew tap

`brew install tapwing-dev/tap/tapwing` installs `Formula/tapwing.rb` from [`tapwing-dev/homebrew-tap`](https://github.com/tapwing-dev/homebrew-tap).

To update the tap automatically on each release, add a fine-grained GitHub token with **Contents: Read and write** access to `tapwing-dev/homebrew-tap` as the `HOMEBREW_TAP_TOKEN` Actions secret (set `HOMEBREW_TAP_REPO` to use a different tap). Without that secret, the release still succeeds and uploads `Formula/tapwing.rb` as the `homebrew-formula` workflow artifact so you can commit it manually.

To generate the formula locally from `dist/`:

```sh
scripts/generate-homebrew-formula.sh --version 0.2.0 --dist-dir dist --out Formula/tapwing.rb
```

### macOS signing

macOS binaries are always signed. Without Apple signing secrets they get an ad-hoc signature (`codesign --sign -`), which runs fine on Apple Silicon when installed via Homebrew or `curl` (downloads through a browser get a quarantine flag that Gatekeeper may block). Adding these repository secrets enables Developer ID signing and notarization:

| Secret | Purpose |
|---|---|
| `APPLE_CERTIFICATE_P12_BASE64` | Base64-encoded Developer ID Application `.p12` certificate |
| `APPLE_CERTIFICATE_PASSWORD` | Password for the `.p12` certificate |
| `APPLE_SIGNING_IDENTITY` | Signing identity (e.g. `Developer ID Application: Name (TEAMID)`) |
| `APPLE_NOTARY_KEY_P8_BASE64` | Base64-encoded App Store Connect API `.p8` key |
| `APPLE_NOTARY_KEY_ID` | App Store Connect API key ID |
| `APPLE_NOTARY_ISSUER_ID` | App Store Connect API issuer UUID |
