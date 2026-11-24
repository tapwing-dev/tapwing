import 'dart:collection';
import 'dart:io';

/// Final state of a logged HTTP request (F8b trace viewer).
enum HttpLogState {
  /// Still open (only reported by the `httpLog` re-read at flow end).
  inFlight,

  /// The response body finished.
  completed,

  /// Connecting, sending, or reading the response failed.
  failed,

  /// The app aborted the request.
  aborted,

  /// The connection was detached for a WebSocket / HTTP upgrade.
  upgraded,

  /// Released by the safety timeout: opened but never closed, or a response
  /// body nobody drained.
  incomplete,
}

/// One HTTP request the app made, with privacy-redacted metadata only: no
/// request or response bodies, redacted URLs, and redacted sensitive headers.
class HttpLogEntry {
  HttpLogEntry._({
    required this.method,
    required this.url,
    required this.request,
    required this.startMs,
  });

  /// Upper-case HTTP method.
  final String method;

  /// Redacted URL (see [HttpRedaction.url]).
  String url;

  /// Id of the RPC request in flight when this request started (or of the
  /// last completed one between requests). The host maps it to a flow step.
  final Object? request;

  /// Milliseconds since driver start when the request was opened.
  final int startMs;

  /// Sequence number assigned when the entry finished (0 while in flight).
  int seq = 0;

  /// Current state.
  HttpLogState state = HttpLogState.inFlight;

  /// HTTP status code, once response headers arrived.
  int? status;

  /// Milliseconds from open until response headers arrived.
  int? headersMs;

  /// Milliseconds from open until the entry finished.
  int? durationMs;

  /// Request body size (declared `Content-Length`, else bytes added).
  int? requestBytes;

  /// Response body bytes read by the app (or declared length when unread).
  int? responseBytes;

  /// Sanitized error (type and first line, query strings stripped).
  String? error;

  /// Redacted request headers (lower-case names).
  Map<String, String>? requestHeaders;

  /// Redacted response headers (lower-case names).
  Map<String, String>? responseHeaders;

  /// Number of redirects followed.
  int redirects = 0;

  /// Serializes to the wire shape.
  Map<String, Object?> toJson() => <String, Object?>{
    if (seq > 0) 'seq': seq,
    'method': method,
    'url': url,
    'state': state.name,
    if (status != null) 'status': status,
    'startMs': startMs,
    if (headersMs != null) 'headersMs': headersMs,
    if (durationMs != null) 'durationMs': durationMs,
    if (requestBytes != null) 'requestBytes': requestBytes,
    if (responseBytes != null) 'responseBytes': responseBytes,
    if (error != null) 'error': error,
    if (redirects > 0) 'redirects': redirects,
    if (requestHeaders != null && requestHeaders!.isNotEmpty)
      'requestHeaders': requestHeaders,
    if (responseHeaders != null && responseHeaders!.isNotEmpty)
      'responseHeaders': responseHeaders,
    'request': request,
  };
}

/// Bounded log of [HttpLogEntry]s with per-request attribution and
/// at-least-once delivery to the host CLI (F8b). Disabled by default: only
/// `configure` `httpLog: true` (sent by `tapwing test --trace`) enables it.
class HttpRequestLog {
  /// Creates an empty, disabled log.
  HttpRequestLog({this.maxEntries = defaultMaxEntries, int Function()? clockMs})
    : _clockMs = clockMs ?? _defaultClock;

  /// Default ring buffer capacity (finished entries).
  static const int defaultMaxEntries = 500;

  /// Maximum number of requests tracked as in flight at once; more are not
  /// logged (counted in [dropped]).
  static const int maxInFlight = 200;

  static final Stopwatch _clock = Stopwatch()..start();
  static int _defaultClock() => _clock.elapsedMilliseconds;

  /// Ring buffer capacity; the oldest finished entries are dropped past it.
  final int maxEntries;
  final int Function() _clockMs;

  /// Whether requests are logged.
  bool enabled = false;

  /// Request id attributed to new entries (set by the RPC session).
  Object? currentRequest;

  final ListQueue<HttpLogEntry> _finished = ListQueue<HttpLogEntry>();
  final LinkedHashSet<HttpLogEntry> _inFlight = LinkedHashSet<HttpLogEntry>();
  int _seq = 0;
  int _deliveredSeq = 0;

  /// Entries evicted or never logged before delivery.
  int dropped = 0;

