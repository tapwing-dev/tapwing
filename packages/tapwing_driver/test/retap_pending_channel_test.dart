import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/channel_tracker.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';

/// Messenger whose replies arrive only when the test completes them, like a
/// plugin call that waits for the user to answer a system alert.
class _HeldMessenger implements BinaryMessenger {
  final List<String> channels = <String>[];
  final List<Completer<ByteData?>> replies = <Completer<ByteData?>>[];

  @override
  Future<ByteData?>? send(String channel, ByteData? message) {
    channels.add(channel);
    final Completer<ByteData?> reply = Completer<ByteData?>();
    replies.add(reply);
    return reply.future;
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

void main() {
  const String permissionsChannel = 'dev.tapwing.sample_app/permissions';

  late ChannelTracker channelTracker;
  late _HeldMessenger rawMessenger;
  late MethodChannel permissions;

  setUp(() {
    // No safety timer, so a call stays pending for as long as a test wants.
    channelTracker = ChannelTracker(safetyTimeout: Duration.zero);
    rawMessenger = _HeldMessenger();
    permissions = MethodChannel(
      permissionsChannel,
      const StandardMethodCodec(),
      ObservingBinaryMessenger(
        rawMessenger,
        controller: TextInputController(),
        channelTracker: channelTracker,
      ),
    );
  });

  tearDown(() => channelTracker.reset());

  CommandDispatcher makeDispatcher() => CommandDispatcher(
    DriverContext(
      idle: IdleDetector(
        httpTracker: HttpTracker(),
        channelTracker: channelTracker,
        textInput: TextInputController(),
        readBottomInset: () => 0,
        waitForFirstFrame: (_) async {},
      ),
      pointer: PointerInput(),
      textInput: TextInputController(),
      readBottomInset: () => 0,
    ),
  );

  /// Pumps frames until [future] completes.
  ///
  /// The driver measures the settle deadline and the double-tap wait on a
  /// wall-clock [Stopwatch], while its timers run on the test's fake clock.
  /// Each pump therefore advances the fake clock by the wall time that has
  /// passed, so both clocks agree as they do in an app, on a loaded machine
  /// too.
  Future<Map<String, Object?>> runDriver(
    WidgetTester tester,
    Future<Object?> future,
  ) async {
    Map<String, Object?>? result;
    Object? error;
    unawaited(
      future.then<void>(
        (Object? value) => result = value! as Map<String, Object?>,
        onError: (Object e) => error = e,
      ),
    );
    final Stopwatch wall = Stopwatch()..start();
    Duration pumped = Duration.zero;
    while (result == null && error == null) {
      final Duration now = wall.elapsed;
      if (now > const Duration(seconds: 20)) {
        fail('driver command did not finish');
      }
      await tester.pump(now - pumped);
      pumped = now;
    }
    if (error != null) fail('driver command failed: $error');
    return result!;
  }

  Widget buttonApp({required VoidCallback onTap, VoidCallback? onDoubleTap}) =>
      Directionality(
        textDirection: TextDirection.ltr,
        child: Column(
          children: <Widget>[
            const Text('Permission: unknown'),
            GestureDetector(
              onTap: onTap,
              onDoubleTap: onDoubleTap,
              child: const SizedBox(
                width: 240,
                height: 64,
                child: Center(child: Text('Request Location')),
              ),
            ),
          ],
        ),
      );

  Map<String, Object?> tapRequest({int settleTimeoutMs = 150}) =>
      <String, Object?>{
        'selector': <String, Object?>{'text': 'Request Location'},
        'timeoutMs': 1000,
        'settleTimeoutMs': settleTimeoutMs,
        'retryIfNoChange': true,
      };

  group('retryIfNoChange and platform channel calls', () {
    testWidgets('a tap that started a channel call which is still pending is '
        'delivered once, although the lifecycle stays resumed and the '
        'semantics tree did not change', (WidgetTester tester) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        int taps = 0;
        await tester.pumpWidget(
          buttonApp(
            onTap: () {
              taps++;
              // Replies only once the user answers the system alert.
              unawaited(permissions.invokeMethod<bool>('requestLocation'));
            },
          ),
        );
        // iOS 27 keeps the scene active under a SpringBoard alert.
        tester.binding.handleAppLifecycleStateChanged(
          ui.AppLifecycleState.resumed,
        );

        final Map<String, Object?> result = await runDriver(
          tester,
          makeDispatcher().dispatch('tapOn', tapRequest()),
        );

        expect(result['settle'], 'timeout');
        expect(taps, 1, reason: 'the retry would land under the alert');
        expect(rawMessenger.channels, <String>[permissionsChannel]);
        expect(channelTracker.pending, 1);

        // The reply names what the settle waited for: the channel and how
        // long its call has been pending, never the method or its arguments.
        final List<Object?> busy = result['settleBusy']! as List<Object?>;
        expect(busy, hasLength(1));
        final Map<String, Object?> entry = busy.single! as Map<String, Object?>;
        expect(entry.keys, <String>['source', 'channel', 'pendingMs']);
        expect(entry['source'], 'channel');
        expect(entry['channel'], permissionsChannel);
        expect(entry['pendingMs'], greaterThanOrEqualTo(150));
      } finally {
        handle.dispose();
      }
    });

    testWidgets('a tap that changed nothing is still retried when the only '
        'pending channel call started before it', (WidgetTester tester) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        int taps = 0;
        await tester.pumpWidget(buttonApp(onTap: () => taps++));
        tester.binding.handleAppLifecycleStateChanged(
          ui.AppLifecycleState.resumed,
        );
        // Background traffic of the app, not caused by the tap.
        unawaited(permissions.invokeMethod<void>('longPoll'));
        expect(channelTracker.pending, 1);

        final Map<String, Object?> result = await runDriver(
          tester,
          makeDispatcher().dispatch('tapOn', tapRequest()),
        );

        expect(result['settle'], 'timeout');
        expect(taps, 2);
      } finally {
        handle.dispose();
      }
    });

