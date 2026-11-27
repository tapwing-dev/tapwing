import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/protocol/rpc_transport.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/server/tapwing_server.dart';

/// Records what the driver writes to one connection, on top of the real
/// socket transport.
class _RecordingTransport implements RpcTransport {
  _RecordingTransport(this._inner);

  final RpcTransport _inner;
  final List<Map<String, Object?>> sent = <Map<String, Object?>>[];
  final Completer<void> _closed = Completer<void>();

  /// Completes once the session released this connection.
  Future<void> get closed => _closed.future;

  bool get isClosed => _closed.isCompleted;

  @override
  Stream<String> get lines => _inner.lines;

  @override
  void sendLine(String line) {
    sent.add(
      (jsonDecode(line) as Map<Object?, Object?>).cast<String, Object?>(),
    );
    _inner.sendLine(line);
  }

  @override
  Future<void> close() async {
    await _inner.close();
    if (!_closed.isCompleted) _closed.complete();
  }
}

/// Reports how every `waitForIdle` call ended.
class _ProbeIdleDetector extends IdleDetector {
  _ProbeIdleDetector({required super.waitForFirstFrame})
    : super(
        httpTracker: HttpTracker(),
        textInput: TextInputController(),
        readBottomInset: () => 0,
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

/// The host CLI's end of one accepted connection.
class _HostPeer {
  _HostPeer(this.socket)
    : lines = StreamIterator<String>(
        socket
            .cast<List<int>>()
            .transform(utf8.decoder)
            .transform(const LineSplitter()),
      );

  final Socket socket;
  final StreamIterator<String> lines;

  void request(int id, String method, [Map<String, Object?>? params]) {
    socket.write(
      '${jsonEncode(<String, Object?>{'id': id, 'method': method, 'params': params ?? <String, Object?>{}})}\n',
    );
  }

  Future<Map<String, Object?>> next() async {
    expect(await lines.moveNext(), isTrue, reason: 'driver closed the socket');
    return (jsonDecode(lines.current) as Map<Object?, Object?>)
        .cast<String, Object?>();
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'a host killed mid-waitIdle is replaced by a new connection: the old '
    'handler is cancelled, nothing is written for it, no timer leaks',
    () async {
      // Every timer the driver creates while serving, so leaks are visible.
      final List<Timer> timers = <Timer>[];
      final ZoneSpecification trackTimers = ZoneSpecification(
        createTimer:
            (
              Zone self,
              ZoneDelegate parent,
              Zone zone,
              Duration duration,
              void Function() f,
            ) {
              final Timer timer = parent.createTimer(zone, duration, f);
              timers.add(timer);
              return timer;
            },
        createPeriodicTimer:
            (
              Zone self,
              ZoneDelegate parent,
              Zone zone,
              Duration period,
              void Function(Timer timer) f,
            ) {
              final Timer timer = parent.createPeriodicTimer(zone, period, f);
              timers.add(timer);
              return timer;
            },
      );

      // The host listens; the driver dials out, as on a device.
      final ServerSocket host = await ServerSocket.bind(
        InternetAddress.loopbackIPv4,
        0,
      );
      final StreamIterator<Socket> accepted = StreamIterator<Socket>(host);
      final List<_RecordingTransport> transports = <_RecordingTransport>[];
      final StreamController<void> waitStartedEvents = StreamController<void>();
      final StreamIterator<void> waitStarted = StreamIterator<void>(
        waitStartedEvents.stream,
      );
      // A first frame that never arrives: the wait can only end by cancellation.
      final _ProbeIdleDetector idle = _ProbeIdleDetector(
        waitForFirstFrame: (_) {
          waitStartedEvents.add(null);
          return Completer<void>().future;
        },
      );
      late final TapwingServer server;

      await runZoned(() async {
        server = TapwingServer(
          port: host.port,
          context: DriverContext(idle: idle, textInput: TextInputController()),
          transportFactory: (Socket socket) {
            final _RecordingTransport transport = _RecordingTransport(
              SocketRpcTransport(socket),
            );
            transports.add(transport);
            return transport;
          },
        )..start();
        // 1. First host connection starts a wait that cannot complete.
        expect(await accepted.moveNext(), isTrue);
        final _HostPeer first = _HostPeer(accepted.current);
        expect((await first.next())['event'], 'hello');
        first.request(7, 'waitIdle', <String, Object?>{'timeoutMs': 600000});
        expect(await waitStarted.moveNext(), isTrue);
        expect(idle.results, isEmpty);
        // 2. The host dies without a goodbye.
        first.socket.destroy();

        // 3. The driver dials again and serves the new host normally: the
        // first thing after `hello` is the answer to its own request.
        expect(await accepted.moveNext(), isTrue);
        final _HostPeer second = _HostPeer(accepted.current);
        expect((await second.next())['event'], 'hello');
        second.request(1, 'ping');
        final Map<String, Object?> pong = await second.next();
        expect(pong['id'], 1);
        expect(pong['result'], 'pong');

        // 4. The old handler unwound before the new request ran, and its
        // response went nowhere.
        expect(idle.results, <String>['cancelled']);
        expect(transports, hasLength(2));
        expect(transports[0].isClosed, isTrue);
        expect(
          transports[0].sent.map((Map<String, Object?> m) => m['event']),
          <Object?>['hello'],
        );
        expect(
          transports[1].sent.map((Map<String, Object?> m) => m['id']),
          <Object?>[null, 1],
        );

        // 5. `stop()` cancels a wait running on the live connection too.
        second.request(2, 'waitIdle', <String, Object?>{'timeoutMs': 600000});
        expect(await waitStarted.moveNext(), isTrue);
        server.stop();
        await transports[1].closed;
        expect(idle.results, <String>['cancelled', 'cancelled']);
        expect(
          transports[1].sent.map((Map<String, Object?> m) => m['id']),
          <Object?>[null, 1],
          reason: 'no response for the cancelled request',
        );
        await first.lines.cancel();
        await second.lines.cancel();
        second.socket.destroy();
      }, zoneSpecification: trackTimers);

      await waitStarted.cancel();
      await waitStartedEvents.close();
      await accepted.cancel();
      await host.close();
      await pumpEventQueue();
      expect(
        timers.where((Timer t) => t.isActive),
        isEmpty,
        reason: 'the stopped driver left timers running',
      );
      expect(transports, hasLength(2), reason: 'no dial after stop()');
    },
  );
}
