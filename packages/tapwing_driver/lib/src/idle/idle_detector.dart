import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

import '../input/text_input.dart';
import '../snapshot/snapshot.dart';
import 'animation_scanner.dart';
import 'http_tracker.dart';
import 'keyboard_waiter.dart';

/// Strategy used to decide when the UI has settled despite active tickers.
enum SettlePolicy {
  /// F0 baseline: semantics hash stable for 8 rendered frames.
  ///
  /// Can be fooled by route transitions that animate paint only (snapshot
  /// zoom, fades); retained for A/B benchmarking against [v2].
  legacy,

  /// Default policy: semantics hash stable for 3 rendered frames AND no
  /// blocking significant animation (>= 2 % of the screen) currently running.
  ///
  /// Small infinite animations (spinners, blinking cursors), in-place repaints
  /// (ink ripples), and endless large animations running > 1.5 s with an
  /// unchanged semantics tree do not block settling, while route transitions,
  /// dialogs, snackbars, hero flights, and active scrolls do.
  v2,
}

/// Snapshot of current driver idleness signals returned by the `busy` RPC.
class BusyReport {
  /// Creates a busy diagnostics report.
  const BusyReport({
    required this.framesIdle,
    required this.loadingIdle,
    required this.transientCallbacks,
    required this.httpInFlight,
    required this.pendingImages,
    required this.animations,
  });

  /// Whether no frame is scheduled, no tickers are active, and the scheduler
  /// is idle.
  final bool framesIdle;

  /// Whether no tracked HTTP requests or image loads are pending.
  final bool loadingIdle;

  /// Number of active transient frame callbacks (tickers/animations).
  final int transientCallbacks;

  /// Number of in-flight `dart:io` HTTP requests.
  final int httpInFlight;

  /// Number of images currently pending in Flutter's [ImageCache].
  final int pendingImages;

  /// Running animations covering >= 2 % of the screen (endless ones carry
  /// `infinite: true`).
  final List<SignificantAnimation> animations;

  /// Serializes to the wire shape consumed by the host CLI (`--check-settle`).
  Map<String, Object?> toJson() => <String, Object?>{
    'framesIdle': framesIdle,
    'loadingIdle': loadingIdle,
    'transientCallbacks': transientCallbacks,
    'httpInFlight': httpInFlight,
    'pendingImages': pendingImages,
    'animations': animations
        .map((SignificantAnimation a) => a.toJson())
        .toList(),
  };
}

/// Observes (never controls) whether the Flutter application is busy.
///
/// The app is fully `'idle'` when no frame is scheduled, no transient frame
/// callbacks (tickers/animations) are active, no tracked HTTP requests are in
/// flight, and no images are pending.
///
/// Because infinite animations (such as loading spinners or text cursors) keep
/// transient callbacks active forever, [waitForIdle] also reports `'stable'`
/// once the semantics tree hash has been unchanged for `needStable` rendered
/// frames (3 in [SettlePolicy.v2], 8 in [SettlePolicy.legacy]) and — under
/// [SettlePolicy.v2] — no non-infinite significant animation or active scroll
/// is running.
class IdleDetector {
  /// Creates an idle detector with injectable collaborators for testing.
  IdleDetector({
    HttpTracker? httpTracker,
    TextInputController? textInput,
    AnimationScanner? animationScanner,
    int Function()? computeSemanticsHash,
    double Function()? readBottomInset,
    Future<void> Function(Duration timeout)? waitForFirstFrame,
    Duration Function()? clock,
  }) : _httpTracker = httpTracker ?? HttpTracker.instance,
       _animationScanner = animationScanner ?? AnimationScanner(clock: clock),
       _computeSemanticsHash = computeSemanticsHash ?? Snapshot.computeHash,
       _readBottomInset = readBottomInset ?? _defaultBottomInset,
       _waitForFirstFrameOverride = waitForFirstFrame {
    _keyboardWaiter = KeyboardWaiter(
      textInput ?? TextInputController.instance,
      _clock,
      waitForNextFrame,
      readBottomInset: _readBottomInset,
    );
  }

