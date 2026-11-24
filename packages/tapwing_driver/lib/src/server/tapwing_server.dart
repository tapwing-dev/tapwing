import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../protocol/rpc_transport.dart';
import 'command_handlers.dart';
import 'driver_context.dart';
import 'rpc_session.dart';

/// Default loopback TCP port the driver dials (reversed to the host CLI via
/// `adb reverse tcp:7781 tcp:<host-port>` on Android).
const int defaultDriverPort = int.fromEnvironment(
  'TAPWING_PORT',
  defaultValue: 7781,
);

/// Whether a release build opted in to running the driver
/// (`--dart-define=TAPWING_ALLOW_RELEASE=true`).
const bool allowDriverInRelease = bool.fromEnvironment('TAPWING_ALLOW_RELEASE');

/// Whether the driver may run in a build with the given mode.
///
/// The driver gives whoever answers on its loopback port full control of the
/// app's UI and a view of everything on screen. That is the point in a debug
/// or profile test build; a release build that ships the Tapwing entrypoint by
/// mistake must not expose it, so release builds need an explicit opt-in.
bool isDriverEnabled({
  bool releaseMode = kReleaseMode,
  bool allowRelease = allowDriverInRelease,
}) => !releaseMode || allowRelease;

/// Background TCP dial-out client that connects to the Tapwing host CLI and
/// hands each connection to [RpcSession].
///
/// The app dials out (rather than listening) so multiple devices can run in
/// parallel without port collisions: Android via per-device `adb reverse`,
/// iOS simulators (which share the host loopback) via a per-session host port
/// handed over in a file (see [resolvePort]).
class TapwingServer {
  /// Creates a server dialing a fixed [port] (or, when `null`, the port
  /// resolved by [resolvePort] before every dial) and serving commands via
  /// [context]. Tests pass [transportFactory] to observe what is written to
  /// each connection.
  TapwingServer({
    int? port,
    DriverContext? context,
    @visibleForTesting RpcTransport Function(Socket socket)? transportFactory,
  }) : _fixedPort = port,
       _transportFactory = transportFactory ?? SocketRpcTransport.new,
       _session = RpcSession(CommandDispatcher(context ?? DriverContext()));

  /// Name of the port handoff file the host CLI writes into the app's
  /// temporary directory (`<simulator data container>/tmp/tapwing_port`).
  static const String portFileName = 'tapwing_port';

  /// Resolves the host port to dial, in order:
  /// 1. `TAPWING_PORT` from the process environment,
  /// 2. the [portFileName] file in [tempDir] (iOS simulator: Flutter on iOS
  ///    exposes no process environment to Dart, so the CLI writes its
  ///    per-session port there before launching or attaching),
  /// 3. [defaultDriverPort] (Android: reversed by `adb reverse`).
  ///
  /// Re-evaluated before each dial, so an app that outlives a session picks
  /// up the next session's port without being restarted.
  static int resolvePort({
    Map<String, String>? environment,
    Directory? tempDir,
    int fallback = defaultDriverPort,
  }) {
    final Map<String, String> env = environment ?? Platform.environment;
    final int? fromEnv = _parsePort(env['TAPWING_PORT']);
    if (fromEnv != null) return fromEnv;
    try {
      final File f = File(
        '${(tempDir ?? Directory.systemTemp).path}/$portFileName',
      );
      if (f.existsSync()) {
        final int? fromFile = _parsePort(f.readAsStringSync());
        if (fromFile != null) return fromFile;
      }
    } on FileSystemException {
      // Not readable: fall back to the compiled-in port.
    }
    return fallback;
  }

  static int? _parsePort(String? raw) {
    final int? p = int.tryParse(raw?.trim() ?? '');
    return p != null && p > 0 && p < 65536 ? p : null;
  }

  /// Shared singleton started by [TapwingBindingMixin.initInstances].
  static final TapwingServer instance = TapwingServer();

  static const Duration _connectTimeout = Duration(seconds: 1);
  static const Duration _reconnectDelay = Duration(milliseconds: 150);
  static const Duration _idleReconnectDelay = Duration(seconds: 1);
  static const Duration _fastRetryWindow = Duration(seconds: 60);

  /// Pause before the next dial, given how long no host has been connected
  /// (since start, or since the last connection ended).
  ///
  /// A test session reconnects within moments, so dials stay frequent for a
  /// minute. An instrumented app left running with no host slows down
  /// instead of waking up several times a second for as long as it lives.
  @visibleForTesting
  static Duration reconnectDelayAfter(Duration withoutHost) =>
      withoutHost < _fastRetryWindow ? _reconnectDelay : _idleReconnectDelay;

  final int? _fixedPort;
  final RpcTransport Function(Socket socket) _transportFactory;

  /// Loopback port dialed next to reach the host CLI.
  int get port => _fixedPort ?? resolvePort();
  final RpcSession _session;
  bool _started = false;
  bool _stopped = false;
  int _generation = 0;
  Socket? _activeSocket;

  /// Starts the dial-out reconnect loop if not already running. Does nothing
  /// in a build where [isDriverEnabled] is false.
  void start() {
    if (_started || !isDriverEnabled()) return;
    _started = true;
    _stopped = false;
    final int gen = ++_generation;
    unawaited(_loop(gen));
  }

  /// Stops the reconnect loop and closes any active socket.
  void stop() {
    _stopped = true;
    _started = false;
    _generation++;
    _activeSocket?.destroy();
    _activeSocket = null;
  }

  Future<void> _loop(int gen) async {
    final Stopwatch withoutHost = Stopwatch()..start();
    while (!_stopped && gen == _generation) {
      try {
        final Socket socket = await Socket.connect(
          InternetAddress.loopbackIPv4,
          port,
          timeout: _connectTimeout,
        );
        if (_stopped || gen != _generation) {
          socket.destroy();
          return;
        }
        _activeSocket = socket;
        try {
          socket.setOption(SocketOption.tcpNoDelay, true);
          await _session.serve(_transportFactory(socket));
        } finally {
          if (identical(_activeSocket, socket)) {
            _activeSocket = null;
          }
          socket.destroy();
          withoutHost.reset();
        }
      } catch (_) {
        // Host CLI is not listening yet; retry shortly.
      }
      if (_stopped || gen != _generation) return;
      await Future<void>.delayed(reconnectDelayAfter(withoutHost.elapsed));
    }
  }
}
