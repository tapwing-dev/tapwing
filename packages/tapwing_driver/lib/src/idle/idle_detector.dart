import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

import '../input/text_input.dart';
import '../snapshot/snapshot.dart';
import 'animation_scanner.dart';
import 'channel_tracker.dart';
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
    this.pendingChannels = 0,
  });

  /// Whether no frame is scheduled, no tickers are active, and the scheduler
  /// is idle.
  final bool framesIdle;

  /// Whether no tracked HTTP request, platform-channel reply, or image load
  /// currently blocks settle (HTTP requests and image loads older than the
  /// settle load bound and ignored URLs do not).
  final bool loadingIdle;

  /// Number of active transient frame callbacks (tickers/animations).
  final int transientCallbacks;

  /// Number of in-flight `dart:io` HTTP requests that block settle.
  final int httpInFlight;

  /// Number of non-system platform-channel calls currently awaiting a reply.
  final int pendingChannels;

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
    'pendingChannels': pendingChannels,
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
/// flight, no non-system platform-channel replies are pending, and no images
/// are pending.
///
/// Known gap: native FFI/JNI HTTP clients (`cupertino_http`, `cronet_http`)
/// bypass `dart:io` [HttpOverrides] and do not use `BinaryMessenger`, so their
/// in-flight requests are not observed by [HttpTracker] or [ChannelTracker].
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
    ChannelTracker? channelTracker,
    TextInputController? textInput,
    AnimationScanner? animationScanner,
    int Function()? computeSemanticsHash,
    double Function()? readBottomInset,
    ui.AppLifecycleState? Function()? readLifecycleState,
    Future<void> Function(Duration timeout)? waitForFirstFrame,
    Duration Function()? clock,
    int Function()? readPendingImages,
  }) : _httpTracker = httpTracker ?? HttpTracker.instance,
       _channelTracker = channelTracker ?? ChannelTracker.instance,
       _animationScanner = animationScanner ?? AnimationScanner(clock: clock),
       _computeSemanticsHash = computeSemanticsHash ?? Snapshot.computeHash,
       _readBottomInset = readBottomInset ?? _defaultBottomInset,
       _readLifecycleState = readLifecycleState ?? _defaultLifecycleState,
       _readPendingImages = readPendingImages ?? _defaultPendingImages,
       _nowOverride = clock,
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
  final ChannelTracker _channelTracker;
  final AnimationScanner _animationScanner;
  final int Function() _computeSemanticsHash;
  final double Function() _readBottomInset;
  final ui.AppLifecycleState? Function() _readLifecycleState;
  final int Function() _readPendingImages;
  final Duration Function()? _nowOverride;
  final Future<void> Function(Duration timeout)? _waitForFirstFrameOverride;
  late final KeyboardWaiter _keyboardWaiter;

  final Set<Completer<void>> _frameWaiters = <Completer<void>>{};
  bool _frameCallbackScheduled = false;

  /// Number of tracked `dart:io` HTTP requests currently in flight.
  int get httpInFlight => _httpTracker.inFlight;

  /// Number of non-system platform-channel calls currently awaiting a reply.
  int get pendingChannels => _channelTracker.pending;

  /// HTTP tracker observing `dart:io` traffic (also owns the F8b request log).
  HttpTracker get httpTracker => _httpTracker;

  /// Platform-channel tracker observing pending `BinaryMessenger.send` replies.
  ChannelTracker get channelTracker => _channelTracker;

  /// Whether the adaptive keyboard waiter currently treats the soft keyboard
  /// as absent (hardware-keyboard device or CLI hint).
  bool get softKeyboardAbsent => _keyboardWaiter.softKeyboardAbsent;

  /// How long a single HTTP request or a set of pending images counts as
  /// busy for settle (`null`: until it ends). See [HttpTracker.settleBound].
  Duration? get settleLoadBound => _httpTracker.settleBound;

  /// Applies session-level hints from the `configure` RPC.
  void configure({bool? softKeyboard}) {
    _keyboardWaiter.configure(softKeyboard: softKeyboard);
  }

  /// Applies the `configure` settle load options: [timeoutMs] bounds how long
  /// one HTTP request or pending image blocks settle (`0`: no bound), and
  /// [httpIgnore] replaces the URL patterns that never block it.
  void configureSettleLoad({int? timeoutMs, List<RegExp>? httpIgnore}) {
    if (timeoutMs != null) {
      _httpTracker.settleBound = timeoutMs <= 0
          ? null
          : Duration(milliseconds: timeoutMs);
    }
    if (httpIgnore != null) {
      _httpTracker.settleIgnore = httpIgnore;
    }
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

  /// Whether no tracked HTTP request, platform-channel reply, or image load
  /// currently blocks settle.
  ///
  /// Reading it samples the pending image count (see [_imagesIdle]).
  bool get loadingIdle =>
      _httpTracker.isIdle && _channelTracker.isIdle && _imagesIdle;

  int _imagesSeen = 0;
  int _imagesStale = 0;
  Duration _imagesSince = Duration.zero;

  /// Whether no pending image blocks settle.
  ///
  /// `ImageCache` exposes only a count, so the age of pending images is
  /// tracked from what successive reads see: a higher count means a load
  /// just started. Once the count has not grown for [settleLoadBound], the
  /// images pending at that moment are written off as stale and stop
  /// blocking (an image that never resolves would otherwise make every step
  /// wait for its whole settle timeout). Stale images that do finish later
  /// are forgotten again.
  bool get _imagesIdle {
    final int count = _readPendingImages();
    if (count == 0) {
      _imagesSeen = 0;
      _imagesStale = 0;
      return true;
    }
    final Duration now = _nowOverride?.call() ?? _clock.elapsed;
    if (count > _imagesSeen) {
      _imagesSince = now;
    } else if (_imagesStale > count) {
      _imagesStale = count;
    }
    _imagesSeen = count;
    final Duration? bound = settleLoadBound;
    if (bound == null) {
      _imagesStale = 0;
    } else if (now - _imagesSince >= bound) {
      _imagesStale = count;
    }
    return count <= _imagesStale;
  }

  static int _defaultPendingImages() =>
      PaintingBinding.instance.imageCache.pendingImageCount;

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

  /// Whether the app has left [ui.AppLifecycleState.resumed] (for example,
  /// paused or inactive while a system permission dialog or external activity
  /// holds foreground focus).
  ///
  /// Treats `null` (before the engine reports an initial lifecycle state,
  /// including default `TestWidgetsFlutterBinding` tests) as resumed.
  bool get _isAppPaused {
    final ui.AppLifecycleState? state = _readLifecycleState();
    return state != null && state != ui.AppLifecycleState.resumed;
  }

  /// Waits for the next rendered frame or [timeout], cancelling the timer and
  /// removing the waiter as soon as either completes or [cancelSignal] fires.
  /// Returns `true` if a frame rendered.
  Future<bool> waitForNextFrame(
    Duration timeout, {
    Future<void>? cancelSignal,
  }) {
    if (timeout <= Duration.zero) return Future<bool>.value(false);
    final Completer<void> waiter = Completer<void>();
    final Completer<bool> result = Completer<bool>();
    _frameWaiters.add(waiter);
    _ensureFrameCallback();
    final Timer timer = Timer(timeout, () {
      _frameWaiters.remove(waiter);
      if (!result.isCompleted) result.complete(false);
    });
    if (cancelSignal != null) {
      unawaited(
        cancelSignal.then(
          (_) {
            timer.cancel();
            _frameWaiters.remove(waiter);
            if (!result.isCompleted) result.complete(false);
          },
          onError: (Object _, StackTrace _) {
            timer.cancel();
            _frameWaiters.remove(waiter);
            if (!result.isCompleted) result.complete(false);
          },
        ),
      );
    }
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
    pendingChannels: _channelTracker.pending,
    pendingImages: _readPendingImages(),
    animations: _animationScanner.scan(treeHash: _computeSemanticsHash()),
  );

  /// Waits until the UI settles or [timeout] elapses.
  ///
  /// Returns `'idle'`, `'stable'`, `'background'` (when the OS pauses or
  /// deactivates the app, e.g. for a system permission dialog), `'cancelled'`,
  /// or `'timeout'`.
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

    bool stopWaiting() =>
        (isCancelled != null && isCancelled()) || _isAppPaused;

    // Immediately after launch nothing is scheduled yet, which would look idle.
    await _waitForFirstFrame(timeout, isCancelled: stopWaiting);
    if (isCancelled != null && isCancelled()) return 'cancelled';
    if (_isAppPaused) return 'background';

    final Completer<void> pauseSignal = Completer<void>();
    final _LifecyclePauseObserver lifecycleObserver = _LifecyclePauseObserver(
      () {
        if (_isAppPaused && !pauseSignal.isCompleted) {
          pauseSignal.complete();
        }
      },
    );
    WidgetsBinding.instance.addObserver(lifecycleObserver);
    try {
      await _keyboardWaiter.waitIfPending(
        sw,
        timeout,
        isCancelled: stopWaiting,
      );
      if (isCancelled != null && isCancelled()) return 'cancelled';
      if (_isAppPaused) return 'background';

      int? lastHash;
      int stable = 0;
      double lastInset = _readBottomInset();
      _keyboardWaiter.observeBottomInset(lastInset);

      while (sw.elapsed < timeout) {
        if (isCancelled != null && isCancelled()) return 'cancelled';
        if (_isAppPaused) return 'background';

        if (framesIdle && loadingIdle) {
          // Yield a zero-duration timer so pending microtasks/callbacks (such as
          // a setState after a future completes or TextInput.show after focus)
          // run before declaring the app idle.
          await Future<void>.delayed(Duration.zero);
          if (isCancelled != null && isCancelled()) return 'cancelled';
          if (_isAppPaused) return 'background';
          if (_keyboardWaiter.hasPendingChange()) {
            await _keyboardWaiter.waitIfPending(
              sw,
              timeout,
              isCancelled: stopWaiting,
            );
            if (isCancelled != null && isCancelled()) return 'cancelled';
            if (_isAppPaused) return 'background';
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
              isCancelled: stopWaiting,
            );
            if (isCancelled != null && isCancelled()) return 'cancelled';
            if (_isAppPaused) return 'background';
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
            cancelSignal: pauseSignal.future,
          );
          if (isCancelled != null && isCancelled()) return 'cancelled';
          if (_isAppPaused) return 'background';
          if (_keyboardWaiter.hasPendingChange()) {
            await _keyboardWaiter.waitIfPending(
              sw,
              timeout,
              isCancelled: stopWaiting,
            );
            if (isCancelled != null && isCancelled()) return 'cancelled';
            if (_isAppPaused) return 'background';
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
          // Frames are idle, but HTTP requests, platform-channel replies, or
          // image loads are still pending.
          await _waitForLoadingStep(
            remaining,
            isCancelled: stopWaiting,
            cancelSignal: pauseSignal.future,
          );
          if (isCancelled != null && isCancelled()) return 'cancelled';
          if (_isAppPaused) return 'background';
        }
      }
      return 'timeout';
    } finally {
      WidgetsBinding.instance.removeObserver(lifecycleObserver);
    }
  }

  Future<void> _waitForLoadingStep(
    Duration maxWait, {
    bool Function()? isCancelled,
    Future<void>? cancelSignal,
  }) {
    if (maxWait <= Duration.zero || (isCancelled != null && isCancelled())) {
      return Future<void>.value();
    }
    final bool imagesIdle = _imagesIdle;
    // When only HTTP requests or platform-channel replies are pending (and no
    // fresh image is still blocking), wake immediately upon drain, when the
    // last HTTP request reaches the settle load bound, or when the app pauses
    // for a system dialog / background transition.
    if (!_httpTracker.isIdle && imagesIdle) {
      return _httpTracker.waitForIdle(
        maxWait,
        isCancelled: isCancelled,
        cancelSignal: cancelSignal,
      );
    }
    if (!_channelTracker.isIdle && imagesIdle) {
      return _channelTracker.waitForIdle(
        maxWait,
        isCancelled: isCancelled,
        cancelSignal: cancelSignal,
      );
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

  static ui.AppLifecycleState? _defaultLifecycleState() =>
      SchedulerBinding.instance.lifecycleState;

  static double _defaultBottomInset() {
    final ui.PlatformDispatcher dispatcher =
        WidgetsBinding.instance.platformDispatcher;
    return (dispatcher.implicitView ?? dispatcher.views.firstOrNull)
            ?.viewInsets
            .bottom ??
        0;
  }
}

class _LifecyclePauseObserver with WidgetsBindingObserver {
  _LifecyclePauseObserver(this._onLifecycleChange);

  final VoidCallback _onLifecycleChange;

  @override
  void didChangeAppLifecycleState(ui.AppLifecycleState state) {
    if (state != ui.AppLifecycleState.resumed) {
      _onLifecycleChange();
    }
  }
}
