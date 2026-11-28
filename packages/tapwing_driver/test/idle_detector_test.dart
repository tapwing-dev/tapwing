import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart' show Ticker;
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/animation_scanner.dart';
import 'package:tapwing_driver/src/idle/channel_tracker.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';

void main() {
  IdleDetector createDetector({ChannelTracker? channelTracker}) => IdleDetector(
    httpTracker: HttpTracker(),
    channelTracker: channelTracker ?? ChannelTracker(),
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
                transitionsBuilder:
                    (_, Animation<double> anim, _, Widget child) =>
                        FadeTransition(opacity: anim, child: child),
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
        ti.onOutgoing(
          codec.encodeMethodCall(const MethodCall('TextInput.show')),
        );

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

    testWidgets(
      'AnimationScanner ignores AlwaysStoppedAnimation, stopped mid-range '
      'AnimationController, and wrappers around them',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final AnimationController stoppedValueCtrl = AnimationController(
          vsync: tester,
          duration: const Duration(milliseconds: 500),
        )..value = 0.5;
        final AnimationController stoppedMidCtrl = AnimationController(
          vsync: tester,
          duration: const Duration(milliseconds: 500),
        );
        final AnimationController runningCtrl = AnimationController(
          vsync: tester,
          duration: const Duration(milliseconds: 500),
        );
        final CurvedAnimation curvedStopped = CurvedAnimation(
          parent: stoppedMidCtrl,
          curve: Curves.easeInOut,
        );
        final ProxyAnimation proxyStopped = ProxyAnimation(stoppedMidCtrl);
        final ReverseAnimation reverseStopped = ReverseAnimation(
          stoppedMidCtrl,
        );
        final CurvedAnimation curvedRunning = CurvedAnimation(
          parent: runningCtrl,
          curve: Curves.easeIn,
        );

        try {
          // Verify Flutter's status quirk: stopped mid-range controllers still
          // report status == forward (so status.isAnimating == true), while
          // controller.isAnimating is false.
          unawaited(stoppedMidCtrl.forward());
          stoppedMidCtrl.stop();
          expect(stoppedValueCtrl.status, AnimationStatus.forward);
          expect(stoppedValueCtrl.isAnimating, isFalse);
          expect(stoppedMidCtrl.status, AnimationStatus.forward);
          expect(stoppedMidCtrl.isAnimating, isFalse);

          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Stack(
                  fit: StackFit.expand,
                  children: <Widget>[
                    const FadeTransition(
                      opacity: AlwaysStoppedAnimation<double>(0.5),
                      child: SizedBox(width: 200, height: 200),
                    ),
                    FadeTransition(
                      opacity: stoppedValueCtrl,
                      child: const SizedBox(width: 200, height: 200),
                    ),
                    FadeTransition(
                      opacity: curvedStopped,
                      child: const SizedBox(width: 200, height: 200),
                    ),
                    FadeTransition(
                      opacity: proxyStopped,
                      child: const SizedBox(width: 200, height: 200),
                    ),
                    FadeTransition(
                      opacity: reverseStopped,
                      child: const SizedBox(width: 200, height: 200),
                    ),
                    FadeTransition(
                      opacity: Tween<double>(
                        begin: 0.2,
                        end: 0.8,
                      ).animate(stoppedMidCtrl),
                      child: const SizedBox(width: 200, height: 200),
                    ),
                    FadeTransition(
                      opacity: curvedRunning,
                      child: const SizedBox(width: 200, height: 200),
                    ),
                  ],
                ),
              ),
            ),
          );

          // None of the static animations should be reported.
          expect(AnimationScanner().scan(), isEmpty);

          // An actively ticking controller (even wrapped in CurvedAnimation)
          // MUST still be detected and block v2 settle until it finishes.
          unawaited(runningCtrl.forward());
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 16));
          final List<SignificantAnimation> running = AnimationScanner().scan();
          expect(running, hasLength(1));

          final IdleDetector detector = createDetector();
          String? settleState;
          unawaited(
            detector
                .waitForIdle(const Duration(seconds: 2))
                .then((String s) => settleState = s),
          );
          for (int i = 0; i < 4; i++) {
            await tester.pump(const Duration(milliseconds: 16));
          }
          expect(
            settleState,
            isNull,
            reason: 'ticking CurvedAnimation must block settle',
          );

          await tester.pump(const Duration(milliseconds: 500));
          await tester.pump(Duration.zero);
          await tester.pump(Duration.zero);
          expect(settleState, anyOf('idle', 'stable'));
        } finally {
          curvedRunning.dispose();
          curvedStopped.dispose();
          stoppedValueCtrl.dispose();
          stoppedMidCtrl.dispose();
          runningCtrl.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'waitForIdle waits for pending non-system MethodChannel reply before '
      'post-reply navigation instead of returning idle prematurely',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final ChannelTracker channelTracker = ChannelTracker();
        try {
          final _MockAsyncMessenger rawMessenger = _MockAsyncMessenger();
          final ObservingBinaryMessenger messenger = ObservingBinaryMessenger(
            rawMessenger,
            controller: TextInputController(),
            channelTracker: channelTracker,
          );
          final MethodChannel appChannel = MethodChannel(
            'dev.tapwing.sample_app/async',
            const StandardMethodCodec(),
            messenger,
          );

          await tester.pumpWidget(
            MaterialApp(
              home: Builder(
                builder: (BuildContext context) => Scaffold(
                  body: Center(
                    child: ElevatedButton(
                      onPressed: () async {
                        await appChannel.invokeMethod<String>('fetch');
                        if (!context.mounted) return;
                        await Navigator.of(context).push(
                          MaterialPageRoute<void>(
                            builder: (_) => const Scaffold(
                              body: Center(child: Text('Async Channel Done')),
                            ),
                          ),
                        );
                      },
                      child: const Text('Start Async Nav'),
                    ),
                  ),
                ),
              ),
            ),
          );

          final IdleDetector idle = createDetector(
            channelTracker: channelTracker,
          );
          final CommandDispatcher dispatcher = CommandDispatcher(
            DriverContext(
              idle: idle,
              pointer: PointerInput(),
              textInput: TextInputController(),
              readBottomInset: () => 0,
            ),
          );

          String? rpcResultSettle;
          bool rpcDone = false;
          unawaited(
            dispatcher
                .dispatch('tapOn', <String, Object?>{
                  'selector': <String, Object?>{'text': 'Start Async Nav'},
                  'timeoutMs': 1000,
                  'settleTimeoutMs': 2000,
                })
                .then((Object? value) {
                  rpcResultSettle =
                      (value! as Map<String, Object?>)['settle'] as String?;
                  rpcDone = true;
                }),
          );

          // Pump several frames while the MethodChannel reply is still in
          // flight: frames are idle, but ChannelTracker keeps loadingIdle false.
          for (int i = 0; i < 6; i++) {
            await tester.pump(const Duration(milliseconds: 16));
          }
          expect(channelTracker.pending, 1);
          expect(idle.loadingIdle, isFalse);
          expect(idle.busyReport().pendingChannels, 1);
          expect(
            rpcDone,
            isFalse,
            reason:
                'tapOn must not settle while MethodChannel reply is pending',
          );
          expect(find.text('Async Channel Done'), findsNothing);

          // Now complete the platform channel reply; tapOn should stay waiting
          // through the subsequent Navigator.push transition and then settle.
          rawMessenger.pendingReplies.single.complete(
            const StandardMethodCodec().encodeSuccessEnvelope('ok'),
          );
          for (int i = 0; i < 40 && !rpcDone; i++) {
            await tester.pump(const Duration(milliseconds: 16));
          }
          expect(rpcDone, isTrue);
          expect(rpcResultSettle, anyOf('idle', 'stable'));
          expect(channelTracker.pending, 0);
          expect(find.text('Async Channel Done'), findsOneWidget);
        } finally {
          channelTracker.reset();
          handle.dispose();
        }
      },
    );
  });

  group('busySources', () {
    testWidgets('is empty on a settled screen', (WidgetTester tester) async {
      await tester.pumpWidget(
        const Directionality(
          textDirection: TextDirection.ltr,
          child: Text('Static'),
        ),
      );
      expect(createDetector().busySources(), isEmpty);
    });

    testWidgets('lists pending channels oldest first with the age of their '
        'oldest call, and sums up channels past the cap', (
      WidgetTester tester,
    ) async {
      await tester.pumpWidget(
        const Directionality(
          textDirection: TextDirection.ltr,
          child: Text('Static'),
        ),
      );
      Duration now = Duration.zero;
      final ChannelTracker tracker = ChannelTracker(
        safetyTimeout: Duration.zero,
        clock: () => now,
      );
      final IdleDetector detector = createDetector(channelTracker: tracker);

      tracker.track('dev.tapwing/permissions', Completer<ByteData?>().future);
      now = const Duration(milliseconds: 1200);
      tracker.track('dev.tapwing/share', Completer<ByteData?>().future);
      tracker.track('dev.tapwing/permissions', Completer<ByteData?>().future);
      now = const Duration(milliseconds: 3000);

      expect(detector.busySources(), <Map<String, Object?>>[
        <String, Object?>{
          'source': 'channel',
          'channel': 'dev.tapwing/permissions',
          'pendingMs': 3000,
          'calls': 2,
        },
        <String, Object?>{
          'source': 'channel',
          'channel': 'dev.tapwing/share',
          'pendingMs': 1800,
        },
      ]);

      for (int i = 0; i < IdleDetector.maxBusyEntries; i++) {
        tracker.track('dev.tapwing/extra$i', Completer<ByteData?>().future);
      }
      final List<Map<String, Object?>> capped = detector.busySources();
      expect(capped, hasLength(IdleDetector.maxBusyEntries + 1));
      expect(capped.last, <String, Object?>{'source': 'channel', 'more': 2});
      tracker.reset();
    });

    testWidgets('names a blocking animation, and frames only when nothing '
        'else explains the wait', (WidgetTester tester) async {
      final AnimationController big = AnimationController(
        vsync: tester,
        duration: const Duration(seconds: 2),
      )..forward();
      try {
        await tester.pumpWidget(
          Directionality(
            textDirection: TextDirection.ltr,
            child: FadeTransition(
              opacity: big,
              child: const SizedBox.expand(
                child: ColoredBox(color: Color(0xFF4CAF50)),
              ),
            ),
          ),
        );
        await tester.pump(const Duration(milliseconds: 16));
        final List<Map<String, Object?>> animating = createDetector()
            .busySources();
        expect(animating, hasLength(1));
        expect(animating.single['source'], 'animation');
        expect(animating.single['widget'], 'FadeTransition');

        // A ticker that paints nothing significant: frames keep coming, no
        // animation blocks.
        big.stop();
        await tester.pumpWidget(
          const Directionality(
            textDirection: TextDirection.ltr,
            child: Text('Ticking'),
          ),
        );
        final Ticker ticker = tester.createTicker((_) {})..start();
        await tester.pump(const Duration(milliseconds: 16));
        final ChannelTracker tracker = ChannelTracker(
          safetyTimeout: Duration.zero,
        );
        final IdleDetector detector = createDetector(channelTracker: tracker);
        expect(detector.busySources(), <Map<String, Object?>>[
          <String, Object?>{'source': 'frames', 'transientCallbacks': 1},
        ]);

        // Next to a pending channel call the frames are not what holds the
        // settle, so only the channel is named.
        tracker.track('dev.tapwing/permissions', Completer<ByteData?>().future);
        expect(
          detector.busySources().map((Map<String, Object?> e) => e['source']),
          <String>['channel'],
        );
        tracker.reset();
        ticker.dispose();
      } finally {
        big.dispose();
      }
    });
  });

  group('ChannelTracker', () {
    test(
      'ignores flutter/* channels and releases on error or safety timeout',
      () async {
        final ChannelTracker tracker = ChannelTracker(
          safetyTimeout: const Duration(milliseconds: 40),
        );
        final _MockAsyncMessenger raw = _MockAsyncMessenger();
        final ObservingBinaryMessenger messenger = ObservingBinaryMessenger(
          raw,
          controller: TextInputController(),
          channelTracker: tracker,
        );

        // System channel send is ignored.
        unawaited(messenger.send('flutter/platform', ByteData(0)));
        expect(tracker.pending, 0);
        expect(raw.pendingReplies, hasLength(1));
        raw.pendingReplies.removeLast().complete(null);

        // Error reply decrements pending.
        final Future<ByteData?>? errFuture = messenger.send(
          'dev.tapwing/plugin',
          ByteData(0),
        );
        expect(tracker.pending, 1);
        raw.pendingReplies.removeLast().completeError(StateError('boom'));
        await expectLater(errFuture, throwsStateError);
        expect(tracker.pending, 0);

        // Never-replying channel call is released by the safety cap.
        unawaited(messenger.send('dev.tapwing/hanging', ByteData(0)));
        expect(tracker.pending, 1);
        await tracker.waitForIdle(const Duration(milliseconds: 200));
        expect(tracker.pending, 0);
        expect(tracker.isIdle, isTrue);
      },
    );

    test('hasPendingSince counts only tracked calls that started after the '
        'mark and still await their reply', () async {
      final ChannelTracker tracker = ChannelTracker(
        safetyTimeout: const Duration(milliseconds: 40),
      );
      final Completer<ByteData?> before = Completer<ByteData?>();
      tracker.track('dev.tapwing/poll', before.future);
      final int mark = tracker.started;
      expect(tracker.hasPendingSince(mark), isFalse);

      // System channels are not tracked, so they never count.
      tracker.track('flutter/platform', Completer<ByteData?>().future);
      expect(tracker.started, mark);
      expect(tracker.hasPendingSince(mark), isFalse);

      final Completer<ByteData?> after = Completer<ByteData?>();
      tracker.track('dev.tapwing/permissions', after.future);
      expect(tracker.hasPendingSince(mark), isTrue);

      // The older call getting its reply changes nothing.
      before.complete(null);
      await Future<void>.delayed(Duration.zero);
      expect(tracker.hasPendingSince(mark), isTrue);

      after.complete(null);
      await Future<void>.delayed(Duration.zero);
      expect(tracker.hasPendingSince(mark), isFalse);

      // A call released by the safety cap no longer counts either.
      final int second = tracker.started;
      tracker.track('dev.tapwing/hanging', Completer<ByteData?>().future);
      expect(tracker.hasPendingSince(second), isTrue);
      await tracker.waitForIdle(const Duration(milliseconds: 200));
      expect(tracker.hasPendingSince(second), isFalse);

      // A reply that arrives after reset() does not release a newer call.
      final Completer<ByteData?> stale = Completer<ByteData?>();
      tracker.track('dev.tapwing/stale', stale.future);
      tracker.reset();
      final int third = tracker.started;
      tracker.track('dev.tapwing/fresh', Completer<ByteData?>().future);
      stale.complete(null);
      await Future<void>.delayed(Duration.zero);
      expect(tracker.pending, 1);
      expect(tracker.hasPendingSince(third), isTrue);
      tracker.reset();
    });
  });
}

class _MockAsyncMessenger implements BinaryMessenger {
  final List<Completer<ByteData?>> pendingReplies = <Completer<ByteData?>>[];

  @override
  Future<ByteData?>? send(String channel, ByteData? message) {
    final Completer<ByteData?> completer = Completer<ByteData?>();
    pendingReplies.add(completer);
    return completer.future;
  }

  @override
  void setMessageHandler(String channel, MessageHandler? handler) {}

  @override
  // Required by Flutter's BinaryMessenger abstract interface.
  // ignore: deprecated_member_use
  Future<void> handlePlatformMessage(
    String channel,
    ByteData? data,
    ui.PlatformMessageResponseCallback? callback,
  ) async {}
}
