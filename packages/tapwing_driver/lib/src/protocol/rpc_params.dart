import '../idle/idle_detector.dart';
import '../platform.dart';
import '../selector/selector.dart';
import 'rpc_envelope.dart';
import 'rpc_error.dart';

/// Parameters for `configure`.
class ConfigureParams {
  /// Creates `configure` parameters.
  const ConfigureParams({
    this.settlePolicy,
    this.softKeyboard,
    this.strictInput,
    this.platform,
    this.appName,
    this.strictHideKeyboard,
    this.appIssues,
    this.httpLog,
    this.settleLoadTimeoutMs,
    this.settleHttpIgnore,
  });

  /// Parses `configure` parameters from JSON.
  factory ConfigureParams.fromJson(Map<String, Object?> json) {
    final Object? raw = json['settlePolicy'];
    SettlePolicy? settlePolicy;
    if (raw is String) {
      settlePolicy = SettlePolicy.values.asNameMap()[raw];
      if (settlePolicy == null) {
        throw DriverError.invalidCommand('Unknown settlePolicy: $raw');
      }
    }
    final Object? platform = json['platform'];
    if (platform != null && DriverPlatform.tryParse(platform) == null) {
      throw DriverError.invalidCommand('Unknown platform: $platform');
    }
    final Object? loadTimeout = json['settleLoadTimeoutMs'];
    if (loadTimeout != null &&
        (loadTimeout is! num ||
            !loadTimeout.isFinite ||
            loadTimeout < 0 ||
            loadTimeout > maxSettleLoadTimeoutMs)) {
      throw DriverError.invalidCommand(
        'settleLoadTimeoutMs must be between 0 and $maxSettleLoadTimeoutMs: '
        '$loadTimeout',
      );
    }
    return ConfigureParams(
      settleLoadTimeoutMs: (loadTimeout as num?)?.toInt(),
      settleHttpIgnore: _parsePatterns(json['settleHttpIgnore']),
      settlePolicy: settlePolicy,
      softKeyboard: json['softKeyboard'] is bool
          ? json['softKeyboard']! as bool
          : null,
      strictInput: json['strictInput'] is bool
          ? json['strictInput']! as bool
          : null,
      platform: DriverPlatform.tryParse(platform),
      appName: json['appName'] is String ? json['appName']! as String : null,
      strictHideKeyboard: json['strictHideKeyboard'] is bool
          ? json['strictHideKeyboard']! as bool
          : null,
      appIssues: json['appIssues'] is bool ? json['appIssues']! as bool : null,
      httpLog: json['httpLog'] is bool ? json['httpLog']! as bool : null,
    );
  }

  static List<RegExp>? _parsePatterns(Object? raw) {
    if (raw == null) {
      return null;
    }
    if (raw is! List<Object?>) {
      throw DriverError.invalidCommand(
        'settleHttpIgnore must be a list of regular expressions',
      );
    }
    return <RegExp>[
      for (final Object? pattern in raw)
        if (pattern is String)
          _compile(pattern)
        else
          throw DriverError.invalidCommand(
            'settleHttpIgnore must be a list of regular expressions',
          ),
    ];
  }

  static RegExp _compile(String pattern) {
    try {
      return RegExp(pattern);
    } on FormatException catch (e) {
      throw DriverError.invalidCommand(
        'Invalid settleHttpIgnore pattern `$pattern`: ${e.message}',
      );
    }
  }

  /// Maximum accepted `settleLoadTimeoutMs` (24 hours), preventing 64-bit
  /// microsecond overflow in `Duration(milliseconds: ...)`. Use `0` for no bound.
  static const int maxSettleLoadTimeoutMs = 24 * 60 * 60 * 1000;

  /// Optional bound (ms) on how long one HTTP request or pending image
  /// blocks settle (`--settle-load-timeout-ms`); `0` removes the bound.
  final int? settleLoadTimeoutMs;

  /// Optional URL patterns whose requests never block settle
  /// (`--settle-http-ignore`); replaces the previous list.
  final List<RegExp>? settleHttpIgnore;

  /// Optional new settle policy (`v2` or `legacy`).
  final SettlePolicy? settlePolicy;

