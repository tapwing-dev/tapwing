import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/animation_scanner.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/text_input.dart';

void main() {
  IdleDetector createDetector() => IdleDetector(
    httpTracker: HttpTracker(),
    textInput: TextInputController(),
    readBottomInset: () => 0,
    waitForFirstFrame: (_) async {},
  );

  group('IdleDetector & AnimationScanner (widget tests)', () {
    testWidgets('returns idle immediately on a static screen', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(
          const MaterialApp(
            home: Scaffold(body: Center(child: Text('Ready'))),
          ),
        );

        final IdleDetector detector = createDetector();
        String? state;
        unawaited(
          detector
              .waitForIdle(const Duration(seconds: 2))
              .then((String s) => state = s),
        );
        // WidgetTester.pump() without a duration does not elapse FakeAsync
        // timers; pass Duration.zero so Future.delayed(Duration.zero) fires.
        await tester.pump(Duration.zero);
        await tester.pump(Duration.zero);
        expect(state, 'idle');
      } finally {
        handle.dispose();
      }
    });

    testWidgets('small spinner settles in 3 frames (v2) vs 8 frames (legacy)', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(
          const MaterialApp(
            home: Scaffold(
              body: Center(
                child: SizedBox(
                  width: 16,
                  height: 16,
                  child: CircularProgressIndicator(),
                ),
              ),
            ),
          ),
        );

        // 16x16 on 800x600 is ~0.05% of screen -> well below the 2% threshold.
        expect(AnimationScanner().scan(), isEmpty);

        // Policy v2: requires 3 rendered frames with unchanged semantics hash.
        final IdleDetector detectorV2 = createDetector();
        String? v2Result;
        unawaited(
          detectorV2
              .waitForIdle(const Duration(seconds: 2), policy: SettlePolicy.v2)
              .then((String state) => v2Result = state),
        );

        await tester.pump(const Duration(milliseconds: 16));
        await tester.pump(const Duration(milliseconds: 16));
        expect(v2Result, isNull);
        await tester.pump(const Duration(milliseconds: 16));
        expect(v2Result, 'stable');

        // Legacy policy: requires 8 rendered frames.
        final IdleDetector detectorLegacy = createDetector();
        String? legacyResult;
        unawaited(
          detectorLegacy
              .waitForIdle(
                const Duration(seconds: 2),
                policy: SettlePolicy.legacy,
              )
              .then((String state) => legacyResult = state),
        );

        for (int i = 0; i < 7; i++) {
          await tester.pump(const Duration(milliseconds: 16));
        }
        expect(legacyResult, isNull);
        await tester.pump(const Duration(milliseconds: 16));
        expect(legacyResult, 'stable');
      } finally {
        handle.dispose();
      }
    });

    testWidgets(
      'significant route transition blocks v2 settle until animation finishes',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          late BuildContext homeContext;
          await tester.pumpWidget(
            MaterialApp(
              home: Builder(
                builder: (BuildContext context) {
                  homeContext = context;
                  return const Scaffold(body: Center(child: Text('Home')));
                },
              ),
            ),
          );

          // Push a route with a 300ms full-screen fade transition.
          unawaited(
            Navigator.of(homeContext).push<void>(
              PageRouteBuilder<void>(
                transitionDuration: const Duration(milliseconds: 300),
                pageBuilder: (_, _, _) =>
                    const Scaffold(body: Center(child: Text('Second'))),
                transitionsBuilder: (
                  _,
                  Animation<double> anim,
                  _,
                  Widget child,
                ) => FadeTransition(opacity: anim, child: child),
              ),
            ),
          );
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 16));

          final List<SignificantAnimation> running = AnimationScanner().scan();
          expect(running, isNotEmpty);
          expect(
            running.first.fraction,
            greaterThanOrEqualTo(AnimationScanner.significantFraction),
          );

          final IdleDetector detector = createDetector();
          String? result;
          unawaited(
            detector
                .waitForIdle(
                  const Duration(seconds: 5),
                  policy: SettlePolicy.v2,
                )
                .then((String state) => result = state),
          );

          // Even after 4 stable frames (64 ms), the transition is still mid-flight.
          for (int i = 0; i < 4; i++) {
            await tester.pump(const Duration(milliseconds: 16));
          }
          expect(result, isNull);

          // Let the 300 ms route transition finish and flush final idle yield.
          await tester.pump(const Duration(milliseconds: 300));
          await tester.pump();
          await tester.pump();
          expect(result, anyOf('idle', 'stable'));
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'skips TickerMode(enabled: false) and Offstage(offstage: true) animations',
      (WidgetTester tester) async {
        final AnimationController mutedCtrl = AnimationController(
          vsync: tester,
          duration: const Duration(seconds: 1),
        )..repeat();
        final AnimationController offstageCtrl = AnimationController(
          vsync: tester,
          duration: const Duration(seconds: 1),
        )..repeat();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Stack(
                  fit: StackFit.expand,
                  children: <Widget>[
                    TickerMode(
                      enabled: false,
                      child: FadeTransition(
                        opacity: mutedCtrl,
                        child: const ColoredBox(color: Colors.blue),
                      ),
                    ),
                    Offstage(
                      offstage: true,
                      child: FadeTransition(
                        opacity: offstageCtrl,
                        child: const ColoredBox(color: Colors.red),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          );

          expect(AnimationScanner().scan(), isEmpty);
        } finally {
          mutedCtrl.dispose();
          offstageCtrl.dispose();
        }
      },
    );

    testWidgets(
      'detects ImplicitlyAnimatedWidget (AnimatedContainer) covering >= 2% of screen',
      (WidgetTester tester) async {
        bool expanded = false;
        late StateSetter rebuild;

        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: StatefulBuilder(
                builder: (BuildContext context, StateSetter setState) {
                  rebuild = setState;
                  return AnimatedContainer(
                    duration: const Duration(milliseconds: 200),
                    width: 400,
                    height: 300,
                    color: expanded ? Colors.green : Colors.orange,
                  );
                },
              ),
            ),
          ),
        );

        expect(AnimationScanner().scan(), isEmpty);
        rebuild(() => expanded = true);
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 50));

        final List<SignificantAnimation> anims = AnimationScanner().scan();
        expect(anims, isNotEmpty);
        expect(anims.first.widget, 'AnimatedContainer');

        await tester.pump(const Duration(milliseconds: 250));
        expect(AnimationScanner().scan(), isEmpty);
      },
    );

    testWidgets(
      'endless significant animation stops blocking after 1.5 s with unchanged tree',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final AnimationController ctrl = AnimationController(
          vsync: tester,
          duration: const Duration(milliseconds: 800),
        )..repeat(reverse: true);
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Center(
                  child: FadeTransition(
                    opacity: ctrl,
                    child: const SizedBox(
                      width: 300,
                      height: 300,
                      child: ColoredBox(color: Colors.teal),
                    ),
                  ),
                ),
              ),
            ),
          );

          final DateTime epoch = tester.binding.clock.now();
          final AnimationScanner scanner = AnimationScanner(
            clock: () => tester.binding.clock.now().difference(epoch),
          );
          final IdleDetector detector = IdleDetector(
            httpTracker: HttpTracker(),
            textInput: TextInputController(),
            animationScanner: scanner,
            readBottomInset: () => 0,
            waitForFirstFrame: (_) async {},
          );

          // Initially blocks because it hasn't run for 1.5 s yet.
          expect(scanner.hasSignificantAnimation(treeHash: 42), isTrue);
          expect(scanner.scan(treeHash: 42).first.infinite, isFalse);

          // After 1.0 s still blocking (finite transitions may last ~600 ms).
          await tester.pump(const Duration(milliseconds: 1000));
          expect(scanner.hasSignificantAnimation(treeHash: 42), isTrue);

          // Past 1.5 s with unchanged tree hash -> classified infinite & non-blocking!
          await tester.pump(const Duration(milliseconds: 600));
          final List<SignificantAnimation> scanned = scanner.scan(treeHash: 42);
          expect(scanned, isNotEmpty);
          expect(scanned.first.infinite, isTrue);
          expect(scanner.hasSignificantAnimation(treeHash: 42), isFalse);

          // Subsequent settle completes in 3 frames (~48 ms), not a 3 s timeout!
          String? state;
          unawaited(
            detector
                .waitForIdle(
                  const Duration(seconds: 3),
                  policy: SettlePolicy.v2,
                )
                .then((String s) => state = s),
          );
          await tester.pump(const Duration(milliseconds: 16));
          await tester.pump(const Duration(milliseconds: 16));
          await tester.pump(const Duration(milliseconds: 16));
          expect(state, 'stable');
        } finally {
          ctrl.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'active scrolling Scrollable counts as a significant animation until scroll stops',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final ScrollController scrollCtrl = ScrollController();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: ListView.builder(
                  controller: scrollCtrl,
                  itemCount: 80,
                  itemBuilder: (_, int i) => ListTile(title: Text('Row $i')),
                ),
              ),
            ),
          );

          final AnimationScanner scanner = AnimationScanner();
          expect(scanner.scan(), isEmpty);

          unawaited(
            scrollCtrl.animateTo(
              600,
              duration: const Duration(milliseconds: 200),
              curve: Curves.linear,
            ),
          );
          await tester.pump(const Duration(milliseconds: 50));

          final List<SignificantAnimation> active = scanner.scan();
          expect(
            active.any((SignificantAnimation a) => a.widget == 'Scrollable'),
            isTrue,
          );

          await tester.pumpAndSettle();
          expect(scanner.scan(), isEmpty);
        } finally {
          scrollCtrl.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'KeyboardWaiter adaptive latch requires 2 misses, ignores readOnly, and clears on inset > 0',
      (WidgetTester tester) async {
        final TextInputController ti = TextInputController();
        double inset = 0;
        final IdleDetector detector = IdleDetector(
          httpTracker: HttpTracker(),
          textInput: ti,
          readBottomInset: () => inset,
          waitForFirstFrame: (_) async {},
        );

        // CLI hint softKeyboard: false latches immediately, true clears it.
        expect(detector.softKeyboardAbsent, isFalse);
        detector.configure(softKeyboard: false);
        expect(detector.softKeyboardAbsent, isTrue);
        detector.configure(softKeyboard: true);
        expect(detector.softKeyboardAbsent, isFalse);

        // readOnly client must NOT increment absent latch even when no inset appears.
        const JSONMethodCodec codec = JSONMethodCodec();
        ti.onOutgoing(
          codec.encodeMethodCall(
            const MethodCall('TextInput.setClient', <Object?>[
              1,
              <String, Object?>{'readOnly': true},
            ]),
          ),
        );
        expect(ti.suppressesSoftKeyboard, isTrue);

        // Normal editable client does not suppress soft keyboard.
        ti.onOutgoing(
          codec.encodeMethodCall(
            const MethodCall('TextInput.setClient', <Object?>[
              2,
              <String, Object?>{
                'readOnly': false,
                'inputType': <String, Object?>{'name': 'TextInputType.text'},
              },
            ]),
          ),
        );
        expect(ti.suppressesSoftKeyboard, isFalse);

        // When inset appears > 0, latch clears even if previously hinted absent.
        detector.configure(softKeyboard: false);
        inset = 800;
        String? state;
        unawaited(
          detector
              .waitForIdle(const Duration(milliseconds: 200))
              .then((String s) => state = s),
        );
        await tester.pump(Duration.zero);
        await tester.pump(Duration.zero);
        expect(state, 'idle');
        expect(detector.softKeyboardAbsent, isFalse);
      },
    );

    testWidgets(
      'waitForIdle unwinds immediately when cancelled during waitForFirstFrame',
      (WidgetTester tester) async {
        final Completer<void> neverFirstFrame = Completer<void>();
        final IdleDetector detector = IdleDetector(
          httpTracker: HttpTracker(),
          textInput: TextInputController(),
          readBottomInset: () => 0,
          waitForFirstFrame: (_) => neverFirstFrame.future,
        );
        bool cancelled = false;
        String? result;
        unawaited(
          detector
              .waitForIdle(
                const Duration(seconds: 30),
                isCancelled: () => cancelled,
              )
              .then((String s) => result = s),
        );

        await tester.pump(const Duration(milliseconds: 25));
        expect(result, isNull);

        cancelled = true;
        await tester.pump(const Duration(milliseconds: 25));
        expect(result, 'cancelled');
      },
    );

    testWidgets(
      'waitForIdle unwinds immediately when cancelled during KeyboardWaiter',
      (WidgetTester tester) async {
        final TextInputController ti = TextInputController();
        const JSONMethodCodec codec = JSONMethodCodec();
        ti.onOutgoing(codec.encodeMethodCall(const MethodCall('TextInput.show')));

        final IdleDetector detector = IdleDetector(
          httpTracker: HttpTracker(),
          textInput: ti,
          readBottomInset: () => 0,
          waitForFirstFrame: (_) async {},
        );
        bool cancelled = false;
        String? result;
        unawaited(
          detector
              .waitForIdle(
                const Duration(seconds: 30),
                isCancelled: () => cancelled,
              )
              .then((String s) => result = s),
        );

        await tester.pump(const Duration(milliseconds: 16));
        expect(result, isNull);

        cancelled = true;
        await tester.pump(const Duration(milliseconds: 16));
        expect(result, 'cancelled');
      },
    );

    testWidgets(
      'waitForNextFrame removes timed-out waiters and still wakes on a later frame',
      (WidgetTester tester) async {
        final IdleDetector detector = createDetector();
        bool? timedOut;
        unawaited(
          detector
              .waitForNextFrame(const Duration(milliseconds: 20))
              .then((bool v) => timedOut = v),
        );
        // Advance FakeAsync timer past 20 ms without pumping a scheduled frame.
        await tester.pump(const Duration(milliseconds: 25));
        expect(timedOut, isFalse);

        // A new waiter still wakes when a real frame is scheduled and pumped.
        await tester.pumpWidget(
          const MaterialApp(home: Scaffold(body: Text('Next'))),
        );
        bool? rendered;
        unawaited(
          detector
              .waitForNextFrame(const Duration(seconds: 1))
              .then((bool v) => rendered = v),
        );
        tester.binding.scheduleFrame();
        await tester.pump();
        expect(rendered, isTrue);
      },
    );
  });
}
