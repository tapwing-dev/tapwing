import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/back_signals.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';

/// Host-injected Android Back (`armBack` / `awaitBack`): the CLI sends a real
/// `KEYCODE_BACK` so predictive back (`enableOnBackInvokedCallback`) behaves
/// exactly like under Maestro, and the driver only observes the outcome.
void main() {
  ByteData popRoute() =>
      const JSONMethodCodec().encodeMethodCall(const MethodCall('popRoute'));
  ByteData pushRoute() => const JSONMethodCodec().encodeMethodCall(
    const MethodCall('pushRoute', '/x'),
  );
  ByteData commitBackGesture() => const StandardMethodCodec().encodeMethodCall(
    const MethodCall('commitBackGesture'),
  );

  group('BackSignals', () {
    test('counts popRoute and commitBackGesture only', () {
      final BackSignals s = BackSignals();
      s.onIncoming(SystemChannels.navigation.name, popRoute());
      s.onIncoming(SystemChannels.navigation.name, pushRoute());
      s.onIncoming(SystemChannels.backGesture.name, commitBackGesture());
      s.onIncoming(SystemChannels.navigation.name, ByteData(3));
      s.onIncoming(SystemChannels.navigation.name, null);
      s.onIncoming(SystemChannels.lifecycle.name, popRoute());
      expect(s.deliveries, 2);
    });

    test('ObservingBinaryMessenger counts before the framework handler runs '
        'and still delivers the message', () async {
      final _RecordingMessenger inner = _RecordingMessenger();
      final BackSignals signals = BackSignals();
      final ObservingBinaryMessenger messenger = ObservingBinaryMessenger(
        inner,
        controller: TextInputController(),
        backSignals: signals,
      );
      int seenAtHandler = -1;
      messenger.setMessageHandler(SystemChannels.navigation.name, (
        ByteData? data,
      ) async {
        seenAtHandler = signals.deliveries;
        return null;
      });
      await inner.handlers[SystemChannels.navigation.name]!(popRoute());
      expect(seenAtHandler, 1);
      expect(signals.deliveries, 1);

      messenger.setMessageHandler(SystemChannels.navigation.name, null);
      expect(inner.handlers.containsKey(SystemChannels.navigation.name), false);
    });
  });

  group('armBack / awaitBack', () {
    late BackSignals signals;
    late double bottomInset;
    late AppLifecycleState? lifecycle;
    late int inAppBacks;

    setUp(() {
      signals = BackSignals();
      bottomInset = 0;
      lifecycle = AppLifecycleState.resumed;
      inAppBacks = 0;
    });

    CommandDispatcher makeDispatcher() {
      final TextInputController ti = TextInputController();
      return CommandDispatcher(
        DriverContext(
          idle: IdleDetector(
            httpTracker: HttpTracker(),
            textInput: ti,
            readBottomInset: () => bottomInset,
            waitForFirstFrame: (_) async {},
          ),
          pointer: _SpyBackPointerInput(() => inAppBacks++),
          textInput: ti,
          readBottomInset: () => bottomInset,
          backSignals: signals,
          readLifecycleState: () => lifecycle,
        ),
      );
    }

    Future<Map<String, Object?>> call(
      WidgetTester tester,
      CommandDispatcher d,
      String method, [
      Map<String, Object?> params = const <String, Object?>{},
    ]) async {
      Map<String, Object?>? result;
      Object? error;
      unawaited(
        d
            .dispatch(method, params)
            .then(
              (Object? v) => result = v! as Map<String, Object?>,
              onError: (Object e) => error = e,
            ),
      );
      for (int i = 0; i < 200 && result == null && error == null; i++) {
        await tester.pump(const Duration(milliseconds: 16));
      }
      if (error != null) throw error!;
      expect(result, isNotNull, reason: '$method did not complete');
      return result!;
    }

    testWidgets('navigation: Back reached the framework', (
      WidgetTester tester,
    ) async {
      await tester.pumpWidget(const SizedBox());
      final CommandDispatcher d = makeDispatcher();
      expect((await call(tester, d, 'armBack'))['armed'], true);
      signals.onIncoming(SystemChannels.navigation.name, popRoute());
      final Map<String, Object?> r = await call(tester, d, 'awaitBack', {
        'settleTimeoutMs': 0,
      });
      expect(r['outcome'], 'navigation');
      expect(inAppBacks, 0, reason: 'the driver never pops in-app');
    });

    testWidgets('background: the OS finished or backgrounded the activity '
        '(predictive back the framework did not claim)', (
      WidgetTester tester,
    ) async {
      await tester.pumpWidget(const SizedBox());
      final CommandDispatcher d = makeDispatcher();
      await call(tester, d, 'armBack');
      lifecycle = AppLifecycleState.inactive;
      final Map<String, Object?> r = await call(tester, d, 'awaitBack');
      expect(r['outcome'], 'background');
      expect(r['settle'], 'idle');
    });

    testWidgets('keyboard: the IME consumed Back', (WidgetTester tester) async {
      await tester.pumpWidget(const SizedBox());
      bottomInset = 600;
      final CommandDispatcher d = makeDispatcher();
      await call(tester, d, 'armBack');
      bottomInset = 300;
      final Map<String, Object?> r = await call(tester, d, 'awaitBack', {
        'settleTimeoutMs': 0,
      });
      expect(r['outcome'], 'keyboard');
    });

    testWidgets('none: nothing observable within timeoutMs', (
      WidgetTester tester,
    ) async {
      await tester.pumpWidget(const SizedBox());
      final CommandDispatcher d = makeDispatcher();
      await call(tester, d, 'armBack');
      final Map<String, Object?> r = await call(tester, d, 'awaitBack', {
        'timeoutMs': 50,
        'settleTimeoutMs': 0,
      });
      expect(r['outcome'], 'none');
    });

    testWidgets('hideKeyboard hostBack without keyboard leaves Back to the '
        'host and keeps the Maestro quirk warning', (
      WidgetTester tester,
    ) async {
      await tester.pumpWidget(const SizedBox());
      final CommandDispatcher d = makeDispatcher();
      final Map<String, Object?> r = await call(tester, d, 'hideKeyboard', {
        'hostBack': true,
        'settleTimeoutMs': 0,
      });
      expect(r['hostBack'], true);
      expect(r['warning'], isA<String>());
      expect(inAppBacks, 0);
    });
  });
}

class _SpyBackPointerInput extends PointerInput {
  _SpyBackPointerInput(this._onBack);

  final VoidCallback _onBack;

  @override
  void back() => _onBack();
}

class _RecordingMessenger implements BinaryMessenger {
  final Map<String, MessageHandler> handlers = <String, MessageHandler>{};

  @override
  void setMessageHandler(String channel, MessageHandler? handler) {
    if (handler == null) {
      handlers.remove(channel);
    } else {
      handlers[channel] = handler;
    }
  }

  @override
  Future<ByteData?>? send(String channel, ByteData? message) => null;

  @override
  // ignore: deprecated_member_use
  Future<void> handlePlatformMessage(
    String channel,
    ByteData? data,
    ui.PlatformMessageResponseCallback? callback,
  ) async {}
}
