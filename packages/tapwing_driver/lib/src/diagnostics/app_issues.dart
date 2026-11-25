import 'package:flutter/foundation.dart';

/// Kind of app health issue captured while a flow runs (F7).
///
/// `appCrash` is detected by the host CLI (the process dies with the driver),
/// so it has no driver-side kind.
enum AppIssueKind {
  /// Layout overflow (`A RenderFlex overflowed by …`).
  layoutOverflow,

  /// Exception thrown while building a widget (renders `ErrorWidget`).
  buildError,

  /// Exception thrown during layout.
  layoutError,

  /// Exception thrown during paint.
  paintError,

  /// Image failed to load or decode.
  imageLoadError,

  /// Any other Flutter framework error or assertion
  /// (`setState() called after dispose()`, gesture errors, …).
  frameworkAssertion,

  /// Uncaught asynchronous Dart error (`PlatformDispatcher.onError`).
  uncaughtError,

  /// Non-blocking test setup warning (for example, Flutter's `Zone mismatch.`
  /// when `TapwingBinding.ensureInitialized()` runs outside the app's zone).
  setupWarning,
}

/// One deduplicated app issue as sent to the host CLI.
class AppIssue {
  /// Creates an issue record.
  AppIssue({
    required this.seq,
    required this.kind,
    required this.message,
    required this.library,
    required this.summary,
    required this.stack,
    required this.request,
    required this.firstSeenMs,
  }) : id = seq,
       lastSeenMs = firstSeenMs;

  /// Stable id (the first sequence number). A repeat is re-sent with the
  /// same [id] and an updated [count]; the host keeps the latest copy.
  final int id;

  /// Latest sequence number. A deduplicated repeat bumps it so the issue is
  /// delivered again with its updated [count].
  int seq;

  /// Issue classification.
  final AppIssueKind kind;

  /// First line of the error (stable across repeats; used in fingerprints).
  final String message;

  /// Flutter `FlutterErrorDetails.library` (`rendering library`, …), if any.
  final String? library;

  /// Trimmed multi-line description for reports.
  final String summary;

  /// Top stack frames, app frames first (empty when unavailable).
  final List<String> stack;

  /// Id of the RPC request that was in flight when the issue fired, or of
  /// the last completed request when it fired between requests. The host
  /// maps it to the flow step that caused the issue.
  final Object? request;

  /// Milliseconds since driver start when the issue first fired.
  final int firstSeenMs;

  /// Milliseconds since driver start of the latest repeat.
  int lastSeenMs;

  /// Number of times this issue fired for the same [request].
  int count = 1;

  /// Deduplication key: kind, message, and first app frame.
  String get fingerprint =>
      '${kind.name}|$message|${stack.isEmpty ? '' : stack.first}';

  /// Serializes to the wire shape.
  Map<String, Object?> toJson() => <String, Object?>{
    'id': id,
    'seq': seq,
    'kind': kind.name,
    'message': message,
    if (library != null) 'library': library,
    'summary': summary,
    if (stack.isNotEmpty) 'stack': stack,
    'request': request,
    'count': count,
    'firstSeenMs': firstSeenMs,
    'lastSeenMs': lastSeenMs,
  };
}

/// Bounded, deduplicating log of [AppIssue]s with per-request attribution
/// and at-least-once delivery to the host CLI.
///
/// Issues are delivered by piggybacking on RPC responses ([takeUndelivered])
/// and can be re-read by sequence number after a reconnect ([since]).
class AppIssueLog {
  /// Creates an empty log.
  AppIssueLog({this.maxEntries = defaultMaxEntries, int Function()? clockMs})
    : _clockMs = clockMs ?? _defaultClock;

  /// Shared singleton used by the binding, error hooks, and RPC session.
  static final AppIssueLog instance = AppIssueLog();

  /// Default ring buffer capacity.
  static const int defaultMaxEntries = 500;

  /// Maximum length of [AppIssue.summary].
  static const int maxSummaryChars = 2000;

  /// Maximum number of stack frames kept per issue.
  static const int maxStackFrames = 8;

  static final Stopwatch _clock = Stopwatch()..start();
  static int _defaultClock() => _clock.elapsedMilliseconds;

  /// Ring buffer capacity; the oldest issues are dropped past it.
  final int maxEntries;
  final int Function() _clockMs;

  /// Insertion-ordered map (`LinkedHashMap`) keyed by dedup key, kept strictly
  /// in ascending [AppIssue.seq] order so both repeat promotion and oldest-entry
  /// eviction run in O(1) under high-rate error floods.
  final Map<String, AppIssue> _byKey = <String, AppIssue>{};
  int _seq = 0;
  int _deliveredSeq = 0;

  /// Number of issues evicted from the ring buffer before delivery.
  int dropped = 0;

  /// Request id attributed to new issues (set by the RPC session when a
  /// request starts; kept after it completes).
  Object? currentRequest;

