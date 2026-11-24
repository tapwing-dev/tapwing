import 'dart:io' as io;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';

import '../platform.dart';

/// Wire protocol version advertised in the initial `hello` event.
const int protocolVersion = 1;

/// Default timeouts matching Maestro 2.12.1 and Tapwing's F0 contract.
abstract final class DriverTimeouts {
  /// Default element lookup and assertion timeout (`tapOn`, `assertVisible`,
  /// `assertNotVisible`, `copyTextFrom`; `Orchestra.kt:142, 560-561, 1116-1135`).
  static const int lookupMs = 17000;

  /// Default UI settle timeout (`waitIdle` and post-action settles).
  static const int settleMs = 3000;

  /// Default timeout for `scrollUntilVisible`.
  static const int scrollUntilVisibleMs = 20000;

  /// Default settle cap between scroll gestures in `scrollUntilVisible`.
  static const Duration scrollStepSettle = Duration(milliseconds: 1500);

  /// Default long-press hold duration for `tapOn`/`tapPoint` (`longPress: true`).
  static const Duration longPressHold = Duration(milliseconds: 3000);

  /// Default inter-tap delay for repeated `tapOn` calls.
  static const int repeatDelayMs = 100;

  /// Default swipe gesture duration.
  static const int swipeDurationMs = 400;

  /// Default scroll duration for `scrollUntilVisible` (`speed = 40` ->
  /// `(1000 * (100 - 40) / 100) + 1 = 601 ms`, `Commands.kt:160, 183`).
  static const int defaultScrollDurationMs = 601;

  /// Maximum wait for `tapOn` with `waitUntilVisible: true` (`Maestro.kt:511-520`).
  static const Duration waitUntilVisibleTimeout = Duration(seconds: 10);
}

/// Incoming JSON-RPC request envelope sent by the Rust CLI.
class RpcRequest {
  /// Creates a decoded RPC request.
  const RpcRequest({
    required this.id,
    required this.method,
    required this.params,
  });

  /// Parses an RPC request from a decoded JSON map.
  factory RpcRequest.fromJson(Map<String, Object?> json) => RpcRequest(
    id: json['id'],
    method: json['method']! as String,
    params: _optionalMap(json['params']),
  );

  /// Request correlation id echoed verbatim in the response.
  final Object? id;

  /// RPC method name.
  final String method;

  /// Raw method parameter map (empty when omitted or `null`).
  final Map<String, Object?> params;
}

/// Outgoing JSON-RPC success response envelope.
class RpcSuccessResponse {
  /// Creates a success response carrying [result] and handler duration [us].
  const RpcSuccessResponse({
    required this.id,
    required this.result,
    required this.us,
    this.issues = const <Map<String, Object?>>[],
    this.droppedIssues = 0,
    this.http = const <Map<String, Object?>>[],
    this.droppedHttp = 0,
  });

  /// App health issues (F7) recorded since the previous response, attached
  /// only when the host enabled delivery via `configure` `appIssues: true`.
  final List<Map<String, Object?>> issues;

  /// HTTP requests (F8b) finished since the previous response, attached only
  /// when the host enabled the log via `configure` `httpLog: true`.
  final List<Map<String, Object?>> http;

  /// Cumulative HTTP log entries dropped before delivery (omitted when zero).
  final int droppedHttp;

  /// Cumulative undelivered issues evicted from the driver ring buffer (omitted
  /// on the wire when zero).
  final int droppedIssues;

  /// Request id echoed from [RpcRequest.id].
  final Object? id;

  /// JSON-encodable result payload.
  final Object? result;

  /// Server-side execution time in microseconds.
  final int us;

  /// Serializes to the newline-delimited wire object.
  Map<String, Object?> toJson() => <String, Object?>{
    'id': id,
    'result': result,
    'us': us,
    if (issues.isNotEmpty) 'issues': issues,
    if (droppedIssues > 0) 'droppedIssues': droppedIssues,
    if (http.isNotEmpty) 'http': http,
    if (droppedHttp > 0) 'droppedHttp': droppedHttp,
  };
}

/// Outgoing JSON-RPC error response envelope.
class RpcErrorResponse {
  /// Creates an error response carrying a structured [error] map.
  const RpcErrorResponse({
    required this.id,
    required this.error,
    this.issues = const <Map<String, Object?>>[],
    this.droppedIssues = 0,
    this.http = const <Map<String, Object?>>[],
    this.droppedHttp = 0,
  });

  /// Creates an error response for an unhandled exception with stack trace.
  factory RpcErrorResponse.internal(
    Object? id,
    Object error,
    StackTrace stackTrace,
  ) => RpcErrorResponse(
    id: id,
    error: <String, Object?>{
      'code': 'INTERNAL',
      'message': '$error',
      'stack': '$stackTrace',
    },
  );