    testWidgets('a tap whose channel call already got its reply is retried '
        'like any tap that changed nothing', (WidgetTester tester) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        int taps = 0;
        await tester.pumpWidget(
          buttonApp(
            onTap: () {
              taps++;
              unawaited(permissions.invokeMethod<void>('ping'));
              rawMessenger.replies.last.complete(
                const StandardMethodCodec().encodeSuccessEnvelope(null),
              );
            },
          ),
        );

        final Map<String, Object?> result = await runDriver(
          tester,
          makeDispatcher().dispatch('tapOn', tapRequest()),
        );

        expect(result['settle'], 'idle');
        expect(result.containsKey('settleBusy'), isFalse);
        expect(taps, 2);
        expect(channelTracker.pending, 0);
      } finally {
        handle.dispose();
      }
    });

    testWidgets('a tap whose handler starts its channel call only during '
        'the double-tap wait is delivered once', (WidgetTester tester) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        int taps = 0;
        await tester.pumpWidget(
          buttonApp(
            // Nothing is pending right after the tap, so the settle ends
            // `idle` at once and the driver waits out `kDoubleTapTimeout`
            // before a retry. The call starts within that wait, like the
            // `onTap` of a widget that also has `onDoubleTap`.
            onTap: () {
              taps++;
              Timer(const Duration(milliseconds: 100), () {
                unawaited(permissions.invokeMethod<bool>('requestLocation'));
              });
            },
          ),
        );

        final Map<String, Object?> result = await runDriver(
          tester,
          makeDispatcher().dispatch('tapOn', tapRequest()),
        );

        expect(result['settle'], 'idle');
        expect(taps, 1);
        expect(channelTracker.pending, 1);
      } finally {
        handle.dispose();
      }
    });

    testWidgets('waitIdle that times out names the pending channel too', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(buttonApp(onTap: () {}));
        unawaited(permissions.invokeMethod<void>('longPoll'));
        unawaited(permissions.invokeMethod<void>('longPoll'));

        final Map<String, Object?> result = await runDriver(
          tester,
          makeDispatcher().dispatch('waitIdle', <String, Object?>{
            'timeoutMs': 100,
          }),
        );

        expect(result['state'], 'timeout');
        final Map<String, Object?> entry =
            (result['settleBusy']! as List<Object?>).single!
                as Map<String, Object?>;
        expect(entry['channel'], permissionsChannel);
        expect(entry['calls'], 2);
        expect(entry['pendingMs'], greaterThanOrEqualTo(100));

        // The next command starts clean: no stale diagnostics on a result
        // that did not time out.
        for (final Completer<ByteData?> reply in rawMessenger.replies) {
          reply.complete(
            const StandardMethodCodec().encodeSuccessEnvelope(null),
          );
        }
        final Map<String, Object?> idle = await runDriver(
          tester,
          makeDispatcher().dispatch('waitIdle', <String, Object?>{
            'timeoutMs': 100,
          }),
        );
        expect(idle['state'], 'idle');
        expect(idle.containsKey('settleBusy'), isFalse);
      } finally {
        handle.dispose();
      }
    });

    testWidgets('tapPoint follows the same rule', (WidgetTester tester) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        int taps = 0;
        await tester.pumpWidget(
          buttonApp(
            onTap: () {
              taps++;
              unawaited(permissions.invokeMethod<bool>('requestLocation'));
            },
          ),
        );
        final ui.Offset center = tester.getCenter(
          find.text('Request Location'),
        );
        final double dpr = tester.view.devicePixelRatio;

        final Map<String, Object?> result = await runDriver(
          tester,
          makeDispatcher().dispatch('tapPoint', <String, Object?>{
            'point':
                '${(center.dx * dpr).round()}, ${(center.dy * dpr).round()}',
            'settleTimeoutMs': 150,
            'retryIfNoChange': true,
          }),
        );

        expect(result['settle'], 'timeout');
        expect(taps, 1);
      } finally {
        handle.dispose();
      }
    });
  });
}