  /// Whether issues are attached to RPC responses (enabled by the host via
  /// `configure` `appIssues: true`; older hosts never see the field).
  bool deliveryEnabled = false;

  /// Latest assigned sequence number.
  int get latestSeq => _seq;

  /// Records an issue, merging it into an existing entry with the same
  /// fingerprint for the same [currentRequest].
  AppIssue record({
    required AppIssueKind kind,
    required String message,
    String? library,
    String? summary,
    StackTrace? stack,
  }) {
    final List<String> frames = trimStack(stack);
    final String firstLine = _firstLine(message);
    final String key =
        '${currentRequest ?? ''}#${kind.name}|$firstLine|${frames.isEmpty ? '' : frames.first}';
    final int now = _clockMs();
    final AppIssue? existing = _byKey[key];
    if (existing != null) {
      existing
        ..count += 1
        ..lastSeenMs = now
        ..seq = ++_seq;
      // Keep ascending sequence order in the map for `since` / `takeUndelivered`.
      _byKey.remove(key);
      _byKey[key] = existing;
      return existing;
    }
    final AppIssue issue = AppIssue(
      seq: ++_seq,
      kind: kind,
      message: firstLine,
      library: library,
      summary: _truncate((summary ?? message).trim()),
      stack: frames,
      request: currentRequest,
      firstSeenMs: now,
    );
    _byKey[key] = issue;
    while (_byKey.length > maxEntries) {
      final AppIssue evicted = _byKey.remove(_byKey.keys.first)!;
      if (evicted.seq > _deliveredSeq) {
        dropped++;
      }
    }
    return issue;
  }

  /// Issues not yet attached to a response, in sequence order; marks them
  /// delivered. Returns an empty list when delivery is disabled.
  List<AppIssue> takeUndelivered() {
    if (!deliveryEnabled || _seq == _deliveredSeq) {
      return const <AppIssue>[];
    }
    final List<AppIssue> out = since(_deliveredSeq);
    _deliveredSeq = _seq;
    return out;
  }

  /// Buffered issues with a sequence number greater than [seq].
  List<AppIssue> since(int seq) => <AppIssue>[
    for (final AppIssue issue in _byKey.values)
      if (issue.seq > seq) issue,
  ];

  /// Clears all state (tests and `configure` `appIssues: false`).
  void reset() {
    _byKey.clear();
    _seq = 0;
    _deliveredSeq = 0;
    dropped = 0;
    currentRequest = null;
  }

  /// Top [maxStackFrames] frames of [stack], app frames (outside `dart:` and
  /// `package:flutter/`) first, normalized to `package:...:line` style.
  static List<String> trimStack(StackTrace? stack) {
    if (stack == null) {
      return const <String>[];
    }
    final List<String> lines = stack
        .toString()
        .split('\n')
        .map((String l) => l.trim())
        .where((String l) => l.isNotEmpty && !l.startsWith('<asynchronous'))
        .toList();
    bool isFramework(String line) =>
        line.contains('(dart:') ||
        line.contains('(package:flutter/') ||
        line.contains('(package:tapwing_driver/');
    final List<String> app = lines
        .where((String l) => !isFramework(l))
        .toList();
    final List<String> picked = app.isNotEmpty ? app : lines;
    return picked.take(maxStackFrames).toList();
  }

  static String _firstLine(String text) {
    final String trimmed = text.trim();
    final int nl = trimmed.indexOf('\n');
    return nl < 0 ? trimmed : trimmed.substring(0, nl).trim();
  }

  static String _truncate(String text) => text.length <= maxSummaryChars
      ? text
      : '${text.substring(0, maxSummaryChars)}…';
}

/// Returns true when [message] is Flutter's `BindingBase.debugCheckZone`
/// `"Zone mismatch."` diagnostic report.
bool isZoneMismatchError(String message) =>
    message.contains('Zone mismatch.') &&
    message.contains(
      'The Flutter bindings were initialized in a different zone than is now being used.',
    );

/// Classifies a [FlutterErrorDetails] report into an [AppIssueKind].
AppIssueKind classifyFlutterError(FlutterErrorDetails details) {
  final String message = details.exceptionAsString();
  if (isZoneMismatchError(message)) {
    return AppIssueKind.setupWarning;
  }
  final String library = details.library ?? '';
  final String context = details.context?.toDescription() ?? '';
  if (library == 'rendering library' && message.contains(' overflowed by ')) {
    return AppIssueKind.layoutOverflow;
  }
  if (library == 'image resource service') {
    return AppIssueKind.imageLoadError;
  }
  if (library == 'widgets library' && context.contains('building')) {
    return AppIssueKind.buildError;
  }
  if (library == 'rendering library') {
    if (context.contains('performLayout') || context.contains('layout')) {
      return AppIssueKind.layoutError;
    }
    if (context.contains('paint')) {
      return AppIssueKind.paintError;
    }
  }
  return AppIssueKind.frameworkAssertion;
}
