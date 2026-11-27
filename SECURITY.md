# Security Policy

## Supported Versions

Tapwing is currently pre-1.0 (`0.x`). Security fixes are applied to the latest `0.x` release on `main`.

| Version | Supported |
|---|---|
| Latest `0.x` | Yes |
| Older `0.x` releases | Upgrade to latest |

## Reporting a Vulnerability

Please do **not** open a public GitHub issue for security vulnerabilities.

Instead, report vulnerabilities privately through [GitHub Security Advisories](https://github.com/tapwing-dev/tapwing/security/advisories/new) on `tapwing-dev/tapwing`. Include:

- `tapwing --version` output and target OS/device details.
- Steps to reproduce or proof of concept.
- Potential impact (for example, exposure outside loopback or release builds, secret leakage in trace outputs, or driver verification bypass).

## Security Boundaries & Design Notes

- **In-app driver release guard:** `tapwing_driver` stays disabled in Flutter release builds unless explicitly compiled with `--dart-define=TAPWING_ALLOW_RELEASE=true` ([docs/setup.md](docs/setup.md)). Only use Tapwing entrypoints (`lib/main_tapwing.dart`) in test/profile/debug builds.
- **Bundled native driver verification:** Release tarballs ship `.sha256` sidecars verified at runtime by `tapwing` before loading bundled Android/iOS native drivers ([docs/install.md](docs/install.md)).
- **Trace redaction (`--trace`):** Request and response bodies are never captured; credentials, fragments, sensitive query parameters, token-like path segments, sensitive headers, and redirect URLs are redacted both inside `tapwing_driver` and again in the CLI before writing `trace.json` / `trace.html` ([docs/failure-diagnostics.md](docs/failure-diagnostics.md)).
