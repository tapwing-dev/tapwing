import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

/// Records how every `waitForIdle` call ended.
class _RecordingIdleDetector extends IdleDetector {
  _RecordingIdleDetector({required super.clock})
    : super(
        httpTracker: HttpTracker(),
        textInput: TextInputController(),
        readBottomInset: () => 0,
        waitForFirstFrame: (_) async {},
      );

  final List<String> results = <String>[];

  @override
  Future<String> waitForIdle(
    Duration timeout, {
    SettlePolicy policy = SettlePolicy.v2,
    int? stableFrames,
    bool includeScrollables = true,
    double? maxContentVelocity,
    bool Function()? isCancelled,
  }) async {
    final String result = await super.waitForIdle(
      timeout,
      policy: policy,
      stableFrames: stableFrames,
      includeScrollables: includeScrollables,
      maxContentVelocity: maxContentVelocity,
      isCancelled: isCancelled,
    );
    results.add(result);
    return result;
  }
}

/// Returns scripted `waitForIdle` outcomes (the last one repeats) and records
/// whether each call asked for the between-swipes tolerance.
class _ScriptedIdleDetector extends IdleDetector {
  _ScriptedIdleDetector(this.outcomes)
    : super(
        httpTracker: HttpTracker(),
        textInput: TextInputController(),
        readBottomInset: () => 0,
        waitForFirstFrame: (_) async {},
      );

  final List<String> outcomes;
  final List<bool> toleranceRequested = <bool>[];
  final List<Duration> timeouts = <Duration>[];
  int _next = 0;

  @override
  Future<String> waitForIdle(
    Duration timeout, {
    SettlePolicy policy = SettlePolicy.v2,
    int? stableFrames,
    bool includeScrollables = true,
    double? maxContentVelocity,
    bool Function()? isCancelled,
  }) async {
    toleranceRequested.add(maxContentVelocity != null);
    timeouts.add(timeout);
    final String outcome =
        outcomes[_next < outcomes.length ? _next : outcomes.length - 1];
    _next++;
    return outcome;
  }
}

Snapshot _snapshotWith(ui.Rect? target) {
  const ui.Size screen = ui.Size(1000, 2000);
  KNode node({KNode? parent, required ui.Rect bounds, String? text}) {
    final KNode n = KNode(
      id: text == null ? 1 : 2,
      parent: parent,
      bounds: bounds,
      text: text,
      hintText: null,
      accessibilityText: text,
      resourceId: '',
      clickable: false,
      enabled: true,
      focused: false,
      checked: false,
      selected: false,
      isTextField: false,
      scrollable: false,
    );
    parent?.children.add(n);
    return n;
  }

  final KNode root = node(bounds: ui.Offset.zero & screen);
  final List<KNode> nodes = <KNode>[root];
  if (target != null) {
    nodes.add(node(parent: root, bounds: target, text: 'Target'));
  }
  return Snapshot(root, nodes, screen, target?.top.toInt() ?? 0);
}

/// Hosts [child] without Material so the tests stay free of its deprecation.
Widget _host(WidgetTester tester, Widget child) => Directionality(
  textDirection: TextDirection.ltr,
  child: MediaQuery(data: MediaQueryData.fromView(tester.view), child: child),
);

