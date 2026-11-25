import 'dart:async';
import 'dart:io';

import 'package:fake_async/fake_async.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/diagnostics/http_log.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/protocol/rpc_error.dart';
import 'package:tapwing_driver/src/protocol/rpc_messages.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';

/// An `HttpClient` whose requests stay open until the test ends them, so
/// request lifetimes are driven by the fake clock alone (no sockets).
class _PendingClient implements HttpClient {
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

class _PendingOverrides extends HttpOverrides {
  _PendingOverrides(this.client);

  final _PendingClient client;

  @override
  HttpClient createHttpClient(SecurityContext? context) => client;
}

class _ChainingOverrides extends HttpOverrides {
  _ChainingOverrides(this._previous);

  final HttpOverrides? _previous;

  @override
  HttpClient createHttpClient(SecurityContext? context) =>
      _previous?.createHttpClient(context) ?? super.createHttpClient(context);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('HTTP settle bound (fakeAsync)', () {
    late _PendingClient inner;
    late HttpTracker tracker;

    setUp(() {
      inner = _PendingClient();
      HttpOverrides.global = _PendingOverrides(inner);
      tracker = HttpTracker()..install();
    });

    tearDown(() => HttpOverrides.global = null);

    /// Opens a request through the tracker; it stays in flight.
    void open(String url) {
      unawaited(
        HttpClient()
            .getUrl(Uri.parse(url))
            .then<void>((_) {}, onError: (Object _) {}),
      );
    }

    /// Fails the [index]-th opened request (connection error).
    void fail(FakeAsync async, int index) {
      inner.opened[index].completeError(const SocketException('refused'));
      async.flushMicrotasks();
    }

    test('the default bound is within 2-5 s and not below the default settle '
        'timeout, so one settle never ends early because of it', () {
      expect(HttpTracker.defaultSettleBound, const Duration(seconds: 5));
      expect(tracker.settleBound, HttpTracker.defaultSettleBound);
      expect(
        HttpTracker.defaultSettleBound.inMilliseconds,
        greaterThan(DriverTimeouts.settleMs),
      );
    });

    test('a request shorter than the bound blocks settle until it ends', () {
      fakeAsync((FakeAsync async) {
        open('http://api.test/login');
        expect(tracker.inFlight, 1);
        Duration? idleAt;
        unawaited(
          tracker
              .waitForIdle(const Duration(minutes: 1))
              .then((_) => idleAt = async.elapsed),
        );

        async.elapse(const Duration(milliseconds: 1200));
        expect(tracker.isIdle, isFalse);
        expect(idleAt, isNull);

        fail(async, 0);
        expect(tracker.isIdle, isTrue);
        expect(idleAt, const Duration(milliseconds: 1200));
        expect(async.pendingTimers, isEmpty, reason: 'bound timer cancelled');
      });
    });

    test('a longer request stops blocking exactly at the bound', () {
      fakeAsync((FakeAsync async) {
        open('http://api.test/long-poll');
        Duration? idleAt;
        unawaited(
          tracker
              .waitForIdle(const Duration(minutes: 1))
              .then((_) => idleAt = async.elapsed),
        );

        async.elapse(const Duration(milliseconds: 4999));
        expect(tracker.inFlight, 1);
        expect(idleAt, isNull);

        async.elapse(const Duration(milliseconds: 1));
        expect(tracker.inFlight, 0);
        expect(idleAt, const Duration(seconds: 5));

        // Ending later must not release idleness a second time.
        open('http://api.test/other');
        fail(async, 0);
        expect(tracker.inFlight, 1);
        fail(async, 1);
        expect(tracker.inFlight, 0);
        expect(async.pendingTimers, isEmpty);
      });
    });

    test('each request is measured from its own start', () {
      fakeAsync((FakeAsync async) {
        open('http://api.test/a');
        async.elapse(const Duration(seconds: 3));
        open('http://api.test/b');
        async.elapse(const Duration(seconds: 2));
        expect(tracker.inFlight, 1, reason: 'a expired, b is 2 s old');
        async.elapse(const Duration(milliseconds: 2999));
        expect(tracker.inFlight, 1);
        async.elapse(const Duration(milliseconds: 1));
        expect(tracker.inFlight, 0);
      });
    });

    test('a request past the bound stays in the request log until it ends', () {
      fakeAsync((FakeAsync async) {
        tracker.log.enabled = true;
        open('http://api.test/stream');
        async.elapse(const Duration(seconds: 5));
        expect(tracker.isIdle, isTrue);
        expect(tracker.log.inFlight, hasLength(1));
        expect(tracker.log.since(0), isEmpty);

        async.elapse(const Duration(seconds: 2));
        fail(async, 0);
        final HttpLogEntry entry = tracker.log.since(0).single;
        expect(entry.state, HttpLogState.failed);
        expect(entry.url, 'http://api.test/stream');
        expect(tracker.log.inFlight, isEmpty);
        expect(async.pendingTimers, isEmpty);
      });
    });

    test('a configured bound applies to new requests; no bound waits for '
        'the safety timeout as before', () {
      fakeAsync((FakeAsync async) {
        tracker.settleBound = const Duration(milliseconds: 800);
        open('http://api.test/a');
        async.elapse(const Duration(milliseconds: 799));
        expect(tracker.inFlight, 1);
        async.elapse(const Duration(milliseconds: 1));
        expect(tracker.inFlight, 0);

        tracker.settleBound = null;
        open('http://api.test/b');
        async.elapse(
          HttpTracker.openSafetyTimeout - const Duration(milliseconds: 1),
        );
        expect(tracker.inFlight, 1);
        async.elapse(const Duration(milliseconds: 1));
        expect(tracker.inFlight, 0);
        expect(async.pendingTimers, isEmpty);
      });
    });

    test('an ignored URL never blocks settle and is still logged', () {
      fakeAsync((FakeAsync async) {
        tracker
          ..settleIgnore = <RegExp>[
            RegExp(r'^https://analytics\.'),
            RegExp('/poll'),
          ]
          ..log.enabled = true;
        open('https://analytics.test/collect?e=tap');
        open('http://api.test/v1/poll?since=3');
        expect(tracker.isIdle, isTrue);
        expect(tracker.log.inFlight, hasLength(2));

        // A URL no pattern matches is counted as usual.
        open('http://api.test/v1/items');
        expect(tracker.inFlight, 1);
        fail(async, 2);
        expect(tracker.isIdle, isTrue);

        fail(async, 0);
        fail(async, 1);
        expect(tracker.inFlight, 0, reason: 'never counted, never released');
        expect(tracker.log.since(0), hasLength(3));
        expect(async.pendingTimers, isEmpty);
      });
    });

    test('an ignored URL with the log off is a pure pass-through', () {
      fakeAsync((FakeAsync async) {
        tracker.settleIgnore = <RegExp>[RegExp('/poll')];
        open('http://api.test/poll');
        expect(tracker.isIdle, isTrue);
        expect(async.pendingTimers, isEmpty, reason: 'no timer armed');
        fail(async, 0);
      });
    });

    test('ensureInstalled keeps settleBound and settleIgnore active on '
        'app-installed overrides without double-counting', () {
      fakeAsync((FakeAsync async) {
        tracker
          ..settleBound = const Duration(milliseconds: 500)
          ..settleIgnore = <RegExp>[RegExp('/poll')];

        // Non-chaining override installed by the app after TapwingBinding.
        HttpOverrides.global = _PendingOverrides(inner);
        tracker.ensureInstalled();

        open('http://api.test/poll');
        expect(tracker.inFlight, 0);

        open('http://api.test/non-chaining');
        expect(tracker.inFlight, 1);
        async.elapse(const Duration(milliseconds: 500));
        expect(tracker.inFlight, 0);

        // Chaining override installed on top of Tapwing's override.
        HttpOverrides.global = _ChainingOverrides(HttpOverrides.current);
        tracker.ensureInstalled();

        open('http://api.test/chaining');
        expect(tracker.inFlight, 1, reason: 'must not double-count');
        async.elapse(const Duration(milliseconds: 500));
        expect(tracker.inFlight, 0);

        fail(async, 0);
        fail(async, 1);
        fail(async, 2);
        expect(tracker.inFlight, 0);
        expect(async.pendingTimers, isEmpty);
      });
    });
  });

