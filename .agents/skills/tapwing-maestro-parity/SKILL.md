---
name: tapwing-maestro-parity
description: >-
  Encodes Tapwing's Maestro 2.12.1 compatibility invariants, Android vs. iOS
  hierarchy/geometry rules, and hybrid Flutter+Native routing semantics. Use
  when implementing or reviewing commands, selectors, native lanes, timeouts,
  or differential tests against Maestro.
---

# Tapwing Maestro 2.12.1 Parity & Hybrid Routing

Use this checklist whenever touching command execution, selectors, hierarchy mapping, timeouts, or native-lane routing.

## 1. Platform Geometry & Hierarchy Parity

| Dimension | Android (`AndroidDriver`) | iOS Simulator (`IOSDriver`) |
|---|---|---|
| Coordinates / bounds / `width` / `height` | **Physical pixels** | **Logical points** (pointer packets still injected in physical px) |
| `accessibilityText` / `text` / `hintText` | Mirrors Android `AccessibilityBridge` composition | `accessibilityText = label`, `text = value` (`"1"`/`"0"` for switches), empty `hintText`, root named `CFBundleDisplayName` |
| Empty XML / AX attributes | Normalize `""` → `None` (never `Some("")`) | Normalize `""` → `None` (never `Some("")`) |
| `back` & `pressKey: Back` | Dismisses soft keyboard first, then pops route | No-op (matches `IOSDriver`) |
| `directional swipe: UP` / `scroll` | Standard Android ratios | Starts at 90%, `scroll` lasts 333 ms |

- **Why normalizing `""` → `None` matters:** storing `Some("")` breaks `traits: [TEXT]`, causes `copyTextFrom` to return `""` instead of `UnableToCopy`, and makes regexes like `text: ".*"` match layout containers.
- **`ListTile` merging:** merged semantics join `title` and `subtitle` with `\n` (matched as space by `Filters.textMatches`); selectors must account for full-match regex semantics.

## 2. Hybrid Routing & Multi-App State (`--input auto|flutter|native`)

- **Pure-Flutter fast path must pay ~0 ms for native lanes:** never wait on background warm-up or dump native hierarchies unless a Flutter lookup misses or an external window owns focus.
- **Per-app black-box tracking:** track `blackbox_apps` per `appId`, update `flow_app = req.app_id` on every `launchApp`, reset `native_hint = false`, and treat `req.app_id != prev_app` as a restart (disconnecting the previous app's driver socket).
- **Timeout accounting:**
  - Respect Maestro's defaults (`assertVisible` / `assertNotVisible`: 17 s; optional element lookups: 7 s) adjusted via `adjustedToLatestInteraction`.
  - **Snapshot remaining timeout before joining native warm-up** (`let remaining = left(started);`) so cold driver startup never drains the selector lookup budget to 0 ms.
- **Accurate `laneStats`:** increment `lanes.native` for speculative fallback probes (`try_native`) only when the probe actually succeeds, so pure-Flutter `optional: true` misses never misreport `"lane": "hybrid"`.
- **Orientation invalidation:** invalidate cached native screen dimensions on `setOrientation`, and do not cache screen size during background warm-up (`Pending`) before the first native command runs.
- **External-window transitions:** set `native_hint = true` after `openLink` or `pressKey: Home` on Android so subsequent `tapOn: point`, `inputText`, `pressKey`, or `scroll` reach the foreground window.

## 3. Error Boundary Parity (`optional` & `retry`)

- `optional: true` (on command or selector, never `cropOn.optional`) downgrades **only** Maestro command-level errors (`MaestroError`).
- `retry` (default 1, capped at 3) retries **only** Maestro command-level errors.
- Transport disconnects, invalid numeric fields, JS runtime errors, and internal errors must always fail hard.

## 4. Verification

- Run parser parity: `cd crates/tapwing && cargo test --test integration compat::` (463/463 Maestro fixtures + 81/81 probes).
- Run orchestrator parity: `cd crates/tapwing && cargo test --test integration -- integration_orchestrator:: integration_core::`.
- Run live differential verification against real `maestro test` whenever modifying selectors, hierarchy mapping, or native-lane actions.
