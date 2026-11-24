import 'dart:ui' as ui;

import 'package:flutter/gestures.dart';

import '../protocol/rpc_error.dart';
import '../protocol/rpc_messages.dart';
import '../selector/selector.dart';
import '../snapshot/snapshot.dart';
import 'driver_context.dart';

/// Pointer and gesture RPC handlers (`tapOn`, `tapPoint`, `swipe`,
/// `scrollUntilVisible`) plus shared re-aim, visibility, and retry helpers.
class GestureHandlers {
  /// Creates gesture handlers operating against [_context].
  const GestureHandlers(this._context);

  /// Required stable frames after each `scrollUntilVisible` drag step.
  static const int _scrollStepStableFrames = 2;

  /// Wall-clock duration for accelerated drags (`swipe`, `scroll`,
  /// `scrollUntilVisible`). Pointer timestamps still span the full logical
  /// `duration` so Flutter's `VelocityTracker` computes the exact same fling
  /// velocity as Maestro's real-time `input swipe`.
  static const Duration _acceleratedDragWallDuration = Duration(
    milliseconds: 16,
  );

  /// Initial reactive lookup window before the first swipe in
  /// `scrollUntilVisible` when the UI is still busy (`Orchestra.kt:867`).
  static const Duration _initialScrollLookupCap = Duration(milliseconds: 500);

  /// Maximum extra swipes to center an already-visible element in
  /// `scrollUntilVisible` with `centerElement: true` (`Orchestra.kt:862`).
  static const int _maxRetryCenterCount = 4;

  static const Duration _visibilityPollCap = Duration(milliseconds: 100);

  final DriverContext _context;

  /// Handles `tapOn`: locates the selector, settles, re-aims, taps, and
  /// optionally waits until unobstructed if `waitUntilVisible` is set.
  Future<Object?> handleTapOn(Map<String, Object?> raw) async {
    final TapOnParams params = TapOnParams.fromJson(raw);
    final Stopwatch sw = Stopwatch()..start();

    final KNode? found = await _context.waitFor(
      params.selector,
      params.timeout,
    );
    if (found == null) {
      throw _context.elementNotFound(params.selector);
    }

    // Maestro.tap (`Maestro.kt:233-251`): settle first, then re-aim with the
    // element's refreshed bounds (it may have been found mid-transition).
    await _context.settle(params.settleTimeout);
    final Snapshot now = _context.captureSnapshot();
    KNode target = _reaim(found, params.selector, now);

    final ui.Offset tapPoint = params.relativePoint != null
        ? _context.resolveElementPoint(target, params.relativePoint!)
        : target.bounds.center;

    final int findUs = sw.elapsedMicroseconds;
    final int logMark = _context.textInput.log.mark;
    final int beforeHash = _context.computeHash();

    ({String settle, bool changed, int actUs, int settleUs}) outcome =
        await _performTap(
          tapPoint,
          beforeHash: beforeHash,
          retryIfNoChange: params.retryIfNoChange,
          longPress: params.longPress,
          repeat: params.repeat,
          delay: params.delay,
          settleTimeout: params.settleTimeout,
          logTap: true,
        );

    // Maestro.tap (`Maestro.kt:262-281`, `Orchestra.kt:1402-1425`):
    // waitUntilVisible only applies to non-relative element taps when the first
    // tap caused no hierarchy change and the element is occluded at its center.
    if (params.relativePoint == null &&
        params.waitUntilVisible &&
        !outcome.changed) {
      final Snapshot after = _context.captureSnapshot();
      final KNode refreshed = _reaim(target, params.selector, after);
      if (!after.isVisible(refreshed)) {
        final Snapshot visibleSnap = await _waitUntilNodeVisible(
          refreshed,
          params.selector,
        );
        target = _reaim(refreshed, params.selector, visibleSnap);
        outcome = await _performTap(
          target.bounds.center,
          beforeHash: _context.computeHash(),
          retryIfNoChange: false,
          longPress: params.longPress,
          repeat: params.repeat,
          delay: params.delay,
          settleTimeout: params.settleTimeout,
          logTap: true,
        );
      }
    }

    return TapOnResult(
      element: target,
      settle: outcome.settle,
      timings: TapOnTimings(
        findUs: findUs,
        actUs: outcome.actUs,
        settleUs: outcome.settleUs,
      ),
      ime: _context.textInput.diagnosticsSince(logMark),
    ).toJson();
  }

