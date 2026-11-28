import 'dart:ui' as ui;

import '../diagnostics/text_input_log.dart';
import '../idle/idle_detector.dart';
import '../platform.dart';
import '../snapshot/snapshot.dart';

/// Result payload for `configure`.
class ConfigureResult {
  /// Creates a `configure` result.
  const ConfigureResult({
    required this.settlePolicy,
    this.strictInput = false,
    this.platform,
    this.appName,
    this.strictHideKeyboard = false,
    this.pixelsPerUnit,
    this.appIssues = false,
    this.httpLog = false,
    this.settleLoadTimeoutMs,
    this.settleHttpIgnore,
  });

  /// Active settle load bound in ms (`0`: none). Absent from older drivers,
  /// so the host can tell a driver without the bound apart.
  final int? settleLoadTimeoutMs;

  /// Number of active `settleHttpIgnore` patterns (absent from older
  /// drivers).
  final int? settleHttpIgnore;

  /// Whether app health issues are attached to responses.
  final bool appIssues;

  /// Whether the HTTP request log is enabled (absent from older drivers, so
  /// the host can tell a driver without the log apart).
  final bool httpLog;

  /// Active settle policy after configuration.
  final SettlePolicy settlePolicy;

  /// Whether strict text-input focus enforcement is enabled.
  final bool strictInput;

  /// Active Maestro platform semantics.
  final DriverPlatform? platform;

  /// Active iOS app display name (hierarchy root element), if any.
  final String? appName;

  /// Whether strict Maestro iOS `hideKeyboard` emulation is enabled.
  final bool strictHideKeyboard;

  /// Physical pixels per Maestro unit (`1` on Android, the device pixel ratio
  /// on iOS), letting the host map element bounds onto screenshots.
  final double? pixelsPerUnit;

  /// Serializes to `{"settlePolicy": "...", "strictInput": ..., ...}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'settlePolicy': settlePolicy.name,
    if (strictInput) 'strictInput': true,
    if (platform != null) 'platform': platform!.name,
    if (appName != null) 'appName': appName,
    if (strictHideKeyboard) 'strictHideKeyboard': true,
    if (pixelsPerUnit != null) 'pixelsPerUnit': pixelsPerUnit,
    if (appIssues) 'appIssues': true,
    if (httpLog) 'httpLog': true,
    if (settleLoadTimeoutMs != null) 'settleLoadTimeoutMs': settleLoadTimeoutMs,
    if (settleHttpIgnore != null) 'settleHttpIgnore': settleHttpIgnore,
  };
}

/// Result payload for `snapshot`.
class SnapshotResult {
  /// Creates a `snapshot` result from a captured [Snapshot].
  const SnapshotResult({required this.snapshot});

  /// Captured semantics snapshot.
  final Snapshot snapshot;

  /// Serializes to `{"screen": [w, h], "root": ...}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'screen': <double>[snapshot.screen.width, snapshot.screen.height],
    'root': snapshot.root?.toJson(withChildren: true),
  };
}

/// Result payload for `waitIdle`.
class WaitIdleResult {
  /// Creates a `waitIdle` result.
  const WaitIdleResult({required this.state});

  /// Terminal settle state: `'idle'`, `'stable'`, or `'timeout'`.
  final String state;

  /// Serializes to `{"state": "..."}`.
  Map<String, Object?> toJson() => <String, Object?>{'state': state};
}

/// Result payload for action commands returning only a settle state
/// (`tapPoint`, `eraseText`, `hideKeyboard`, `back`, `swipe`).
class SettleResult {
  /// Creates a settle-only result, optionally carrying a non-fatal [warning].
  const SettleResult({required this.settle, this.warning, this.noop = false});

  /// Terminal settle state: `'idle'`, `'stable'`, or `'timeout'`.
  final String settle;

  /// Optional non-fatal diagnostic warning reported by the host CLI.
  final String? warning;

  /// Whether the command intentionally did nothing (Maestro iOS no-ops such
  /// as `back`).
  final bool noop;

  /// Serializes to `{"settle": "..."}` (plus `"warning"` / `"noop"` when
  /// present).
  Map<String, Object?> toJson() => <String, Object?>{
    'settle': settle,
    if (warning != null) 'warning': warning,
    if (noop) 'noop': true,
  };
}

/// Result payload for `assertVisible`.
class ElementResult {
  /// Creates an element result.
  const ElementResult({required this.element});

  /// Matched element node.
  final KNode element;

