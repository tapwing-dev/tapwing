import 'dart:ui' as ui;

import 'package:flutter/animation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/diagnostics/text_input_log.dart';
import 'package:tapwing_driver/src/idle/animation_scanner.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/protocol/rpc_error.dart';
import 'package:tapwing_driver/src/protocol/rpc_messages.dart';
import 'package:tapwing_driver/src/selector/selector.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

void main() {
  group('Typed RPC parameter parsing', () {
    test('TapOnParams handles omitted and null optional fields like Rust Option::None', () {
      final TapOnParams params = TapOnParams.fromJson(<String, Object?>{
        'selector': <String, Object?>{'text': 'Login'},
        'timeoutMs': 17000,
        'longPress': false,
        'repeat': null,
        'delayMs': null,
        'settleTimeoutMs': null,
      });

      expect(params.selector.text, 'Login');
      expect(params.timeout, const Duration(milliseconds: 17000));
      expect(params.longPress, isFalse);
      expect(params.repeat, 1);
      expect(params.delay, const Duration(milliseconds: 100));
      expect(params.settleTimeout, const Duration(milliseconds: 3000));
    });

    test('defaults match Maestro and Tapwing F0 wire contract', () {
      expect(
        WaitIdleParams.fromJson(const <String, Object?>{}).timeout,
        const Duration(milliseconds: 3000),
      );
      expect(EraseTextParams.fromJson(const <String, Object?>{}).count, 50);
      final ScrollUntilVisibleParams scroll = ScrollUntilVisibleParams.fromJson(
        <String, Object?>{
          'selector': <String, Object?>{'text': 'Item 40'},
        },
      );
      expect(scroll.direction, 'DOWN');
      expect(scroll.timeout, const Duration(milliseconds: 20000));
      expect(scroll.visibilityPercentage, 100);

      final SwipeParams swipe = SwipeParams.fromJson(<String, Object?>{
        'start': '50%, 70%',
        'end': '50%, 30%',
      });
      expect(swipe.duration, const Duration(milliseconds: 400));
      expect(swipe.settleTimeout, const Duration(milliseconds: 3000));

      final ConfigureParams cfg = ConfigureParams.fromJson(<String, Object?>{
        'settlePolicy': 'legacy',
      });
      expect(cfg.settlePolicy, SettlePolicy.legacy);
    });
  });

  group('Typed RPC result and error wire serialization', () {
    final KNode sampleNode = KNode(
      id: 7,
      parent: null,
      bounds: const ui.Rect.fromLTWH(10, 20, 100, 40),
      text: null,
      hintText: null,
      accessibilityText: 'Submit',
      resourceId: 'submit_btn',
      clickable: true,
      enabled: true,
      focused: false,
      checked: false,
      selected: false,
      isTextField: false,
      scrollable: false,
    );

    test('TapOnResult and InputTextResult preserve exact wire keys', () {
      final Map<String, Object?> tapJson = TapOnResult(
        element: sampleNode,
        settle: 'idle',
        timings: const TapOnTimings(findUs: 100, actUs: 200, settleUs: 300),
        ime: const ImeDiagnostics(
          clientId: 3,
          keyboard: true,
          log: <String>['10 tap 60,40'],
        ),
      ).toJson();

      expect(tapJson['settle'], 'idle');
      expect(tapJson['t'], <String, int>{
        'findUs': 100,
        'actUs': 200,
        'settleUs': 300,
      });
      final Map<String, Object?> tapIme =
          tapJson['ime']! as Map<String, Object?>;
      expect(tapIme.containsKey('usedClient'), isFalse);
      expect(tapIme['clientId'], 3);
      expect(tapIme['keyboard'], isTrue);
      expect(tapIme['log'], <String>['10 tap 60,40']);

      final Map<String, Object?> inputJson = InputTextResult(
        settle: 'stable',
        ime: const ImeDiagnostics(
          usedClient: 3,
          clientId: 3,
          keyboard: true,
          log: <String>['12 out TextInput.setEditingState text="a"'],
        ),
      ).toJson();
      final Map<String, Object?> inputIme =
          inputJson['ime']! as Map<String, Object?>;
      expect(inputIme['usedClient'], 3);
      expect(inputJson['settle'], 'stable');
    });

    test(
      'BusyReport, RpcHelloEvent, and DriverError serialize expected wire keys',
      () {
        final Map<String, Object?> busy = const BusyReport(
          framesIdle: false,
          loadingIdle: true,
          transientCallbacks: 1,
          httpInFlight: 0,
          pendingChannels: 2,
          pendingImages: 0,
          animations: <SignificantAnimation>[
            SignificantAnimation(
              widget: 'FadeTransition',
              fraction: 0.9996,
              status: AnimationStatus.forward,
            ),
          ],
        ).toJson();
        expect(busy['framesIdle'], isFalse);
        expect(busy['transientCallbacks'], 1);
        expect(busy['pendingChannels'], 2);
        expect(busy['animations'], <Map<String, Object?>>[
          <String, Object?>{
            'widget': 'FadeTransition',
            'fraction': 1.0,
            'status': 'forward',
          },
        ]);

        final Map<String, Object?> hello = const RpcHelloEvent(
          protocol: 1,
          mode: 'profile',
          pid: 42,
          dartVersion: '3.13.4',
          screenWidth: 1080,
          screenHeight: 2400,
          dpr: 2.625,
        ).toJson();
        expect(hello['event'], 'hello');
        final Map<String, Object?> helloParams =
            hello['params']! as Map<String, Object?>;
        expect(helloParams['protocol'], 1);
        expect(helloParams['screen'], <double>[1080, 2400]);
        expect(helloParams['dpr'], 2.625);

        final Map<String, Object?> notFound = DriverError.elementNotFound(
          Selector(text: 'Missing'),
          <String>['Welcome', 'Login'],
        ).toJson();
        expect(notFound['code'], 'ELEMENT_NOT_FOUND');
        expect(
          notFound['message'],
          'Element not found: Text matching regex: Missing',
        );
        expect(notFound['visibleTexts'], <String>['Welcome', 'Login']);
        expect(notFound.containsKey('platformViewCoverage'), isFalse);

        expect(
          DriverError.elementNotFound(
            Selector(text: 'Missing'),
            const <String>[],
            platformViewCoverage: 0.0,
          ).toJson().containsKey('platformViewCoverage'),
          isFalse,
        );
        expect(
          DriverError.elementNotFound(
            Selector(text: 'Missing'),
            const <String>[],
            platformViewCoverage: -0.2,
          ).toJson().containsKey('platformViewCoverage'),
          isFalse,
        );
        expect(
          DriverError.elementNotFound(
            Selector(text: 'Missing'),
            const <String>[],
            platformViewCoverage: double.nan,
          ).toJson().containsKey('platformViewCoverage'),
          isFalse,
        );
        final Map<String, Object?> notFoundWithPv = DriverError.elementNotFound(
          Selector(text: 'Sign In'),
          <String>['Header'],
          platformViewCoverage: 0.85,
        ).toJson();
        expect(notFoundWithPv['platformViewCoverage'], 0.85);
        expect(
          DriverError.elementNotFound(
            Selector(text: 'Sign In'),
            const <String>[],
            platformViewCoverage: 1.5,
          ).toJson()['platformViewCoverage'],
          1.0,
        );

        expect(
          DriverError.stillVisible(Selector(text: 'Dialog')).code,
          'ASSERTION',
        );
        expect(DriverError.noFocus().code, 'NO_FOCUS');
        expect(DriverError.unknownMethod('foo').code, 'UNKNOWN_METHOD');
      },
    );
  });

  group('TextInputLog diagnostics', () {
    test('ring buffer caps at 300 entries and filters by sequence mark', () {
      final TextInputLog log = TextInputLog();
      for (int i = 0; i < 310; i++) {
        log.record('entry-$i');
      }
      final List<String> all = log.since(0);
      expect(all.length, TextInputLog.maxEntries);
      expect(all.first, endsWith('entry-10'));
      expect(all.last, endsWith('entry-309'));

      final int mark = log.mark;
      log.record('after-mark');
      final List<String> recent = log.since(mark);
      expect(recent.length, 1);
      expect(recent.single, endsWith('after-mark'));
    });

    test(
      'describeCall formats textinput method calls and handles bad payloads',
      () {
        const JSONMethodCodec codec = JSONMethodCodec();
        final ByteData setClient = codec.encodeMethodCall(
          const MethodCall('TextInput.setClient', <Object?>[
            5,
            <String, Object?>{},
          ]),
        );
        expect(
          TextInputLog.describeCall(setClient),
          'TextInput.setClient client=5 text=""',
        );

        final ByteData update = codec.encodeMethodCall(
          const MethodCall('TextInputClient.updateEditingState', <Object?>[
            5,
            <String, Object?>{'text': 'hello'},
          ]),
        );
        expect(
          TextInputLog.describeCall(update),
          'TextInputClient.updateEditingState client=5 text="hello"',
        );

        final ByteData setState = codec.encodeMethodCall(
          const MethodCall('TextInput.setEditingState', <String, Object?>{
            'text': 'world',
          }),
        );
        expect(
          TextInputLog.describeCall(setState),
          'TextInput.setEditingState text="world"',
        );

        final ByteData garbage = ByteData.sublistView(
          Uint8List.fromList(<int>[0xFF, 0xFE]),
        );
        expect(TextInputLog.describeCall(garbage), '<undecodable>');
      },
    );
  });

  group('DriverContext point parsing', () {
    test(
      'parses percentage and physical pixel coordinates and validates ranges',
      () {
        final DriverContext ctx = DriverContext(
          readScreenSize: () => const ui.Size(1080, 2400),
        );
        expect(ctx.parsePoint('50%, 25%'), const ui.Offset(540, 600));
        expect(ctx.parsePoint('120, 340'), const ui.Offset(120, 340));
        expect(
          () => ctx.parsePoint('120%, 50%'),
          throwsA(
            isA<DriverError>().having(
              (DriverError e) => e.code,
              'code',
              DriverErrorCode.invalidCommand,
            ),
          ),
        );
        expect(
          () => ctx.parsePoint('-5, 10'),
          throwsA(
            isA<DriverError>().having(
              (DriverError e) => e.code,
              'code',
              DriverErrorCode.invalidCommand,
            ),
          ),
        );
      },
    );

    // Ported from Maestro ElementCoordinateUtilTest.kt
    test('resolveElementPoint computes percentage and pixel offsets inside element bounds', () {
      final DriverContext ctx = DriverContext(
        readScreenSize: () => const ui.Size(1080, 2400),
      );
      final KNode elem = KNode(
        id: 1,
        parent: null,
        bounds: const ui.Rect.fromLTWH(100, 100, 100, 100),
        text: 'Test Button',
        hintText: null,
        accessibilityText: null,
        resourceId: 'test-button',
        clickable: true,
        enabled: true,
        focused: false,
        checked: false,
        selected: false,
        isTextField: false,
        scrollable: false,
      );

      expect(
        ctx.resolveElementPoint(elem, '50%, 90%'),
        const ui.Offset(150, 190),
      );
      expect(
        ctx.resolveElementPoint(elem, '25, 75'),
        const ui.Offset(125, 175),
      );
      expect(
        ctx.resolveElementPoint(elem, '0%, 0%'),
        const ui.Offset(100, 100),
      );
      expect(
        ctx.resolveElementPoint(elem, '100%, 100%'),
        const ui.Offset(200, 200),
      );
      expect(
        ctx.resolveElementPoint(elem, '25%, 75%'),
        const ui.Offset(125, 175),
      );

      expect(
        () => ctx.resolveElementPoint(elem, '101%, 50%'),
        throwsA(isA<DriverError>()),
      );
      expect(
        () => ctx.resolveElementPoint(elem, '100, 50'),
        throwsA(isA<DriverError>()),
      );
    });
  });

  group('F2 RPC message params and results', () {
    test('parses SwipeParams element mode, ScrollUntilVisibleParams, PressKeyParams, DarkModeResult', () {
      final SwipeParams elemSwipe = SwipeParams.fromJson(<String, Object?>{
        'selector': <String, Object?>{'id': 'card_1'},
        'direction': 'left',
        'relativePoint': '80%, 50%',
        'timeoutMs': 7000,
        'durationMs': 350,
        'settleTimeoutMs': 500,
      });
      expect(elemSwipe.selector?.id, 'card_1');
      expect(elemSwipe.direction, 'LEFT');
      expect(elemSwipe.relativePoint, '80%, 50%');
      expect(elemSwipe.timeout, const Duration(milliseconds: 7000));
      expect(elemSwipe.duration, const Duration(milliseconds: 350));
      expect(elemSwipe.settleTimeout, const Duration(milliseconds: 500));

      final ScrollUntilVisibleParams scroll = ScrollUntilVisibleParams.fromJson(
        <String, Object?>{
          'selector': <String, Object?>{'text': 'End'},
          'direction': 'right',
          'durationMs': 201,
          'centerElement': true,
          'visibilityPercentage': 50,
          'settleTimeoutMs': 400,
        },
      );
      expect(scroll.direction, 'RIGHT');
      expect(scroll.duration, const Duration(milliseconds: 201));
      expect(scroll.centerElement, isTrue);
      expect(scroll.visibilityPercentage, 50);
      expect(scroll.settleTimeout, const Duration(milliseconds: 400));

      final PressKeyParams key = PressKeyParams.fromJson(<String, Object?>{
        'key': 'enter',
      });
      expect(key.key, 'ENTER');

      expect(const DarkModeResult(darkMode: true).toJson(), <String, Object?>{
        'darkMode': true,
        'brightness': 'dark',
      });
      expect(const DarkModeResult(darkMode: false).toJson(), <String, Object?>{
        'darkMode': false,
        'brightness': 'light',
      });
    });
  });
}
