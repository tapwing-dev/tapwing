import 'dart:async';
import 'dart:convert';
import 'dart:io';

import '../diagnostics/http_log.dart';

/// Tracks in-flight `dart:io` HTTP requests by wrapping [HttpOverrides.global]
/// without altering request or response behavior (read-only observation).
///
/// Requests are counted from connection open until the response stream finishes,
/// errors, detaches (WebSocket/HTTP upgrade), aborts, or hits a safety timeout
/// so abandoned requests can never wedge idleness permanently.
///
/// With [log] enabled (`configure` `httpLog: true`, F8b `--trace`), every
/// request is also recorded with privacy-redacted metadata. [ensureInstalled]
/// re-wraps any `HttpOverrides` the app installed after the binding so both
/// idle counting and (when enabled) request logging stay active without
/// double-counting chaining overrides.
///
/// Known limitation: `HttpTracker` hooks `dart:io`'s `HttpOverrides`. Native
/// FFI/JNI HTTP clients such as `cupertino_http` and `cronet_http` bypass
/// `dart:io` `HttpClient` and are not observed for idle waiting or `httpLog`.
class HttpTracker {
  /// Creates an HTTP request tracker.
  HttpTracker();

  /// Shared singleton installed by [TapwingBindingMixin].
  static final HttpTracker instance = HttpTracker();

  /// Safety timeout if a request is opened and never closed or aborted.
  static const Duration openSafetyTimeout = Duration(seconds: 30);

  /// Safety timeout if a response is returned but nobody drains its body.
  static const Duration responseSafetyTimeout = Duration(seconds: 10);

  static const Duration _cancelPollInterval = Duration(milliseconds: 25);

  int _inFlight = 0;
  bool _creatingClient = false;
  final Set<Completer<void>> _idleWaiters = <Completer<void>>{};

  /// Number of `dart:io` HTTP requests currently in flight.
  int get inFlight => _inFlight;

  /// Whether no tracked HTTP requests are currently in flight.
  bool get isIdle => _inFlight == 0;

  /// Request log for the trace viewer (disabled by default).
  final HttpRequestLog log = HttpRequestLog();

  /// Installs a wrapping [HttpOverrides] around the current global override.
  void install() {
    HttpOverrides.global = _TrackingHttpOverrides(HttpOverrides.current, this);
  }

  /// Makes sure a Tapwing layer is outermost: an app that set
  /// `HttpOverrides.global` after the binding (crash reporters, certificate
  /// pinning, proxies) gets wrapped so idle counting (and HTTP logging when
  /// enabled) stays active while delegating to the app's override. Called at
  /// first frame and every RPC boundary; an identity check when nothing changed.
  void ensureInstalled() {
    final HttpOverrides? current = HttpOverrides.current;
    if (current is _TrackingHttpOverrides &&
        identical(current._tracker, this)) {
      return;
    }
    HttpOverrides.global = _TrackingHttpOverrides(current, this);
  }

  /// Waits until [inFlight] drops to zero, [maxWait] elapses, or [isCancelled]
  /// returns `true`.
  ///
  /// Tracks each waiter individually so timed-out or cancelled waits remove
  /// themselves cleanly without accumulating `.then()` chains.
  Future<void> waitForIdle(Duration maxWait, {bool Function()? isCancelled}) {
    if (_inFlight == 0 ||
        maxWait <= Duration.zero ||
        (isCancelled != null && isCancelled())) {
      return Future<void>.value();
    }
    final Completer<void> waiter = Completer<void>();
    _idleWaiters.add(waiter);
    Timer? cancelPoll;
    late final Timer timer;
    void cleanup() {
      _idleWaiters.remove(waiter);
      timer.cancel();
      cancelPoll?.cancel();
    }

    timer = Timer(maxWait, () {
      cleanup();
      if (!waiter.isCompleted) waiter.complete();
    });
    if (isCancelled != null) {
      cancelPoll = Timer.periodic(_cancelPollInterval, (_) {
        if (isCancelled()) {
          cleanup();
          if (!waiter.isCompleted) waiter.complete();
        }
      });
    }
    return waiter.future.whenComplete(cleanup);
  }

  void _onRequestOpened() {
    _inFlight++;
  }

