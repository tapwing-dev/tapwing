import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/channel_tracker.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/idle/semantics_motion_probe.dart';
import 'package:tapwing_driver/src/input/text_input.dart';

/// Frame cadence of the simulated display.
const Duration _frame = Duration(milliseconds: 16);

/// A semantics box whose top edge follows `topAt(elapsed)` (logical pixels)
/// and whose label follows `labelAt(frameIndex)`, advanced from a ticker so
/// the scheduler keeps producing frames like a coasting list does, without any
/// animation widget the animation scanner would count.
class _Mover {
  _Mover(this.tester, {required this.topAt, this.labelAt = _constantLabel});

  final WidgetTester tester;
  final double Function(Duration elapsed) topAt;
  final String Function(int frame) labelAt;
  final ValueNotifier<double> top = ValueNotifier<double>(0);
  final ValueNotifier<String> label = ValueNotifier<String>('box');
  late final Ticker _ticker;
  int _frames = 0;

  static String _constantLabel(int frame) => 'box';

  Future<void> start() async {
    top.value = topAt(Duration.zero);
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: MediaQuery(
          data: MediaQueryData.fromView(tester.view),
          child: ValueListenableBuilder<double>(
            valueListenable: top,
            builder: (BuildContext context, double y, Widget? child) => Stack(
              children: <Widget>[
                Positioned(
                  left: 20,
                  top: y,
                  child: ValueListenableBuilder<String>(
                    valueListenable: label,
                    builder: (BuildContext context, String text, Widget? _) =>
                        Semantics(
                          container: true,
                          label: text,
                          child: const SizedBox(width: 100, height: 100),
                        ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
    _ticker = tester.createTicker((Duration elapsed) {
      _frames++;
      top.value = topAt(elapsed);
      label.value = labelAt(_frames);
    })..start();
  }

  void dispose() {
    _ticker.dispose();
    top.dispose();
    label.dispose();
  }
}

/// Position of a body decelerating exponentially from [v0] logical px/s.
double Function(Duration) _decay({required double v0, required double tau}) =>
    (Duration t) {
      final double s = t.inMicroseconds / 1e6;
      return v0 * tau * (1 - math.exp(-s / tau));
    };

void main() {
  IdleDetector detectorFor(WidgetTester tester) {
    final DateTime epoch = tester.binding.clock.now();
    return IdleDetector(
      httpTracker: HttpTracker(),
      textInput: TextInputController(),
      readBottomInset: () => 0,
      waitForFirstFrame: (_) async {},
      clock: () => tester.binding.clock.now().difference(epoch),
    );
  }

  /// Runs `waitForIdle` on [detector] for at most [maxFrames] frames of fake
  /// time. `result` is `null` when it was still waiting, in which case it is
  /// cancelled so no timer outlives the test.
  Future<({String? result, Duration took})> settle(
    WidgetTester tester,
    IdleDetector detector, {
    double? maxContentVelocity,
    int maxFrames = 400,
  }) async {
    bool cancelled = false;
    String? result;
    unawaited(
      detector
          .waitForIdle(
            const Duration(seconds: 60),
            stableFrames: 2,
            includeScrollables: false,
            maxContentVelocity: maxContentVelocity,
            isCancelled: () => cancelled,
          )
          .then((String s) => result = s),
    );
    final DateTime start = tester.binding.clock.now();
    for (int i = 0; i < maxFrames && result == null; i++) {
      await tester.pump(_frame);
    }
    final Duration took = tester.binding.clock.now().difference(start);
    if (result == null) {
      cancelled = true;
      await tester.pump(_frame);
      await tester.pump(_frame);
      expect(result, 'cancelled');
      return (result: null, took: took);
    }
    return (result: result, took: took);
  }

  group('SemanticsMotionProbe', () {
    testWidgets('reports how far a node moved and what content changed', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      String name = 'box';
      final _Mover mover = _Mover(
        tester,
        topAt: (Duration t) => 100 + t.inMilliseconds / 16 * 4.0,
        labelAt: (int frame) => name,
      );
      try {
        await mover.start();
        final SemanticsMotionProbe probe = SemanticsMotionProbe();
        expect(probe.sample(), isNull, reason: 'first sample only seeds');

        await tester.pump(_frame);
        await tester.pump(_frame);
        final SemanticsMotion? moved = probe.sample();
        expect(moved, isNotNull);
        // 4 logical px per 16 ms frame, reported in physical pixels.
        expect(moved!.shift, closeTo(4.0 * tester.view.devicePixelRatio, 0.5));
        final int hashWhileMoving = moved.contentHash;

        await tester.pump(_frame);
        expect(probe.sample()!.contentHash, hashWhileMoving);

        name = 'renamed';
        await tester.pump(_frame);
        expect(probe.sample()!.contentHash, isNot(hashWhileMoving));
      } finally {
        mover.dispose();
        handle.dispose();
      }
    });

    testWidgets('reset forgets the previous sample', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      final _Mover mover = _Mover(tester, topAt: (Duration t) => 100);
      try {
        await mover.start();
        final SemanticsMotionProbe probe = SemanticsMotionProbe();
        probe.sample();
        probe.reset();
        expect(probe.sample(), isNull);
      } finally {
        mover.dispose();
        handle.dispose();
      }
    });
  });

  group('IdleDetector maxContentVelocity', () {
    // Coasts 1000 px/s with a 250 ms time constant: slower than 300 px/s after
    // ~300 ms, but the hash (rounded rects) only stops changing once it moves
    // less than ~10 px/s, after ~1.15 s.
    double Function(Duration) fling() => _decay(v0: 1000, tau: 0.25);

    testWidgets(
      'a decelerating fling ends calm as soon as it is slower than the limit',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final _Mover mover = _Mover(tester, topAt: fling());
        try {
          await mover.start();
          final out = await settle(
            tester,
            detectorFor(tester),
            maxContentVelocity: 300,
          );
          expect(out.result, 'calm');
          expect(out.took, greaterThan(const Duration(milliseconds: 300)));
          expect(out.took, lessThan(const Duration(milliseconds: 500)));
        } finally {
          mover.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'without a velocity limit the same fling settles only once the hash is stable',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final _Mover mover = _Mover(tester, topAt: fling());
        try {
          await mover.start();
          final out = await settle(tester, detectorFor(tester));
          expect(out.result, 'stable');
          expect(out.took, greaterThan(const Duration(milliseconds: 900)));
        } finally {
          mover.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'content that is about to speed up is not calm before it was seen moving fast',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        // A spring leaving rest: 5000 px/s² for 200 ms (the first frames are
        // far below the limit), then a decay from 1000 px/s.
        final _Mover mover = _Mover(
          tester,
          topAt: (Duration t) {
            final double s = t.inMicroseconds / 1e6;
            if (s < 0.2) return 0.5 * 5000 * s * s;
            return 100 + 1000 * 0.25 * (1 - math.exp(-(s - 0.2) / 0.25));
          },
        );
        try {
          await mover.start();
          final out = await settle(
            tester,
            detectorFor(tester),
            maxContentVelocity: 300,
          );
          expect(out.result, 'calm');
          expect(
            out.took,
            greaterThan(const Duration(milliseconds: 450)),
            reason: 'must wait for the ramp-up and the decay below the limit',
          );
        } finally {
          mover.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets('content changing besides position is never calm', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      final _Mover mover = _Mover(
        tester,
        topAt: fling(),
        labelAt: (int frame) => 'box $frame',
      );
      try {
        await mover.start();
        // Far longer than the ~350 ms it takes to slow down.
        final out = await settle(
          tester,
          detectorFor(tester),
          maxContentVelocity: 300,
          maxFrames: 120,
        );
        expect(out.result, isNull);
      } finally {
        mover.dispose();
        handle.dispose();
      }
    });

    testWidgets('a pending load keeps the wait going even when calm', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      final _Mover mover = _Mover(tester, topAt: fling());
      final Completer<ByteData?> reply = Completer<ByteData?>();
      try {
        await mover.start();
        final ChannelTracker channels = ChannelTracker();
        channels.track('app/pending', reply.future);
        final DateTime epoch = tester.binding.clock.now();
        final IdleDetector detector = IdleDetector(
          httpTracker: HttpTracker(),
          channelTracker: channels,
          textInput: TextInputController(),
          readBottomInset: () => 0,
          waitForFirstFrame: (_) async {},
          clock: () => tester.binding.clock.now().difference(epoch),
        );
        final out = await settle(
          tester,
          detector,
          maxContentVelocity: 300,
          maxFrames: 120,
        );
        expect(out.result, isNull);
      } finally {
        reply.complete(null);
        mover.dispose();
        handle.dispose();
      }
    });
  });
}