void main() {
  late _RecordingIdleDetector idle;

  CommandDispatcher dispatcherFor(WidgetTester tester) {
    final DateTime epoch = tester.binding.clock.now();
    Duration clock() => tester.binding.clock.now().difference(epoch);
    idle = _RecordingIdleDetector(clock: clock);
    final TextInputController textInput = TextInputController();
    return CommandDispatcher(
      DriverContext(
        idle: idle,
        pointer: PointerInput(clock: clock),
        textInput: textInput,
        readBottomInset: () => 0,
      ),
    );
  }

  /// Drives FakeAsync frames until [future] completes; returns its value and
  /// the fake time it took.
  Future<(T, Duration)> runDriver<T>(
    WidgetTester tester,
    Future<T> future,
  ) async {
    T? result;
    Object? error;
    StackTrace? stack;
    bool done = false;
    unawaited(
      future.then(
        (T value) {
          result = value;
          done = true;
        },
        onError: (Object e, StackTrace s) {
          error = e;
          stack = s;
          done = true;
        },
      ),
    );
    final DateTime start = tester.binding.clock.now();
    for (int i = 0; i < 4000 && !done; i++) {
      await tester.pump(const Duration(milliseconds: 16));
    }
    expect(done, isTrue, reason: 'Driver RPC did not complete');
    if (error != null) Error.throwWithStackTrace(error!, stack!);
    return (result as T, tester.binding.clock.now().difference(start));
  }

  Future<(Map<String, Object?>, Duration)> scrollTo(
    WidgetTester tester,
    CommandDispatcher dispatcher,
    String text, {
    String direction = 'DOWN',
    int durationMs = 601,
    int settleTimeoutMs = 1500,
    int visibilityPercentage = 100,
  }) async {
    final (Object?, Duration) out = await runDriver<Object?>(
      tester,
      dispatcher.dispatch('scrollUntilVisible', <String, Object?>{
        'selector': <String, Object?>{'text': text},
        'direction': direction,
        'durationMs': durationMs,
        'visibilityPercentage': visibilityPercentage,
        'timeoutMs': 60000,
        'settleTimeoutMs': settleTimeoutMs,
      }),
    );
    return (out.$1! as Map<String, Object?>, out.$2);
  }

  bool scrolling(WidgetTester tester) => tester
      .stateList<ScrollableState>(find.byType(Scrollable))
      .any((ScrollableState s) => s.position.isScrollingNotifier.value);

  Widget list(
    WidgetTester tester,
    ScrollPhysics physics, {
    void Function(int)? onTap,
  }) => _host(
    tester,
    ListView.builder(
      physics: physics,
      itemExtent: 72,
      itemCount: 400,
      itemBuilder: (BuildContext context, int i) => GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap == null ? null : () => onTap(i),
        child: Center(child: Text('Item $i')),
      ),
    ),
  );

  group('scrollUntilVisible between swipes', () {
    testWidgets(
      'bouncing list: swipes follow once the coast is slow, not after the whole tail',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(list(tester, const BouncingScrollPhysics()));
          final CommandDispatcher dispatcher = dispatcherFor(tester);

          final (Map<String, Object?> result, Duration took) = await scrollTo(
            tester,
            dispatcher,
            'Item 60',
          );

          final int swipes = result['swipes']! as int;
          expect(swipes, greaterThan(1));
          // Every intermediate wait ended on observed motion, the last full
          // settle ended with the list at rest.
          expect(idle.results.where((String r) => r == 'calm'), isNotEmpty);
          expect(scrolling(tester), isFalse);
          // A bouncing fling creeps for over a second; waiting the default
          // 1.5 s cap after every swipe would cost at least that much each.
          expect(
            took,
            lessThan(Duration(milliseconds: 1300 * swipes)),
            reason: 'waited the whole coast after $swipes swipes',
          );
          final Map<String, Object?> element =
              result['element']! as Map<String, Object?>;
          expect(element['accessibilityText'], 'Item 60');
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets('clamping list: finds the target and ends at rest', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(list(tester, const ClampingScrollPhysics()));
        final CommandDispatcher dispatcher = dispatcherFor(tester);

        final (Map<String, Object?> result, _) = await scrollTo(
          tester,
          dispatcher,
          'Item 60',
        );

        expect(result['swipes']! as int, greaterThan(1));
        expect(scrolling(tester), isFalse);
        expect(find.text('Item 60').hitTestable(), findsOneWidget);
      } finally {
        handle.dispose();
      }
    });

    testWidgets(
      'target entering the viewport mid-coast is still visible once the list rests',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(list(tester, const BouncingScrollPhysics()));
          final CommandDispatcher dispatcher = dispatcherFor(tester);

          // Sweep targets so some enter the viewport during the coast of a
          // swipe and others between two swipes; each must end fully visible.
          for (final int index in <int>[14, 23, 31, 47]) {
            final (Map<String, Object?> result, _) = await scrollTo(
              tester,
              dispatcher,
              'Item $index',
            );
            expect(scrolling(tester), isFalse);
            final Rect box = tester.getRect(find.text('Item $index'));
            final Size view =
                tester.view.physicalSize / tester.view.devicePixelRatio;
            expect(
              box.top >= 0 && box.bottom <= view.height,
              isTrue,
              reason: 'Item $index not fully visible at rest: $box',
            );
            expect(
              (result['element']! as Map<String, Object?>)['accessibilityText'],
              'Item $index',
            );
          }
          expect(idle.results, contains('calm'));
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets('a tap right after the step is not swallowed by the coast', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        final List<int> taps = <int>[];
        await tester.pumpWidget(
          list(tester, const BouncingScrollPhysics(), onTap: taps.add),
        );
        final CommandDispatcher dispatcher = dispatcherFor(tester);

        await scrollTo(tester, dispatcher, 'Item 60');
        expect(idle.results, contains('calm'));
        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('tapOn', <String, Object?>{
            'selector': <String, Object?>{'text': 'Item 60'},
            'timeoutMs': 5000,
          }),
        );

        expect(taps, <int>[60]);
      } finally {
        handle.dispose();
      }
    });

    testWidgets('PageView still snaps to the page the swipe leads to', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      final PageController controller = PageController();
      try {
        await tester.pumpWidget(
          _host(
            tester,
            PageView(
              controller: controller,
              physics: const BouncingScrollPhysics().applyTo(
                const PageScrollPhysics(),
              ),
              children: <Widget>[
                for (int i = 1; i <= 5; i++)
                  Center(child: Text('Snap Page $i')),
              ],
            ),
          ),
        );
        final CommandDispatcher dispatcher = dispatcherFor(tester);

        final (Map<String, Object?> result, _) = await scrollTo(
          tester,
          dispatcher,
          'Snap Page 4',
          direction: 'RIGHT',
          durationMs: 400,
        );

        expect(result['swipes']! as int, greaterThanOrEqualTo(3));
        expect(scrolling(tester), isFalse);
        // At rest exactly on a page, never between two.
        expect(controller.page, 3.0);
      } finally {
        controller.dispose();
        handle.dispose();
      }
    });

    group('with scripted observations', () {
      // 1000 x 2000 screen; the target is fully visible at `inView` and above
      // the top edge (gone) at `gone`.
      const ui.Rect inView = ui.Rect.fromLTWH(0, 300, 1000, 100);
      const ui.Rect gone = ui.Rect.fromLTWH(0, -150, 1000, 100);

      Future<(Map<String, Object?>, _ScriptedIdleDetector)> run(
        WidgetTester tester, {
        required List<ui.Rect?> observations,
        required List<String> settles,
      }) async {
        await tester.pumpWidget(const SizedBox());
        final _ScriptedIdleDetector scripted = _ScriptedIdleDetector(settles);
        int next = 0;
        final DateTime epoch = tester.binding.clock.now();
        final CommandDispatcher dispatcher = CommandDispatcher(
          DriverContext(
            idle: scripted,
            pointer: PointerInput(
              clock: () => tester.binding.clock.now().difference(epoch),
            ),
            textInput: TextInputController(),
            readBottomInset: () => 0,
            readScreenSize: () => const ui.Size(1000, 2000),
            captureSnapshot: () {
              final ui.Rect? rect =
                  observations[next < observations.length
                      ? next
                      : observations.length - 1];
              next++;
              return _snapshotWith(rect);
            },
          ),
        );
        final (Map<String, Object?> result, _) = await scrollTo(
          tester,
          dispatcher,
          'Target',
        );
        return (result, scripted);
      }

      testWidgets(
        'a target that left the viewport while the list finished coasting is not returned',
        (WidgetTester tester) async {
          final (
            Map<String, Object?> result,
            _ScriptedIdleDetector idle,
          ) = await run(
            tester,
            // check, [swipe], check (seen), full settle, at rest: gone,
            // [swipe], check (seen), full settle, at rest: still there.
            observations: <ui.Rect?>[null, inView, gone, inView, inView],
            settles: <String>['calm', 'stable', 'calm', 'stable'],
          );

          expect(result['swipes'], 2);
          expect(
            (result['element']! as Map<String, Object?>)['bounds']!
                as List<Object?>,
            <int>[0, 300, 1000, 400],
          );
          // Both swipes' waits asked for the coast tolerance, the full settles
          // did not.
          expect(idle.toleranceRequested, <bool>[true, false, true, false]);
          // The full settle gets back what the early exit left unused, so
          // from the swipe on the step waits for rest as long as it ever did.
          expect(idle.timeouts[0], const Duration(milliseconds: 1500));
          expect(
            idle.timeouts[1],
            greaterThan(const Duration(milliseconds: 2900)),
          );
        },
      );

      testWidgets(
        'a wait that ended on a stable tree needs no second look at rest',
        (WidgetTester tester) async {
          final (
            Map<String, Object?> result,
            _ScriptedIdleDetector idle,
          ) = await run(
            tester,
            observations: <ui.Rect?>[null, inView, gone],
            settles: <String>['stable', 'stable'],
          );

          // Same as before the coast tolerance existed: the full settle
          // follows the check that found the target, nothing more.
          expect(result['swipes'], 1);
          expect(idle.timeouts[1], const Duration(milliseconds: 1500));
        },
      );
    });
  });
}
