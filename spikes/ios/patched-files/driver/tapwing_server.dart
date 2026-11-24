import 'dart:async';
import 'dart:io';

import '../protocol/rpc_transport.dart';
import 'command_handlers.dart';
import 'driver_context.dart';
import 'rpc_session.dart';

/// Default loopback TCP port the driver dials (reversed to the host CLI via
/// `adb reverse tcp:7781 tcp:<host-port>`).
const int defaultDriverPort = int.fromEnvironment(
  'TAPWING_PORT',
  defaultValue: 7781,
);

/// Background TCP dial-out client that connects to the Tapwing host CLI and
/// hands each connection to [RpcSession].
///
/// The app dials out (rather than listening) so multiple Android devices can
/// run in parallel without port collisions via per-device `adb reverse`.
class TapwingServer {
  /// Creates a server dialing [port] and serving commands via [context].
  TapwingServer({int? port, DriverContext? context})
    : port = port ?? _runtimePort(),
      _session = RpcSession(CommandDispatcher(context ?? DriverContext()));

  /// iOS simulator: the host CLI passes its per-session port at launch
  /// (`SIMCTL_CHILD_TAPWING_PORT`), since the simulator shares the host
  /// loopback and needs no port forwarding.
  ///
  /// Flutter on iOS exposes no process environment to Dart, so the CLI also
  /// writes the port to `tapwing_port` in the app's temp directory (inside
  /// its simulator data container) before launching.
  static int _runtimePort() {
    final int? fromEnv = int.tryParse(Platform.environment['TAPWING_PORT'] ?? '');
    if (fromEnv != null) return fromEnv;
    try {
      final File f = File('${Directory.systemTemp.path}/tapwing_port');
      if (f.existsSync()) {
        final int? fromFile = int.tryParse(f.readAsStringSync().trim());
        if (fromFile != null) return fromFile;
      }
    } on FileSystemException {
      // Not readable: fall back to the compiled-in port.
    }
    return defaultDriverPort;
  }

  /// Shared singleton started by [TapwingBindingMixin.initInstances].
  static final TapwingServer instance = TapwingServer();

  static const Duration _connectTimeout = Duration(seconds: 1);
  static const Duration _reconnectDelay = Duration(milliseconds: 150);

  /// Loopback port dialed to reach the host CLI.
  final int port;
  final RpcSession _session;
  bool _started = false;

  /// Starts the dial-out reconnect loop if not already running.
  void start() {
    if (_started) return;
    _started = true;
    // ignore: avoid_print
    print('tapwing: dialing port $port (temp ${Directory.systemTemp.path})');
    unawaited(_loop());
  }

  Future<void> _loop() async {
    while (true) {
      try {
        final Socket socket = await Socket.connect(
          InternetAddress.loopbackIPv4,
          port,
          timeout: _connectTimeout,
        );
        try {
          socket.setOption(SocketOption.tcpNoDelay, true);
          await _session.serve(SocketRpcTransport(socket));
        } finally {
          socket.destroy();
        }
      } catch (_) {
        // Host CLI is not listening yet; retry shortly.
      }
      await Future<void>.delayed(_reconnectDelay);
    }
  }
}
