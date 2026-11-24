import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';

/// Injects pointer gestures and system navigation at the engine -> framework
/// boundary, matching what the Flutter Android embedding delivers from the OS.
///
/// Black-box contract: gestures flow through
/// [ui.PlatformDispatcher.onPointerDataPacket] (hit testing, gesture arena,
/// modal barriers) and back navigation flows through `flutter/navigation`
/// `popRoute`; app callbacks are never invoked directly.
class PointerInput {
  /// Creates a pointer input injector with an optional monotonic [clock]
  /// (defaults to a wall-clock [Stopwatch]).
  PointerInput({Duration Function()? clock}) : _readNow = clock;

  /// Shared singleton used by the default driver binding.
  static final PointerInput instance = PointerInput();

  final Stopwatch _clock = Stopwatch()..start();
  final Duration Function()? _readNow;
  Duration? _lastTimeStamp;
  int _device = 1000;
  int _pointer = 1000;

  Duration get _rawNow => _readNow != null ? _readNow() : _clock.elapsed;

  /// Returns a strictly monotonic pointer timestamp using a high-water mark:
  /// `max(raw, requested, last + 1 µs)`.
  ///
  /// Accelerated drags emit synthetic timestamps spanning their full logical
  /// duration (so Flutter's `VelocityTracker` computes the exact Maestro fling
  /// velocity without blocking wall clock). Once wall time catches up past the
  /// high-water mark, subsequent events naturally return to real wall time
  /// instead of staying permanently offset.
  Duration _nextTimeStamp([Duration? requested]) {
    Duration candidate = _rawNow;
    if (requested != null && requested > candidate) {
      candidate = requested;
    }
    final Duration? prev = _lastTimeStamp;
    if (prev != null && candidate <= prev) {
      candidate = prev + const Duration(microseconds: 1);
    }
    _lastTimeStamp = candidate;
    return candidate;
  }

  int get _viewId {
    final ui.FlutterView? view =
        ui.PlatformDispatcher.instance.implicitView ??
        ui.PlatformDispatcher.instance.views.firstOrNull;
    if (view == null) {
      throw StateError('No active Flutter view');
    }
    return view.viewId;
  }

  ui.PointerData _pointerData(
    ui.PointerChange change,
    int device,
    int pointer,
    double x,
    double y, {
    required Duration timeStamp,
    double dx = 0,
    double dy = 0,
  }) {
    final bool pressed =
        change == ui.PointerChange.down || change == ui.PointerChange.move;
    return ui.PointerData(
      viewId: _viewId,
      timeStamp: timeStamp,
      change: change,
      kind: ui.PointerDeviceKind.touch,
      device: device,
      pointerIdentifier: pointer,
      physicalX: x,
      physicalY: y,
      physicalDeltaX: dx,
      physicalDeltaY: dy,
      buttons: pressed ? 1 : 0,
      pressure: pressed ? 1 : 0,
      pressureMax: 1,
    );
  }

  void _send(List<ui.PointerData> data) {
    final ui.PointerDataPacketCallback? callback =
        ui.PlatformDispatcher.instance.onPointerDataPacket;
    if (callback == null) {
      throw StateError('No pointer handler installed');
    }
    callback(ui.PointerDataPacket(data: data));
  }

  /// Taps at physical pixels ([x], [y]).
  ///
  /// Passing a positive [hold] turns the gesture into a long press. Even for a
  /// plain tap (`hold == Duration.zero`), the down and up packets are separated
  /// by a zero-duration timer so the down event is processed in its own
  /// event-loop turn, matching a real touch.
  Future<void> tap(double x, double y, {Duration hold = Duration.zero}) async {
    final int device = _device++;
    final int pointer = _pointer++;
    final Duration downTs = _nextTimeStamp();
    _send(<ui.PointerData>[
      _pointerData(
        ui.PointerChange.add,
        device,
        pointer,
        x,
        y,
        timeStamp: downTs,
      ),
      _pointerData(
        ui.PointerChange.down,
        device,
        pointer,
        x,
        y,
        timeStamp: downTs,
      ),
    ]);
    try {
      if (hold > Duration.zero) {
        await Future<void>.delayed(hold);
      } else {
        await Future<void>.delayed(Duration.zero);
      }
      final Duration upTs = _nextTimeStamp();
      _send(<ui.PointerData>[
        _pointerData(
          ui.PointerChange.up,
          device,
          pointer,
          x,
          y,
          timeStamp: upTs,
        ),
        _pointerData(
          ui.PointerChange.remove,
          device,
          pointer,
          x,
          y,
          timeStamp: upTs,
        ),
      ]);
    } catch (_) {
      _cancelPointer(device, pointer, x, y);
      rethrow;
    }
  }

  void _cancelPointer(int device, int pointer, double x, double y) {
    try {
      final Duration ts = _nextTimeStamp();
      _send(<ui.PointerData>[
        _pointerData(
          ui.PointerChange.cancel,
          device,
          pointer,
          x,
          y,
          timeStamp: ts,
        ),
        _pointerData(
          ui.PointerChange.remove,
          device,
          pointer,
          x,
          y,
          timeStamp: ts,
        ),
      ]);
    } on Object {
      // Best-effort cleanup so a subsequent gesture never collides with a stuck pointer.
    }
  }

