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

  int _pending = 0;
  final Set<Timer> _safetyTimers = <Timer>{};
  final Set<Completer<void>> _idleWaiters = <Completer<void>>{};

  /// Number of non-system platform-channel calls currently awaiting a reply.
  int get pending => _pending;

  /// Whether no tracked platform-channel replies are currently pending.
  bool get isIdle => _pending == 0;

  /// Whether [channel] is a non-system channel whose replies should block
  /// idleness.
  bool tracks(String channel) =>
      channel != 'flutter' && !channel.startsWith('flutter/');

  /// Tracks a non-null [reply] future returned by `BinaryMessenger.send` on
  /// [channel] until it completes, errors, or [safetyTimeout] elapses.
  void track(String channel, Future<ByteData?> reply) {
    if (!tracks(channel)) return;
    _pending++;
    bool released = false;
    Timer? timer;
    void release() {
      if (released) return;
      released = true;
      if (timer != null) {
        timer.cancel();
        _safetyTimers.remove(timer);
      }
      _onCallFinished();
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

  /// Waits until [pending] drops to zero, [maxWait] elapses, or [isCancelled]
  /// returns `true`.
  Future<void> waitForIdle(Duration maxWait, {bool Function()? isCancelled}) {
    if (_pending == 0 ||
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

    timer = Timer(maxWait, () {
      cleanup();
      if (!waiter.isCompleted) waiter.complete();
    });
    if (isCancelled != null) {
      cancelPoll = Timer.periodic(_cancelPollInterval, (_) {
        if (isCancelled()) {
          cleanup();
          if (!waiter.isCompleted) waiter.complete();
        }
      });
    }
    return waiter.future.whenComplete(cleanup);
  }

  /// Cancels all active safety timers and wakes any idle waiters (for tests).
  void reset() {
    for (final Timer timer in _safetyTimers) {
      timer.cancel();
    }
    _safetyTimers.clear();
    _pending = 0;
    _wakeWaiters();
  }

  void _onCallFinished() {
    _pending--;
    if (_pending <= 0) {
      _pending = 0;
      _wakeWaiters();
    }
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
