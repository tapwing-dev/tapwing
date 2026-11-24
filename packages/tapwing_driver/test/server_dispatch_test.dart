import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/protocol/rpc_error.dart';
import 'package:tapwing_driver/src/protocol/rpc_messages.dart';
import 'package:tapwing_driver/src/protocol/rpc_transport.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/server/rpc_session.dart';

class _FakeRpcTransport implements RpcTransport {
  final StreamController<String> _incoming = StreamController<String>();
  final List<Map<String, Object?>> sent = <Map<String, Object?>>[];
  bool closed = false;

  @override
  Stream<String> get lines => _incoming.stream;

  @override
  void sendLine(String line) {
    if (closed) return;
    final Map<String, Object?> decoded =
        (jsonDecode(line) as Map<Object?, Object?>).cast<String, Object?>();
    sent.add(decoded);
  }

  void pushRequest(
    int id,
    String method, [
    Map<String, Object?> params = const <String, Object?>{},
  ]) {
    _incoming.add(
      jsonEncode(<String, Object?>{
        'id': id,
        'method': method,
        'params': params,
      }),
    );
  }

  void pushRaw(String line) {
    _incoming.add(line);
  }

  Future<void> closeIncoming() => _incoming.close();

  @override
  Future<void> close() async {
    closed = true;
    if (!_incoming.isClosed) {
      await _incoming.close();
    }
  }
}

