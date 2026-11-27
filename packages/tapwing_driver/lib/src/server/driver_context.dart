import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/rendering.dart' show RendererBinding;
import 'package:flutter/scheduler.dart' show SchedulerBinding;
import 'package:flutter/widgets.dart' show AppLifecycleState;

import '../diagnostics/error_hooks.dart';
import '../idle/idle_detector.dart';
import '../input/back_signals.dart';
import '../input/pointer_input.dart';
import '../input/text_input.dart';
import '../platform.dart';
import '../protocol/rpc_error.dart';
import '../selector/selector.dart';
import '../snapshot/snapshot.dart';

/// Shared execution context passed to RPC command handlers.
///
/// Holds the active session's [settlePolicy] and [strictInput] alongside
/// injectable collaborators ([idle], [pointer], [textInput], [captureSnapshot],
/// [readScreenSize], [readBottomInset]) so every handler stays small, focused,
/// and testable without touching global singletons.
class DriverContext {
  /// Creates a driver context backed by the provided collaborators (defaulting
  /// to the live framework singletons).
  DriverContext({
    IdleDetector? idle,
    PointerInput? pointer,
    TextInputController? textInput,
    Snapshot Function()? captureSnapshot,
    int Function()? computeSemanticsHash,
    SemanticsHashCache? semanticsHashCache,
    ui.Size Function()? readScreenSize,
    ui.Brightness Function()? readPlatformBrightness,
    double Function()? readBottomInset,
    double Function()? readDevicePixelRatio,
    BackSignals? backSignals,
    AppLifecycleState? Function()? readLifecycleState,
    SettlePolicy initialPolicy = SettlePolicy.v2,
    DriverPlatform? platform,
    this.strictInput = false,
    this.strictHideKeyboard = false,
    this.appName,
    AppErrorHooks? errorHooks,
  }) : idle = idle ?? IdleDetector.instance,
       errorHooks = errorHooks ?? AppErrorHooks.instance,
       pointer = pointer ?? PointerInput.instance,
       textInput = textInput ?? TextInputController.instance,
       _captureOverride = captureSnapshot,
       _semanticsHashCache = semanticsHashCache ?? SemanticsHashCache.instance,
       // Only use allocation-free cached semantics hashing for hash-gating when
       // captureSnapshot wasn't overridden (or caller also injected a matching
       // hash function).
       _computeSemanticsHash =
           computeSemanticsHash ??
           (captureSnapshot == null
               ? (semanticsHashCache ?? SemanticsHashCache.instance).hash
               : null),
       _screenSizeOverride = readScreenSize,
       readPlatformBrightness =
           readPlatformBrightness ?? _defaultPlatformBrightness,
       readBottomInset = readBottomInset ?? _defaultBottomInset,
       readDevicePixelRatio = readDevicePixelRatio ?? _defaultDevicePixelRatio,
       backSignals = backSignals ?? BackSignals.instance,
       readLifecycleState = readLifecycleState ?? _defaultLifecycleState,
       platform = platform ?? DriverPlatform.detect(),
       settlePolicy = initialPolicy;

  static const Duration _waitPollCap = Duration(milliseconds: 100);
  static const int _maxVisibleTextsInError = 40;

  /// Settle and frame-wait observer.
  final IdleDetector idle;

  /// App health hooks (F7); scanned for layout overflows after each settle.
  final AppErrorHooks errorHooks;

  /// Engine-boundary pointer and navigation injector.
  final PointerInput pointer;

  /// `flutter/textinput` observer and IME text injector.
  final TextInputController textInput;

  final Snapshot Function()? _captureOverride;

  final SemanticsHashCache _semanticsHashCache;

  final int Function()? _computeSemanticsHash;

  final ui.Size Function()? _screenSizeOverride;

  /// Returns the current device pixel ratio (physical pixels per logical
  /// point).
  final double Function() readDevicePixelRatio;

  /// Maestro driver semantics to reproduce (hierarchy composition, geometry
  /// units, `back`/`hideKeyboard`/`pressKey` behavior). Defaults to the
  /// platform the app runs on; the host CLI confirms it via `configure`.
  DriverPlatform platform;