  /// Handles `tapPoint`: taps an explicit screen coordinate or percentage spec.
  Future<Object?> handleTapPoint(Map<String, Object?> raw) async {
    final TapPointParams params = TapPointParams.fromJson(raw);
    final ui.Offset pt = _context.parsePoint(params.point);
    final int beforeHash = _context.computeHash();
    final ({String settle, bool changed, int actUs, int settleUs}) outcome =
        await _performTap(
          pt,
          beforeHash: beforeHash,
          retryIfNoChange: params.retryIfNoChange,
          longPress: params.longPress,
          repeat: params.repeat,
          delay: params.delay,
          settleTimeout: params.settleTimeout,
          logTap: false,
        );
    return SettleResult(settle: outcome.settle).toJson();
  }

  /// Handles `swipe`: drags between explicit points or from a matched element
  /// in a cardinal direction.
  Future<Object?> handleSwipe(Map<String, Object?> raw) async {
    final SwipeParams params = SwipeParams.fromJson(raw);
    final ui.Offset startPt;
    final ui.Offset endPt;

    if (params.selector != null) {
      // Orchestra.kt:1790-1801 -> AndroidDriver.kt:450-475
      final KNode? found = await _context.waitFor(
        params.selector!,
        params.timeout,
      );
      if (found == null) {
        throw _context.elementNotFound(params.selector!);
      }
      await _context.settle(params.settleTimeout);
      final KNode target = _reaim(
        found,
        params.selector!,
        _context.captureSnapshot(),
      );
      startPt = params.relativePoint != null
          ? _context.resolveElementPoint(target, params.relativePoint!)
          : target.bounds.center;
      final String dir = params.direction ?? '';
      endPt = _directionalSwipeEnd(startPt, dir, _context.readScreenSize());
    } else if (params.start != null && params.end != null) {
      startPt = _context.parsePoint(params.start!);
      endPt = _context.parsePoint(params.end!);
    } else {
      throw DriverError.invalidCommand(
        'swipe requires either start/end coordinates or selector/direction',
      );
    }

    // Virtual-timestamp drag spans full `params.duration` so VelocityTracker
    // computes the exact Maestro fling velocity without burning wall clock.
    await _context.drag(
      startPt,
      endPt,
      params.duration,
      wallDuration: _acceleratedDragWallDuration,
    );
    final String settle = await _context.settle(params.settleTimeout);
    return SettleResult(settle: settle).toJson();
  }

