import 'dart:ui' as ui;

import '../input/text_input.dart';

/// Waits for native Android soft-keyboard show/hide animations to settle.
///
/// The soft keyboard animates outside Flutter; the framework only observes
/// `FlutterView.viewInsets.bottom` changing, often several frames after
/// `TextInput.show` or `TextInput.hide` is sent. This waiter watches for:
/// 1. Phase 1: the inset change starting within 700 ms of the request (bounded
///    so devices without a soft keyboard never block indefinitely).
/// 2. Phase 2: `viewInsets.bottom` remaining constant for 64 ms (~4 frames).
///
/// Transitions that have already been waited on are deduplicated so subsequent
/// commands within 1 s do not re-pay the 64 ms inset-stability wait.
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

  /// Required duration of unchanged bottom insets before considering the
  /// keyboard animation finished (phase 2).
  static const Duration stableWindow = Duration(milliseconds: 64);

  /// Frame wait slice used while observing `viewInsets.bottom`.
  static const Duration pollInterval = Duration(milliseconds: 16);

  final TextInputController _textInput;
  final Stopwatch _clock;
  final Future<bool> Function(Duration timeout) _waitForNextFrame;
  final double Function() _readBottomInset;

  bool? _settledRequested;
  Duration? _settledChangeAt;

  /// Learned per session: a show request produced no inset change (hardware
  /// keyboard, e.g. the iOS simulator default). Later requests then only get
  /// a short start window instead of [animationStartTimeout]; seeing insets
  /// again clears it.
  bool _softKeyboardAbsent = false;

  /// Start window once the soft keyboard is known to be absent.
  static const Duration absentStartTimeout = Duration(milliseconds: 120);

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
  /// measured against [elapsed].
  Future<void> waitIfPending(Stopwatch elapsed, Duration timeout) async {
    if (!hasPendingChange()) return;

    final bool showing = _textInput.keyboardRequested;
    final Duration startChangeAt =
        _clock.elapsed - _textInput.sinceKeyboardChange;
    bool reached() =>
        showing ? _readBottomInset() > 0 : _readBottomInset() == 0;

    // Phase 1: wait for the inset animation to start, at most 700 ms after the
    // show/hide request was issued.
    final Stopwatch phase = Stopwatch()..start();
    double last = _readBottomInset();
    final Duration startBy = _softKeyboardAbsent
        ? absentStartTimeout
        : animationStartTimeout;
    while (!reached() &&
        elapsed.elapsed < timeout &&
        _textInput.sinceKeyboardChange < startBy) {
      await _waitForNextFrame(pollInterval);
    }
    if (!reached() && _readBottomInset() == last) {
      if (showing) _softKeyboardAbsent = true;
      _markSettled(showing, startChangeAt);
      return;
    }
    if (showing) _softKeyboardAbsent = false;

    // Phase 2: wait until viewInsets.bottom stays unchanged for 64 ms.
    Duration stableSince = phase.elapsed;
    last = _readBottomInset();
    while (elapsed.elapsed < timeout &&
        phase.elapsed - stableSince < stableWindow) {
      await _waitForNextFrame(pollInterval);
      final double now = _readBottomInset();
      if (now != last) {
        last = now;
        stableSince = phase.elapsed;
      }
    }
    _markSettled(showing, startChangeAt);
  }

  void _markSettled(bool showing, Duration changeAt) {
    _settledRequested = showing;
    _settledChangeAt = changeAt;
  }

  static double _defaultBottomInset() =>
      (ui.PlatformDispatcher.instance.implicitView ??
              ui.PlatformDispatcher.instance.views.firstOrNull)
          ?.viewInsets
          .bottom ??
      0;
}
