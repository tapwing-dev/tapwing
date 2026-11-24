import 'dart:ui' as ui;

import 'package:flutter/rendering.dart';

import '../input/text_input.dart';

/// Waits for native Android soft-keyboard show/hide animations to settle.
///
/// The soft keyboard animates outside Flutter; the framework only observes
/// `FlutterView.viewInsets.bottom` changing, often several frames after
/// `TextInput.show` or `TextInput.hide` is sent. This waiter watches for:
/// 1. Phase 1: the inset change starting within [animationStartTimeout] (700 ms)
///    of the request (or [absentStartTimeout] = 120 ms once a hardware-keyboard
///    session is detected or hinted by the CLI).
/// 2. Phase 2: `viewInsets.bottom` remaining constant for [stableWindow]
///    (48 ms, ~3 frames).
///
/// Safeguards against slow cold-starting IMEs (e.g. Gboard 150–400 ms):
/// - The absent-keyboard latch only activates after [absentLatchThreshold] (2)
///   consecutive misses, and never for `readOnly` or `TextInputType.none`
///   clients.
/// - Any observed non-zero `viewInsets.bottom` immediately clears the latch
///   and resets the miss counter, and [IdleDetector] separately watches for
///   late `viewInsets.bottom` changes inside the main settle loop via
///   [waitUntilInsetStable].
///
/// Transitions that have already been waited on are deduplicated so subsequent
/// commands within 1 s do not re-pay the inset-stability wait.
class KeyboardWaiter {
  /// Creates a keyboard transition waiter.
  KeyboardWaiter(
    this._textInput,
    this._clock,
    this._waitForNextFrame, {
    double Function()? readBottomInset,
  }) : _readBottomInset = readBottomInset ?? _defaultBottomInset;

  /// Window after a keyboard show/hide request during which it is considered
  /// potentially still in progress.
  static const Duration recentChangeWindow = Duration(seconds: 1);

  /// Tolerance when comparing computed change timestamps for deduplication.
  static const Duration dedupTolerance = Duration(milliseconds: 2);

  /// Maximum time since the show/hide request to wait for insets to begin
  /// moving (phase 1).
  static const Duration animationStartTimeout = Duration(milliseconds: 700);

  /// Start window once the soft keyboard is known (or hinted) to be absent.
  static const Duration absentStartTimeout = Duration(milliseconds: 120);

  /// Consecutive show-request misses required before latching
  /// `_softKeyboardAbsent`, guarding against a single slow cold IME start.
  static const int absentLatchThreshold = 2;

  /// Required duration of unchanged bottom insets before considering the
  /// keyboard animation finished (phase 2, ~3 frames at 60 Hz).
  static const Duration stableWindow = Duration(milliseconds: 48);

  /// Frame wait slice used while observing `viewInsets.bottom`.
  static const Duration pollInterval = Duration(milliseconds: 16);

  final TextInputController _textInput;
  final Stopwatch _clock;
  final Future<bool> Function(Duration timeout) _waitForNextFrame;
  final double Function() _readBottomInset;

  bool? _settledRequested;
  Duration? _settledChangeAt;
  bool _softKeyboardAbsent = false;
  int _consecutiveMisses = 0;

  /// Whether the adaptive window is currently latched to [absentStartTimeout].
  bool get softKeyboardAbsent => _softKeyboardAbsent;

  /// Applies an optional CLI hint (`softKeyboard: false` when a hardware
  /// keyboard suppresses the soft IME).
  void configure({bool? softKeyboard}) {
    if (softKeyboard == false) {
      _softKeyboardAbsent = true;
      _consecutiveMisses = absentLatchThreshold;
    } else if (softKeyboard == true) {
      _softKeyboardAbsent = false;
      _consecutiveMisses = 0;
    }
  }

  /// Clears the absent-keyboard latch whenever a non-zero bottom inset appears
  /// (e.g. a late Gboard animation starting after phase 1 timed out).
  void observeBottomInset(double bottomInset) {
    if (bottomInset > 0) {
      _softKeyboardAbsent = false;
      _consecutiveMisses = 0;
    }
  }