  /// Optional host hint indicating whether a soft keyboard is expected to
  /// appear (`false` on hardware-keyboard AVDs/simulators).
  final bool? softKeyboard;

  /// Optional opt-in strict mode failing `inputText`/`eraseText` when no text
  /// field is focused (default `false` matches Maestro `AndroidDriver.kt:547-559`).
  final bool? strictInput;

  /// Optional Maestro platform semantics (`android` / `ios`); defaults to the
  /// platform the app runs on.
  final DriverPlatform? platform;

  /// Optional iOS app display name for the hierarchy root element (`""`
  /// clears it).
  final String? appName;

  /// Optional strict Maestro iOS `hideKeyboard` emulation
  /// (`--maestro-compat=strict`).
  final bool? strictHideKeyboard;

  /// Optional app health capture (F7, `--app-errors`): `true` attaches
  /// issues to every response, `false` stops capturing and clears the log.
  final bool? appIssues;

  /// Optional HTTP request log (F8b, `--trace`): `true` records requests and
  /// attaches finished ones to every response, `false` stops and clears it.
  final bool? httpLog;
}

/// Parameters for `waitIdle`.
class WaitIdleParams {
  /// Creates `waitIdle` parameters.
  const WaitIdleParams({required this.timeout});

  /// Parses `waitIdle` parameters from JSON (defaults to 3000 ms).
  factory WaitIdleParams.fromJson(Map<String, Object?> json) =>
      WaitIdleParams(timeout: _ms(json['timeoutMs'], DriverTimeouts.settleMs));

  /// Maximum duration to wait for the app to settle.
  final Duration timeout;
}

/// Shared settle-timeout parameters for action commands (`hideKeyboard`,
/// `back`).
class SettleParams {
  /// Creates action settle parameters.
  const SettleParams({required this.settleTimeout});

  /// Parses `settleTimeoutMs` from JSON (defaults to 3000 ms).
  factory SettleParams.fromJson(Map<String, Object?> json) => SettleParams(
    settleTimeout: _ms(json['settleTimeoutMs'], DriverTimeouts.settleMs),
  );

  /// Maximum duration to wait for settling after the action.
  final Duration settleTimeout;
}

/// Parameters for `tapOn`.
class TapOnParams {
  /// Creates `tapOn` parameters.
  const TapOnParams({
    required this.selector,
    required this.timeout,
    required this.longPress,
    required this.repeat,
    required this.delay,
    required this.settleTimeout,
    this.relativePoint,
    this.retryIfNoChange = false,
    this.waitUntilVisible = false,
  });

  /// Parses `tapOn` parameters from JSON with Maestro/Tapwing defaults.
  factory TapOnParams.fromJson(Map<String, Object?> json) => TapOnParams(
    selector: _requireSelector(json),
    timeout: _ms(json['timeoutMs'], DriverTimeouts.lookupMs),
    longPress: json['longPress'] == true,
    repeat: (json['repeat'] as num?)?.toInt() ?? 1,
    delay: _ms(json['delayMs'], DriverTimeouts.repeatDelayMs),
    settleTimeout: _ms(json['settleTimeoutMs'], DriverTimeouts.settleMs),
    relativePoint: json['relativePoint'] as String?,
    retryIfNoChange: json['retryIfNoChange'] == true,
    waitUntilVisible: json['waitUntilVisible'] == true,
  );

  /// Target element selector.
  final Selector selector;

  /// Lookup timeout waiting for the element to appear.
  final Duration timeout;

  /// Whether to hold for 3000 ms (`longPressOn`).
  final bool longPress;

  /// Number of taps to emit (defaults to 1).
  final int repeat;

  /// Delay between repeated taps (defaults to 100 ms).
  final Duration delay;

  /// Settle timeout before re-aiming and after tapping.
  final Duration settleTimeout;

  /// Optional element-relative tap point (`"50%, 90%"` or `"25, 75"`).
  final String? relativePoint;

  /// Whether to retry the tap once if the semantics tree did not change
  /// (`Maestro.kt:379-381, 416-440`).
  final bool retryIfNoChange;

  /// Whether to wait for the element to become topmost at its center and
  /// re-tap if the first tap caused no change (`Maestro.kt:262-281`).
  final bool waitUntilVisible;
}