  /// Latest assigned sequence number.
  int get latestSeq => _seq;

  /// Milliseconds since driver start.
  int now() => _clockMs();

  /// Requests currently in flight, oldest first.
  List<HttpLogEntry> get inFlight => _inFlight.toList(growable: false);

  /// Starts an entry, or returns `null` when the log is disabled or full.
  HttpLogEntry? begin(String method, Uri? url) {
    if (!enabled) {
      return null;
    }
    if (_inFlight.length >= maxInFlight) {
      dropped++;
      return null;
    }
    final HttpLogEntry entry = HttpLogEntry._(
      method: method.toUpperCase(),
      url: url == null ? '' : HttpRedaction.url(url),
      request: currentRequest,
      startMs: _clockMs(),
    );
    _inFlight.add(entry);
    return entry;
  }

  /// Finishes [entry] with [state]; later calls for the same entry are ignored.
  void finish(HttpLogEntry entry, HttpLogState state, {Object? error}) {
    if (!_inFlight.remove(entry)) {
      return;
    }
    entry
      ..state = state
      ..durationMs = _clockMs() - entry.startMs
      ..seq = ++_seq;
    if (error != null) {
      entry.error = HttpRedaction.error(error);
    }
    _finished.add(entry);
    while (_finished.length > maxEntries) {
      final HttpLogEntry evicted = _finished.removeFirst();
      if (evicted.seq > _deliveredSeq) {
        dropped++;
      }
    }
  }

  /// Finished entries not yet attached to a response; marks them delivered.
  List<HttpLogEntry> takeUndelivered() {
    if (!enabled || _seq == _deliveredSeq) {
      return const <HttpLogEntry>[];
    }
    final List<HttpLogEntry> out = since(_deliveredSeq);
    _deliveredSeq = _seq;
    return out;
  }

  /// Finished entries with a sequence number greater than [seq].
  List<HttpLogEntry> since(int seq) => <HttpLogEntry>[
    for (final HttpLogEntry e in _finished)
      if (e.seq > seq) e,
  ];

  /// Clears all state (tests and `configure` `httpLog: false`).
  void reset() {
    _finished.clear();
    _inFlight.clear();
    _seq = 0;
    _deliveredSeq = 0;
    dropped = 0;
    currentRequest = null;
  }
}

/// Privacy redaction for the HTTP request log. Applied before anything is
/// stored, so secrets never leave the app process.
abstract final class HttpRedaction {
  /// Replacement for redacted values.
  static const String redacted = '<redacted>';

  /// Maximum kept length of a non-sensitive query value.
  static const int maxQueryValueChars = 64;

  /// Maximum length of a stored URL.
  static const int maxUrlChars = 512;

  /// Maximum number of headers kept per direction.
  static const int maxHeaders = 24;

  /// Maximum length of a header value.
  static const int maxHeaderValueChars = 200;

  /// Maximum length of an error message.
  static const int maxErrorChars = 300;

  static const Set<String> _sensitiveHeaders = <String>{
    'authorization',
    'proxy-authorization',
    'cookie',
    'set-cookie',
    'x-api-key',
  };

  static const List<String> _sensitiveHeaderParts = <String>[
    'token',
    'secret',
    'auth',
    'session',
    'password',
    'passwd',
    'signature',
    'csrf',
    'xsrf',
    'key',
    'credential',
  ];

  static const List<String> _sensitiveQueryParts = <String>[
    'token',
    'secret',
    'auth',
    'session',
    'password',
    'passwd',
    'pwd',
    'pass',
    'sig',
    'key',
    'code',
    'credential',
    'otp',
    'jwt',
    'nonce',
    'ticket',
    'hash',
    'email',
    'phone',
  ];

  static final RegExp _jwt = RegExp(
    r'^[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}$',
  );
  static final RegExp _tokenChars = RegExp(r'^[A-Za-z0-9+/=_\-.~%:]+$');
  static final RegExp _hasDigit = RegExp(r'[0-9]');
  static final RegExp _hasLetter = RegExp(r'[A-Za-z]');
  static final RegExp _queryInText = RegExp(r'\?[^\s,)\]]*');
  static final RegExp _urlInText = RegExp(r'https?://[^\s<>"]+');
  static final RegExp _whitespace = RegExp(r'\s');

  /// Whether a header named [name] carries a secret.
  static bool isSensitiveHeader(String name) {
    final String n = name.toLowerCase();
    return _sensitiveHeaders.contains(n) ||
        _sensitiveHeaderParts.any(n.contains);
  }