  void _onRequestFinished() {
    _inFlight--;
    if (_inFlight <= 0) {
      _inFlight = 0;
      if (_idleWaiters.isNotEmpty) {
        final List<Completer<void>> waiters = _idleWaiters.toList(
          growable: false,
        );
        _idleWaiters.clear();
        for (final Completer<void> waiter in waiters) {
          if (!waiter.isCompleted) {
            waiter.complete();
          }
        }
      }
    }
  }
}

class _TrackingHttpOverrides extends HttpOverrides {
  _TrackingHttpOverrides(this._previous, this._tracker);

  final HttpOverrides? _previous;
  final HttpTracker _tracker;

  @override
  HttpClient createHttpClient(SecurityContext? context) {
    if (_tracker._creatingClient) {
      // An outer _TrackingHttpOverrides for the same tracker is already
      // wrapping this client creation chain (e.g. a chaining app override).
      return _previous?.createHttpClient(context) ??
          super.createHttpClient(context);
    }
    _tracker._creatingClient = true;
    final HttpClient inner;
    try {
      inner =
          _previous?.createHttpClient(context) ??
          super.createHttpClient(context);
    } finally {
      _tracker._creatingClient = false;
    }
    if (inner is _TrackingHttpClient && identical(inner._tracker, _tracker)) {
      return inner;
    }
    return _TrackingHttpClient(inner, _tracker);
  }

  @override
  String findProxyFromEnvironment(Uri url, Map<String, String>? environment) =>
      _previous?.findProxyFromEnvironment(url, environment) ??
      super.findProxyFromEnvironment(url, environment);
}

/// Per-request bookkeeping: the idle counter release and the log entry.
class _RequestTracking {
  _RequestTracking(this._tracker, this.entry);

  final HttpTracker _tracker;

  /// Log entry, or `null` when the request log is disabled.
  final HttpLogEntry? entry;

  bool _idleReleased = false;
  bool _logFinished = false;
  Timer? _safety;

  /// Bytes the app added to the request body.
  int requestBytes = 0;

  /// Response body bytes the app read.
  int responseBytes = 0;

  void armTimeout(Duration duration) {
    if (!_idleReleased || !_logFinished) {
      _safety?.cancel();
      _safety = Timer(duration, () => release(HttpLogState.incomplete));
    }
  }

  void cancelTimeout() {
    _safety?.cancel();
    _safety = null;
  }

  /// Releases idle counting immediately (e.g. for `text/event-stream` SSE
  /// streams) while keeping the HTTP log entry open until the stream ends or
  /// cancels.
  void releaseIdle() {
    if (!_idleReleased) {
      _idleReleased = true;
      _tracker._onRequestFinished();
    }
    if (entry == null) {
      _logFinished = true;
      cancelTimeout();
    }
  }

  void release(HttpLogState state, {Object? error}) {
    if (!_idleReleased) {
      _idleReleased = true;
      _tracker._onRequestFinished();
    }
    if (_logFinished) {
      return;
    }
    _logFinished = true;
    cancelTimeout();
    final HttpLogEntry? e = entry;
    if (e != null) {
      if (responseBytes > 0) {
        e.responseBytes = responseBytes;
      }
      _tracker.log.finish(e, state, error: error);
    }
  }
}

/// Delegating [HttpClient] that tracks every opened request through completion.
class _TrackingHttpClient implements HttpClient {
  _TrackingHttpClient(this._inner, this._tracker);

  final HttpClient _inner;
  final HttpTracker _tracker;

  Future<HttpClientRequest> _track(
    String method,
    Uri? url,
    Future<HttpClientRequest> pending,
  ) async {
    final HttpLogEntry? entry = _tracker.log.begin(method, url);
    _tracker._onRequestOpened();
    final _RequestTracking tracking = _RequestTracking(_tracker, entry);
    // Safety net in case a request is opened and abandoned without close()/abort().
    tracking.armTimeout(HttpTracker.openSafetyTimeout);
    try {
      // The app owns (and closes) the request; it is only wrapped here.
      // ignore: close_sinks
      final HttpClientRequest request = await pending;
      if (entry != null) {
        entry.url = HttpRedaction.url(request.uri);
      }
      return _TrackingRequest(request, tracking);
    } catch (error) {
      tracking.release(HttpLogState.failed, error: error);
      rethrow;
    }
  }

  static Uri? _uri(String host, int port, String path) {
    try {
      return Uri.parse('http://$host:$port$path');
    } on FormatException {
      return null;
    }
  }