  /// Serializes to `{"element": ...}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'element': element.toJson(),
  };
}

/// Sub-phase microsecond breakdown for `tapOn`.
class TapOnTimings {
  /// Creates a `tapOn` timing breakdown.
  const TapOnTimings({
    required this.findUs,
    required this.actUs,
    required this.settleUs,
  });

  /// Microseconds spent locating, pre-settling, and re-aiming the element.
  final int findUs;

  /// Microseconds spent dispatching tap pointer events and microtask yield.
  final int actUs;

  /// Microseconds spent waiting for post-tap UI settling.
  final int settleUs;

  /// Serializes to `{"findUs": ..., "actUs": ..., "settleUs": ...}`.
  Map<String, int> toJson() => <String, int>{
    'findUs': findUs,
    'actUs': actUs,
    'settleUs': settleUs,
  };
}

/// Result payload for `tapOn`.
class TapOnResult {
  /// Creates a `tapOn` result.
  const TapOnResult({
    required this.element,
    required this.settle,
    required this.timings,
    required this.ime,
  });

  /// Re-aimed element that was tapped.
  final KNode element;

  /// Post-tap settle outcome.
  final String settle;

  /// Sub-phase timing breakdown.
  final TapOnTimings timings;

  /// IME diagnostic log captured across the tap.
  final ImeDiagnostics ime;

  /// Serializes to `{"element": ..., "settle": ..., "t": ..., "ime": ...}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'element': element.toJson(),
    'settle': settle,
    't': timings.toJson(),
    'ime': ime.toJson(),
  };
}

/// Result payload for `inputText`.
class InputTextResult {
  /// Creates an `inputText` result.
  const InputTextResult({
    required this.settle,
    required this.ime,
    this.warning,
  });

  /// Post-input settle outcome.
  final String settle;

  /// IME diagnostic snapshot including `usedClient` and log entries.
  final ImeDiagnostics ime;

  /// Optional non-fatal warning (e.g. no focused text field in Maestro-parity
  /// mode).
  final String? warning;

  /// Serializes to `{"settle": ..., "ime": ...}` (plus `"warning"` when set).
  Map<String, Object?> toJson() => <String, Object?>{
    'settle': settle,
    'ime': ime.toJson(),
    if (warning != null) 'warning': warning,
  };
}

/// Result payload for `scrollUntilVisible`.
class ScrollUntilVisibleResult {
  /// Creates a `scrollUntilVisible` result.
  const ScrollUntilVisibleResult({required this.element, required this.swipes});

  /// Element found once it reached the requested visibility threshold.
  final KNode element;

  /// Number of drag gestures performed before the element became visible.
  final int swipes;

  /// Serializes to `{"element": ..., "swipes": ...}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'element': element.toJson(),
    'swipes': swipes,
  };
}

/// Result payload for `copyTextFrom`.
class CopyTextResult {
  /// Creates a `copyTextFrom` result.
  const CopyTextResult({required this.text});

  /// Extracted element text (`Orchestra.resolveText`).
  final String text;

  /// Serializes to `{"text": "..."}`.
  Map<String, Object?> toJson() => <String, Object?>{'text': text};
}

/// Result payload for `darkMode` (reads Flutter platform brightness).
class DarkModeResult {
  /// Creates a `darkMode` result.
  const DarkModeResult({required this.darkMode});

  /// Whether platform brightness is currently [ui.Brightness.dark].
  final bool darkMode;

  /// Serializes to `{"darkMode": bool, "brightness": "dark"|"light"}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'darkMode': darkMode,
    'brightness': darkMode ? 'dark' : 'light',
  };
}

/// Result payload for `callHook`.
class CallHookResult {
  /// Creates a `callHook` result.
  const CallHookResult({
    required this.hook,
    required this.value,
    required this.settle,
    required this.hookUs,
    required this.settleUs,
  });

  /// Invoked hook name.
  final String hook;

  /// JSON-encodable return value produced by the hook handler.
  final Object? value;

  /// Post-hook settle state (`'idle'`, `'stable'`, `'timeout'`, or `'skipped'`).
  final String settle;

  /// Microseconds spent executing the hook callback itself.
  final int hookUs;

  /// Microseconds spent waiting for post-hook UI settle (`0` if skipped).
  final int settleUs;

  /// Serializes to `{"hook": ..., "value": ..., "settle": ..., "t": ...}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'hook': hook,
    'value': value,
    'settle': settle,
    't': <String, int>{'hookUs': hookUs, 'settleUs': settleUs},
  };
}