  /// iOS application display name added as the hierarchy root element
  /// (Maestro's XCUIApplication node), passed by the host CLI.
  String? appName;

  /// Opt-in Maestro iOS `hideKeyboard` emulation (`--maestro-compat=strict`):
  /// two 50 ms micro-swipes, then fail if an editable field is still focused.
  bool strictHideKeyboard;

  /// Whether geometry is expressed in logical points (Maestro iOS) instead of
  /// physical pixels (Maestro Android).
  bool get usesPoints => platform == DriverPlatform.ios;

  /// Physical pixels per Maestro unit: `1` on Android, the device pixel ratio
  /// on iOS. Pointer packets are always injected in physical pixels.
  double get pixelsPerUnit {
    if (!usesPoints) return 1.0;
    final double dpr = readDevicePixelRatio();
    return dpr > 0 ? dpr : 1.0;
  }

  /// Captures the current semantics tree snapshot in Maestro units.
  Snapshot captureSnapshot() =>
      _captureOverride?.call() ??
      Snapshot.capture(
        platform: platform,
        appName: appName,
        hashCache: _semanticsHashCache,
      );

  /// Marks the cached semantics hash dirty (for example on `configure` or
  /// session reset).
  void invalidateSemanticsCache() {
    _semanticsHashCache.invalidate();
  }

  /// Detaches any live semantics hash listener held by this context.
  void dispose() {
    _semanticsHashCache.dispose();
  }

  /// Returns the current screen size in Maestro units (physical pixels on
  /// Android, whole logical points on iOS) for percentage point parsing.
  ui.Size readScreenSize() {
    final ui.Size? override = _screenSizeOverride?.call();
    if (override != null) return override;
    final ui.Size physical = _defaultScreenSize();
    if (!usesPoints) return physical;
    final double ppu = pixelsPerUnit;
    return ui.Size(
      (physical.width / ppu + 1e-6).truncateToDouble(),
      (physical.height / ppu + 1e-6).truncateToDouble(),
    );
  }

  /// Taps at [point] given in Maestro units (converted to physical pixels).
  Future<void> tap(ui.Offset point, {Duration hold = Duration.zero}) {
    final double ppu = pixelsPerUnit;
    return pointer.tap(point.dx * ppu, point.dy * ppu, hold: hold);
  }

  /// Drags from [start] to [end] given in Maestro units (converted to
  /// physical pixels); see [PointerInput.drag].
  Future<void> drag(
    ui.Offset start,
    ui.Offset end,
    Duration duration, {
    Duration hold = Duration.zero,
    Duration? wallDuration,
  }) {
    final double ppu = pixelsPerUnit;
    return pointer.drag(
      start * ppu,
      end * ppu,
      duration,
      hold: hold,
      wallDuration: wallDuration,
    );
  }

  /// System Back deliveries from the engine (see [BackSignals]).
  final BackSignals backSignals;

  /// Returns the app lifecycle state (`null` before the engine reported one).
  final AppLifecycleState? Function() readLifecycleState;

  /// Returns the current platform brightness (`dark` / `light`).
  final ui.Brightness Function() readPlatformBrightness;

  /// Returns the current bottom `viewInsets` (soft keyboard height in physical
  /// pixels, `0` when the soft keyboard is hidden).
  final double Function() readBottomInset;

  /// Active settle policy configured for this driver session.
  SettlePolicy settlePolicy;

  /// Whether `inputText`/`eraseText` fail with `NO_FOCUS` when no text-input
  /// client is attached (opt-in `--strict-input`). Default `false` matches
  /// Maestro (`AndroidDriver.kt:547-559`).
  bool strictInput;

  /// Cancellation check updated per active [RpcSession] connection so wait
  /// loops unwind promptly if the host disconnects mid-command.
  bool Function() isCancelled = _neverCancelled;

  static bool _neverCancelled() => false;

  /// Computes the structural semantics hash without allocating [KNode]s when
  /// using the live semantics tree.
  int computeHash() => _computeSemanticsHash != null
      ? _computeSemanticsHash()
      : captureSnapshot().hash;