/// Parameters for `tapPoint`.
class TapPointParams {
  /// Creates `tapPoint` parameters.
  const TapPointParams({
    required this.point,
    required this.longPress,
    required this.settleTimeout,
    this.repeat = 1,
    this.delay = const Duration(milliseconds: DriverTimeouts.repeatDelayMs),
    this.retryIfNoChange = false,
  });

  /// Parses `tapPoint` parameters from JSON.
  factory TapPointParams.fromJson(Map<String, Object?> json) => TapPointParams(
    point: json['point']! as String,
    longPress: json['longPress'] == true,
    settleTimeout: _ms(json['settleTimeoutMs'], DriverTimeouts.settleMs),
    repeat: (json['repeat'] as num?)?.toInt() ?? 1,
    delay: _ms(json['delayMs'], DriverTimeouts.repeatDelayMs),
    retryIfNoChange: json['retryIfNoChange'] == true,
  );

  /// Maestro coordinate specification (`"50%, 50%"` or `"100, 250"`).
  final String point;

  /// Whether to hold for 3000 ms (`longPressOn`).
  final bool longPress;

  /// Post-tap settle timeout.
  final Duration settleTimeout;

  /// Number of taps to emit (defaults to 1).
  final int repeat;

  /// Delay between repeated taps (defaults to 100 ms).
  final Duration delay;

  /// Whether to retry the tap once if the semantics tree did not change.
  final bool retryIfNoChange;
}

/// Parameters for selector + timeout commands (`assertVisible`,
/// `assertNotVisible`, `copyTextFrom`).
class SelectorTimeoutParams {
  /// Creates selector-with-timeout parameters.
  const SelectorTimeoutParams({required this.selector, required this.timeout});

  /// Parses `selector` and `timeoutMs` with [defaultTimeoutMs].
  factory SelectorTimeoutParams.fromJson(
    Map<String, Object?> json, {
    required int defaultTimeoutMs,
  }) => SelectorTimeoutParams(
    selector: _requireSelector(json),
    timeout: _ms(json['timeoutMs'], defaultTimeoutMs),
  );

  /// Target element selector.
  final Selector selector;

  /// Wait timeout.
  final Duration timeout;
}

/// Parameters for `inputText`.
class InputTextParams {
  /// Creates `inputText` parameters.
  const InputTextParams({required this.text, required this.settleTimeout});

  /// Parses `inputText` parameters from JSON.
  factory InputTextParams.fromJson(Map<String, Object?> json) =>
      InputTextParams(
        text: json['text']! as String,
        settleTimeout: _ms(json['settleTimeoutMs'], DriverTimeouts.settleMs),
      );

  /// Text string to type into the currently focused text field.
  final String text;

  /// Post-input settle timeout.
  final Duration settleTimeout;
}

/// Parameters for `eraseText`.
class EraseTextParams {
  /// Creates `eraseText` parameters.
  const EraseTextParams({required this.count, required this.settleTimeout});

  /// Parses `eraseText` parameters from JSON (defaults to 50 characters).
  factory EraseTextParams.fromJson(Map<String, Object?> json) =>
      EraseTextParams(
        count: (json['count'] as num?)?.toInt() ?? 50,
        settleTimeout: _ms(json['settleTimeoutMs'], DriverTimeouts.settleMs),
      );

  /// Number of backspace steps to perform.
  final int count;

  /// Post-erase settle timeout.
  final Duration settleTimeout;
}

/// Parameters for `swipe`.
///
/// Supports both explicit coordinate/percentage mode ([start] + [end]) and
/// element-origin mode ([selector] + [direction] + optional [relativePoint]).
class SwipeParams {
  /// Creates `swipe` parameters.
  const SwipeParams({
    this.start,
    this.end,
    this.selector,
    this.direction,
    this.relativePoint,
    this.timeout = const Duration(milliseconds: DriverTimeouts.lookupMs),
    required this.duration,
    required this.settleTimeout,
  });