  /// Shared singleton used by the default driver binding.
  static final IdleDetector instance = IdleDetector();

  /// Required consecutive unchanged semantics frames in [SettlePolicy.v2].
  static const int v2StableFrames = 3;

  /// Required consecutive unchanged semantics frames in [SettlePolicy.legacy].
  static const int legacyStableFrames = 8;

  static const Duration _frameStepCap = Duration(milliseconds: 100);
  static const Duration _imagePollStep = Duration(milliseconds: 4);
  static const Duration _cancelPollInterval = Duration(milliseconds: 25);

  final Stopwatch _clock = Stopwatch()..start();
  final HttpTracker _httpTracker;
  final AnimationScanner _animationScanner;
  final int Function() _computeSemanticsHash;
  final double Function() _readBottomInset;
  final Future<void> Function(Duration timeout)? _waitForFirstFrameOverride;
  late final KeyboardWaiter _keyboardWaiter;

  final Set<Completer<void>> _frameWaiters = <Completer<void>>{};
  bool _frameCallbackScheduled = false;

  /// Number of tracked `dart:io` HTTP requests currently in flight.
  int get httpInFlight => _httpTracker.inFlight;

  /// HTTP tracker observing `dart:io` traffic (also owns the F8b request log).
  HttpTracker get httpTracker => _httpTracker;

  /// Whether the adaptive keyboard waiter currently treats the soft keyboard
  /// as absent (hardware-keyboard device or CLI hint).
  bool get softKeyboardAbsent => _keyboardWaiter.softKeyboardAbsent;

  /// Applies session-level hints from the `configure` RPC.
  void configure({bool? softKeyboard}) {
    _keyboardWaiter.configure(softKeyboard: softKeyboard);
  }

  /// Installs the global `HttpOverrides` hook to observe `dart:io` traffic.
  void install() {
    _httpTracker.install();
  }

  /// Whether the Flutter scheduler has no scheduled frame, no active tickers,
  /// and is currently in [SchedulerPhase.idle].
  bool get framesIdle =>
      !SchedulerBinding.instance.hasScheduledFrame &&
      SchedulerBinding.instance.transientCallbackCount == 0 &&
      SchedulerBinding.instance.schedulerPhase == SchedulerPhase.idle;

  /// Whether no tracked HTTP requests or image loads are currently pending.
  bool get loadingIdle =>
      _httpTracker.isIdle &&
      PaintingBinding.instance.imageCache.pendingImageCount == 0;

  void _ensureFrameCallback() {
    if (_frameCallbackScheduled) return;
    _frameCallbackScheduled = true;
    SchedulerBinding.instance.addPostFrameCallback((_) {
      _frameCallbackScheduled = false;
      final List<Completer<void>> waiters = _frameWaiters.toList(
        growable: false,
      );
      _frameWaiters.clear();
      for (final Completer<void> waiter in waiters) {
        if (!waiter.isCompleted) waiter.complete();
      }
    });
  }

  /// Completes after the next rendered frame without scheduling one.
  ///
  /// Shares a single registered post-frame callback across concurrent waiters so
  /// timed-out waits never accumulate leaked callbacks.
  Future<void> nextFrame() {
    final Completer<void> completer = Completer<void>();
    _frameWaiters.add(completer);
    _ensureFrameCallback();
    return completer.future;
  }

  /// Waits for the next rendered frame or [timeout], cancelling the timer and
  /// removing the waiter as soon as either completes. Returns `true` if a frame
  /// rendered.
  Future<bool> waitForNextFrame(Duration timeout) {
    if (timeout <= Duration.zero) return Future<bool>.value(false);
    final Completer<void> waiter = Completer<void>();
    final Completer<bool> result = Completer<bool>();
    _frameWaiters.add(waiter);
    _ensureFrameCallback();
    final Timer timer = Timer(timeout, () {
      _frameWaiters.remove(waiter);
      if (!result.isCompleted) result.complete(false);
    });
    unawaited(
      waiter.future.then((_) {
        timer.cancel();
        if (!result.isCompleted) result.complete(true);
      }),
    );
    return result.future;
  }