  /// Drags linearly from [start] to [end] (physical pixels) over logical
  /// [duration].
  ///
  /// When [wallDuration] is omitted (or >= [duration]), the drag is paced in
  /// real time against a deadline so timer jitter does not inflate the total
  /// duration. When a shorter [wallDuration] is provided (used by `swipe`,
  /// `scroll`, and `scrollUntilVisible`), pointer timestamps still span the
  /// full logical [duration] so Flutter's `VelocityTracker` computes the exact
  /// same fling physics as a real-time swipe of [duration] without blocking
  /// wall clock.
  ///
  /// A [hold] before release (> 40 ms, Flutter's `VelocityTracker` "pointer
  /// stopped" window) suppresses ballistic fling, matching a finger that stops
  /// before lifting.
  Future<void> drag(
    ui.Offset start,
    ui.Offset end,
    Duration duration, {
    Duration hold = Duration.zero,
    Duration? wallDuration,
  }) async {
    final int device = _device++;
    final int pointer = _pointer++;
    final bool accelerated = wallDuration != null && wallDuration < duration;
    final Duration effectiveWall = accelerated ? wallDuration : duration;
    final int steps = accelerated
        ? (duration.inMilliseconds / 25).ceil().clamp(8, 40)
        : (duration.inMilliseconds / 16).ceil().clamp(2, 100);

    final Duration startRaw = _rawNow;
    final Duration startVirtual = _nextTimeStamp();

    _send(<ui.PointerData>[
      _pointerData(
        ui.PointerChange.add,
        device,
        pointer,
        start.dx,
        start.dy,
        timeStamp: startVirtual,
      ),
      _pointerData(
        ui.PointerChange.down,
        device,
        pointer,
        start.dx,
        start.dy,
        timeStamp: startVirtual,
      ),
    ]);

    ui.Offset last = start;
    try {
      for (int i = 1; i <= steps; i++) {
        final Duration targetRaw =
            startRaw +
            Duration(microseconds: effectiveWall.inMicroseconds * i ~/ steps);
        final Duration wait = targetRaw - _rawNow;
        if (wait > Duration.zero) {
          await Future<void>.delayed(wait);
        } else {
          await Future<void>.delayed(Duration.zero);
        }
        final Duration moveTs = accelerated
            ? _nextTimeStamp(
                startVirtual +
                    Duration(
                      microseconds: duration.inMicroseconds * i ~/ steps,
                    ),
              )
            : _nextTimeStamp();
        final ui.Offset next = ui.Offset.lerp(start, end, i / steps)!;
        _send(<ui.PointerData>[
          _pointerData(
            ui.PointerChange.move,
            device,
            pointer,
            next.dx,
            next.dy,
            timeStamp: moveTs,
            dx: next.dx - last.dx,
            dy: next.dy - last.dy,
          ),
        ]);
        last = next;
      }
      if (hold > Duration.zero) {
        await Future<void>.delayed(hold);
      }
      final Duration upTs = accelerated
          ? _nextTimeStamp(startVirtual + duration + hold)
          : _nextTimeStamp();
      _send(<ui.PointerData>[
        _pointerData(
          ui.PointerChange.up,
          device,
          pointer,
          end.dx,
          end.dy,
          timeStamp: upTs,
        ),
        _pointerData(
          ui.PointerChange.remove,
          device,
          pointer,
          end.dx,
          end.dy,
          timeStamp: upTs,
        ),
      ]);
    } catch (_) {
      _cancelPointer(device, pointer, last.dx, last.dy);
      rethrow;
    }
  }

  /// Triggers a system back navigation by delivering `popRoute` on
  /// `flutter/navigation` via `ui.channelBuffers.push`.
  void back() {
    ui.channelBuffers.push(
      SystemChannels.navigation.name,
      const JSONMethodCodec().encodeMethodCall(const MethodCall('popRoute')),
      (ByteData? _) {},
    );
  }

  /// Injects a hardware key down + up sequence at the engine -> framework
  /// boundary via [ui.PlatformDispatcher.onKeyData].
  ///
  /// Uses `synthesized: true` so Flutter's `KeyEventManager.handleKeyData`
  /// dispatches solitary `KeyData` events immediately without waiting for a
  /// paired legacy `RawKeyEvent`.
  Future<void> pressKey(
    PhysicalKeyboardKey physical,
    LogicalKeyboardKey logical,
  ) async {
    final ui.KeyDataCallback? callback =
        ui.PlatformDispatcher.instance.onKeyData;
    if (callback == null) {
      throw StateError('No key data handler installed');
    }
    callback(
      ui.KeyData(
        timeStamp: _nextTimeStamp(),
        type: ui.KeyEventType.down,
        physical: physical.usbHidUsage,
        logical: logical.keyId,
        character: null,
        synthesized: true,
      ),
    );
    await Future<void>.delayed(Duration.zero);
    callback(
      ui.KeyData(
        timeStamp: _nextTimeStamp(),
        type: ui.KeyEventType.up,
        physical: physical.usbHidUsage,
        logical: logical.keyId,
        character: null,
        synthesized: true,
      ),
    );
  }
}