  /// Handles `scrollUntilVisible`: repeatedly swipes until the target selector
  /// meets its visibility and optional centering threshold.
  Future<Object?> handleScrollUntilVisible(Map<String, Object?> raw) async {
    final ScrollUntilVisibleParams params = ScrollUntilVisibleParams.fromJson(
      raw,
    );
    // ScrollDirection.toSwipeDirection (`ScrollDirection.kt:10-15`)
    final String swipeDirection = switch (params.direction) {
      'DOWN' => 'UP',
      'UP' => 'DOWN',
      'LEFT' => 'RIGHT',
      'RIGHT' => 'LEFT',
      _ => throw DriverError.invalidCommand(
        'Invalid scroll direction: ${params.direction}',
      ),
    };

    final Stopwatch sw = Stopwatch()..start();
    int swipes = 0;
    int retryCenterCount = 0;

    KNode? checkReached(Snapshot snap) {
      final KNode? match = resolve(params.selector, snap);
      if (match == null) return null;
      final double visibility = DriverContext.visiblePercent(
        match,
        snap.screen,
      );
      // Orchestra.kt:876-883
      if (params.centerElement &&
          visibility > 0.1 &&
          retryCenterCount <= _maxRetryCenterCount) {
        if (isElementNearScreenCenter(
          match,
          swipeDirection,
          snap.screen.width,
          snap.screen.height,
        )) {
          return match;
        }
        retryCenterCount++;
        return null;
      }
      if (visibility * 100 + 1e-9 >= params.visibilityPercentage) {
        return match;
      }
      return null;
    }

    // Orchestra.kt:864-892: loop checks visibility before each swipe and exits
    // when timeout elapses without an extra post-loop check. Before the first
    // swipe, if the UI is still animating/loading, wait reactively up to 500 ms
    // (0 ms cost when already idle) so async elements aren't swiped past.
    do {
      if (_context.isCancelled()) {
        throw _context.elementNotFound(params.selector);
      }
      final KNode? reached = checkReached(_context.captureSnapshot());
      if (reached != null) {
        if (swipes > 0) {
          // Ensure ballistic scroll activity (`isScrollingNotifier`) has
          // completely ended before returning, so a following `tapOn` is never
          // swallowed by `ScrollActivity.shouldIgnorePointer`.
          await _context.settle(
            params.settleTimeout,
            stableFrames: _scrollStepStableFrames,
          );
          final KNode refreshed = _reaim(
            reached,
            params.selector,
            _context.captureSnapshot(),
          );
          return ScrollUntilVisibleResult(
            element: refreshed,
            swipes: swipes,
          ).toJson();
        }
        return ScrollUntilVisibleResult(
          element: reached,
          swipes: swipes,
        ).toJson();
      }
      if (swipes == 0 && !_context.idle.loadingIdle) {
        final Duration remaining = params.timeout - sw.elapsed;
        final Duration initialCap = remaining < _initialScrollLookupCap
            ? remaining
            : _initialScrollLookupCap;
        if (initialCap > Duration.zero) {
          final KNode? early = await _waitForScrollTargetWhileBusy(
            checkReached,
            initialCap,
          );
          if (early != null) {
            return ScrollUntilVisibleResult(
              element: early,
              swipes: swipes,
            ).toJson();
          }
        }
      }
      if (sw.elapsed >= params.timeout) break;

      // Maestro.swipeFromCenter (`Maestro.kt:203-211`) ->
      // AndroidDriver.swipe(center, direction, durationMs) (`AndroidDriver.kt:450-475`)
      final ui.Size screen = _context.readScreenSize();
      final ui.Offset center = ui.Offset(
        (screen.width.round() ~/ 2).toDouble(),
        (screen.height.round() ~/ 2).toDouble(),
      );
      final ui.Offset end = _directionalSwipeEnd(
        center,
        swipeDirection,
        screen,
      );
      await _context.drag(
        center,
        end,
        params.duration,
        wallDuration: _acceleratedDragWallDuration,
      );
      swipes++;
      // Between intermediate swipes, waiting for the semantics hash to stabilize
      // (2 frames) already waits for the visible fling motion while skipping the
      // sub-pixel tail (the next pointer down catches it cleanly with < 1 px
      // difference). Full scroll-activity settle runs before returning above.
      await _context.settle(
        params.settleTimeout,
        stableFrames: _scrollStepStableFrames,
        includeScrollables: false,
      );
    } while (sw.elapsed < params.timeout);

    throw _context.elementNotFound(params.selector);
  }

  /// Reactively checks [checkReached] while frames or loads are active, up to
  /// [cap], returning immediately once the UI goes idle or the target appears.
  Future<KNode?> _waitForScrollTargetWhileBusy(
    KNode? Function(Snapshot) checkReached,
    Duration cap,
  ) async {
    final Stopwatch waitSw = Stopwatch()..start();
    while (waitSw.elapsed < cap) {
      if (_context.isCancelled()) return null;
      if (_context.idle.framesIdle && _context.idle.loadingIdle) {
        return checkReached(_context.captureSnapshot());
      }
      final Duration left = cap - waitSw.elapsed;
      await _context.idle.waitForNextFrame(
        left < _visibilityPollCap ? left : _visibilityPollCap,
      );
      final KNode? found = checkReached(_context.captureSnapshot());
      if (found != null) return found;
    }
    return null;
  }

  KNode _reaim(KNode original, Selector selector, Snapshot snap) =>
      snap.nodes.where((KNode node) => node.id == original.id).firstOrNull ??
      resolve(selector, snap) ??
      original;