  /// Waits for the UI to settle under the current [settlePolicy], then
  /// records layout overflows visible on the settled screen (profile and
  /// release builds; debug builds report them through `FlutterError`).
  ///
  /// [maxContentVelocity] is forwarded to [IdleDetector.waitForIdle]; only the
  /// wait between two gestures of a scroll sets it.
  Future<String> settle(
    Duration timeout, {
    int? stableFrames,
    bool includeScrollables = true,
    double? maxContentVelocity,
  }) async {
    final String result = await idle.waitForIdle(
      timeout,
      policy: settlePolicy,
      stableFrames: stableFrames,
      includeScrollables: includeScrollables,
      maxContentVelocity: maxContentVelocity,
      isCancelled: isCancelled,
    );
    errorHooks.scanOverflows();
    return result;
  }

  /// Reactively waits until [selector] resolves to a visible node or [timeout]
  /// elapses, re-evaluating after rendered frames whose semantics hash changed
  /// (capped at 100 ms per step).
  Future<KNode?> waitFor(Selector selector, Duration timeout) async {
    final Stopwatch sw = Stopwatch()..start();
    int? lastHash;
    while (true) {
      if (isCancelled()) return null;
      final int? hash = _computeSemanticsHash?.call();
      if (hash == null || hash != lastHash) {
        lastHash = hash;
        final KNode? match = resolve(selector, captureSnapshot());
        if (match != null) return match;
      }
      final Duration remaining = timeout - sw.elapsed;
      if (remaining <= Duration.zero) return null;
      await idle.waitForNextFrame(
        remaining < _waitPollCap ? remaining : _waitPollCap,
      );
    }
  }

  /// Reactively waits until [selector] no longer resolves to any visible node
  /// or [timeout] elapses, gating snapshot allocation on semantics hash
  /// changes.
  Future<bool> waitGone(Selector selector, Duration timeout) async {
    final Stopwatch sw = Stopwatch()..start();
    int? lastHash;
    while (true) {
      if (isCancelled()) return false;
      final int? hash = _computeSemanticsHash?.call();
      if (hash == null || hash != lastHash) {
        lastHash = hash;
        if (resolve(selector, captureSnapshot()) == null) return true;
      }
      final Duration remaining = timeout - sw.elapsed;
      if (remaining <= Duration.zero) return false;
      await idle.waitForNextFrame(
        remaining < _waitPollCap ? remaining : _waitPollCap,
      );
    }
  }

  /// Builds an `ELEMENT_NOT_FOUND` [DriverError] populated with up to 40
  /// non-empty texts currently visible in the semantics tree and any visible
  /// platform-view coverage.
  DriverError elementNotFound(Selector selector) {
    final Snapshot snap = captureSnapshot();
    final List<String> texts = snap.nodes
        .map(
          (KNode n) => _firstNonEmpty(n.accessibilityText, n.text, n.hintText),
        )
        .whereType<String>()
        .take(_maxVisibleTextsInError)
        .toList();
    return DriverError.elementNotFound(
      selector,
      texts,
      platformViewCoverage: snap.platformViewCoverage,
    );
  }

  /// Parses Maestro point syntax: `"50%, 50%"` (relative to the screen size)
  /// or `"100, 250"` (absolute Maestro units: physical pixels on Android,
  /// points on iOS), with Maestro's range
  /// checks (`Orchestra.kt:1450-1479`, `NumericFields.kt:41-45`).
  ui.Offset parsePoint(String spec) {
    final ui.Size screen = readScreenSize();
    final (int x, int y) = _parsePointPair(spec);
    if (spec.contains('%')) {
      if (x < 0 || x > 100 || y < 0 || y > 100) {
        throw DriverError.invalidCommand(
          'Invalid point: $spec. Percentages must be between 0 and 100.',
        );
      }
      return ui.Offset(
        (screen.width.round() * x ~/ 100).toDouble(),
        (screen.height.round() * y ~/ 100).toDouble(),
      );
    }
    if (x < 0 || y < 0) {
      throw DriverError.invalidCommand(
        'Invalid point: $spec. Coordinates must not be negative.',
      );
    }
    return ui.Offset(x.toDouble(), y.toDouble());
  }