  @override
  Future<HttpClientRequest> open(
    String method,
    String host,
    int port,
    String path,
  ) => _track(
    method,
    _uri(host, port, path),
    _inner.open(method, host, port, path),
  );
  @override
  Future<HttpClientRequest> openUrl(String method, Uri url) =>
      _track(method, url, _inner.openUrl(method, url));
  @override
  Future<HttpClientRequest> get(String host, int port, String path) =>
      _track('GET', _uri(host, port, path), _inner.get(host, port, path));
  @override
  Future<HttpClientRequest> getUrl(Uri url) =>
      _track('GET', url, _inner.getUrl(url));
  @override
  Future<HttpClientRequest> post(String host, int port, String path) =>
      _track('POST', _uri(host, port, path), _inner.post(host, port, path));
  @override
  Future<HttpClientRequest> postUrl(Uri url) =>
      _track('POST', url, _inner.postUrl(url));
  @override
  Future<HttpClientRequest> put(String host, int port, String path) =>
      _track('PUT', _uri(host, port, path), _inner.put(host, port, path));
  @override
  Future<HttpClientRequest> putUrl(Uri url) =>
      _track('PUT', url, _inner.putUrl(url));
  @override
  Future<HttpClientRequest> delete(String host, int port, String path) =>
      _track('DELETE', _uri(host, port, path), _inner.delete(host, port, path));
  @override
  Future<HttpClientRequest> deleteUrl(Uri url) =>
      _track('DELETE', url, _inner.deleteUrl(url));
  @override
  Future<HttpClientRequest> patch(String host, int port, String path) =>
      _track('PATCH', _uri(host, port, path), _inner.patch(host, port, path));
  @override
  Future<HttpClientRequest> patchUrl(Uri url) =>
      _track('PATCH', url, _inner.patchUrl(url));
  @override
  Future<HttpClientRequest> head(String host, int port, String path) =>
      _track('HEAD', _uri(host, port, path), _inner.head(host, port, path));
  @override
  Future<HttpClientRequest> headUrl(Uri url) =>
      _track('HEAD', url, _inner.headUrl(url));

  @override
  bool get autoUncompress => _inner.autoUncompress;
  @override
  set autoUncompress(bool value) => _inner.autoUncompress = value;
  @override
  Duration? get connectionTimeout => _inner.connectionTimeout;
  @override
  set connectionTimeout(Duration? value) => _inner.connectionTimeout = value;
  @override
  Duration get idleTimeout => _inner.idleTimeout;
  @override
  set idleTimeout(Duration value) => _inner.idleTimeout = value;
  @override
  int? get maxConnectionsPerHost => _inner.maxConnectionsPerHost;
  @override
  set maxConnectionsPerHost(int? value) => _inner.maxConnectionsPerHost = value;
  @override
  String? get userAgent => _inner.userAgent;
  @override
  set userAgent(String? value) => _inner.userAgent = value;
  @override
  void addCredentials(
    Uri url,
    String realm,
    HttpClientCredentials credentials,
  ) => _inner.addCredentials(url, realm, credentials);
  @override
  void addProxyCredentials(
    String host,
    int port,
    String realm,
    HttpClientCredentials credentials,
  ) => _inner.addProxyCredentials(host, port, realm, credentials);
  @override
  set authenticate(
    Future<bool> Function(Uri url, String scheme, String? realm)? f,
  ) => _inner.authenticate = f;
  @override
  set authenticateProxy(
    Future<bool> Function(String host, int port, String scheme, String? realm)?
    f,
  ) => _inner.authenticateProxy = f;
  @override
  set badCertificateCallback(
    bool Function(X509Certificate cert, String host, int port)? callback,
  ) => _inner.badCertificateCallback = callback;
  @override
  set connectionFactory(
    Future<ConnectionTask<Socket>> Function(
      Uri url,
      String? proxyHost,
      int? proxyPort,
    )?
    f,
  ) => _inner.connectionFactory = f;
  @override
  set findProxy(String Function(Uri url)? f) => _inner.findProxy = f;
  @override
  set keyLog(void Function(String line)? callback) => _inner.keyLog = callback;
  @override
  void close({bool force = false}) => _inner.close(force: force);
}

class _TrackingRequest implements HttpClientRequest {
  _TrackingRequest(this._inner, this._tracking);

  final HttpClientRequest _inner;
  final _RequestTracking _tracking;

  Future<HttpClientResponse>? _doneFuture;
  _TrackingResponse? _wrappedResponse;
  bool _requestMetaRecorded = false;

