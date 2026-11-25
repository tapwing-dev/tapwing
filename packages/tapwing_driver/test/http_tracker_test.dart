import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/diagnostics/http_log.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';

/// An app-level override that does NOT chain to the previous one (the common
/// `class MyOverrides extends HttpOverrides` pattern).
class _AppOverrides extends HttpOverrides {
  int created = 0;

  @override
  HttpClient createHttpClient(SecurityContext? context) {
    created++;
    return super.createHttpClient(context)..userAgent = 'app-agent';
  }
}

/// An app-level override that customizes proxy resolution.
class _ProxyOverrides extends HttpOverrides {
  int proxyCalls = 0;

  @override
  String findProxyFromEnvironment(Uri url, Map<String, String>? environment) {
    proxyCalls++;
    return 'PROXY custom.proxy:8080';
  }
}

/// An app-level override that chains to whatever was current when installed.
class _ChainingOverrides extends HttpOverrides {
  _ChainingOverrides(this._previous);

  final HttpOverrides? _previous;
  int created = 0;

  @override
  HttpClient createHttpClient(SecurityContext? context) {
    created++;
    return _previous?.createHttpClient(context) ??
        super.createHttpClient(context);
  }
}

/// An app-level override that wraps the inner client in its own [HttpClient]
/// facade (e.g. custom headers / pinning wrapper chaining to previous).
class _WrappingOverrides extends HttpOverrides {
  _WrappingOverrides(this._previous);

  final HttpOverrides? _previous;
  int created = 0;

  @override
  HttpClient createHttpClient(SecurityContext? context) {
    created++;
    final HttpClient inner =
        _previous?.createHttpClient(context) ?? super.createHttpClient(context);
    return _WrappedHttpClient(inner);
  }
}

class _WrappedHttpClient implements HttpClient {
  _WrappedHttpClient(this._inner);

  final HttpClient _inner;

  @override
  Future<HttpClientRequest> openUrl(String method, Uri url) =>
      _inner.openUrl(method, url);

  @override
  Future<HttpClientRequest> getUrl(Uri url) => _inner.getUrl(url);

  @override
  void close({bool force = false}) => _inner.close(force: force);

  @override
  dynamic noSuchMethod(Invocation invocation) =>
      throw UnimplementedError('${invocation.memberName}');
}

Future<HttpServer> _server() async {
  final HttpServer server = await HttpServer.bind(
    InternetAddress.loopbackIPv4,
    0,
  );
  server.listen((HttpRequest req) async {
    final List<int> body = await req.fold<List<int>>(
      <int>[],
      (List<int> acc, List<int> chunk) => acc..addAll(chunk),
    );
    final HttpResponse res = req.response;
    res.headers
      ..set('set-cookie', 'sid=server-secret')
      ..set('x-request-id', 'abc');
    if (req.uri.path.startsWith('/missing')) {
      res.statusCode = 404;
      res.write('not found');
    } else if (req.uri.path == '/slow') {
      await Future<void>.delayed(const Duration(milliseconds: 150));
      res.write('slow');
    } else if (req.uri.path == '/sse') {
      res.headers.contentType = ContentType(
        'text',
        'event-stream',
        charset: 'utf-8',
      );
      res.bufferOutput = false;
      res.write('data: first\n\n');
      await res.flush();
      await Future<void>.delayed(const Duration(milliseconds: 150));
      res.write('data: second\n\n');
      await res.flush();
    } else if (req.uri.path == '/stream') {
      res.bufferOutput = false;
      res.write('chunk-1');
      await res.flush();
      await Future<void>.delayed(const Duration(seconds: 5));
    } else {
      res.write(jsonEncode(<String, Object?>{'ok': true, 'got': body.length}));
    }
    await res.close();
  });
  return server;
}

Future<int> _get(Uri url, {Map<String, String> headers = const {}}) async {
  final HttpClient client = HttpClient();
  try {
    final HttpClientRequest req = await client.getUrl(url);
    headers.forEach(req.headers.set);
    final HttpClientResponse res = await req.close();
    await res.drain<void>();
    return res.statusCode;
  } finally {
    client.close();
  }
}

