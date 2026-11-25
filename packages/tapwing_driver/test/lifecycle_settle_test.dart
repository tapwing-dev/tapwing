import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/scheduler.dart';
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

void main() {
  IdleDetector createDetector({
    ChannelTracker? channelTracker,
    HttpTracker? httpTracker,
    ui.AppLifecycleState? Function()? readLifecycleState,
  }) => IdleDetector(
    httpTracker: httpTracker ?? HttpTracker(),
    channelTracker: channelTracker ?? ChannelTracker(),
    textInput: TextInputController(),
    readBottomInset: () => 0,
    readLifecycleState: readLifecycleState,
    waitForFirstFrame: (_) async {},
  );

  group('Lifecycle-aware early settle exit', () {
    testWidgets(
      'pending ChannelTracker blocks waitForIdle while resumed (or null), '
      'and wakes immediately (<50 ms vs 3000 ms) on AppLifecycleState.inactive',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final ChannelTracker channelTracker = ChannelTracker();
        try {
          await tester.pumpWidget(
            const Directionality(
              textDirection: TextDirection.ltr,
              child: Center(child: Text('Permission Screen')),
            ),
          );

          // In TestWidgetsFlutterBinding, lifecycleState starts as null or
          // resumed; neither should prematurely exit settle.
          final ui.AppLifecycleState? initial =
              SchedulerBinding.instance.lifecycleState;
          expect(
            initial == null || initial == ui.AppLifecycleState.resumed,
            isTrue,
          );

          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );

          final Completer<ByteData?> pendingPermission = Completer<ByteData?>();
          channelTracker.track(
            'dev.tapwing.sample_app/permissions',
            pendingPermission.future,
          );
          expect(channelTracker.pending, 1);

          final IdleDetector detector = createDetector(
            channelTracker: channelTracker,
          );

          String? settleState;
          final DateTime start = tester.binding.clock.now();
          Duration? finishedAt;
          unawaited(
            detector.waitForIdle(const Duration(seconds: 3)).then((String s) {
              settleState = s;
              finishedAt = tester.binding.clock.now().difference(start);
            }),
          );

          // At 48 ms while still resumed, waitForIdle MUST still be waiting
          // for the pending channel reply.
          await tester.pump(const Duration(milliseconds: 48));
          expect(settleState, isNull);

          // Simulate OS pausing the Flutter app for a system permission dialog
          // at 50 ms. waitForIdle must wake immediately without waiting the
          // remaining 2950 ms and without leaving any pending timers.
          await tester.pump(const Duration(milliseconds: 2));
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.inactive,
          );
          await tester.pump();

          expect(settleState, 'background');
          expect(finishedAt, const Duration(milliseconds: 50));
          // Pending channel stays tracked until the user dismisses the dialog
          // in the next step.
          expect(channelTracker.pending, 1);
        } finally {
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );
          channelTracker.reset();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'exits immediately for all non-resumed states (inactive, paused, hidden, detached)',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final ChannelTracker channelTracker = ChannelTracker();
        try {
          await tester.pumpWidget(
            const Directionality(
              textDirection: TextDirection.ltr,
              child: Center(child: Text('Paused')),
            ),
          );
          channelTracker.track(
            'dev.tapwing.sample_app/permissions',
            Completer<ByteData?>().future,
          );

          for (final ui.AppLifecycleState state in <ui.AppLifecycleState>[
            ui.AppLifecycleState.inactive,
            ui.AppLifecycleState.paused,
            ui.AppLifecycleState.hidden,
            ui.AppLifecycleState.detached,
          ]) {
            final IdleDetector detector = createDetector(
              channelTracker: channelTracker,
              readLifecycleState: () => state,
            );
            final String result = await detector.waitForIdle(
              const Duration(seconds: 3),
            );
            expect(result, 'background', reason: 'state=$state');
          }
        } finally {
          channelTracker.reset();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'wakes immediately during active animation frames when OS pauses the app',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final AnimationController ctrl = AnimationController(
          vsync: tester,
          duration: const Duration(seconds: 2),
        )..repeat();
        try {
          await tester.pumpWidget(
            Directionality(
              textDirection: TextDirection.ltr,
              child: FadeTransition(
                opacity: ctrl,
                child: const SizedBox.expand(
                  child: ColoredBox(color: Color(0xFF4CAF50)),
                ),
              ),
            ),
          );
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );

          final IdleDetector detector = createDetector();
          String? settleState;
          final DateTime start = tester.binding.clock.now();
          Duration? finishedAt;
          unawaited(
            detector.waitForIdle(const Duration(seconds: 3)).then((String s) {
              settleState = s;
              finishedAt = tester.binding.clock.now().difference(start);
            }),
          );

          await tester.pump(const Duration(milliseconds: 16));
          expect(settleState, isNull);

          // Pause mid-animation: must wake immediately without waiting for the
          // 100 ms frame step cap or the 2 s animation.
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.paused,
          );
          await tester.pump();

          expect(settleState, 'background');
          expect(finishedAt, const Duration(milliseconds: 16));
        } finally {
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );
          ctrl.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'supports injected readLifecycleState callback transitions without binding events',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final ChannelTracker channelTracker = ChannelTracker();
        try {
          await tester.pumpWidget(
            const Directionality(
              textDirection: TextDirection.ltr,
              child: Center(child: Text('Injected Lifecycle')),
            ),
          );
          channelTracker.track(
            'dev.tapwing/permissions',
            Completer<ByteData?>().future,
          );

          ui.AppLifecycleState? fakeState = ui.AppLifecycleState.resumed;
          final IdleDetector detector = createDetector(
            channelTracker: channelTracker,
            readLifecycleState: () => fakeState,
          );

          String? settleState;
          unawaited(
            detector
                .waitForIdle(const Duration(seconds: 3))
                .then((String s) => settleState = s),
          );

          await tester.pump(const Duration(milliseconds: 25));
          expect(settleState, isNull);

          // Flip the injected callback to inactive without calling binding.
          fakeState = ui.AppLifecycleState.inactive;
          await tester.pump(const Duration(milliseconds: 25));
          expect(settleState, 'background');
        } finally {
          channelTracker.reset();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'tapOn permission button with retryIfNoChange=true exits on OS pause '
      'without burning 3000 ms settle or double-tapping over the dialog',
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
          final MethodChannel permissions = MethodChannel(
            'dev.tapwing.sample_app/permissions',
            const StandardMethodCodec(),
            messenger,
          );

          int tapCount = 0;
          String status = 'Permission: unknown';
          await tester.pumpWidget(
            Directionality(
              textDirection: TextDirection.ltr,
              child: StatefulBuilder(
                builder: (BuildContext context, StateSetter setState) => Column(
                  children: <Widget>[
                    Text(status),
                    GestureDetector(
                      onTap: () async {
                        tapCount++;
                        final bool granted =
                            await permissions.invokeMethod<bool>(
                              'requestLocation',
                            ) ??
                            false;
                        setState(
                          () => status = granted
                              ? 'Permission: granted'
                              : 'Permission: denied',
                        );
                      },
                      child: const SizedBox(
                        width: 240,
                        height: 64,
                        child: Center(child: Text('Request Location')),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          );
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
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

          Map<String, Object?>? tapResult;
          final DateTime start = tester.binding.clock.now();
          Duration? tapDuration;
          unawaited(
            dispatcher
                .dispatch('tapOn', <String, Object?>{
                  'selector': <String, Object?>{'text': 'Request Location'},
                  'timeoutMs': 1000,
                  'settleTimeoutMs': 3000,
                  'retryIfNoChange': true,
                })
                .then((Object? value) {
                  tapResult = value! as Map<String, Object?>;
                  tapDuration = tester.binding.clock.now().difference(start);
                }),
          );

          // Pump past pre-tap settle and pointer-up; permission request is now
          // pending in ChannelTracker and app is still resumed.
          await tester.pump(const Duration(milliseconds: 16));
          await tester.pump(const Duration(milliseconds: 16));
          expect(tapCount, 1);
          expect(channelTracker.pending, 1);
          expect(tapResult, isNull);

          // OS presents the permission dialog -> app transitions to inactive.
          await tester.pump(const Duration(milliseconds: 18));
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.inactive,
          );
          await tester.pump();
          await tester.pump();

          expect(tapResult, isNotNull);
          expect(tapResult!['settle'], 'background');
          expect(
            tapDuration!.inMilliseconds,
            lessThan(100),
            reason:
                'tapOn must not stall for 3000 ms settle or 300 ms retry arena',
          );
          expect(
            tapCount,
            1,
            reason: 'must not retry-tap over the system permission dialog',
          );

          // Next step: native lane taps "Allow" -> OS resumes app and delivers
          // the MethodChannel reply.
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );
          rawMessenger.pendingReplies.single.complete(
            const StandardMethodCodec().encodeSuccessEnvelope(true),
          );
          await tester.pump();
          await tester.pump();

          expect(channelTracker.pending, 0);
          expect(find.text('Permission: granted'), findsOneWidget);
        } finally {
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );
          channelTracker.reset();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'pending HttpTracker request also wakes immediately when app pauses '
      'with zero pending timers',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final _PendingHttpClient inner = _PendingHttpClient();
        final HttpOverrides? previousOverrides = HttpOverrides.current;
        HttpOverrides.global = _PendingHttpOverrides(inner);
        final HttpTracker httpTracker = HttpTracker()..install();
        try {
          await tester.pumpWidget(
            const Directionality(
              textDirection: TextDirection.ltr,
              child: Center(child: Text('HTTP Pause')),
            ),
          );
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );

          unawaited(
            HttpClient()
                .getUrl(Uri.parse('http://api.test/slow'))
                .then<void>((_) {}, onError: (Object _) {}),
          );
          expect(httpTracker.inFlight, 1);

          final IdleDetector detector = createDetector(
            httpTracker: httpTracker,
          );
          String? settleState;
          final DateTime start = tester.binding.clock.now();
          Duration? finishedAt;
          unawaited(
            detector.waitForIdle(const Duration(seconds: 3)).then((String s) {
              settleState = s;
              finishedAt = tester.binding.clock.now().difference(start);
            }),
          );

          await tester.pump(const Duration(milliseconds: 50));
          expect(settleState, isNull);

          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.paused,
          );
          await tester.pump();

          expect(settleState, 'background');
          expect(finishedAt, const Duration(milliseconds: 50));
        } finally {
          for (final Completer<HttpClientRequest> req in inner.opened) {
            if (!req.isCompleted) {
              req.completeError(const SocketException('closed'));
            }
          }
          HttpOverrides.global = previousOverrides;
          tester.binding.handleAppLifecycleStateChanged(
            ui.AppLifecycleState.resumed,
          );
          handle.dispose();
        }
      },
    );
  });
}

class _PendingHttpClient implements HttpClient {
  final List<Completer<HttpClientRequest>> opened =
      <Completer<HttpClientRequest>>[];

  @override
  Future<HttpClientRequest> getUrl(Uri url) {
    final Completer<HttpClientRequest> request = Completer<HttpClientRequest>();
    opened.add(request);
    return request.future;
  }

  @override
  void close({bool force = false}) {}

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _PendingHttpOverrides extends HttpOverrides {
  _PendingHttpOverrides(this.client);

  final _PendingHttpClient client;

  @override
  HttpClient createHttpClient(SecurityContext? context) => client;
}
