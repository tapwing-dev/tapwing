---
name: dart-flutter-driver
description: >-
  Guides clean, deterministic, black-box-compliant Dart and Flutter code in
  `packages/tapwing_driver` and `examples/sample_app`. Use when modifying the
  in-app Flutter driver, idle detector, semantics snapshot, selector engine,
  input injector, or sample app widget tests.
---

# Dart & Flutter Driver Engineering

`packages/tapwing_driver` runs inside the app under test in **profile and release builds** (as well as debug). Every change must preserve black-box integrity, minimal runtime overhead, and zero test flakiness.

## 1. Black-Box Contract (Non-Negotiable)

- **Observation is read-only:** inspect UI state exclusively through `SemanticsBinding` / `SemanticsOwner` (and `RenderView` / `RenderBox` geometry for viewport & hit-target coordinates). Never inspect private widget state or call widget callbacks directly.
- **Input crosses the engine boundary:** inject taps/drags via `PlatformDispatcher.onPointerDataPacket` and text/navigation via `ServicesBinding.channelBuffers.push` (`flutter/textinput`, `flutter/navigation`, `flutter/keyevent`). Never invoke `onPressed`, `TextEditingController.text = ...`, or `Navigator.pop` directly.
- **Binding coexistence:** `TapwingBindingMixin` must compose cleanly when mixed in last alongside crash/APM bindings (e.g. Sentry).

## 2. Determinism, Speed & Resource Hygiene

- **Reactive waiting over sleeps:** re-evaluate predicates on frame/semantics callbacks (`SchedulerBinding.addPostFrameCallback` / semantics listener) rather than polling with fixed `Future.delayed` intervals.
- **Idle/settle discipline:** track `!hasScheduledFrame && transientCallbackCount == 0`, pending HTTP requests (`HttpOverrides`), `imageCache.pendingImageCount`, and active scrolling (`ScrollableState.position.isScrollingNotifier`). Treat repeating animations (> 1.5 s with unchanged semantics tree) as non-blocking (`infinite: true`) so flows never time out on spinners.
- **Zero resource leaks:** cancel every `Timer`, `StreamSubscription`, `Socket`, and semantics/frame listener on disconnect or `dispose()`. Widget tests must finish with **zero pending timers**.
- **Modularity:** keep each Dart file focused (~300–550 LOC). Separate pure Dart logic (protocol DTOs, selector matching, string composition, geometry) from Flutter binding/rendering hooks so pure logic tests run in milliseconds.

## 3. Strict Typing & Lints

Follow `packages/tapwing_driver/analysis_options.yaml` strictly:
- `strict-casts`, `strict-inference`, `strict-raw-types` enabled.
- `always_declare_return_types`, `avoid_dynamic_calls`, `unawaited_futures`, `cancel_subscriptions`, `close_sinks`, `prefer_final_locals`, `prefer_single_quotes`, `public_member_api_docs`, `use_enums`, `use_super_parameters`, `directives_ordering`.
- Never silence lints with `// ignore:` unless there is an explicit one-line justification explaining the framework constraint.
- All code comments, dartdoc, and tests must be written in **English**.

## 4. Verification Gate

```sh
(cd packages/tapwing_driver && dart analyze && flutter test) && (cd examples/sample_app && flutter analyze)
```
