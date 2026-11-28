import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/hooks/hook_registry.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/protocol/rpc_error.dart';
import 'package:tapwing_driver/src/protocol/rpc_transport.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/server/rpc_session.dart';
import 'package:tapwing_driver/tapwing_driver.dart';

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

  @override
  Future<void> close() async {
    closed = true;
    if (!_incoming.isClosed) {
      await _incoming.close();
    }
  }
}

class _NonJsonObject {
  const _NonJsonObject();
}

class _FakeScope extends InheritedWidget {
  const _FakeScope({required this.label, required super.child});

  final String label;

  static String of(BuildContext context) {
    final _FakeScope? scope = context
        .dependOnInheritedWidgetOfExactType<_FakeScope>();
    if (scope == null) {
      throw StateError('No _FakeScope found in context');
    }
    return scope.label;
  }

  @override
  bool updateShouldNotify(_FakeScope oldWidget) => label != oldWidget.label;
}

class _MarkerWidget extends StatelessWidget {
  const _MarkerWidget({required this.label});

  final String label;

  @override
  Widget build(BuildContext context) => Text(label);
}

void main() {
  group('HookRegistry and TapwingBinding hook API', () {
    tearDown(TapwingBinding.clearHooks);

    testWidgets(
      'register, registerAll, overwrite, lookup, names, unregister, and clear',
      (WidgetTester tester) async {
        await tester.pumpWidget(const SizedBox());
        final BuildContext ctx = tester.element(find.byType(SizedBox));
        final HookRegistry registry = HookRegistry();
        expect(registry.names, isEmpty);
        expect(() => registry.register('', (_, _) => null), throwsArgumentError);
        expect(
          () => registry.register('   ', (_, _) => null),
          throwsArgumentError,
        );

        registry.register('zebra', (_, _) => 1);
        registry.register('alpha', (_, Map<String, Object?> args) => args['x']);
        expect(registry.names, <String>['alpha', 'zebra']);
        expect(registry.has('alpha'), isTrue);
        expect(
          registry.lookup('alpha')!(ctx, <String, Object?>{'x': 42}),
          equals(42),
        );

        // Overwrite existing hook cleanly and registerAll merges entries.
        registry.registerAll(<String, TapwingHookHandler>{
          'alpha': (_, _) => 'updated',
          'beta': (_, _) => 'second',
        });
        expect(registry.names, <String>['alpha', 'beta', 'zebra']);
        expect(
          registry.lookup('alpha')!(ctx, const <String, Object?>{}),
          equals('updated'),
        );

        expect(registry.unregister('alpha'), isTrue);
        expect(registry.unregister('alpha'), isFalse);
        expect(registry.names, <String>['beta', 'zebra']);

        registry.clear();
        expect(registry.names, isEmpty);
      },
    );

    test('TapwingBinding static hook helpers manage shared registry', () {
      TapwingBinding.clearHooks();
      TapwingBinding.registerHooks(<String, TapwingHookHandler>{
        'seed': (_, _) => <String, Object?>{'ok': true},
        'reset': (_, _) => null,
      });
      expect(HookRegistry.instance.names, <String>['reset', 'seed']);
      expect(TapwingBinding.unregisterHook('seed'), isTrue);
      expect(HookRegistry.instance.has('seed'), isFalse);
    });
  });

  group('active-route BuildContext resolution and callHook RPC dispatch', () {
    testWidgets(
      'passes live active-route BuildContext so Navigator and InheritedWidgets work with zero GlobalKeys',
      (WidgetTester tester) async {
        final SemanticsHandle semanticsHandle = tester.ensureSemantics();
        final HookRegistry hooks = HookRegistry();
        try {
          await tester.pumpWidget(
            _FakeScope(
              label: 'root-scope',
              child: MaterialApp(
                theme: ThemeData(useMaterial3: true),
                home: const Scaffold(
                  body: _MarkerWidget(label: 'Login Screen'),
                ),
              ),
            ),
          );

          hooks.register('read_scope_and_seed', (
            BuildContext context,
            Map<String, Object?> args,
          ) async {
            final String scope = _FakeScope.of(context);
            final Color primary = Theme.of(context).colorScheme.primary;
            final String user = (args['email'] as String?) ?? 'anon@example.com';
            await Future<void>.delayed(const Duration(milliseconds: 10));
            unawaited(
              Navigator.of(context).push(
                PageRouteBuilder<void>(
                  pageBuilder: (_, _, _) => Scaffold(
                    body: _MarkerWidget(label: 'Products for $user'),
                  ),
                  transitionDuration: Duration.zero,
                  reverseTransitionDuration: Duration.zero,
                ),
              ),
            );
            return <String, Object?>{
              'scope': scope,
              'hasPrimary': primary.toARGB32() != 0,
              'email': user,
            };
          });

          hooks.register('read_current_marker', (
            BuildContext context,
            Map<String, Object?> _,
          ) {
            final BuildContext? markerCtx =
                TapwingBinding.contextOf<_MarkerWidget>();
            final _MarkerWidget? marker =
                markerCtx?.widget as _MarkerWidget?;
            return <String, Object?>{
              'activeSameTree': TapwingBinding.activeContext != null,
              'topMarker': marker?.label,
              'canPop': Navigator.of(context).canPop(),
            };
          });

          hooks.register('sync_math', (_, Map<String, Object?> args) {
            final int a = (args['a'] as num?)?.toInt() ?? 0;
            final int b = (args['b'] as num?)?.toInt() ?? 0;
            return <String, Object?>{'sum': a + b};
          });

          hooks.register('throws_sync', (_, _) => throw StateError('boom sync'));

          hooks.register('throws_async', (_, _) async {
            await Future<void>.delayed(const Duration(milliseconds: 5));
            throw ArgumentError('boom async');
          });

          hooks.register('hangs', (_, _) => Completer<Object?>().future);

          hooks.register('bad_json', (_, _) => const _NonJsonObject());

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
            hooks: hooks,
          );
          final RpcSession session = RpcSession(CommandDispatcher(context));
          final _FakeRpcTransport transport = _FakeRpcTransport();
          unawaited(session.serve(transport));

          Future<Map<String, Object?>> sendAndAwait(
            int id,
            String method,
            Map<String, Object?> params,
          ) async {
            final int before = transport.sent.length;
            transport.pushRequest(id, method, params);
            for (int i = 0; i < 60; i++) {
              await tester.pump(const Duration(milliseconds: 20));
              for (int j = before; j < transport.sent.length; j++) {
                final Map<String, Object?> msg = transport.sent[j];
                if (msg['id'] == id && msg['event'] == null) {
                  return msg;
                }
              }
            }
            fail('Timed out waiting for response to $method#$id');
          }

          // 1. Async hook reads InheritedWidget + Theme and pushes via Navigator.of(context).
          final Map<String, Object?> r1 = await sendAndAwait(1, 'callHook', <
            String,
            Object?
          >{
            'name': 'read_scope_and_seed',
            'args': <String, Object?>{'email': 'jan@example.com'},
          });
          expect(r1['error'], isNull);
          final Map<String, Object?> res1 =
              r1['result']! as Map<String, Object?>;
          expect(res1['value'], <String, Object?>{
            'scope': 'root-scope',
            'hasPrimary': true,
            'email': 'jan@example.com',
          });
          expect(res1['settle'], anyOf('idle', 'stable'));

          // 2. Covered route's Offstage _MarkerWidget is skipped; active route's widget wins.
          final Map<String, Object?> rTop = await sendAndAwait(
            2,
            'callHook',
            <String, Object?>{'name': 'read_current_marker'},
          );
          expect(
            (rTop['result']! as Map<String, Object?>)['value'],
            <String, Object?>{
              'activeSameTree': true,
              'topMarker': 'Products for jan@example.com',
              'canPop': true,
            },
          );

          // 3. Sync hook with settle: false.
          final Map<String, Object?> r2 = await sendAndAwait(3, 'callHook', <
            String,
            Object?
          >{
            'name': 'sync_math',
            'args': <String, Object?>{'a': 7, 'b': 35},
            'settle': false,
          });
          final Map<String, Object?> res2 =
              r2['result']! as Map<String, Object?>;
          expect(res2['value'], <String, Object?>{'sum': 42});
          expect(res2['settle'], 'skipped');
          expect((res2['t']! as Map<String, Object?>)['settleUs'], 0);

          // 4. Missing hook -> HOOK_NOT_FOUND with availableHooks.
          final Map<String, Object?> rMissing = await sendAndAwait(
            4,
            'callHook',
            <String, Object?>{'name': 'does_not_exist'},
          );
          final Map<String, Object?> errMissing =
              rMissing['error']! as Map<String, Object?>;
          expect(errMissing['code'], DriverErrorCode.hookNotFound);
          expect(errMissing['availableHooks'], contains('read_scope_and_seed'));

          // 5. Sync and async exceptions -> HOOK_FAILED.
          final Map<String, Object?> rSyncErr = await sendAndAwait(
            5,
            'callHook',
            <String, Object?>{'name': 'throws_sync'},
          );
          expect(
            (rSyncErr['error']! as Map<String, Object?>)['code'],
            DriverErrorCode.hookFailed,
          );
          expect(
            (rSyncErr['error']! as Map<String, Object?>)['message'],
            contains('boom sync'),
          );

          final Map<String, Object?> rAsyncErr = await sendAndAwait(
            6,
            'callHook',
            <String, Object?>{'name': 'throws_async'},
          );
          expect(
            (rAsyncErr['error']! as Map<String, Object?>)['code'],
            DriverErrorCode.hookFailed,
          );
          expect(
            (rAsyncErr['error']! as Map<String, Object?>)['message'],
            contains('boom async'),
          );

          // 6. Timeout -> HOOK_TIMEOUT, leaves zero pending timers.
          final Map<String, Object?> rTimeout = await sendAndAwait(
            7,
            'callHook',
            <String, Object?>{'name': 'hangs', 'timeoutMs': 40},
          );
          expect(
            (rTimeout['error']! as Map<String, Object?>)['code'],
            DriverErrorCode.hookTimeout,
          );

          // 7. Non-JSON-encodable return -> HOOK_FAILED.
          final Map<String, Object?> rBadJson = await sendAndAwait(
            8,
            'callHook',
            <String, Object?>{'name': 'bad_json'},
          );
          expect(
            (rBadJson['error']! as Map<String, Object?>)['code'],
            DriverErrorCode.hookFailed,
          );

          // 8. Invalid params -> INVALID_COMMAND.
          final Map<String, Object?> rBadParams = await sendAndAwait(
            9,
            'callHook',
            <String, Object?>{'name': 'sync_math', 'args': 'not-a-map'},
          );
          expect(
            (rBadParams['error']! as Map<String, Object?>)['code'],
            DriverErrorCode.invalidCommand,
          );

          await transport.close();
          await tester.pump();
        } finally {
          semanticsHandle.dispose();
        }
      },
    );

    testWidgets(
      'returns HOOK_FAILED cleanly when no mounted BuildContext exists',
      (WidgetTester tester) async {
        final HookRegistry hooks = HookRegistry()
          ..register('seed', (_, _) => 'ok');
        final TextInputController textInput = TextInputController();
        final DriverContext context = DriverContext(
          idle: IdleDetector(
            httpTracker: HttpTracker(),
            textInput: textInput,
            readBottomInset: () => 0,
            waitForFirstFrame: (_) async {},
          ),
          pointer: PointerInput(),
          textInput: textInput,
          hooks: hooks,
          resolveHookContext: () => null,
        );
        final RpcSession session = RpcSession(CommandDispatcher(context));
        final _FakeRpcTransport transport = _FakeRpcTransport();
        unawaited(session.serve(transport));

        transport.pushRequest(1, 'callHook', <String, Object?>{'name': 'seed'});
        await tester.pump();
        final Map<String, Object?> msg = transport.sent.lastWhere(
          (Map<String, Object?> m) => m['id'] == 1,
        );
        expect(
          (msg['error']! as Map<String, Object?>)['code'],
          DriverErrorCode.hookFailed,
        );
        expect(
          (msg['error']! as Map<String, Object?>)['message'],
          contains('BuildContext'),
        );
        await transport.close();
      },
    );
  });
}