  static bool _isEventStream(HttpClientResponse response) {
    final String? mime = response.headers.contentType?.mimeType;
    if (mime != null) {
      return mime.toLowerCase() == 'text/event-stream';
    }
    final String? raw = response.headers.value(HttpHeaders.contentTypeHeader);
    return raw != null &&
        raw.split(';').first.trim().toLowerCase() == 'text/event-stream';
  }

  void _recordRequestMeta() {
    if (_requestMetaRecorded) return;
    _requestMetaRecorded = true;
    final HttpLogEntry? entry = _tracking.entry;
    if (entry != null) {
      // Read-only: headers and the declared length are not modified.
      entry.requestHeaders = HttpRedaction.headers(_inner.headers);
      final int declared = _inner.contentLength;
      final int sent = _tracking.requestBytes;
      entry.requestBytes = declared >= 0 ? declared : (sent > 0 ? sent : null);
    }
  }

  _TrackingResponse _wrapResponse(HttpClientResponse response) {
    final _TrackingResponse? existing = _wrappedResponse;
    if (existing != null) return existing;
    _recordRequestMeta();
    final HttpLogEntry? entry = _tracking.entry;
    if (entry != null) {
      final HttpTracker tracker = _tracking._tracker;
      entry
        ..status = response.statusCode
        ..headersMs = tracker.log.now() - entry.startMs
        ..redirects = response.redirects.length
        ..responseHeaders = HttpRedaction.headers(response.headers);
      if (response.contentLength >= 0) {
        entry.responseBytes = response.contentLength;
      }
    }
    if (_isEventStream(response)) {
      // Long-lived SSE streams must not block idleness, while the log entry
      // keeps recording bytes until stream end or cancellation.
      _tracking
        ..cancelTimeout()
        ..releaseIdle();
    } else {
      // Released when the response stream finishes; arm a shorter safety
      // timeout so an unread response cannot block idleness forever.
      _tracking.armTimeout(HttpTracker.responseSafetyTimeout);
    }
    return _wrappedResponse = _TrackingResponse(response, _tracking);
  }

  @override
  Future<HttpClientResponse> close() async {
    _recordRequestMeta();
    try {
      final HttpClientResponse response = await _inner.close();
      return _wrapResponse(response);
    } catch (error) {
      _tracking.release(HttpLogState.failed, error: error);
      rethrow;
    }
  }

  @override
  Future<HttpClientResponse> get done => _doneFuture ??= _inner.done.then(
    _wrapResponse,
    onError: (Object error, StackTrace stackTrace) {
      _tracking.release(HttpLogState.failed, error: error);
      Error.throwWithStackTrace(error, stackTrace);
    },
  );

  @override
  bool get bufferOutput => _inner.bufferOutput;
  @override
  set bufferOutput(bool value) => _inner.bufferOutput = value;
  @override
  int get contentLength => _inner.contentLength;
  @override
  set contentLength(int value) => _inner.contentLength = value;
  @override
  bool get followRedirects => _inner.followRedirects;
  @override
  set followRedirects(bool value) => _inner.followRedirects = value;
  @override
  int get maxRedirects => _inner.maxRedirects;
  @override
  set maxRedirects(int value) => _inner.maxRedirects = value;
  @override
  bool get persistentConnection => _inner.persistentConnection;
  @override
  set persistentConnection(bool value) => _inner.persistentConnection = value;
  @override
  HttpConnectionInfo? get connectionInfo => _inner.connectionInfo;
  @override
  List<Cookie> get cookies => _inner.cookies;
  @override
  HttpHeaders get headers => _inner.headers;
  @override
  String get method => _inner.method;
  @override
  Uri get uri => _inner.uri;
  @override
  Encoding get encoding => _inner.encoding;
  @override
  set encoding(Encoding value) => _inner.encoding = value;

  @override
  void abort([Object? exception, StackTrace? stackTrace]) {
    _tracking.release(HttpLogState.aborted, error: exception);
    _inner.abort(exception, stackTrace);
  }

  /// Counts written text toward the logged request size. Never throws: an
  /// unencodable string fails in the app's own `write` call exactly as before.
  void _countText(String text) {
    if (_tracking.entry == null) {
      return;
    }
    try {
      _tracking.requestBytes += _inner.encoding.encode(text).length;
    } on Object {
      // Left to the inner request to report.
    }
  }

  @override
  void add(List<int> data) {
    if (_tracking.entry != null) {
      _tracking.requestBytes += data.length;
    }
    _inner.add(data);
  }