  /// Whether a query parameter named [name] carries a secret or PII.
  static bool isSensitiveQueryKey(String name) {
    final String n = name.toLowerCase();
    return _sensitiveQueryParts.any(n.contains);
  }

  /// Whether [value] looks like a token (JWT, long mixed alphanumeric run),
  /// or contains an e-mail address.
  static bool looksLikeToken(String value) {
    if (value.contains('@')) {
      return true;
    }
    if (_jwt.hasMatch(value)) {
      return true;
    }
    return value.length >= 24 &&
        _tokenChars.hasMatch(value) &&
        _hasDigit.hasMatch(value) &&
        _hasLetter.hasMatch(value);
  }

  /// Scheme, host, port, and path (token-like segments redacted), plus the
  /// query with sensitive or token-like values redacted. User info and the
  /// fragment are dropped.
  static String url(Uri uri) {
    final StringBuffer out = StringBuffer();
    if (uri.hasScheme) {
      out.write('${uri.scheme}://');
    }
    out.write(uri.host);
    if (uri.hasPort) {
      out.write(':${uri.port}');
    }
    final String path = uri.path
        .split('/')
        .map((String s) {
          final String decoded = _decode(s);
          return looksLikeToken(decoded) ? redacted : s;
        })
        .join('/');
    out.write(path);
    if (uri.hasQuery && uri.query.isNotEmpty) {
      final List<String> parts = <String>[];
      for (final String pair in uri.query.split('&')) {
        if (pair.isEmpty) {
          continue;
        }
        final int eq = pair.indexOf('=');
        final String key = eq < 0 ? pair : pair.substring(0, eq);
        if (eq < 0) {
          parts.add(key);
          continue;
        }
        final String raw = pair.substring(eq + 1);
        final String value = _decode(raw);
        if (isSensitiveQueryKey(_decode(key)) || looksLikeToken(value)) {
          parts.add('$key=$redacted');
        } else if (raw.length > maxQueryValueChars) {
          parts.add('$key=${raw.substring(0, maxQueryValueChars)}…');
        } else {
          parts.add('$key=$raw');
        }
      }
      if (parts.isNotEmpty) {
        out.write('?${parts.join('&')}');
      }
    }
    final String s = out.toString();
    return s.length <= maxUrlChars ? s : '${s.substring(0, maxUrlChars)}…';
  }

  /// Up to [maxHeaders] headers with sensitive values redacted.
  static Map<String, String> headers(HttpHeaders headers) {
    final Map<String, String> out = <String, String>{};
    headers.forEach((String name, List<String> values) {
      if (out.length >= maxHeaders) {
        return;
      }
      final String n = name.toLowerCase();
      if (isSensitiveHeader(n)) {
        out[n] = redacted;
        return;
      }
      final String v = headerValue(values.join(', '));
      out[n] = v.length <= maxHeaderValueChars
          ? v
          : '${v.substring(0, maxHeaderValueChars)}…';
    });
    return out;
  }

  /// [value] with the URLs it carries redacted like [url]: a value that is
  /// itself a relative reference (`Location: /cb?code=…`) and every absolute
  /// `http(s)://` URL inside it (`Referer`, `Link`, `Refresh`). An OAuth
  /// redirect's `code` or a reset link's `token` would otherwise be logged
  /// verbatim.
  static String headerValue(String value) {
    if (value.startsWith('/') && !_whitespace.hasMatch(value)) {
      return _urlText(value);
    }
    return value.replaceAllMapped(
      _urlInText,
      (Match m) => _urlText(m.group(0)!),
    );
  }

  static String _urlText(String text) {
    final Uri? uri = Uri.tryParse(text);
    return uri == null ? redacted : url(uri);
  }

  /// Error type and first message line, query strings stripped.
  static String error(Object error) {
    String text = error.toString().trim();
    final int nl = text.indexOf('\n');
    if (nl >= 0) {
      text = text.substring(0, nl).trim();
    }
    text = text.replaceAll(_queryInText, '?…');
    final String type = error.runtimeType.toString();
    if (!text.startsWith(type)) {
      text = '$type: $text';
    }
    return text.length <= maxErrorChars
        ? text
        : '${text.substring(0, maxErrorChars)}…';
  }

  static String _decode(String s) {
    try {
      return Uri.decodeQueryComponent(s);
    } on Object {
      return s;
    }
  }
}
