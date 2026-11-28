import 'dart:async';

import 'package:flutter/services.dart';

/// Tracks pending outgoing [BinaryMessenger.send] replies on non-system
/// platform channels so [IdleDetector] does not declare the UI idle while an
/// action is awaiting an async plugin reply (for example before `setState` or
/// `Navigator.push`).
///
/// Standard Flutter framework channels (`flutter/*`) are excluded because they
/// are internal engine traffic (including long-lived or fire-and-forget calls),
/// and each tracked call is bounded by [safetyTimeout] so a plugin call that
/// never replies cannot wedge settling forever.
class ChannelTracker {
  /// Creates a platform-channel reply tracker.
  ChannelTracker({this.safetyTimeout = defaultSafetyTimeout});

  /// Shared singleton wired into [ObservingBinaryMessenger] by default.
  static final ChannelTracker instance = ChannelTracker();

  /// Safety cap applied to each pending non-system channel call.
  static const Duration defaultSafetyTimeout = Duration(seconds: 10);

  static const Duration _cancelPollInterval = Duration(milliseconds: 25);

  /// Maximum duration a single pending channel reply can block idleness.
  final Duration safetyTimeout;

  int _started = 0;

  /// Start numbers (see [started]) of the calls still awaiting a reply.
  final Set<int> _open = <int>{};
  final Set<Timer> _safetyTimers = <Timer>{};
  final Set<Completer<void>> _idleWaiters = <Completer<void>>{};

  /// Number of non-system platform-channel calls currently awaiting a reply.
  int get pending => _open.length;

  /// Whether no tracked platform-channel replies are currently pending.
  bool get isIdle => _open.isEmpty;

  /// Number of tracked calls started so far. Read it before an action and
  /// pass it to [hasPendingSince] afterwards.
  int get started => _started;

  /// Whether a tracked call that started after [mark] (an earlier value of
  /// [started]) still awaits its reply.
  ///
  /// Calls that were already pending at [mark], calls on `flutter/*`
  /// channels, and calls released by [safetyTimeout] never count.
  bool hasPendingSince(int mark) => _open.any((int call) => call > mark);

  /// Whether [channel] is a non-system channel whose replies should block
  /// idleness.
  bool tracks(String channel) =>
      channel != 'flutter' && !channel.startsWith('flutter/');

  /// Tracks a non-null [reply] future returned by `BinaryMessenger.send` on
  /// [channel] until it completes, errors, or [safetyTimeout] elapses.
  void track(String channel, Future<ByteData?> reply) {
    if (!tracks(channel)) return;
    final int call = ++_started;
    _open.add(call);
    Timer? timer;
    void release() {
      // Also false after [reset], so a late reply cannot touch newer calls.
      if (!_open.remove(call)) return;
      if (timer != null) {
        timer.cancel();
        _safetyTimers.remove(timer);
      }
      if (_open.isEmpty) _wakeWaiters();
    }

    if (safetyTimeout > Duration.zero) {
      timer = Timer(safetyTimeout, release);
      _safetyTimers.add(timer);
    }
    unawaited(
      reply.then<void>(
        (_) => release(),
        onError: (Object _, StackTrace _) => release(),
      ),
    );
  }

  /// Waits until [pending] drops to zero, [maxWait] elapses, [isCancelled]
  /// returns `true`, or [cancelSignal] completes.
  Future<void> waitForIdle(
    Duration maxWait, {
    bool Function()? isCancelled,
    Future<void>? cancelSignal,
  }) {
    if (_open.isEmpty ||
        maxWait <= Duration.zero ||
        (isCancelled != null && isCancelled())) {
      return Future<void>.value();
    }
    final Completer<void> waiter = Completer<void>();
    _idleWaiters.add(waiter);
    Timer? cancelPoll;
    late final Timer timer;
    void cleanup() {
      _idleWaiters.remove(waiter);
      timer.cancel();
      cancelPoll?.cancel();
    }

    void finish() {
      cleanup();
      if (!waiter.isCompleted) waiter.complete();
    }

    timer = Timer(maxWait, finish);
    if (isCancelled != null) {
      cancelPoll = Timer.periodic(_cancelPollInterval, (_) {
        if (isCancelled()) finish();
      });
    }
    if (cancelSignal != null) {
      unawaited(
        cancelSignal.then(
          (_) => finish(),
          onError: (Object _, StackTrace _) => finish(),
        ),
      );
    }
    return waiter.future.whenComplete(cleanup);
  }

  /// Cancels all active safety timers and wakes any idle waiters (for tests).
  void reset() {
    for (final Timer timer in _safetyTimers) {
      timer.cancel();
    }
    _safetyTimers.clear();
    _open.clear();
    _wakeWaiters();
  }

  void _wakeWaiters() {
    if (_idleWaiters.isEmpty) return;
    final List<Completer<void>> waiters = _idleWaiters.toList(growable: false);
    _idleWaiters.clear();
    for (final Completer<void> waiter in waiters) {
      if (!waiter.isCompleted) {
        waiter.complete();
      }
    }
  }
}