  /// Captures the current idleness and animation diagnostics (`busy` RPC).
  BusyReport busyReport() => BusyReport(
    framesIdle: framesIdle,
    loadingIdle: loadingIdle,
    transientCallbacks: SchedulerBinding.instance.transientCallbackCount,
    httpInFlight: _httpTracker.inFlight,
    pendingImages: PaintingBinding.instance.imageCache.pendingImageCount,
    animations: _animationScanner.scan(treeHash: _computeSemanticsHash()),
  );

  /// Waits until the UI settles or [timeout] elapses.
  ///
  /// Returns `'idle'`, `'stable'`, `'cancelled'`, or `'timeout'`.
  Future<String> waitForIdle(
    Duration timeout, {
    SettlePolicy policy = SettlePolicy.v2,
    int? stableFrames,
    bool includeScrollables = true,
    bool Function()? isCancelled,
  }) async {
    final int needStable =
        stableFrames ??
        (policy == SettlePolicy.legacy ? legacyStableFrames : v2StableFrames);
    final Stopwatch sw = Stopwatch()..start();

    // Immediately after launch nothing is scheduled yet, which would look idle.
    await _waitForFirstFrame(timeout, isCancelled: isCancelled);
    if (isCancelled != null && isCancelled()) return 'cancelled';

    await _keyboardWaiter.waitIfPending(sw, timeout, isCancelled: isCancelled);
    if (isCancelled != null && isCancelled()) return 'cancelled';

    int? lastHash;
    int stable = 0;
    double lastInset = _readBottomInset();
    _keyboardWaiter.observeBottomInset(lastInset);

    while (sw.elapsed < timeout) {
      if (isCancelled != null && isCancelled()) return 'cancelled';

      if (framesIdle && loadingIdle) {
        // Yield a zero-duration timer so pending microtasks/callbacks (such as
        // a setState after a future completes or TextInput.show after focus)
        // run before declaring the app idle.
        await Future<void>.delayed(Duration.zero);
        if (isCancelled != null && isCancelled()) return 'cancelled';
        if (_keyboardWaiter.hasPendingChange()) {
          await _keyboardWaiter.waitIfPending(
            sw,
            timeout,
            isCancelled: isCancelled,
          );
          if (isCancelled != null && isCancelled()) return 'cancelled';
          lastInset = _readBottomInset();
          lastHash = null;
          stable = 0;
          continue;
        }
        // Safeguard for late-starting soft keyboards (e.g. cold Gboard): any
        // viewInsets.bottom change between checks must finish stabilizing.
        final double nowInset = _readBottomInset();
        if (nowInset != lastInset) {
          _keyboardWaiter.observeBottomInset(nowInset);
          await _keyboardWaiter.waitUntilInsetStable(
            sw,
            timeout,
            isCancelled: isCancelled,
          );
          if (isCancelled != null && isCancelled()) return 'cancelled';
          lastInset = _readBottomInset();
          lastHash = null;
          stable = 0;
          continue;
        }
        if (framesIdle && loadingIdle) {
          _animationScanner.clearTracking();
          return 'idle';
        }
        continue;
      }

      final Duration remaining = timeout - sw.elapsed;
      if (remaining <= Duration.zero) break;

      if (!framesIdle) {
        final bool rendered = await waitForNextFrame(
          remaining < _frameStepCap ? remaining : _frameStepCap,
        );
        if (isCancelled != null && isCancelled()) return 'cancelled';
        if (_keyboardWaiter.hasPendingChange()) {
          await _keyboardWaiter.waitIfPending(
            sw,
            timeout,
            isCancelled: isCancelled,
          );
          if (isCancelled != null && isCancelled()) return 'cancelled';
          lastInset = _readBottomInset();
          lastHash = null;
          stable = 0;
          continue;
        }
        final double nowInset = _readBottomInset();
        if (nowInset != lastInset) {
          _keyboardWaiter.observeBottomInset(nowInset);
          lastInset = nowInset;
          stable = 0;
          continue;
        }
        if (!rendered) continue;

        if (loadingIdle) {
          final int hash = _computeSemanticsHash();
          stable = hash == lastHash ? stable + 1 : 1;
          lastHash = hash;
          // Rescan every time (never cache a blocking animation): an animation
          // inside a route that was just covered is muted by TickerMode, stays
          // "animating" forever, and must stop blocking immediately. Passing
          // `treeHash` lets endless animations (> 1.5 s with unchanged tree)
          // stop blocking settling.
          if (stable >= needStable &&
              (policy == SettlePolicy.legacy ||
                  !_animationScanner.hasSignificantAnimation(
                    treeHash: hash,
                    includeScrollables: includeScrollables,
                  ))) {
            return 'stable';
          }
        } else {
          stable = 0;
        }
      } else {
        // Frames are idle, but HTTP requests or image loads are still pending.
        await _waitForLoadingStep(remaining, isCancelled: isCancelled);
      }
    }
    return 'timeout';
  }