  @override
  void addError(Object error, [StackTrace? stackTrace]) =>
      _inner.addError(error, stackTrace);
  @override
  Future<void> addStream(Stream<List<int>> stream) => _inner.addStream(stream);
  @override
  Future<void> flush() => _inner.flush();
  @override
  void write(Object? object) {
    _countText('$object');
    _inner.write(object);
  }

  @override
  void writeAll(Iterable<Object?> objects, [String separator = '']) =>
      _inner.writeAll(objects, separator);
  @override
  void writeCharCode(int charCode) => _inner.writeCharCode(charCode);
  @override
  void writeln([Object? object = '']) {
    _countText('$object\n');
    _inner.writeln(object);
  }
}

class _TrackingResponse extends StreamView<List<int>>
    implements HttpClientResponse {
  _TrackingResponse(this._inner, this._tracking)
    : super(_inner.transform(_transformer(_tracking)));

  static StreamTransformer<List<int>, List<int>> _transformer(
    _RequestTracking tracking,
  ) {
    void done(EventSink<List<int>> sink) {
      tracking.release(HttpLogState.completed);
      sink.close();
    }

    void error(Object error, StackTrace stackTrace, EventSink<List<int>> sink) {
      tracking.release(HttpLogState.failed, error: error);
      sink.addError(error, stackTrace);
    }

    if (tracking.entry == null) {
      return StreamTransformer<List<int>, List<int>>.fromHandlers(
        handleDone: done,
        handleError: error,
      );
    }
    return StreamTransformer<List<int>, List<int>>.fromHandlers(
      handleData: (List<int> data, EventSink<List<int>> sink) {
        tracking.responseBytes += data.length;
        sink.add(data);
      },
      handleDone: done,
      handleError: error,
    );
  }

  final HttpClientResponse _inner;
  final _RequestTracking _tracking;

  @override
  StreamSubscription<List<int>> listen(
    void Function(List<int> event)? onData, {
    Function? onError,
    void Function()? onDone,
    bool? cancelOnError,
  }) => _CancelTrackingSubscription(
    super.listen(
      onData,
      onError: onError,
      onDone: onDone,
      cancelOnError: cancelOnError,
    ),
    _tracking,
  );

  @override
  X509Certificate? get certificate => _inner.certificate;
  @override
  HttpClientResponseCompressionState get compressionState =>
      _inner.compressionState;
  @override
  HttpConnectionInfo? get connectionInfo => _inner.connectionInfo;
  @override
  int get contentLength => _inner.contentLength;
  @override
  List<Cookie> get cookies => _inner.cookies;

  @override
  Future<Socket> detachSocket() {
    // WebSocket / HTTP upgrade: no HTTP response body will follow.
    _tracking.release(HttpLogState.upgraded);
    return _inner.detachSocket();
  }

  @override
  HttpHeaders get headers => _inner.headers;
  @override
  bool get isRedirect => _inner.isRedirect;
  @override
  bool get persistentConnection => _inner.persistentConnection;
  @override
  String get reasonPhrase => _inner.reasonPhrase;
  @override
  Future<HttpClientResponse> redirect([
    String? method,
    Uri? url,
    bool? followLoops,
  ]) {
    _tracking.release(HttpLogState.completed);
    return _inner.redirect(method, url, followLoops);
  }

  @override
  List<RedirectInfo> get redirects => _inner.redirects;
  @override
  int get statusCode => _inner.statusCode;
}

class _CancelTrackingSubscription implements StreamSubscription<List<int>> {
  _CancelTrackingSubscription(this._inner, this._tracking);

  final StreamSubscription<List<int>> _inner;
  final _RequestTracking _tracking;

  @override
  Future<void> cancel() {
    _tracking.release(HttpLogState.aborted);
    return _inner.cancel();
  }

  @override
  void onData(void Function(List<int> data)? handleData) =>
      _inner.onData(handleData);

  @override
  void onError(Function? handleError) => _inner.onError(handleError);

  @override
  void onDone(void Function()? handleDone) => _inner.onDone(handleDone);

  @override
  void pause([Future<void>? resumeSignal]) => _inner.pause(resumeSignal);

  @override
  void resume() => _inner.resume();

  @override
  bool get isPaused => _inner.isPaused;

  @override
  Future<E> asFuture<E>([E? futureValue]) => _inner.asFuture<E>(futureValue);
}