void main() {
  group('RPC dispatch through fake transport', () {
    testWidgets(
      'handles handshake, ping, configure, busy, snapshot, assertions, tapOn, and errors',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          int taps = 0;
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: StatefulBuilder(
                  builder: (BuildContext context, StateSetter setState) =>
                      Column(
                        children: <Widget>[
                          Text('Count: $taps'),
                          ElevatedButton(
                            onPressed: () => setState(() => taps++),
                            child: const Text('Increment'),
                          ),
                        ],
                      ),
                ),
              ),
            ),
          );

          final TextInputController textInput = TextInputController();
          final IdleDetector idle = IdleDetector(
            httpTracker: HttpTracker(),
            textInput: textInput,
            readBottomInset: () => 0,
            waitForFirstFrame: (_) async {},
          );
          final DriverContext context = DriverContext(
            idle: idle,
            pointer: PointerInput(),
            textInput: textInput,
          );
          final RpcSession session = RpcSession(CommandDispatcher(context));
          final _FakeRpcTransport transport = _FakeRpcTransport();
          bool sessionDone = false;
          unawaited(session.serve(transport).then((_) => sessionDone = true));

          // 1. Handshake event sent immediately on connect.
          expect(transport.sent.length, 1);
          expect(transport.sent.first['event'], 'hello');
          final Map<String, Object?> helloParams =
              transport.sent.first['params']! as Map<String, Object?>;
          expect(helloParams['protocol'], protocolVersion);

          Future<Map<String, Object?>> pumpUntilResponse(
            int previousCount,
          ) async {
            for (
              int i = 0;
              i < 40 && transport.sent.length == previousCount;
              i++
            ) {
              await tester.pump(const Duration(milliseconds: 16));
            }
            expect(
              transport.sent.length,
              greaterThan(previousCount),
              reason: 'Timed out waiting for RPC response',
            );
            return transport.sent.last;
          }

          Future<Map<String, Object?>> call(
            int id,
            String method, [
            Map<String, Object?> params = const <String, Object?>{},
          ]) {
            final int before = transport.sent.length;
            transport.pushRequest(id, method, params);
            return pumpUntilResponse(before);
          }

          // 2. ping
          final Map<String, Object?> pingResp = await call(1, 'ping');
          expect(pingResp['id'], 1);
          expect(pingResp['result'], 'pong');
          expect(pingResp['us'], isA<int>());

          // 3. configure
          final Map<String, Object?> cfgResp = await call(
            2,
            'configure',
            <String, Object?>{'settlePolicy': 'legacy'},
          );
          expect(
            (cfgResp['result']! as Map<String, Object?>)['settlePolicy'],
            'legacy',
          );
          expect(context.settlePolicy, SettlePolicy.legacy);

          await call(3, 'configure', <String, Object?>{'settlePolicy': 'v2'});
          expect(context.settlePolicy, SettlePolicy.v2);

          // 4. busy
          final Map<String, Object?> busyResp = await call(4, 'busy');
          final Map<String, Object?> busy =
              busyResp['result']! as Map<String, Object?>;
          expect(busy['framesIdle'], isTrue);
          expect(busy['loadingIdle'], isTrue);
          expect(busy['animations'], isEmpty);

          // 5. snapshot
          final Map<String, Object?> snapResp = await call(5, 'snapshot');
          final Map<String, Object?> snapResult =
              snapResp['result']! as Map<String, Object?>;
          expect(snapResult['screen'], isA<List<Object?>>());
          expect(snapResult['root'], isA<Map<String, Object?>>());

          // 6. assertVisible & copyTextFrom
          final Map<String, Object?> visibleResp = await call(
            6,
            'assertVisible',
            <String, Object?>{
              'selector': <String, Object?>{'text': 'Count: 0'},
              'timeoutMs': 100,
            },
          );
          expect(
            (visibleResp['result']! as Map<String, Object?>)['element'],
            isA<Map<String, Object?>>(),
          );

          final Map<String, Object?> copyResp = await call(
            7,
            'copyTextFrom',
            <String, Object?>{
              'selector': <String, Object?>{'text': 'Count: 0'},
              'timeoutMs': 100,
            },
          );
          expect(
            (copyResp['result']! as Map<String, Object?>)['text'],
            'Count: 0',
          );

          // 7. assertNotVisible passes for absent element
          final Map<String, Object?> goneResp = await call(
            8,
            'assertNotVisible',
            <String, Object?>{
              'selector': <String, Object?>{'text': 'Does not exist'},
              'timeoutMs': 100,
            },
          );
          expect(goneResp['result'], isTrue);

          // 8. tapOn invokes the real button via PointerDataPacket
          final Map<String, Object?> tapResp = await call(
            9,
            'tapOn',
            <String, Object?>{
              'selector': <String, Object?>{'text': 'Increment'},
              'timeoutMs': 100,
              'settleTimeoutMs': 200,
            },
          );
          expect(taps, 1);
          final Map<String, Object?> tapResult =
              tapResp['result']! as Map<String, Object?>;
          expect(tapResult['element'], isA<Map<String, Object?>>());
          expect(tapResult['settle'], anyOf('idle', 'stable'));
          final Map<String, Object?> timings =
              tapResult['t']! as Map<String, Object?>;
          expect(
            timings.keys,
            containsAll(<String>['findUs', 'actUs', 'settleUs']),
          );
          final Map<String, Object?> ime =
              tapResult['ime']! as Map<String, Object?>;
          expect(ime.containsKey('usedClient'), isFalse);
          expect(
            (ime['log']! as List<Object?>).any(
              (Object? l) => '$l'.contains('tap '),
            ),
            isTrue,
          );

          // 9. inputText and eraseText without focus succeed with warning in
          // default Maestro-parity mode, and return NO_FOCUS under strictInput.
          final Map<String, Object?> noFocusResp = await call(
            10,
            'inputText',
            <String, Object?>{'text': 'abc'},
          );
          final Map<String, Object?> noFocusResult =
              noFocusResp['result']! as Map<String, Object?>;
          expect(noFocusResult['warning'], isA<String>());
          expect(
            (noFocusResult['ime']! as Map<String, Object?>)['noClient'],
            isTrue,
          );

          final Map<String, Object?> eraseNoFocus = await call(
            100,
            'eraseText',
            <String, Object?>{'count': 3},
          );
          expect(
            (eraseNoFocus['result']! as Map<String, Object?>)['warning'],
            isA<String>(),
          );

          // Opt-in strictInput: true restores NO_FOCUS errors.
          await call(101, 'configure', <String, Object?>{'strictInput': true});
          final Map<String, Object?> strictNoFocus = await call(
            102,
            'inputText',
            <String, Object?>{'text': 'abc'},
          );
          expect(
            (strictNoFocus['error']! as Map<String, Object?>)['code'],
            'NO_FOCUS',
          );
          final Map<String, Object?> strictErase = await call(
            103,
            'eraseText',
            <String, Object?>{'count': 3},
          );
          expect(
            (strictErase['error']! as Map<String, Object?>)['code'],
            'NO_FOCUS',
          );
          await call(104, 'configure', <String, Object?>{'strictInput': false});

          // 10. inputText with an active client succeeds and reports usedClient
          const JSONMethodCodec codec = JSONMethodCodec();
          textInput.onOutgoing(
            codec.encodeMethodCall(
              const MethodCall('TextInput.setClient', <Object?>[
                9,
                <String, Object?>{},
              ]),
            ),
          );
          final Map<String, Object?> inputResp = await call(
            11,
            'inputText',
            <String, Object?>{'text': 'hi', 'settleTimeoutMs': 200},
          );
          final Map<String, Object?> inputResult =
              inputResp['result']! as Map<String, Object?>;
          final Map<String, Object?> inputIme =
              inputResult['ime']! as Map<String, Object?>;
          expect(inputIme['usedClient'], 9);
          expect(inputIme['clientId'], 9);
          expect(textInput.editingState?['text'], 'hi');

          // 11. Unknown method returns UNKNOWN_METHOD
          final Map<String, Object?> unknownResp = await call(
            12,
            'nonExistentMethod',
          );
          expect(
            (unknownResp['error']! as Map<String, Object?>)['code'],
            'UNKNOWN_METHOD',
          );

          // 12. Malformed JSON returns INTERNAL error without poisoning the session
          final int beforeBad = transport.sent.length;
          transport.pushRaw('{not json');
          final Map<String, Object?> badResp = await pumpUntilResponse(
            beforeBad,
          );
          expect(
            (badResp['error']! as Map<String, Object?>)['code'],
            'INTERNAL',
          );

          // Subsequent request still succeeds after malformed input!
          final Map<String, Object?> pingAfter = await call(13, 'ping');
          expect(pingAfter['result'], 'pong');

          await transport.closeIncoming();
          await tester.pump();
          expect(sessionDone, isTrue);
          expect(transport.closed, isTrue);
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'returns ELEMENT_NOT_FOUND with visibleTexts and ASSERTION when still visible',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(
            const MaterialApp(
              home: Scaffold(body: Center(child: Text('Visible Label'))),
            ),
          );

          final DriverContext context = DriverContext(
            idle: IdleDetector(
              httpTracker: HttpTracker(),
              textInput: TextInputController(),
              readBottomInset: () => 0,
              waitForFirstFrame: (_) async {},
            ),
          );
          final CommandDispatcher dispatcher = CommandDispatcher(context);

          // Timeout = 0 ms -> fails immediately on the first check without waiting.
          await expectLater(
            dispatcher.dispatch('assertVisible', <String, Object?>{
              'selector': <String, Object?>{'text': 'Missing Widget'},
              'timeoutMs': 0,
            }),
            throwsA(
              isA<DriverError>()
                  .having(
                    (DriverError e) => e.code,
                    'code',
                    'ELEMENT_NOT_FOUND',
                  )
                  .having(
                    (DriverError e) => e.details?['visibleTexts'],
                    'visibleTexts',
                    contains('Visible Label'),
                  ),
            ),
          );

          await expectLater(
            dispatcher.dispatch('assertNotVisible', <String, Object?>{
              'selector': <String, Object?>{'text': 'Visible Label'},
              'timeoutMs': 0,
            }),
            throwsA(
              isA<DriverError>().having(
                (DriverError e) => e.code,
                'code',
                'ASSERTION',
              ),
            ),
          );
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'RpcSession emits progress heartbeats and cancels stale handlers on disconnect',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(
            const MaterialApp(
              home: Scaffold(body: Center(child: Text('Ready'))),
            ),
          );
          final DriverContext context = DriverContext(
            idle: IdleDetector(
              httpTracker: HttpTracker(),
              textInput: TextInputController(),
              readBottomInset: () => 0,
              waitForFirstFrame: (_) async {},
            ),
          );
          final RpcSession session = RpcSession(
            CommandDispatcher(context),
            progressInterval: const Duration(milliseconds: 50),
          );

          // 1. Long waitFor emits progress events before finishing.
          final _FakeRpcTransport t1 = _FakeRpcTransport();
          bool t1Done = false;
          unawaited(session.serve(t1).then((_) => t1Done = true));
          t1.pushRequest(1, 'assertVisible', <String, Object?>{
            'selector': <String, Object?>{'text': 'Never Appears'},
            'timeoutMs': 180,
          });
          for (int i = 0; i < 16; i++) {
            await tester.pump(const Duration(milliseconds: 16));
          }
          expect(
            t1.sent.any(
              (Map<String, Object?> m) =>
                  m['event'] == 'progress' && m['id'] == 1,
            ),
            isTrue,
          );

          // 2. Start a long 10 s wait on t1, then drop t1 and immediately
          // connect t2 -> t1 unwinds promptly and t2 answers ping right away!
          t1.pushRequest(2, 'assertVisible', <String, Object?>{
            'selector': <String, Object?>{'text': 'Never Appears'},
            'timeoutMs': 10000,
          });
          await tester.pump(const Duration(milliseconds: 32));
          await t1.closeIncoming();

          final _FakeRpcTransport t2 = _FakeRpcTransport();
          unawaited(session.serve(t2));
          t2.pushRequest(1, 'ping');
          for (int i = 0; i < 20; i++) {
            await tester.pump(const Duration(milliseconds: 16));
          }
          expect(t1Done, isTrue);
          expect(
            t2.sent.any(
              (Map<String, Object?> m) => m['id'] == 1 && m['result'] == 'pong',
            ),
            isTrue,
          );
          await t2.closeIncoming();
          await tester.pump();
        } finally {
          handle.dispose();
        }
      },
    );
  });
}
