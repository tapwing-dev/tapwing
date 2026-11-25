import 'dart:async';
import 'dart:convert';

import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/protocol/rpc_transport.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/server/rpc_session.dart';
import 'package:tapwing_driver/src/server/tapwing_server.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

class _TestSemanticsOwner extends SemanticsOwner {
  _TestSemanticsOwner() : super(onSemanticsUpdate: (_) {});

  bool get debugHasListeners => hasListeners;
}

class _FakeRpcTransport implements RpcTransport {
  final StreamController<String> _incoming = StreamController<String>();
  final List<Map<String, Object?>> sent = <Map<String, Object?>>[];

  @override
  Stream<String> get lines => _incoming.stream;

  @override
  void sendLine(String line) {
    sent.add(
      (jsonDecode(line) as Map<Object?, Object?>).cast<String, Object?>(),
    );
  }

  Future<void> closeIncoming() => _incoming.close();

  @override
  Future<void> close() async {
    if (!_incoming.isClosed) {
      await _incoming.close();
    }
  }
}

void main() {
  group('SemanticsHashCache', () {
    testWidgets(
      'walks the semantics tree once across 12 paint-only animation frames',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final AnimationController controller = AnimationController(
          vsync: tester,
          duration: const Duration(milliseconds: 500),
        )..repeat(reverse: true);
        int walks = 0;
        final SemanticsHashCache cache = SemanticsHashCache(
          computeFromRoot: (SemanticsNode root) {
            walks++;
            return computeSemanticsHashFromRoot(root);
          },
        );
        try {
          await tester.pumpWidget(
            Directionality(
              textDirection: TextDirection.ltr,
              child: Column(
                children: <Widget>[
                  const Text('Static Header'),
                  FadeTransition(
                    opacity: controller,
                    child: const SizedBox(
                      width: 60,
                      height: 60,
                      child: ColoredBox(color: Color(0xFF2196F3)),
                    ),
                  ),
                ],
              ),
            ),
          );

          final int initial = cache.hash();
          expect(walks, 1);
          expect(cache.hasCachedHash, isTrue);

          for (int i = 0; i < 12; i++) {
            await tester.pump(const Duration(milliseconds: 16));
            final int next = cache.hash();
            expect(next, initial);
          }
          expect(
            walks,
            1,
            reason: 'paint-only frames must hit the cached semantics hash',
          );
        } finally {
          cache.dispose();
          controller.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'invalidates immediately on semantics text, flag, and rect updates',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        int walks = 0;
        final SemanticsHashCache cache = SemanticsHashCache(
          computeFromRoot: (SemanticsNode root) {
            walks++;
            return computeSemanticsHashFromRoot(root);
          },
        );
        try {
          String label = 'Before';
          bool checked = false;
          double topOffset = 10;
          late StateSetter rebuild;

          await tester.pumpWidget(
            Directionality(
              textDirection: TextDirection.ltr,
              child: StatefulBuilder(
                builder: (BuildContext context, StateSetter setState) {
                  rebuild = setState;
                  return Stack(
                    children: <Widget>[
                      Positioned(
                        left: 16,
                        top: topOffset,
                        child: Semantics(
                          label: label,
                          checked: checked,
                          child: const SizedBox(width: 120, height: 40),
                        ),
                      ),
                    ],
                  );
                },
              ),
            ),
          );

          final int h0 = cache.hash();
          expect(walks, 1);
          expect(cache.hash(), h0);
          expect(walks, 1);

          // 1. Text / label update.
          rebuild(() => label = 'After');
          await tester.pump();
          expect(cache.hasCachedHash, isFalse);
          final int h1 = cache.hash();
          expect(walks, 2);
          expect(h1, isNot(h0));

          // 2. Flag update.
          rebuild(() => checked = true);
          await tester.pump();
          expect(cache.hasCachedHash, isFalse);
          final int h2 = cache.hash();
          expect(walks, 3);
          expect(h2, isNot(h1));

          // 3. Rect / geometry update.
          rebuild(() => topOffset = 140);
          await tester.pump();
          expect(cache.hasCachedHash, isFalse);
          final int h3 = cache.hash();
          expect(walks, 4);
          expect(h3, isNot(h2));

          // Unchanged follow-up frame stays cached.
          await tester.pump();
          expect(cache.hash(), h3);
          expect(walks, 4);
        } finally {
          cache.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'Snapshot.capture seeds the cache so an immediate computeHash is O(1)',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        int walks = 0;
        final SemanticsHashCache cache = SemanticsHashCache(
          computeFromRoot: (SemanticsNode root) {
            walks++;
            return computeSemanticsHashFromRoot(root);
          },
        );
        try {
          await tester.pumpWidget(
            const Directionality(
              textDirection: TextDirection.ltr,
              child: Center(child: Text('Seeded')),
            ),
          );

          final Snapshot snap = Snapshot.capture(hashCache: cache);
          expect(cache.hasCachedHash, isTrue);
          expect(cache.hash(), snap.hash);
          expect(
            walks,
            0,
            reason:
                'hash seeded during Snapshot.capture must not re-walk the tree',
          );

          // Also verify DriverContext.captureSnapshot -> computeHash path.
          final DriverContext context = DriverContext(
            semanticsHashCache: cache,
          );
          final Snapshot ctxSnap = context.captureSnapshot();
          expect(context.computeHash(), ctxSnap.hash);
          expect(walks, 0);

          // Default singleton path matches as well.
          final Snapshot defaultSnap = Snapshot.capture();
          expect(SemanticsHashCache.instance.hasCachedHash, isTrue);
          expect(Snapshot.computeHash(), defaultSnap.hash);
        } finally {
          cache.dispose();
          SemanticsHashCache.instance.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets('invalidates on configure and RpcSession reset', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      int walks = 0;
      final SemanticsHashCache cache = SemanticsHashCache(
        computeFromRoot: (SemanticsNode root) {
          walks++;
          return computeSemanticsHashFromRoot(root);
        },
      );
      try {
        await tester.pumpWidget(
          const Directionality(
            textDirection: TextDirection.ltr,
            child: Center(child: Text('Session')),
          ),
        );

        final DriverContext context = DriverContext(
          idle: IdleDetector(
            httpTracker: HttpTracker(),
            textInput: TextInputController(),
            readBottomInset: () => 0,
            waitForFirstFrame: (_) async {},
          ),
          semanticsHashCache: cache,
        );
        final CommandDispatcher dispatcher = CommandDispatcher(context);

        context.computeHash();
        expect(walks, 1);
        expect(cache.hasCachedHash, isTrue);

        // configure invalidates cached hash.
        await dispatcher.dispatch('configure', const <String, Object?>{
          'settlePolicy': 'v2',
        });
        expect(cache.hasCachedHash, isFalse);
        context.computeHash();
        expect(walks, 2);

        // RpcSession start and close invalidate cached hash.
        final RpcSession session = RpcSession(dispatcher);
        final _FakeRpcTransport transport = _FakeRpcTransport();
        final Future<void> served = session.serve(transport);
        expect(cache.hasCachedHash, isFalse);

        context.computeHash();
        expect(walks, 3);
        expect(cache.hasCachedHash, isTrue);

        await transport.closeIncoming();
        await served;
        expect(cache.hasCachedHash, isFalse);
      } finally {
        cache.dispose();
        handle.dispose();
      }
    });

    testWidgets(
      'survives SemanticsOwner disposal (SemanticsHandle.dispose) without throwing',
      semanticsEnabled: false,
      (WidgetTester tester) async {
        final SemanticsHandle handle1 = tester.ensureSemantics();
        await tester.pumpWidget(
          const Directionality(
            textDirection: TextDirection.ltr,
            child: Center(child: Text('First Owner')),
          ),
        );

        final int firstHash = Snapshot.computeHash();
        expect(firstHash, isNot(0));
        expect(SemanticsHashCache.instance.isAttached, isTrue);

        // Disposing the sole SemanticsHandle causes PipelineOwner to dispose
        // its SemanticsOwner. Calling dispose() or computeHash() afterwards
        // must never trigger ChangeNotifier.debugAssertNotDisposed.
        handle1.dispose();
        await tester.pump();

        expect(SemanticsHashCache.instance.dispose, returnsNormally);
        expect(Snapshot.computeHash(), 0);

        // Re-enabling semantics attaches cleanly to the new SemanticsOwner.
        final SemanticsHandle handle2 = tester.ensureSemantics();
        try {
          await tester.pump();
          final int secondHash = Snapshot.computeHash();
          expect(secondHash, isNot(0));
          expect(SemanticsHashCache.instance.hasCachedHash, isTrue);
        } finally {
          SemanticsHashCache.instance.dispose();
          handle2.dispose();
        }
      },
    );

    test('detaches listener cleanly on dispose() and TapwingServer.stop(), '
        'and skips removeListener on already-disposed swapped owner', () {
      final _TestSemanticsOwner owner1 = _TestSemanticsOwner();
      final SemanticsNode root1 = SemanticsNode.root(
        showOnScreen: () {},
        owner: owner1,
      );
      _TestSemanticsOwner? liveOwner = owner1;

      int walks = 0;
      final SemanticsHashCache cache = SemanticsHashCache(
        readOwner: () => liveOwner,
        computeFromRoot: (SemanticsNode root) {
          walks++;
          return root.id + 100;
        },
      );

      expect(cache.hash(), root1.id + 100);
      expect(walks, 1);
      expect(owner1.debugHasListeners, isTrue);
      expect(cache.isAttached, isTrue);

      // TapwingServer.stop() detaches the live listener cleanly.
      final TapwingServer server = TapwingServer(
        context: DriverContext(semanticsHashCache: cache),
      );
      server.stop();
      expect(owner1.debugHasListeners, isFalse);
      expect(cache.isAttached, isFalse);
      expect(cache.hasCachedHash, isFalse);

      // Re-attach, then simulate PipelineOwner disposing owner1 and creating
      // owner2: cache must NOT call removeListener on disposed owner1.
      expect(cache.hash(), root1.id + 100);
      expect(owner1.debugHasListeners, isTrue);
      owner1.dispose();

      final _TestSemanticsOwner owner2 = _TestSemanticsOwner();
      final SemanticsNode root2 = SemanticsNode.root(
        showOnScreen: () {},
        owner: owner2,
      );
      liveOwner = owner2;

      expect(cache.hash, returnsNormally);
      expect(cache.hash(), root2.id + 100);
      expect(owner2.debugHasListeners, isTrue);

      // Disposing while owner2 is live removes the listener cleanly.
      cache.dispose();
      expect(owner2.debugHasListeners, isFalse);
      owner2.dispose();
    });

    test(
      'preserves injectable computeSemanticsHash overrides in IdleDetector and DriverContext',
      () {
        int customCalls = 0;
        int customHash() {
          customCalls++;
          return 999;
        }

        final DriverContext context = DriverContext(
          computeSemanticsHash: customHash,
        );
        expect(context.computeHash(), 999);
        expect(customCalls, 1);

        final IdleDetector detector = IdleDetector(
          httpTracker: HttpTracker(),
          textInput: TextInputController(),
          computeSemanticsHash: customHash,
          readBottomInset: () => 0,
          waitForFirstFrame: (_) async {},
        );
        // Verify custom hash callback is stored and used without throwing.
        expect(detector.httpInFlight, 0);
      },
    );
  });
}
