# Maestro Reference Screenshot Fixtures

These three PNG files (`expected.png`, `actual.png`, `saved_without_search.png`) are copied verbatim from **Maestro 2.12.1** (`maestro-orchestra/src/test/resources/AssertScreenshotMatchTest/`, Apache License 2.0, Copyright Mobile.dev Inc.).

They are used by `crates/tapwing/src/screenshot.rs` (`matches_maestro_reference_fixtures`) to verify bit-exact parity of Tapwing's Euclidean RGB `match_percentage` port against Maestro's pinned test assertions (`AssertScreenshotMatchTest.kt:42, 77, 130`):

- `expected.png` vs `actual.png` (`pixelTolerance = 0.1`): `~99.9511%`
- `actual.png` vs `saved_without_search.png` (`pixelTolerance = 0.1`): `~91.4724%`
- `expected.png` vs `actual.png` (`pixelTolerance = 0.0`): `~99.4251%`