  group('pending image settle bound', () {
    late Duration now;
    late int pending;
    late HttpTracker tracker;
    late IdleDetector detector;

    setUp(() {
      now = const Duration(seconds: 100);
      pending = 0;
      tracker = HttpTracker();
      detector = IdleDetector(
        httpTracker: tracker,
        textInput: TextInputController(),
        readBottomInset: () => 0,
        waitForFirstFrame: (_) async {},
        clock: () => now,
        readPendingImages: () => pending,
      );
    });

    void advance(int ms) => now += Duration(milliseconds: ms);

    test('an image that resolves before the bound blocks until it does', () {
      expect(detector.loadingIdle, isTrue);
      pending = 1;
      expect(detector.loadingIdle, isFalse);
      advance(1500);
      expect(detector.loadingIdle, isFalse);
      pending = 0;
      expect(detector.loadingIdle, isTrue);
    });

    test('an image that never resolves stops blocking exactly at the '
        'bound', () {
      pending = 1;
      expect(detector.loadingIdle, isFalse);
      advance(4999);
      expect(detector.loadingIdle, isFalse);
      advance(1);
      expect(detector.loadingIdle, isTrue);
      advance(60000);
      expect(detector.loadingIdle, isTrue);
      expect(detector.busyReport().pendingImages, 1, reason: 'raw count');
    });

    test('a new image blocks again next to a stale one', () {
      pending = 1;
      expect(detector.loadingIdle, isFalse);
      advance(5000);
      expect(detector.loadingIdle, isTrue);

      pending = 2;
      expect(detector.loadingIdle, isFalse);
      advance(900);
      // The new one finished (or the stale one did): nothing fresh is left.
      pending = 1;
      expect(detector.loadingIdle, isTrue);

      pending = 2;
      expect(detector.loadingIdle, isFalse);
      advance(4999);
      expect(detector.loadingIdle, isFalse);
      advance(1);
      expect(detector.loadingIdle, isTrue);

      // Everything resolved: the next load starts from scratch.
      pending = 0;
      expect(detector.loadingIdle, isTrue);
      pending = 1;
      expect(detector.loadingIdle, isFalse);
    });

    test('follows the configured bound; 0 removes it', () {
      detector.configureSettleLoad(timeoutMs: 250);
      expect(detector.settleLoadBound, const Duration(milliseconds: 250));
      pending = 1;
      expect(detector.loadingIdle, isFalse);
      advance(250);
      expect(detector.loadingIdle, isTrue);

      detector.configureSettleLoad(timeoutMs: 0);
      expect(detector.settleLoadBound, isNull);
      expect(detector.loadingIdle, isFalse, reason: 'unbounded again');
      advance(600000);
      expect(detector.loadingIdle, isFalse);
    });

    testWidgets('waitForIdle returns idle once a hung image passes the bound', (
      WidgetTester tester,
    ) async {
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: Text('Ready'))),
      );
      pending = 1;
      String? state;
      unawaited(
        detector
            .waitForIdle(const Duration(seconds: 30))
            .then((String s) => state = s),
      );
      for (int i = 0; i < 5; i++) {
        await tester.pump(const Duration(milliseconds: 4));
      }
      expect(state, isNull);

      advance(5000);
      await tester.pump(const Duration(milliseconds: 4));
      await tester.pump(Duration.zero);
      await tester.pump(Duration.zero);
      expect(state, 'idle');
    });
  });

  group('configure settle load options', () {
    test('ConfigureParams parses the fields and leaves them unset when '
        'absent', () {
      final ConfigureParams absent = ConfigureParams.fromJson(
        const <String, Object?>{'settlePolicy': 'v2'},
      );
      expect(absent.settleLoadTimeoutMs, isNull);
      expect(absent.settleHttpIgnore, isNull);

      final ConfigureParams set = ConfigureParams.fromJson(
        const <String, Object?>{
          'settleLoadTimeoutMs': 2500,
          'settleHttpIgnore': <Object?>[r'^https://analytics\.', '/poll'],
        },
      );
      expect(set.settleLoadTimeoutMs, 2500);
      expect(set.settleHttpIgnore!.map((RegExp r) => r.pattern), <String>[
        r'^https://analytics\.',
        '/poll',
      ]);
      expect(
        ConfigureParams.fromJson(const <String, Object?>{
          'settleHttpIgnore': <Object?>[],
        }).settleHttpIgnore,
        isEmpty,
      );
    });

    test('ConfigureParams rejects invalid values', () {
      for (final Map<String, Object?> bad in <Map<String, Object?>>[
        <String, Object?>{'settleLoadTimeoutMs': -1},
        <String, Object?>{
          'settleLoadTimeoutMs': ConfigureParams.maxSettleLoadTimeoutMs + 1,
        },
        <String, Object?>{'settleLoadTimeoutMs': double.infinity},
        <String, Object?>{'settleLoadTimeoutMs': '5s'},
        <String, Object?>{'settleHttpIgnore': '/poll'},
        <String, Object?>{
          'settleHttpIgnore': <Object?>[1],
        },
        <String, Object?>{
          'settleHttpIgnore': <Object?>['(unclosed'],
        },
      ]) {
        expect(
          () => ConfigureParams.fromJson(bad),
          throwsA(
            isA<DriverError>().having(
              (DriverError e) => e.code,
              'code',
              DriverErrorCode.invalidCommand,
            ),
          ),
          reason: '$bad',
        );
      }
    });

    test('ConfigureResult reports the fields only when set (older drivers '
        'never send them)', () {
      final Map<String, Object?> legacy = const ConfigureResult(
        settlePolicy: SettlePolicy.v2,
      ).toJson();
      expect(legacy.containsKey('settleLoadTimeoutMs'), isFalse);
      expect(legacy.containsKey('settleHttpIgnore'), isFalse);

      final Map<String, Object?> json = const ConfigureResult(
        settlePolicy: SettlePolicy.v2,
        settleLoadTimeoutMs: 0,
        settleHttpIgnore: 2,
      ).toJson();
      expect(json['settleLoadTimeoutMs'], 0);
      expect(json['settleHttpIgnore'], 2);
    });

    testWidgets('configure applies and echoes them; an older host that sends '
        'neither keeps the current values', (WidgetTester tester) async {
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: Text('Ready'))),
      );
      final HttpTracker tracker = HttpTracker();
      final CommandDispatcher dispatcher = CommandDispatcher(
        DriverContext(
          idle: IdleDetector(
            httpTracker: tracker,
            textInput: TextInputController(),
            readBottomInset: () => 0,
            waitForFirstFrame: (_) async {},
          ),
        ),
      );

      Future<Map<String, Object?>> configure(
        Map<String, Object?> params,
      ) async =>
          (await dispatcher.dispatch('configure', params))!
              as Map<String, Object?>;

      // Defaults, as an older host (no settle load fields) gets them.
      Map<String, Object?> result = await configure(<String, Object?>{
        'settlePolicy': 'v2',
      });
      expect(result['settleLoadTimeoutMs'], 5000);
      expect(result['settleHttpIgnore'], 0);

      result = await configure(<String, Object?>{
        'settleLoadTimeoutMs': 2000,
        'settleHttpIgnore': <Object?>['/poll', 'analytics'],
      });
      expect(result['settleLoadTimeoutMs'], 2000);
      expect(result['settleHttpIgnore'], 2);
      expect(tracker.settleBound, const Duration(seconds: 2));
      expect(tracker.settleIgnore.map((RegExp r) => r.pattern), <String>[
        '/poll',
        'analytics',
      ]);

      result = await configure(<String, Object?>{'settlePolicy': 'legacy'});
      expect(result['settleLoadTimeoutMs'], 2000);
      expect(result['settleHttpIgnore'], 2);

      // The next session resets both explicitly.
      result = await configure(<String, Object?>{
        'settleLoadTimeoutMs': 0,
        'settleHttpIgnore': <Object?>[],
      });
      expect(result['settleLoadTimeoutMs'], 0);
      expect(result['settleHttpIgnore'], 0);
      expect(tracker.settleBound, isNull);
      expect(tracker.settleIgnore, isEmpty);

      // A bad pattern fails the call and changes nothing.
      await expectLater(
        configure(<String, Object?>{
          'settleLoadTimeoutMs': 900,
          'settleHttpIgnore': <Object?>['(unclosed'],
        }),
        throwsA(isA<DriverError>()),
      );
      expect(tracker.settleBound, isNull);
    });
  });
}