  /// Resolves an element-relative point (`"50%, 90%"` or `"25, 75"`) to
  /// absolute screen coordinates in Maestro units.
  ///
  /// Exact port of `calculateElementRelativePoint` (`ElementCoordinateUtil.kt:15-39`).
  ui.Offset resolveElementPoint(KNode element, String spec) {
    final int bx = element.bounds.left.round();
    final int by = element.bounds.top.round();
    final int bw = element.bounds.right.round() - bx;
    final int bh = element.bounds.bottom.round() - by;
    final (int x, int y) = _parsePointPair(spec);

    if (spec.contains('%')) {
      if (x < 0 || x > 100 || y < 0 || y > 100) {
        throw DriverError.invalidCommand(
          'Invalid element-relative point: $spec. Percentages must be between 0 and 100.',
        );
      }
      return ui.Offset(
        (bx + (bw * x ~/ 100)).toDouble(),
        (by + (bh * y ~/ 100)).toDouble(),
      );
    }
    if (x < 0 || y < 0 || x >= bw || y >= bh) {
      throw DriverError.invalidCommand(
        'Invalid element-relative point: $spec. Coordinates must be within '
        'element bounds (0,0) to (${bw - 1},${bh - 1}).',
      );
    }
    return ui.Offset((bx + x).toDouble(), (by + y).toDouble());
  }

  /// Parses `"x, y"` or `"x%, y%"` into integer components, matching
  /// `NumericFields.parsePoint` (`NumericFields.kt:41-45`).
  static (int, int) _parsePointPair(String raw) {
    final List<String> parts = raw
        .replaceAll('%', '')
        .split(',')
        .map((String e) => e.trim())
        .toList();
    if (parts.length < 2) {
      throw DriverError.invalidCommand(
        'Invalid point value "$raw": expected two whole-number coordinates '
        'like "x,y" or "x%,y%".',
      );
    }
    final int? x = int.tryParse(parts[0]);
    final int? y = int.tryParse(parts[1]);
    if (x == null || y == null) {
      throw DriverError.invalidCommand(
        'Invalid point value "$raw": expected two whole-number coordinates '
        'like "x,y" or "x%,y%".',
      );
    }
    return (x, y);
  }

  static String? _firstNonEmpty(String? a, String? b, String? c) {
    if (a != null && a.isNotEmpty) return a;
    if (b != null && b.isNotEmpty) return b;
    if (c != null && c.isNotEmpty) return c;
    return null;
  }

  /// Computes the fraction of [node]'s area intersecting [screen] (`0.0`–`1.0`)
  /// for `scrollUntilVisible`'s `visibilityPercentage` check.
  static double visiblePercent(KNode node, ui.Size screen) =>
      visibleFraction(node, screen.width, screen.height);

  static ui.Size _defaultScreenSize() {
    final ui.FlutterView? view =
        _dispatcher.implicitView ?? _dispatcher.views.firstOrNull;
    return view?.physicalSize ?? ui.Size.zero;
  }

  /// The binding's dispatcher (`PlatformDispatcher.instance` in an app, the
  /// test dispatcher honoring `tester.view` in widget tests).
  static ui.PlatformDispatcher get _dispatcher =>
      RendererBinding.instance.platformDispatcher;

  static double _defaultDevicePixelRatio() =>
      (_dispatcher.implicitView ?? _dispatcher.views.firstOrNull)
          ?.devicePixelRatio ??
      1.0;

  static AppLifecycleState? _defaultLifecycleState() =>
      SchedulerBinding.instance.lifecycleState;

  static ui.Brightness _defaultPlatformBrightness() =>
      _dispatcher.platformBrightness;

  static double _defaultBottomInset() =>
      (_dispatcher.implicitView ?? _dispatcher.views.firstOrNull)
          ?.viewInsets
          .bottom ??
      0;
}
