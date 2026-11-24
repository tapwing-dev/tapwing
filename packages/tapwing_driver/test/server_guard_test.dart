import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/protocol/rpc_transport.dart';
import 'package:tapwing_driver/src/server/tapwing_server.dart';

void main() {
  group('isDriverEnabled', () {
    test('debug and profile builds always run the driver', () {
      expect(isDriverEnabled(releaseMode: false, allowRelease: false), isTrue);
      expect(isDriverEnabled(releaseMode: false, allowRelease: true), isTrue);
    });

    test('a release build runs it only when it opted in', () {
      expect(isDriverEnabled(releaseMode: true, allowRelease: false), isFalse);
      expect(isDriverEnabled(releaseMode: true, allowRelease: true), isTrue);
    });
  });

  group('TapwingServer.reconnectDelayAfter', () {
    test('dials often while a host was around within the last minute', () {
      const Duration fast = Duration(milliseconds: 150);
      expect(TapwingServer.reconnectDelayAfter(Duration.zero), fast);
      expect(
        TapwingServer.reconnectDelayAfter(const Duration(seconds: 59)),
        fast,
      );
    });

    test('slows down once no host has connected for a minute', () {
      const Duration idle = Duration(seconds: 1);
      expect(
        TapwingServer.reconnectDelayAfter(const Duration(seconds: 60)),
        idle,
      );
      expect(TapwingServer.reconnectDelayAfter(const Duration(hours: 3)), idle);
    });
  });

  group('BoundedLineSplitter', () {
    Future<List<String>> split(List<String> chunks, {int max = 64}) =>
        Stream<String>.fromIterable(
          chunks,
        ).transform(BoundedLineSplitter(max)).toList();

    test('joins lines split across chunks and drops a trailing CR', () async {
      expect(
        await split(<String>['{"a":', '1}\n{"b":2}\r\n', '{"c"', ':3}\n']),
        <String>['{"a":1}', '{"b":2}', '{"c":3}'],
      );
    });

    test('keeps empty lines and an unterminated last line', () async {
      expect(await split(<String>['a\n\nb\n', 'tail']), <String>[
        'a',
        '',
        'b',
        'tail',
      ]);
      expect(await split(<String>[]), isEmpty);
    });

    test('accepts a line of exactly the limit', () async {
      expect(await split(<String>['12345678\n'], max: 8), <String>['12345678']);
      expect(await split(<String>['1234', '5678\n'], max: 8), <String>[
        '12345678',
      ]);
    });

    test('fails the stream when a line outgrows the limit', () async {
      final List<String> seen = <String>[];
      Object? error;
      try {
        await for (final String line in Stream<String>.fromIterable(<String>[
          'ok\n',
          '1234',
          '56789',
          'x\n',
        ]).transform(const BoundedLineSplitter(8))) {
          seen.add(line);
        }
      } on RpcLineTooLongException catch (e) {
        error = e;
      }
      expect(seen, <String>['ok']);
      expect(error, isA<RpcLineTooLongException>());
      expect('$error', contains('8'));
    });

    test('stops reading from a peer that never sends a newline', () async {
      int produced = 0;
      Stream<String> endless() async* {
        while (true) {
          produced++;
          yield 'x' * 16;
        }
      }

      await expectLater(
        endless().transform(const BoundedLineSplitter(64)).toList(),
        throwsA(isA<RpcLineTooLongException>()),
      );
      // 64 characters fit; the fifth chunk is the one that overflows.
      expect(produced, 5);
    });
  });

  group('SocketRpcTransport over a real loopback socket', () {
    late ServerSocket server;

    setUp(() async {
      server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    });

    tearDown(() async {
      await server.close();
    });

    Future<(SocketRpcTransport, Socket)> connect() async {
      final Future<Socket> accepted = server.first;
      // Owned by the transport: `SocketRpcTransport.close` destroys it.
      // ignore: close_sinks
      final Socket app = await Socket.connect(
        InternetAddress.loopbackIPv4,
        server.port,
      );
      return (SocketRpcTransport(app), await accepted);
    }

    test('frames requests split across packets and answers the host', () async {
      final (SocketRpcTransport transport, Socket host) = await connect();
      final StreamIterator<String> lines = StreamIterator<String>(
        transport.lines,
      );
      final Future<String> reply = utf8.decoder
          .bind(host)
          .transform(const LineSplitter())
          .first;

      host.add(utf8.encode('{"id":1,"method":"tap","params":{"text":"Př'));
      await host.flush();
      host.add(utf8.encode('ihlásit"}}\n{"id":2}\n'));
      await host.flush();

      expect(await lines.moveNext(), isTrue);
      expect(jsonDecode(lines.current), <String, Object?>{
        'id': 1,
        'method': 'tap',
        'params': <String, Object?>{'text': 'Přihlásit'},
      });
      expect(await lines.moveNext(), isTrue);
      expect(lines.current, '{"id":2}');

      transport.sendLine('{"id":1,"result":{}}');
      expect(await reply, '{"id":1,"result":{}}');

      await lines.cancel();
      await transport.close();
      host.destroy();
    });

    test('drops a host that sends an endless line', () async {
      final (SocketRpcTransport transport, Socket host) = await connect();
      final Future<List<String>> received = transport.lines.toList();
      final List<int> block = List<int>.filled(256 * 1024, 0x78);
      unawaited(() async {
        try {
          for (
            int sent = 0;
            sent <= SocketRpcTransport.maxLineLength;
            sent += block.length
          ) {
            host.add(block);
            await host.flush();
          }
        } on Object {
          // The app side closed the connection: what the test expects.
        }
      }());

      await expectLater(received, throwsA(isA<RpcLineTooLongException>()));
      await transport.close();
      host.destroy();
    });
  });
}