  /// Executes tap(s) with Maestro's `hierarchyBasedTap` retry and repeat delay
  /// semantics (`Maestro.kt:379-381, 414-440`).
  Future<({String settle, bool changed, int actUs, int settleUs})> _performTap(
    ui.Offset point, {
    required int beforeHash,
    required bool retryIfNoChange,
    required bool longPress,
    required int repeat,
    required Duration delay,
    required Duration settleTimeout,
    required bool logTap,
  }) async {
    final int retries = retryIfNoChange ? 2 : 1;
    final Duration hold = longPress
        ? DriverTimeouts.longPressHold
        : Duration.zero;
    final int count = repeat < 1 ? 1 : repeat;

    String settle = 'idle';
    bool changed = false;
    int totalActUs = 0;
    int totalSettleUs = 0;

    for (int attempt = 0; attempt < retries; attempt++) {
      if (_context.isCancelled()) break;
      final Stopwatch sinceTap = Stopwatch()..start();
      final Stopwatch actSw = Stopwatch()..start();
      if (longPress) {
        if (logTap) _context.textInput.logTap(point);
        await _context.tap(point, hold: hold);
      } else if (count > 1) {
        for (int i = 0; i < count; i++) {
          if (_context.isCancelled()) break;
          final Stopwatch tapSw = Stopwatch()..start();
          if (logTap) _context.textInput.logTap(point);
          await _context.tap(point);
          if (i < count - 1) {
            final Duration remaining = delay - tapSw.elapsed;
            if (remaining > Duration.zero) {
              await Future<void>.delayed(remaining);
            }
          }
        }
      } else {
        if (logTap) _context.textInput.logTap(point);
        await _context.tap(point);
      }

      if (_context.isCancelled()) break;

      // Let focus changes triggered by the tap run (they are scheduled as
      // microtasks), so TextInput.show is already observed when settling begins.
      await Future<void>.delayed(Duration.zero);
      totalActUs += actSw.elapsedMicroseconds;

      final Stopwatch settleSw = Stopwatch()..start();
      settle = await _context.settle(settleTimeout);
      totalSettleUs += settleSw.elapsedMicroseconds;

      changed = _context.computeHash() != beforeHash;
      if (changed) break;

      // Before retrying a single tap under `retryIfNoChange`, ensure at least
      // `kDoubleTapTimeout` (300 ms) + 1 frame has elapsed so a widget with
      // both `onTap` and `onDoubleTap` can resolve its first tap instead of
      // interpreting the retry as a double tap.
      if (attempt < retries - 1 && !longPress && count == 1) {
        final Duration waitForArena = kDoubleTapTimeout - sinceTap.elapsed;
        if (waitForArena > Duration.zero) {
          await Future<void>.delayed(waitForArena);
          if (_context.isCancelled()) break;
          await _context.idle.waitForNextFrame(
            const Duration(milliseconds: 32),
          );
        }
        changed = _context.computeHash() != beforeHash;
        if (changed) break;
      }
    }

    return (
      settle: settle,
      changed: changed,
      actUs: totalActUs,
      settleUs: totalSettleUs,
    );
  }

  /// Reactively waits until [node] becomes the topmost element at its center
  /// (`Maestro.kt:509-522`, `ViewHierarchy.kt:40-50`), up to 10 s.
  Future<Snapshot> _waitUntilNodeVisible(KNode node, Selector selector) async {
    final Stopwatch sw = Stopwatch()..start();
    Snapshot snap = _context.captureSnapshot();
    while (sw.elapsed < DriverTimeouts.waitUntilVisibleTimeout) {
      if (_context.isCancelled()) return snap;
      final KNode target = _reaim(node, selector, snap);
      if (snap.isVisible(target)) return snap;
      final Duration remaining =
          DriverTimeouts.waitUntilVisibleTimeout - sw.elapsed;
      if (remaining <= Duration.zero) break;
      await _context.idle.waitForNextFrame(
        remaining < _visibilityPollCap ? remaining : _visibilityPollCap,
      );
      snap = _context.captureSnapshot();
    }
    return snap;
  }

  /// Target endpoint for `AndroidDriver.swipe(startPoint, direction, durationMs)`
  /// (`AndroidDriver.kt:450-475`).
  static ui.Offset _directionalSwipeEnd(
    ui.Offset start,
    String direction,
    ui.Size screen,
  ) => switch (direction) {
    'UP' => ui.Offset(start.dx, (screen.height * 0.1).toInt().toDouble()),
    'DOWN' => ui.Offset(start.dx, (screen.height * 0.9).toInt().toDouble()),
    'RIGHT' => ui.Offset((screen.width * 0.9).toInt().toDouble(), start.dy),
    'LEFT' => ui.Offset((screen.width * 0.1).toInt().toDouble(), start.dy),
    _ => throw DriverError.invalidCommand(
      'Invalid swipe direction: $direction',
    ),
  };
}
