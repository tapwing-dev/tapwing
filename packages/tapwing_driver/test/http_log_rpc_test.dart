// F8b HTTP request log over RPC: `configure` `httpLog`, piggybacked `http`
// entries attributed to the request in flight, and the `httpLog` re-read.
import 'dart:async';
import 'dart:convert';

import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/diagnostics/http_log.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/protocol/rpc_transport.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/server/rpc_session.dart';

void main() {
  testWidgets('http entries ride on responses only after configure httpLog', (
    WidgetTester tester,
  ) async {
    await tester.pumpWidget(const SizedBox());
    final HttpTracker tracker = HttpTracker();
    final TextInputController textInput = TextInputController();
    final DriverContext context = DriverContext(
      idle: IdleDetector(
        httpTracker: tracker,
        textInput: textInput,
        readBottomInset: () => 0,
        waitForFirstFrame: (_) async {},
      ),
      pointer: PointerInput(),
      textInput: textInput,
    );
    final _Transport transport = _Transport();
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

    void request(String path) {
      final HttpLogEntry? e = tracker.log.begin(
        'GET',
        Uri.parse('http://h$path'),
      );
      if (e != null) {
        tracker.log.finish(e, HttpLogState.completed);
      }
    }

    try {
      // Older hosts (and runs without --trace) never enable the log.
      request('/before');
      final Map<String, Object?> ping = await call(1, 'ping');
      expect(ping.containsKey('http'), isFalse);
      final Map<String, Object?> plainCfg = await call(2, 'configure');
      expect(
        (plainCfg['result']! as Map<String, Object?>).containsKey('httpLog'),
        isFalse,
      );

      final Map<String, Object?> cfg = await call(
        3,
        'configure',
        <String, Object?>{'httpLog': true},
      );
      expect((cfg['result']! as Map<String, Object?>)['httpLog'], isTrue);

      // Fires between requests: attributed to the last request (3).
      request('/after-configure');
      final Map<String, Object?> next = await call(4, 'ping');
      final List<Object?> http = next['http']! as List<Object?>;
      expect(http.single, containsPair('url', 'http://h/after-configure'));
      expect(http.single, containsPair('request', 3));
      expect((await call(5, 'ping')).containsKey('http'), isFalse);

      final HttpLogEntry open = tracker.log.begin(
        'POST',
        Uri.parse('http://h/open'),
      )!;
      final Map<String, Object?> reread =
          (await call(6, 'httpLog', <String, Object?>{'since': 0}))['result']!
              as Map<String, Object?>;
      expect(reread['entries'], hasLength(1));
      expect(
        (reread['inFlight']! as List<Object?>).single,
        containsPair('state', 'inFlight'),
      );
      expect(reread['latestSeq'], 1);
      expect(reread['enabled'], isTrue);
      tracker.log.finish(open, HttpLogState.completed);

      final Map<String, Object?> off = await call(
        7,
        'configure',
        <String, Object?>{'httpLog': false},
      );
      expect(
        (off['result']! as Map<String, Object?>).containsKey('httpLog'),
        isFalse,
      );
      expect(tracker.log.since(0), isEmpty, reason: 'cleared on disable');
    } finally {
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