  /// Returns `true` if a recent keyboard show/hide request has not yet been
  /// waited through to completion.
  bool hasPendingChange() {
    final Duration since = _textInput.sinceKeyboardChange;
    if (since >= recentChangeWindow) return false;
    final Duration changeAt = _clock.elapsed - since;
    if (_settledRequested == _textInput.keyboardRequested &&
        _settledChangeAt != null &&
        changeAt <= _settledChangeAt! + dedupTolerance) {
      return false;
    }
    return true;
  }

  /// Waits for a pending keyboard transition to finish, bounded by [timeout]
  /// measured against [elapsed], unwinding immediately if [isCancelled] turns
  /// `true`.
  Future<void> waitIfPending(
    Stopwatch elapsed,
    Duration timeout, {
    bool Function()? isCancelled,
  }) async {
    if (isCancelled != null && isCancelled()) return;
    if (!hasPendingChange()) return;

    final bool showing = _textInput.keyboardRequested;
    final Duration startChangeAt =
        _clock.elapsed - _textInput.sinceKeyboardChange;
    bool reached() =>
        showing ? _readBottomInset() > 0 : _readBottomInset() == 0;

    // Phase 1: wait for the inset animation to start, bounded by startBy.
    final double initial = _readBottomInset();
    observeBottomInset(initial);
    final Duration startBy = _softKeyboardAbsent
        ? absentStartTimeout
        : animationStartTimeout;
    while (!reached() &&
        elapsed.elapsed < timeout &&
        _textInput.sinceKeyboardChange < startBy) {
      if (isCancelled != null && isCancelled()) return;
      await _waitForNextFrame(pollInterval);
      if (isCancelled != null && isCancelled()) return;
    }
    final double afterPhase1 = _readBottomInset();
    observeBottomInset(afterPhase1);
    if (!reached() && afterPhase1 == initial) {
      if (showing && !_textInput.suppressesSoftKeyboard) {
        _consecutiveMisses++;
        if (_consecutiveMisses >= absentLatchThreshold) {
          _softKeyboardAbsent = true;
        }
      }
      _markSettled(showing, startChangeAt);
      return;
    }
    if (showing) {
      _softKeyboardAbsent = false;
      _consecutiveMisses = 0;
    }

    // Phase 2: wait until viewInsets.bottom stays unchanged for stableWindow.
    await waitUntilInsetStable(elapsed, timeout, isCancelled: isCancelled);
    if (isCancelled != null && isCancelled()) return;
    _markSettled(showing, startChangeAt);
  }

  /// Waits until `viewInsets.bottom` stops changing for [stableWindow],
  /// bounded by [timeout] measured against [elapsed], unwinding immediately if
  /// [isCancelled] turns `true`.
  Future<void> waitUntilInsetStable(
    Stopwatch elapsed,
    Duration timeout, {
    bool Function()? isCancelled,
  }) async {
    if (isCancelled != null && isCancelled()) return;
    final Stopwatch phase = Stopwatch()..start();
    Duration stableSince = phase.elapsed;
    double last = _readBottomInset();
    observeBottomInset(last);
    while (elapsed.elapsed < timeout &&
        phase.elapsed - stableSince < stableWindow) {
      if (isCancelled != null && isCancelled()) return;
      await _waitForNextFrame(pollInterval);
      if (isCancelled != null && isCancelled()) return;
      final double now = _readBottomInset();
      observeBottomInset(now);
      if (now != last) {
        last = now;
        stableSince = phase.elapsed;
      }
    }
  }

  void _markSettled(bool showing, Duration changeAt) {
    _settledRequested = showing;
    _settledChangeAt = changeAt;
  }

  static double _defaultBottomInset() {
    final ui.PlatformDispatcher dispatcher =
        RendererBinding.instance.platformDispatcher;
    return (dispatcher.implicitView ?? dispatcher.views.firstOrNull)
            ?.viewInsets
            .bottom ??
        0;
  }
}
