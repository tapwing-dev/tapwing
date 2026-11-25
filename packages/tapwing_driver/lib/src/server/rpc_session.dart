import 'dart:async';
import 'dart:convert';

import '../diagnostics/app_issues.dart';
import '../diagnostics/error_hooks.dart';
import '../diagnostics/http_log.dart';
import '../idle/http_tracker.dart';
import '../protocol/rpc_error.dart';
import '../protocol/rpc_messages.dart';
import '../protocol/rpc_transport.dart';
import 'command_handlers.dart';

/// Serves a single [RpcTransport] connection using newline-delimited JSON.
///
/// Upon connecting, immediately pushes a [RpcHelloEvent], then processes
/// incoming requests strictly one at a time in arrival order on a
/// per-connection queue. While any handler executes, emits periodic
/// [RpcProgressEvent] heartbeats (`progressInterval`, default 2 s) so the host
/// CLI can enforce a short per-read socket timeout without falsely dropping
/// legitimate long-running commands.
///
/// When a connection closes (or a new connection replaces it), the previous
/// connection's cancellation flag is set so long-running wait loops unwind
/// promptly instead of blocking a reconnecting client behind a stale handler.
class RpcSession {
  /// Creates an RPC session backed by [dispatcher], an optional [buildHello]
  /// factory, and an optional [progressInterval] (overridable in tests).
  RpcSession(
    this._dispatcher, {
    RpcHelloEvent Function()? buildHello,
    this.progressInterval = defaultProgressInterval,
  }) : _buildHello = buildHello ?? RpcHelloEvent.current;

  /// App health hooks whose log is attached to responses (F7).
  AppErrorHooks get hooks => _dispatcher.context.errorHooks;

  List<Map<String, Object?>> _takeIssues() => <Map<String, Object?>>[
    for (final AppIssue issue in hooks.log.takeUndelivered()) issue.toJson(),
  ];

  int _droppedIssues() => hooks.log.deliveryEnabled ? hooks.log.dropped : 0;

  /// HTTP request log (F8b) whose finished entries are attached to responses.
  HttpTracker get _http => _dispatcher.context.idle.httpTracker;

  List<Map<String, Object?>> _takeHttp() => <Map<String, Object?>>[
    for (final HttpLogEntry e in _http.log.takeUndelivered()) e.toJson(),
  ];

  int _droppedHttp() => _http.log.enabled ? _http.log.dropped : 0;

  /// Default heartbeat interval emitted during long-running RPC handlers.
  static const Duration defaultProgressInterval = Duration(seconds: 2);

  final CommandDispatcher _dispatcher;
  final RpcHelloEvent Function() _buildHello;

  /// Interval between `progress` events while a request is in flight.
  final Duration progressInterval;

  /// Tracks the previous connection's active work so a new connection cancels
  /// it and waits briefly for it to unwind before touching shared UI state.
  void Function()? _cancelActive;
  Future<void> _previousWork = Future<void>.value();

  /// Cancels any active connection handler and detaches cached driver
  /// listeners (called when `TapwingServer.stop()` tears down the driver).
  void dispose() {
    _cancelActive?.call();
    _dispatcher.context.dispose();
  }

  /// Runs the session until [transport] closes and all queued requests finish.
  Future<void> serve(RpcTransport transport) async {
    // Cancel any lingering wait loop from a dropped prior connection.
    _cancelActive?.call();
    _dispatcher.context.invalidateSemanticsCache();

    bool cancelled = false;
    void cancelThis() {
      cancelled = true;
    }

    _cancelActive = cancelThis;

    void sendJson(Map<String, Object?> message) {
      if (cancelled) return;
      transport.sendLine(jsonEncode(message));
    }

    sendJson(_buildHello().toJson());

    // Per-connection queue, chained after any previous connection's cancelled
    // handler finishes unwinding so UI mutations never overlap.
    Future<void> connectionQueue = _previousWork.catchError((Object _) {});

    try {
      await for (final String line in transport.lines) {
        connectionQueue = connectionQueue
            .then((_) {
              if (cancelled) return Future<void>.value();
              _dispatcher.context.isCancelled = () => cancelled;
              return _processLine(line, sendJson, () => cancelled);
            })
            // Keep the serialization queue healthy even if an unexpected error
            // escapes transport writes.
            .catchError((Object _) {});
      }
      // Stream closed by peer: cancel any in-flight wait loop immediately.
      cancelled = true;
      _previousWork = connectionQueue;
      await connectionQueue;
    } finally {
      cancelled = true;
      _previousWork = connectionQueue;
      _dispatcher.context.invalidateSemanticsCache();
      if (identical(_cancelActive, cancelThis)) {
        _cancelActive = null;
      }
      await transport.close();
    }
  }

  Future<void> _processLine(
    String line,
    void Function(Map<String, Object?>) sendJson,
    bool Function() isCancelled,
  ) async {
    Object? requestId;
    Timer? heartbeat;
    try {
      final Map<String, Object?> raw =
          (jsonDecode(line) as Map<Object?, Object?>).cast<String, Object?>();
      requestId = raw['id'];
      final RpcRequest request = RpcRequest.fromJson(raw);
      final Stopwatch sw = Stopwatch()..start();
      // Attribute issues to this request (and keep it until the next one),
      // and re-wrap error handlers an app replaced since the last request.
      hooks.log.currentRequest = request.id;
      hooks.ensureInstalled();
      // Same attribution for HTTP requests the action starts (F8b).
      _http.log.currentRequest = request.id;
      _http.ensureInstalled();

      if (progressInterval > Duration.zero) {
        heartbeat = Timer.periodic(progressInterval, (_) {
          if (!isCancelled()) {
            sendJson(RpcProgressEvent(id: request.id).toJson());
          }
        });
      }

      final Object? result = await _dispatcher.dispatch(
        request.method,
        request.params,
      );
      if (!isCancelled()) {
        sendJson(
          RpcSuccessResponse(
            id: request.id,
            result: result,
            us: sw.elapsedMicroseconds,
            issues: _takeIssues(),
            droppedIssues: _droppedIssues(),
            http: _takeHttp(),
            droppedHttp: _droppedHttp(),
          ).toJson(),
        );
      }
    } on DriverError catch (error) {
      if (!isCancelled()) {
        sendJson(
          RpcErrorResponse(
            id: requestId,
            error: error.toJson(),
            issues: _takeIssues(),
            droppedIssues: _droppedIssues(),
            http: _takeHttp(),
            droppedHttp: _droppedHttp(),
          ).toJson(),
        );
      }
    } catch (error, stackTrace) {
      if (!isCancelled()) {
        sendJson(
          RpcErrorResponse.internal(requestId, error, stackTrace)
              .withIssues(_takeIssues(), droppedIssues: _droppedIssues())
              .withHttp(_takeHttp(), droppedHttp: _droppedHttp())
              .toJson(),
        );
      }
    } finally {
      heartbeat?.cancel();
    }
  }
}
