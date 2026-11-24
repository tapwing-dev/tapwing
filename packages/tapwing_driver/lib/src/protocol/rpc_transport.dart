import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// Bidirectional newline-delimited JSON transport between the in-app driver
/// and the host CLI.
///
/// Abstracted behind an interface so RPC dispatch and session framing can be
/// verified in unit and widget tests using an in-memory fake transport.
abstract interface class RpcTransport {
  /// Stream of incoming UTF-8 JSON lines (without trailing newlines).
  Stream<String> get lines;

  /// Writes a single JSON line (appending `'\n'`) to the peer.
  ///
  /// Implementations should treat writes to an already-closed peer as a no-op
  /// so an abrupt host disconnect cannot poison the driver's request queue.
  void sendLine(String line);

  /// Closes the underlying transport and releases resources.
  Future<void> close();
}

/// Thrown into [RpcTransport.lines] when the peer sends a line longer than
/// the transport accepts.
class RpcLineTooLongException implements Exception {
  /// Creates the error for a line that exceeded [maxLength] characters.
  const RpcLineTooLongException(this.maxLength);

  /// The limit that was exceeded, in UTF-16 code units.
  final int maxLength;

  @override
  String toString() => 'RPC line longer than $maxLength characters';
}

/// Splits decoded text into `\n`-terminated lines (a trailing `\r` is
/// dropped) and fails the stream with [RpcLineTooLongException] once a line
/// grows past [maxLength], so a peer that never sends a newline cannot make
/// the app buffer without bound.
class BoundedLineSplitter extends StreamTransformerBase<String, String> {
  /// Creates a splitter that accepts lines of up to [maxLength] code units.
  const BoundedLineSplitter(this.maxLength);

  /// Longest accepted line, in UTF-16 code units.
  final int maxLength;

  @override
  Stream<String> bind(Stream<String> stream) async* {
    final StringBuffer pending = StringBuffer();
    String take() {
      final String line = pending.toString();
      pending.clear();
      return line.endsWith('\r') ? line.substring(0, line.length - 1) : line;
    }

    await for (final String chunk in stream) {
      int start = 0;
      while (start < chunk.length) {
        final int newline = chunk.indexOf('\n', start);
        final int end = newline < 0 ? chunk.length : newline;
        if (pending.length + (end - start) > maxLength) {
          throw RpcLineTooLongException(maxLength);
        }
        pending.write(chunk.substring(start, end));
        if (newline < 0) {
          break;
        }
        yield take();
        start = newline + 1;
      }
    }
    if (pending.isNotEmpty) {
      yield take();
    }
  }
}

/// [RpcTransport] backed by a `dart:io` TCP [Socket] (dialed to the host over
/// `adb reverse` on Android).
class SocketRpcTransport implements RpcTransport {
  /// Wraps an already-connected [Socket] with newline-delimited UTF-8 framing.
  SocketRpcTransport(this._socket)
    : lines = _socket
          .cast<List<int>>()
          .transform(utf8.decoder)
          .transform(const BoundedLineSplitter(maxLineLength)) {
    // Absorb asynchronous flush errors (`Broken pipe` / `Connection reset`) so
    // an abrupt host disconnect never becomes an uncaught zone/dispatcher error.
    unawaited(
      _socket.done.then<void>(
        (_) => _closed = true,
        onError: (Object _) => _closed = true,
      ),
    );
  }

  /// Longest request line accepted from the host. Requests are small JSON
  /// objects; the largest carry `inputText` or clipboard text.
  static const int maxLineLength = 4 * 1024 * 1024;

  final Socket _socket;
  bool _closed = false;

  @override
  final Stream<String> lines;

  @override
  void sendLine(String line) {
    if (_closed) return;
    try {
      _socket.write('$line\n');
    } on SocketException {
      _closed = true;
    } on StateError {
      _closed = true;
    }
  }

  @override
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    _socket.destroy();
  }
}