  /// Parses `swipe` parameters from JSON (defaults to 400 ms duration).
  factory SwipeParams.fromJson(Map<String, Object?> json) => SwipeParams(
    start: json['start'] as String?,
    end: json['end'] as String?,
    selector: json['selector'] == null ? null : _requireSelector(json),
    direction: (json['direction'] as String?)?.toUpperCase(),
    relativePoint: json['relativePoint'] as String?,
    timeout: _ms(json['timeoutMs'], DriverTimeouts.lookupMs),
    duration: _ms(json['durationMs'], DriverTimeouts.swipeDurationMs),
    settleTimeout: _ms(json['settleTimeoutMs'], DriverTimeouts.settleMs),
  );

  /// Start coordinate spec (`"50%, 70%"` or physical px) in point mode.
  final String? start;

  /// End coordinate spec (`"50%, 30%"` or physical px) in point mode.
  final String? end;

  /// Origin element selector (`swipe` with `from:`).
  final Selector? selector;

  /// Swipe direction (`UP`, `DOWN`, `LEFT`, `RIGHT`) in element mode.
  final String? direction;

  /// Optional element-relative start point inside [selector].
  final String? relativePoint;

  /// Element lookup timeout in element mode.
  final Duration timeout;

  /// Drag duration.
  final Duration duration;

  /// Post-swipe settle timeout.
  final Duration settleTimeout;
}

/// Parameters for `scrollUntilVisible`.
class ScrollUntilVisibleParams {
  /// Creates `scrollUntilVisible` parameters.
  const ScrollUntilVisibleParams({
    required this.selector,
    required this.timeout,
    required this.direction,
    required this.visibilityPercentage,
    this.duration = const Duration(
      milliseconds: DriverTimeouts.defaultScrollDurationMs,
    ),
    this.centerElement = false,
    this.settleTimeout = DriverTimeouts.scrollStepSettle,
  });

  /// Parses `scrollUntilVisible` parameters from JSON with Maestro defaults.
  factory ScrollUntilVisibleParams.fromJson(Map<String, Object?> json) =>
      ScrollUntilVisibleParams(
        selector: _requireSelector(json),
        timeout: _ms(json['timeoutMs'], DriverTimeouts.scrollUntilVisibleMs),
        direction: ((json['direction'] as String?) ?? 'DOWN').toUpperCase(),
        visibilityPercentage:
            (json['visibilityPercentage'] as num?)?.toInt() ?? 100,
        duration: _ms(
          json['durationMs'],
          DriverTimeouts.defaultScrollDurationMs,
        ),
        centerElement: json['centerElement'] == true,
        settleTimeout: _ms(
          json['settleTimeoutMs'],
          DriverTimeouts.scrollStepSettle.inMilliseconds,
        ),
      );

  /// Target element selector to scroll into view.
  final Selector selector;

  /// Overall scroll-search timeout (defaults to 20 000 ms).
  final Duration timeout;

  /// Scroll direction (`UP`, `DOWN`, `LEFT`, `RIGHT`).
  final String direction;

  /// Required visible area percentage (`0`–`100`, defaults to 100).
  final int visibilityPercentage;

  /// Per-swipe drag duration derived from Maestro's `speed` (`Commands.kt:160`).
  final Duration duration;

  /// Whether to keep scrolling until the element is near screen center
  /// (`Orchestra.kt:876-880`).
  final bool centerElement;

  /// Per-step settle timeout (`waitToSettleTimeoutMs`).
  final Duration settleTimeout;
}

/// Parameters for `pressKey` (in-app keys handled at the engine boundary).
class PressKeyParams {
  /// Creates `pressKey` parameters.
  const PressKeyParams({required this.key, required this.settleTimeout});

  /// Parses `pressKey` parameters from JSON.
  factory PressKeyParams.fromJson(Map<String, Object?> json) => PressKeyParams(
    key: (json['key']! as String).toUpperCase(),
    settleTimeout: _ms(json['settleTimeoutMs'], DriverTimeouts.settleMs),
  );

  /// Uppercase key name (`ENTER`, `BACKSPACE`, `TAB`, `BACK`, `ESCAPE`).
  final String key;

  /// Post-key settle timeout.
  final Duration settleTimeout;
}

Selector _requireSelector(Map<String, Object?> json) => Selector.fromJson(
  (json['selector']! as Map<Object?, Object?>).cast<String, Object?>(),
);

Duration _ms(Object? value, int defaultMs) =>
    Duration(milliseconds: (value as num?)?.toInt() ?? defaultMs);
