// F7 app health capture: classification, deduplication, handler chaining,
// overflow scans, and delivery on RPC responses.
import 'dart:async';
import 'dart:convert';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/diagnostics/app_issues.dart';
import 'package:tapwing_driver/src/diagnostics/error_hooks.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/protocol/rpc_transport.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/server/rpc_session.dart';

const Widget _overflowingRow = SizedBox(
  width: 120,
  height: 30,
  child: Row(
    children: <Widget>[
      Text('A very long product name that never fits the row'),
    ],
  ),
);

class _Thrower extends StatelessWidget {
  const _Thrower();

  @override
  Widget build(BuildContext context) => throw StateError('boom in build');
}

/// Runs [body] with hooks installed on top of the test framework's
/// `FlutterError.onError`, swallowing errors that reach the framework, and
/// always restores the handlers.
Future<void> withHooks(
  AppErrorHooks hooks,
  Future<void> Function() body,
) async {
  final FlutterExceptionHandler? framework = FlutterError.onError;
  final List<FlutterErrorDetails> reachedFramework = <FlutterErrorDetails>[];
  FlutterError.onError = reachedFramework.add;
  hooks.install();
  try {
    await body();
  } finally {
    hooks.uninstall();
    FlutterError.onError = framework;
  }
}

void main() {
  group('AppIssueLog', () {
    test(
      'deduplicates per request and re-delivers repeats with the same id',
      () {
        final AppIssueLog log = AppIssueLog(clockMs: () => 5)
          ..deliveryEnabled = true;
        log.currentRequest = 1;
        log.record(
          kind: AppIssueKind.imageLoadError,
          message: 'HTTP 404\nmore',
        );
        log.record(
          kind: AppIssueKind.imageLoadError,
          message: 'HTTP 404\nother',
        );
        final List<AppIssue> first = log.takeUndelivered();
        expect(first, hasLength(1));
        expect(first.single.count, 2);
        expect(first.single.message, 'HTTP 404');
        expect(first.single.request, 1);

        // Same issue again during the same request: delivered again, same id.
        log.record(kind: AppIssueKind.imageLoadError, message: 'HTTP 404');
        final List<AppIssue> repeat = log.takeUndelivered();
        expect(repeat.single.id, first.single.id);
        expect(repeat.single.count, 3);

        // A new request gets its own record.
        log.currentRequest = 2;
        log.record(kind: AppIssueKind.imageLoadError, message: 'HTTP 404');
        final AppIssue second = log.takeUndelivered().single;
        expect(second.id, isNot(first.single.id));
        expect(second.request, 2);
        expect(log.takeUndelivered(), isEmpty);
      },
    );

    test('ring buffer evicts the oldest and counts undelivered drops', () {
      final AppIssueLog log = AppIssueLog(maxEntries: 3)
        ..deliveryEnabled = true;
      for (int i = 0; i < 5; i++) {
        log.record(kind: AppIssueKind.uncaughtError, message: 'error $i');
      }
      expect(log.dropped, 2);
      expect(log.takeUndelivered().map((AppIssue i) => i.message), <String>[
        'error 2',
        'error 3',
        'error 4',
      ]);
      expect(log.since(4).single.message, 'error 4');
    });

    test('high-rate flood stays bounded at 500 and deduplicates repeats', () {
      final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
      log.currentRequest = 1;
      for (int i = 0; i < 2000; i++) {
        log.record(
          kind: AppIssueKind.uncaughtError,
          message: 'StateError: flood unique #$i',
        );
      }
      expect(log.dropped, 1500);
      final List<AppIssue> uniqueBatch = log.takeUndelivered();
      expect(uniqueBatch, hasLength(AppIssueLog.defaultMaxEntries));
      expect(uniqueBatch.first.message, 'StateError: flood unique #1500');
      expect(uniqueBatch.last.message, 'StateError: flood unique #1999');

      log.currentRequest = 2;
      for (int i = 0; i < 2000; i++) {
        log.record(
          kind: AppIssueKind.uncaughtError,
          message: 'StateError: flood repeated',
        );
      }
      expect(log.dropped, 1500, reason: 'deduped repeats do not evict');
      final List<AppIssue> repeatBatch = log.takeUndelivered();
      expect(repeatBatch, hasLength(1));
      expect(repeatBatch.single.count, 2000);
    });

    test('nothing is attached while delivery is disabled', () {
      final AppIssueLog log = AppIssueLog();
      log.record(kind: AppIssueKind.uncaughtError, message: 'x');
      expect(log.takeUndelivered(), isEmpty);
      log.deliveryEnabled = true;
      expect(log.takeUndelivered(), hasLength(1), reason: 'buffered meanwhile');
    });

    test('stack trimming prefers app frames', () {
      final StackTrace stack = StackTrace.fromString(
        '#0      Element.rebuild (package:flutter/src/widgets/framework.dart:10:1)\n'
        '#1      ProductCard.build (package:shop/product_card.dart:42:7)\n'
        '#2      _rootRun (dart:async/zone.dart:1:1)\n',
      );
      expect(AppIssueLog.trimStack(stack), <String>[
        '#1      ProductCard.build (package:shop/product_card.dart:42:7)',
      ]);
    });
  });

  group('AppErrorHooks', () {
    testWidgets(
      'classifies framework errors and chains to the previous handler',
      (WidgetTester tester) async {
        final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
        final AppErrorHooks hooks = AppErrorHooks(
          log: log,
          detectOverflows: false,
        );
        int reachedPrevious = 0;
        await withHooks(hooks, () async {
          final FlutterExceptionHandler? ours = FlutterError.onError;
          // Re-install below ours to count pass-through calls.
          hooks.uninstall();
          FlutterError.onError = (FlutterErrorDetails d) => reachedPrevious++;
          hooks.install();
          expect(FlutterError.onError, isNot(same(ours)));

          await tester.pumpWidget(
            const Directionality(
              textDirection: TextDirection.ltr,
              child: Align(
                alignment: Alignment.topLeft,
                child: _overflowingRow,
              ),
            ),
          );
          await tester.pumpWidget(
            const Directionality(
              textDirection: TextDirection.ltr,
              child: _Thrower(),
            ),
          );
        });
        final List<AppIssue> issues = log.takeUndelivered();
        expect(issues.map((AppIssue i) => i.kind), <AppIssueKind>[
          AppIssueKind.layoutOverflow,
          AppIssueKind.buildError,
        ]);
        expect(issues.first.message, startsWith('A RenderFlex overflowed by '));
        expect(issues.first.library, 'rendering library');
        expect(issues.last.message, contains('boom in build'));
        expect(issues.last.stack.first, contains('app_issues_test.dart'));
        expect(
          reachedPrevious,
          2,
          reason: 'every error still reaches the app handler',
        );
      },
    );

    test('classifies Flutter Zone mismatch error as a setupWarning', () {
      final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
      final AppErrorHooks hooks = AppErrorHooks(
        log: log,
        detectOverflows: false,
      );
      final FlutterExceptionHandler? framework = FlutterError.onError;
      FlutterError.onError = (_) {};
      hooks.install();
      try {
        FlutterError.reportError(
          FlutterErrorDetails(
            exception: FlutterError(
              'Zone mismatch.\n'
              'The Flutter bindings were initialized in a different zone than is now being used.\n'
              'It is important to use the same zone when calling `ensureInitialized` on the binding '
              'as when calling `runApp` later.',
            ),
            stack: StackTrace.current,
            context: ErrorDescription('during runApp'),
          ),
        );
      } finally {
        hooks.uninstall();
        FlutterError.onError = framework;
      }
      final AppIssue issue = log.takeUndelivered().single;
      expect(issue.kind, AppIssueKind.setupWarning);
      expect(issue.message, 'Zone mismatch.');
      expect(issue.summary, contains('docs/setup.md'));
    });

    test(
      'classifies fatal Zone mismatch in PlatformDispatcher as setupWarning',
      () {
        final ui.PlatformDispatcher dispatcher = ui.PlatformDispatcher.instance;
        final ui.ErrorCallback? original = dispatcher.onError;
        final FlutterExceptionHandler? flutterOriginal = FlutterError.onError;
        final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
        final AppErrorHooks hooks = AppErrorHooks(
          log: log,
          detectOverflows: false,
        );
        try {
          dispatcher.onError = null;
          hooks.install();
          dispatcher.onError!(
            FlutterError(
              'Zone mismatch.\n'
              'The Flutter bindings were initialized in a different zone than is now being used.',
            ),
            StackTrace.current,
          );
        } finally {
          hooks.uninstall();
          dispatcher.onError = original;
          FlutterError.onError = flutterOriginal;
        }
        final AppIssue issue = log.takeUndelivered().single;
        expect(issue.kind, AppIssueKind.setupWarning);
        expect(issue.summary, contains('docs/setup.md'));
      },
    );

    testWidgets('classifies image load failures', (WidgetTester tester) async {
      final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
      final AppErrorHooks hooks = AppErrorHooks(
        log: log,
        detectOverflows: false,
      );
      await withHooks(hooks, () async {
        await tester.pumpWidget(
          Image.memory(Uint8List.fromList(<int>[1, 2, 3])),
        );
        await tester.runAsync(
          () => Future<void>.delayed(const Duration(milliseconds: 100)),
        );
        await tester.pump();
      });
      expect(
        log.takeUndelivered().map((AppIssue i) => i.kind),
        contains(AppIssueKind.imageLoadError),
      );
    });

    testWidgets(
      're-wraps a handler installed later, recording each error once',
      (WidgetTester tester) async {
        final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
        final AppErrorHooks hooks = AppErrorHooks(
          log: log,
          detectOverflows: false,
        );
        int reporterCalls = 0;
        await withHooks(hooks, () async {
          // A crash reporter installed after the binding (Sentry style): it
          // keeps calling the handler it replaced, which is Tapwing's.
          final FlutterExceptionHandler? replaced = FlutterError.onError;
          FlutterError.onError = (FlutterErrorDetails d) {
            reporterCalls++;
            replaced?.call(d);
          };
          hooks.ensureInstalled(); // what every RPC boundary does
          FlutterError.reportError(
            FlutterErrorDetails(exception: StateError('late')),
          );
        });
        expect(reporterCalls, 1);
        final AppIssue issue = log.takeUndelivered().single;
        expect(
          issue.count,
          1,
          reason: 'two Tapwing wrappers in one chain record once',
        );
        expect(issue.kind, AppIssueKind.frameworkAssertion);
      },
    );

    test('PlatformDispatcher.onError keeps the return contract', () {
      final ui.PlatformDispatcher dispatcher = ui.PlatformDispatcher.instance;
      final ui.ErrorCallback? original = dispatcher.onError;
      final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
      final AppErrorHooks hooks = AppErrorHooks(
        log: log,
        detectOverflows: false,
      );
      final FlutterExceptionHandler? flutterOriginal = FlutterError.onError;
      try {
        dispatcher.onError = null;
        hooks.install();
        expect(
          dispatcher.onError!(StateError('async 1'), StackTrace.current),
          isFalse,
        );
        hooks.uninstall();
        expect(dispatcher.onError, isNull, reason: 'restored');

        dispatcher.onError = (Object e, StackTrace s) => true;
        hooks.install();
        expect(
          dispatcher.onError!(StateError('async 2'), StackTrace.current),
          isTrue,
        );
      } finally {
        hooks.uninstall();
        dispatcher.onError = original;
        FlutterError.onError = flutterOriginal;
      }
      final List<AppIssue> issues = log.takeUndelivered();
      expect(issues.map((AppIssue i) => i.kind).toSet(), <AppIssueKind>{
        AppIssueKind.uncaughtError,
      });
      expect(issues.map((AppIssue i) => i.message), <String>[
        'StateError: Bad state: async 1',
        'StateError: Bad state: async 2',
      ]);
    });

    testWidgets('overflow scan records each render object once', (
      WidgetTester tester,
    ) async {
      final AppIssueLog log = AppIssueLog()..deliveryEnabled = true;
      final AppErrorHooks hooks = AppErrorHooks(
        log: log,
        detectOverflows: true,
      );
      // Pump while the hooks are off so only the detector reports.
      final FlutterExceptionHandler? framework = FlutterError.onError;
      FlutterError.onError = (_) {};
      try {
        await tester.pumpWidget(
          const Directionality(
            textDirection: TextDirection.ltr,
            child: Align(alignment: Alignment.topLeft, child: _overflowingRow),
          ),
        );
      } finally {
        FlutterError.onError = framework;
      }
      await withHooks(hooks, () async {
        hooks
          ..scanOverflows()
          ..scanOverflows();
      });
      final AppIssue issue = log.takeUndelivered().single;
      expect(issue.kind, AppIssueKind.layoutOverflow);
      expect(issue.count, 1);
      expect(issue.summary, contains('At 0.0,0.0 120.0x30.0'));
      expect(issue.summary, contains('"A very long product name'));

      // Not scanned when the host did not ask for issues.
      final AppIssueLog quiet = AppIssueLog();
      final AppErrorHooks off = AppErrorHooks(
        log: quiet,
        detectOverflows: true,
      );
      await withHooks(off, () async => off.scanOverflows());
      quiet.deliveryEnabled = true;
      expect(quiet.takeUndelivered(), isEmpty);
    });
  });

  testWidgets('issues ride on RPC responses, attributed to the request', (
    WidgetTester tester,
  ) async {
    await tester.pumpWidget(const SizedBox());
    final AppIssueLog log = AppIssueLog();
    final AppErrorHooks hooks = AppErrorHooks(log: log, detectOverflows: false);
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
      errorHooks: hooks,
    );
    final _Transport transport = _Transport();
    final FlutterExceptionHandler? framework = FlutterError.onError;
    final ui.ErrorCallback? dispatcherOriginal =
        ui.PlatformDispatcher.instance.onError;
    unawaited(RpcSession(CommandDispatcher(context)).serve(transport));

    Future<Map<String, Object?>> call(
      int id,
      String method, [
      Map<String, Object?> params = const <String, Object?>{},
    ]) async {
      final int before = transport.sent.length;
      transport.push(id, method, params);
      for (int i = 0; i < 40 && transport.sent.length == before; i++) {
        await tester.pump(const Duration(milliseconds: 16));
      }
      return transport.sent.last;
    }

    try {
      // Old hosts never configure appIssues: no field on responses.
      log.record(kind: AppIssueKind.uncaughtError, message: 'before configure');
      expect((await call(1, 'ping')).containsKey('issues'), isFalse);

      final Map<String, Object?> cfg = await call(
        2,
        'configure',
        <String, Object?>{'appIssues': true},
      );
      expect((cfg['result']! as Map<String, Object?>)['appIssues'], isTrue);
      // Buffered startup issue is delivered with the configure response.
      expect(
        (cfg['issues']! as List<Object?>).single,
        containsPair('message', 'before configure'),
      );

      await call(3, 'ping');
      // Fires between requests: attributed to the last completed request.
      log.record(kind: AppIssueKind.uncaughtError, message: 'after ping 3');
      final List<Object?> next =
          (await call(4, 'ping'))['issues']! as List<Object?>;
      expect(next.single, containsPair('request', 3));

      final Map<String, Object?> drained =
          (await call(5, 'appIssues', <String, Object?>{'since': 0}))['result']!
              as Map<String, Object?>;
      expect(drained['issues'], hasLength(2));
      expect(drained['latestSeq'], 2);

      final Map<String, Object?> offCfg = await call(
        6,
        'configure',
        <String, Object?>{'appIssues': false},
      );
      expect(
        (offCfg['result']! as Map<String, Object?>).containsKey('appIssues'),
        isFalse,
      );
      expect(hooks.isInstalled, isFalse);
      expect(
        FlutterError.onError,
        same(framework),
        reason: 'restored by uninstall',
      );
    } finally {
      hooks.uninstall();
      FlutterError.onError = framework;
      ui.PlatformDispatcher.instance.onError = dispatcherOriginal;
      await transport.close();
    }
  });
}

class _Transport implements RpcTransport {
  final StreamController<String> _incoming = StreamController<String>();
  final List<Map<String, Object?>> sent = <Map<String, Object?>>[];

  @override
  Stream<String> get lines => _incoming.stream;

  @override
  void sendLine(String line) => sent.add(
    (jsonDecode(line) as Map<Object?, Object?>).cast<String, Object?>(),
  );

  void push(int id, String method, Map<String, Object?> params) =>
      _incoming.add(
        jsonEncode(<String, Object?>{
          'id': id,
          'method': method,
          'params': params,
        }),
      );

  @override
  Future<void> close() async {
    if (!_incoming.isClosed) {
      await _incoming.close();
    }
  }
}