void main() {
  late HttpServer server;
  late Uri base;

  setUp(() async {
    HttpOverrides.global = null;
    server = await _server();
    base = Uri.parse('http://127.0.0.1:${server.port}');
  });

  tearDown(() async {
    HttpOverrides.global = null;
    await server.close(force: true);
  });

  test('disabled log records nothing and keeps idle counting', () async {
    final HttpTracker tracker = HttpTracker()..install();
    expect(await _get(base.replace(path: '/ok')), 200);
    expect(tracker.log.since(0), isEmpty);
    expect(tracker.log.takeUndelivered(), isEmpty);
    expect(tracker.inFlight, 0);
  });

  test('enabled log records a completed request with sizes and '
      'attribution', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log
      ..enabled = true
      ..currentRequest = 7;
    final HttpClient client = HttpClient();
    final HttpClientRequest req = await client.postUrl(
      base.replace(path: '/ok'),
    );
    expect(tracker.inFlight, 1, reason: 'idle counting still active');
    req
      ..add(utf8.encode('hello'))
      ..write('abcd');
    final HttpClientResponse res = await req.close();
    await res.drain<void>();
    client.close();

    final List<HttpLogEntry> entries = tracker.log.takeUndelivered();
    expect(entries, hasLength(1));
    final Map<String, Object?> json = entries.single.toJson();
    expect(json['method'], 'POST');
    expect(json['url'], 'http://127.0.0.1:${server.port}/ok');
    expect(json['state'], 'completed');
    expect(json['status'], 200);
    expect(json['request'], 7);
    expect(json['requestBytes'], 9, reason: 'add + UTF-8 write');
    expect(json['responseBytes'], greaterThan(0));
    expect(json['durationMs'], isA<int>());
    expect(json['headersMs'], isA<int>());
    expect(tracker.inFlight, 0);
    // Delivered once only.
    expect(tracker.log.takeUndelivered(), isEmpty);
  });

  test('chains to an HttpOverrides installed before the tracker', () async {
    final _AppOverrides app = _AppOverrides();
    HttpOverrides.global = app;
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    expect(await _get(base.replace(path: '/ok')), 200);
    expect(app.created, 1, reason: 'previous override still creates clients');
    expect(tracker.log.since(0), hasLength(1));
  });

  test('re-wraps an override the app installs later with idle counting '
      'enabled and without double logging', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    final _AppOverrides app = _AppOverrides();
    HttpOverrides.global = app; // App replaces ours in main().
    tracker.ensureInstalled();
    // Idempotent: a second check does not stack another layer.
    tracker.ensureInstalled();

    final HttpClient client = HttpClient();
    expect(client.userAgent, 'app-agent', reason: "app's override applies");
    final HttpClientRequest req = await client.getUrl(
      base.replace(path: '/slow'),
    );
    expect(tracker.inFlight, 1, reason: 'idle counting stays active');
    final HttpClientResponse res = await req.close();
    await res.drain<void>();
    client.close();

    expect(tracker.inFlight, 0);
    expect(app.created, 1);
    expect(tracker.log.since(0), hasLength(1));
  });

  test(
    'an app override chaining to ours counts idle and logs exactly once',
    () async {
      final HttpTracker tracker = HttpTracker()..install();
      tracker.log.enabled = true;
      final _ChainingOverrides app = _ChainingOverrides(HttpOverrides.current);
      HttpOverrides.global = app;
      tracker.ensureInstalled();

      final HttpClient client = HttpClient();
      final HttpClientRequest req = await client.getUrl(
        base.replace(path: '/slow'),
      );
      expect(
        tracker.inFlight,
        1,
        reason: 'chained override must not double-count',
      );
      final HttpClientResponse res = await req.close();
      await res.drain<void>();
      client.close();

      expect(app.created, 1);
      expect(tracker.log.since(0), hasLength(1));
      expect(tracker.inFlight, 0);
    },
  );

  test(
    'an app override wrapping the chained HttpClient does not double-count',
    () async {
      final HttpTracker tracker = HttpTracker()..install();
      tracker.log.enabled = true;
      final _WrappingOverrides app = _WrappingOverrides(HttpOverrides.current);
      HttpOverrides.global = app;
      tracker.ensureInstalled();

      final HttpClient client = HttpClient();
      final HttpClientRequest req = await client.getUrl(
        base.replace(path: '/slow'),
      );
      expect(
        tracker.inFlight,
        1,
        reason: 'wrapped chained HttpClient must not double-count',
      );
      final HttpClientResponse res = await req.close();
      await res.drain<void>();
      client.close();

      expect(app.created, 1);
      expect(tracker.log.since(0), hasLength(1));
      expect(tracker.inFlight, 0);
    },
  );

  test(
    'ensureInstalled keeps idle counting active while the log is disabled',
    () async {
      final HttpTracker tracker = HttpTracker()..install();
      final _AppOverrides app = _AppOverrides();
      HttpOverrides.global = app;
      tracker.ensureInstalled();

      final HttpClient client = HttpClient();
      expect(client.userAgent, 'app-agent', reason: "app's override applies");
      final HttpClientRequest req = await client.getUrl(
        base.replace(path: '/slow'),
      );
      expect(tracker.inFlight, 1, reason: 'idle counting active with log off');
      final HttpClientResponse res = await req.close();
      await res.drain<void>();
      client.close();

      expect(tracker.inFlight, 0);
      expect(app.created, 1);
      expect(tracker.log.since(0), isEmpty);
    },
  );

  test('redacts secrets in URLs and headers and never stores bodies', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    const String jwt =
        'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXZhbHVl';
    final Uri url = Uri.parse(
      'http://user:pw@127.0.0.1:${server.port}/missing/$jwt/items'
      '?page=2&access_token=s3cr3t&q=shoes&apiKey=k1'
      '&ref=a1b2c3d4e5f6a7b8c9d0e1f2a3b4&mail=jan%40example.com#frag',
    );
    expect(
      await _get(
        url,
        headers: <String, String>{
          'authorization': 'Bearer top-secret',
          'cookie': 'sid=abc',
          'x-api-key': 'k-123',
          'x-custom-token': 't-456',
          'accept': 'application/json',
        },
      ),
      404,
    );
    final Map<String, Object?> json = tracker.log.since(0).single.toJson();
    final String encoded = jsonEncode(json);
    expect(
      json['url'],
      'http://127.0.0.1:${server.port}/missing/<redacted>/items'
      '?page=2&access_token=<redacted>&q=shoes&apiKey=<redacted>'
      '&ref=<redacted>&mail=<redacted>',
    );
    for (final String secret in <String>[
      'top-secret',
      'sid=abc',
      'k-123',
      't-456',
      's3cr3t',
      'server-secret',
      'user:pw',
      'frag',
      'not found',
      jwt,
    ]) {
      expect(encoded, isNot(contains(secret)), reason: secret);
    }
    final Map<String, Object?> reqHeaders =
        (json['requestHeaders']! as Map<Object?, Object?>)
            .cast<String, Object?>();
    expect(reqHeaders['authorization'], HttpRedaction.redacted);
    expect(reqHeaders['cookie'], HttpRedaction.redacted);
    expect(reqHeaders['x-api-key'], HttpRedaction.redacted);
    expect(reqHeaders['x-custom-token'], HttpRedaction.redacted);
    expect(reqHeaders['accept'], 'application/json');
    final Map<String, Object?> resHeaders =
        (json['responseHeaders']! as Map<Object?, Object?>)
            .cast<String, Object?>();
    expect(resHeaders['set-cookie'], HttpRedaction.redacted);
    expect(resHeaders['x-request-id'], 'abc');
    expect(json['status'], 404);
  });

  test('connection errors are recorded as failed with a sanitized '
      'error', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    final HttpServer closed = await HttpServer.bind(
      InternetAddress.loopbackIPv4,
      0,
    );
    final int port = closed.port;
    await closed.close(force: true);
    final HttpClient client = HttpClient();
    await expectLater(
      client.getUrl(Uri.parse('http://127.0.0.1:$port/x?token=abc')),
      throwsA(isA<SocketException>()),
    );
    client.close();
    final Map<String, Object?> json = tracker.log.since(0).single.toJson();
    expect(json['state'], 'failed');
    expect(json['url'], 'http://127.0.0.1:$port/x?token=<redacted>');
    expect(json['error'], startsWith('SocketException'));
    expect(json['error'], isNot(contains('token=abc')));
    expect(tracker.inFlight, 0, reason: 'failed requests release idleness');
  });

  test('aborted requests and in-flight requests are reported', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    final HttpClient client = HttpClient();
    // Aborted below instead of closed.
    // ignore: close_sinks
    final HttpClientRequest open = await client.getUrl(
      base.replace(path: '/slow'),
    );
    expect(tracker.log.inFlight, hasLength(1));
    expect(tracker.log.inFlight.single.toJson()['state'], 'inFlight');
    final Future<void> done = open.done.then<void>(
      (_) {},
      onError: (Object _) {},
    );
    open.abort();
    await done;
    client.close(force: true);
    expect(tracker.log.inFlight, isEmpty);
    expect(tracker.log.since(0).single.state, HttpLogState.aborted);
    expect(tracker.inFlight, 0);
  });

  test('cancelling a response stream releases idle tracking and records '
      'aborted', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    final HttpClient client = HttpClient();
    try {
      final HttpClientRequest req = await client.getUrl(
        base.replace(path: '/stream'),
      );
      final HttpClientResponse res = await req.close();
      expect(tracker.inFlight, 1);

      final Completer<void> gotFirstChunk = Completer<void>();
      final StreamSubscription<List<int>> sub = res.listen((List<int> _) {
        if (!gotFirstChunk.isCompleted) gotFirstChunk.complete();
      });
      await gotFirstChunk.future;
      expect(tracker.inFlight, 1);

      await sub.cancel();
      expect(tracker.inFlight, 0);
      expect(tracker.isIdle, isTrue);
      final HttpLogEntry entry = tracker.log.since(0).single;
      expect(entry.state, HttpLogState.aborted);
      expect(entry.responseBytes, greaterThan(0));
    } finally {
      client.close(force: true);
    }
  });

  test('text/event-stream SSE releases idle immediately while keeping the HTTP '
      'log entry open until stream end', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    final HttpClient client = HttpClient();
    try {
      final HttpClientRequest req = await client.getUrl(
        base.replace(path: '/sse'),
      );
      expect(tracker.inFlight, 1);

      final HttpClientResponse res = await req.close();
      // Idle counter is released immediately once SSE response headers arrive,
      // even though the stream is still open and delivering events.
      expect(tracker.inFlight, 0);
      expect(tracker.isIdle, isTrue);
      // HTTP log entry stays in-flight until the stream actually completes.
      expect(tracker.log.inFlight, hasLength(1));

      final String body = await utf8.decodeStream(res);
      expect(body, contains('data: first'));
      expect(body, contains('data: second'));
      expect(tracker.inFlight, 0);
      expect(tracker.log.inFlight, isEmpty);
      final HttpLogEntry entry = tracker.log.since(0).single;
      expect(entry.state, HttpLogState.completed);
      expect(entry.responseBytes, body.length);
    } finally {
      client.close(force: true);
    }
  });

  test('findProxyFromEnvironment delegates to the wrapped HttpOverrides', () {
    final _ProxyOverrides app = _ProxyOverrides();
    HttpOverrides.global = app;
    final HttpTracker tracker = HttpTracker()..install();
    final Uri target = Uri.parse('https://example.com/api');
    expect(
      HttpOverrides.current!.findProxyFromEnvironment(target, null),
      'PROXY custom.proxy:8080',
    );
    expect(app.proxyCalls, 1);

    // Re-wrapped override added via ensureInstalled also delegates proxy lookup.
    tracker.log.enabled = true;
    final _ProxyOverrides laterApp = _ProxyOverrides();
    HttpOverrides.global = laterApp;
    tracker.ensureInstalled();
    expect(
      HttpOverrides.current!.findProxyFromEnvironment(target, null),
      'PROXY custom.proxy:8080',
    );
    expect(laterApp.proxyCalls, 1);
  });

  test('request.done returns the wrapped response and releases tracking when '
      'consumed', () async {
    final HttpTracker tracker = HttpTracker()..install();
    tracker.log.enabled = true;
    final HttpClient client = HttpClient();
    try {
      final HttpClientRequest req = await client.getUrl(
        base.replace(path: '/ok'),
      );
      expect(tracker.inFlight, 1);

      final Future<HttpClientResponse> doneFuture = req.done;
      final HttpClientResponse closedResponse = await req.close();
      final HttpClientResponse doneResponse = await doneFuture;
      expect(identical(closedResponse, doneResponse), isTrue);

      await doneResponse.drain<void>();
      expect(tracker.inFlight, 0);
      expect(tracker.log.since(0).single.state, HttpLogState.completed);
    } finally {
      client.close(force: true);
    }
  });

  test('waitForIdle unwinds promptly when cancelled', () async {
    final HttpTracker tracker = HttpTracker()..install();
    final HttpClient client = HttpClient();
    // Aborted in finally.
    // ignore: close_sinks
    final HttpClientRequest req = await client.getUrl(
      base.replace(path: '/stream'),
    );
    try {
      expect(tracker.inFlight, 1);
      bool cancelled = false;
      final Timer cancelTimer = Timer(
        const Duration(milliseconds: 40),
        () => cancelled = true,
      );
      final Stopwatch sw = Stopwatch()..start();
      await tracker.waitForIdle(
        const Duration(seconds: 10),
        isCancelled: () => cancelled,
      );
      cancelTimer.cancel();
      expect(sw.elapsed, lessThan(const Duration(seconds: 2)));
    } finally {
      final Future<void> done = req.done.then<void>(
        (_) {},
        onError: (Object _) {},
      );
      req.abort();
      await done;
      client.close(force: true);
    }
  });

  group('HttpRequestLog', () {
    test('ring buffer keeps the newest entries and counts drops', () {
      int clock = 0;
      final HttpRequestLog log = HttpRequestLog(
        maxEntries: 3,
        clockMs: () => clock += 5,
      )..enabled = true;
      for (int i = 0; i < 5; i++) {
        final HttpLogEntry e = log.begin('get', Uri.parse('http://h/$i'))!;
        log.finish(e, HttpLogState.completed);
      }
      final List<HttpLogEntry> kept = log.since(0);
      expect(kept.map((HttpLogEntry e) => e.url), <String>[
        'http://h/2',
        'http://h/3',
        'http://h/4',
      ]);
      expect(log.dropped, 2);
      expect(kept.first.method, 'GET');
      expect(kept.first.durationMs, 5);

      // Delivered entries that are evicted later are not "dropped".
      expect(log.takeUndelivered(), hasLength(3));
      final HttpLogEntry e = log.begin('get', Uri.parse('http://h/5'))!;
      log.finish(e, HttpLogState.completed);
      expect(log.dropped, 2);

      log.reset();
      expect(log.since(0), isEmpty);
      expect(log.dropped, 0);
    });

    test('disabled log hands out no entries; finish is idempotent', () {
      final HttpRequestLog log = HttpRequestLog();
      expect(log.begin('GET', Uri.parse('http://h/')), isNull);
      log.enabled = true;
      final HttpLogEntry e = log.begin('GET', Uri.parse('http://h/'))!;
      log
        ..finish(e, HttpLogState.completed)
        ..finish(e, HttpLogState.failed);
      expect(log.since(0).single.state, HttpLogState.completed);
      expect(log.latestSeq, 1);
    });
  });

  group('HttpRedaction', () {
    test('token detection', () {
      expect(HttpRedaction.looksLikeToken('shoes'), isFalse);
      expect(HttpRedaction.looksLikeToken('12345'), isFalse);
      expect(HttpRedaction.looksLikeToken('products'), isFalse);
      expect(
        HttpRedaction.looksLikeToken('9f86d081884c7d659a2feaa0c55ad015'),
        isTrue,
      );
      expect(
        HttpRedaction.looksLikeToken('550e8400-e29b-41d4-a716-446655440000'),
        isTrue,
      );
      expect(HttpRedaction.looksLikeToken('a@b.cz'), isTrue);
    });

    test('URLs inside header values are redacted', () {
      expect(
        HttpRedaction.headerValue(
          'https://app.example/cb?code=AbC123&state=xyz&page=2',
        ),
        'https://app.example/cb?code=${HttpRedaction.redacted}'
        '&state=xyz&page=2',
      );
      expect(
        HttpRedaction.headerValue('/cb?code=AbC123&next=home'),
        '/cb?code=${HttpRedaction.redacted}&next=home',
      );
      expect(
        HttpRedaction.headerValue(
          '<https://api.example/items?access_token=s3cret&page=2>; '
          'rel="next"',
        ),
        '<https://api.example/items?access_token=${HttpRedaction.redacted}'
        '&page=2>; rel="next"',
      );
      expect(
        HttpRedaction.headerValue('0; url=http://h.example/p?password=pw1'),
        '0; url=http://h.example/p?password=${HttpRedaction.redacted}',
      );
      for (final String plain in <String>[
        '*/*',
        'application/json; charset=utf-8',
        '9f1c2a7e-0b1d-4c55-9d8e-6a1f0c2b3d4e',
        '',
      ]) {
        expect(HttpRedaction.headerValue(plain), plain);
      }
    });

    test('long query values are truncated', () {
      final String long = 'x' * 100;
      expect(
        HttpRedaction.url(Uri.parse('https://h/p?q=$long')),
        'https://h/p?q=${'x' * HttpRedaction.maxQueryValueChars}…',
      );
    });

    test('sensitive header names', () {
      for (final String name in <String>[
        'Authorization',
        'Proxy-Authorization',
        'Cookie',
        'Set-Cookie',
        'X-Api-Key',
        'X-Auth-Token',
        'X-CSRF-Token',
        'x-session-id',
      ]) {
        expect(HttpRedaction.isSensitiveHeader(name), isTrue, reason: name);
      }
      expect(HttpRedaction.isSensitiveHeader('content-type'), isFalse);
      expect(HttpRedaction.isSensitiveHeader('accept'), isFalse);
    });
  });
}