  Future<void> _waitForLoadingStep(
    Duration maxWait, {
    bool Function()? isCancelled,
  }) {
    if (maxWait <= Duration.zero || (isCancelled != null && isCancelled())) {
      return Future<void>.value();
    }
    // When only HTTP requests are pending, wake immediately upon drain.
    if (!_httpTracker.isIdle &&
        PaintingBinding.instance.imageCache.pendingImageCount == 0) {
      return _httpTracker.waitForIdle(maxWait, isCancelled: isCancelled);
    }
    final Duration step = maxWait < _imagePollStep ? maxWait : _imagePollStep;
    return Future<void>.delayed(step);
  }

  Future<void> _waitForFirstFrame(
    Duration timeout, {
    bool Function()? isCancelled,
  }) async {
    if (isCancelled != null && isCancelled()) return;
    final Future<void> Function(Duration timeout)? override =
        _waitForFirstFrameOverride;
    if (override != null) {
      if (isCancelled == null) {
        await override(timeout);
        return;
      }
      await _awaitCancellable(override(timeout), timeout, isCancelled);
      return;
    }
    if (WidgetsBinding.instance.firstFrameRasterized) return;
    if (isCancelled == null) {
      await WidgetsBinding.instance.waitUntilFirstFrameRasterized.timeout(
        timeout,
        onTimeout: () {},
      );
      return;
    }
    await _awaitCancellable(
      WidgetsBinding.instance.waitUntilFirstFrameRasterized,
      timeout,
      isCancelled,
    );
  }

  static Future<void> _awaitCancellable(
    Future<void> future,
    Duration timeout,
    bool Function() isCancelled,
  ) {
    if (isCancelled() || timeout <= Duration.zero) {
      return Future<void>.value();
    }
    final Completer<void> done = Completer<void>();
    Timer? poll;
    Timer? deadline;
    void finish() {
      poll?.cancel();
      deadline?.cancel();
      if (!done.isCompleted) done.complete();
    }

    deadline = Timer(timeout, finish);
    poll = Timer.periodic(_cancelPollInterval, (_) {
      if (isCancelled()) finish();
    });
    unawaited(
      future.then<void>(
        (_) => finish(),
        onError: (Object _, StackTrace _) => finish(),
      ),
    );
    return done.future;
  }

  static double _defaultBottomInset() {
    final ui.PlatformDispatcher dispatcher =
        WidgetsBinding.instance.platformDispatcher;
    return (dispatcher.implicitView ?? dispatcher.views.firstOrNull)
            ?.viewInsets
            .bottom ??
        0;
  }
}
