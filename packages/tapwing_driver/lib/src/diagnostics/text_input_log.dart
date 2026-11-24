import 'package:flutter/services.dart';

/// Fixed-capacity ring buffer recording recent `flutter/textinput` channel
/// traffic and tap coordinates for IME/focus flake diagnostics.
///
/// Entries are prefixed with monotonic elapsed milliseconds (`"<ms> <entry>"`)
/// and tagged with a sequence counter so a command (`tapOn`, `inputText`) can
/// snapshot [mark] before acting and attach only the entries emitted during
/// that command via [since].
class TextInputLog {
  /// Creates an empty diagnostic log ring buffer.
  TextInputLog();

  /// Maximum number of entries retained in the ring buffer.
  static const int maxEntries = 300;

  static final Stopwatch _clock = Stopwatch()..start();

  final List<(int, String)> _entries = <(int, String)>[];
  int _seq = 0;

  /// Current sequence mark; pass to [since] after an operation finishes.
  int get mark => _seq;

  /// Appends [entry] with a timestamp and trims the oldest entry past
  /// [maxEntries].
  void record(String entry) {
    _entries.add((_seq++, '${_clock.elapsedMilliseconds} $entry'));
    if (_entries.length > maxEntries) {
      _entries.removeAt(0);
    }
  }

  /// Returns all formatted log entries recorded at or after [mark].
  List<String> since(int mark) => <String>[
    for (final (int seq, String line) in _entries)
      if (seq >= mark) line,
  ];

  /// Formats a raw `flutter/textinput` JSON method call into a compact single
  /// line (method name, client id when present, and text payload when present).
  static String describeCall(ByteData message) {
    try {
      final MethodCall call = const JSONMethodCodec().decodeMethodCall(message);
      final Object? args = call.arguments;
      String summary = '';
      if (args is List<Object?> && args.isNotEmpty) {
        summary = args.first is int ? ' client=${args.first}' : '';
        if (args.length > 1 && args[1] is Map<Object?, Object?>) {
          final Map<Object?, Object?> state = args[1]! as Map<Object?, Object?>;
          summary += ' text="${state['text'] ?? ''}"';
        }
      } else if (args is Map<Object?, Object?> && args.containsKey('text')) {
        summary = ' text="${args['text']}"';
      }
      return '${call.method}$summary';
    } catch (_) {
      return '<undecodable>';
    }
  }
}

/// Diagnostic payload attached under the `"ime"` key in `tapOn` and
/// `inputText` RPC responses.
class ImeDiagnostics {
  /// Creates an IME diagnostic snapshot.
  const ImeDiagnostics({
    this.usedClient,
    required this.clientId,
    required this.keyboard,
    required this.log,
    this.noClient = false,
  });

  /// Client id used for text injection (`inputText`), or `null` for `tapOn`.
  final int? usedClient;

  /// Currently active text-input client id after the command finished.
  final int? clientId;

  /// Whether the framework last requested the soft keyboard to be shown.
  final bool keyboard;

  /// `flutter/textinput` and tap log entries recorded during the command.
  final List<String> log;

  /// Whether text input was requested while no text-input client was attached
  /// (silently ignored in Maestro-parity mode; see `AndroidDriver.kt:547-559`).
  final bool noClient;

  /// Serializes to the wire format expected by the host CLI.
  ///
  /// Omits `usedClient` when `null` and `noClient` when `false` to keep normal
  /// responses compact.
  Map<String, Object?> toJson() => <String, Object?>{
    if (usedClient != null) 'usedClient': usedClient,
    if (noClient) 'noClient': true,
    'clientId': clientId,
    'keyboard': keyboard,
    'log': log,
  };
}