  /// Request id echoed from the decoded request (or `null` if decoding failed).
  final Object? id;

  /// Wire error object containing `code`, `message`, and any extra details.
  final Map<String, Object?> error;

  /// App health issues (F7) recorded since the previous response; see
  /// [RpcSuccessResponse.issues].
  final List<Map<String, Object?>> issues;

  /// Cumulative undelivered issues evicted from the driver ring buffer (omitted
  /// on the wire when zero).
  final int droppedIssues;

  /// HTTP requests (F8b) finished since the previous response; see
  /// [RpcSuccessResponse.http].
  final List<Map<String, Object?>> http;

  /// Cumulative HTTP log entries dropped before delivery (omitted when zero).
  final int droppedHttp;

  /// Creates a copy carrying [issues] and [droppedIssues].
  RpcErrorResponse withIssues(
    List<Map<String, Object?>> issues, {
    int droppedIssues = 0,
  }) => RpcErrorResponse(
    id: id,
    error: error,
    issues: issues,
    droppedIssues: droppedIssues,
    http: http,
    droppedHttp: droppedHttp,
  );

  /// Creates a copy carrying [http] and [droppedHttp].
  RpcErrorResponse withHttp(
    List<Map<String, Object?>> http, {
    int droppedHttp = 0,
  }) => RpcErrorResponse(
    id: id,
    error: error,
    issues: issues,
    droppedIssues: droppedIssues,
    http: http,
    droppedHttp: droppedHttp,
  );

  /// Serializes to the newline-delimited wire object.
  Map<String, Object?> toJson() => <String, Object?>{
    'id': id,
    'error': error,
    if (issues.isNotEmpty) 'issues': issues,
    if (droppedIssues > 0) 'droppedIssues': droppedIssues,
    if (http.isNotEmpty) 'http': http,
    if (droppedHttp > 0) 'droppedHttp': droppedHttp,
  };
}

/// Initial handshake event pushed by the driver immediately upon connecting to
/// the host CLI.
class RpcHelloEvent {
  /// Creates a `hello` handshake event.
  const RpcHelloEvent({
    required this.protocol,
    required this.mode,
    required this.pid,
    required this.dartVersion,
    required this.screenWidth,
    required this.screenHeight,
    required this.dpr,
  });

  /// Captures handshake metadata for the currently running Flutter process.
  factory RpcHelloEvent.current() {
    final ui.FlutterView? view = ui.PlatformDispatcher.instance.implicitView;
    return RpcHelloEvent(
      protocol: protocolVersion,
      mode: kReleaseMode ? 'release' : (kProfileMode ? 'profile' : 'debug'),
      pid: io.pid,
      dartVersion: io.Platform.version,
      screenWidth: view?.physicalSize.width ?? 0,
      screenHeight: view?.physicalSize.height ?? 0,
      dpr: view?.devicePixelRatio,
    );
  }

  /// Protocol version number.
  final int protocol;

  /// Flutter build mode: `'debug'`, `'profile'`, or `'release'`.
  final String mode;

  /// Operating-system process id of the app under test.
  final int pid;

  /// Dart runtime version string (`Platform.version`).
  final String dartVersion;

  /// Physical display width in pixels.
  final double screenWidth;

  /// Physical display height in pixels.
  final double screenHeight;

  /// Device pixel ratio, or `null` if no implicit view exists yet.
  final double? dpr;

  /// Serializes to the `{"event": "hello", "params": {...}}` wire shape.
  Map<String, Object?> toJson() => <String, Object?>{
    'event': 'hello',
    'params': <String, Object?>{
      'protocol': protocol,
      'mode': mode,
      'pid': pid,
      'dart': dartVersion,
      'screen': <double>[screenWidth, screenHeight],
      'dpr': dpr,
      'platform': DriverPlatform.detect().name,
    },
  };
}

/// Periodic progress heartbeat emitted during long-running RPC handlers so
/// the host CLI can use a short per-read timeout without falsely dropping
/// legitimate multi-stage commands (`longPressOn` + retry + `waitUntilVisible`).
class RpcProgressEvent {
  /// Creates a `progress` event correlated to request [id].
  const RpcProgressEvent({required this.id});

  /// Request id currently executing on the driver.
  final Object? id;

  /// Serializes to `{"event": "progress", "id": ...}`.
  Map<String, Object?> toJson() => <String, Object?>{
    'event': 'progress',
    'id': id,
  };
}

Map<String, Object?> _optionalMap(Object? value) =>
    ((value as Map<Object?, Object?>?) ?? const <Object?, Object?>{})
        .cast<String, Object?>();
