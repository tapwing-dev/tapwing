import 'dart:ui' as ui;

import '../diagnostics/app_issues.dart';
import '../diagnostics/http_log.dart';
import '../idle/http_tracker.dart';
import '../protocol/rpc_error.dart';
import '../protocol/rpc_messages.dart';
import '../snapshot/snapshot.dart';
import 'driver_context.dart';
import 'gesture_handlers.dart';
import 'input_navigation_handlers.dart';

/// Signature for an individual RPC method handler.
typedef RpcCommandHandler =
    Future<Object?> Function(Map<String, Object?> params);

/// Routes RPC method names to dedicated per-command handlers.
///
/// Each RPC method lives in its own focused handler method registered in
/// [_handlers], keeping wire parsing, execution, and result serialization
/// isolated and easy to extend as new Maestro commands are added.
class CommandDispatcher {
  /// Creates a dispatcher operating against [context].
  CommandDispatcher(this._context)
    : _gestures = GestureHandlers(_context),
      _inputNav = InputNavigationHandlers(_context) {
    _handlers = <String, RpcCommandHandler>{
      'ping': _handlePing,
      'configure': _handleConfigure,
      'busy': _handleBusy,
      'snapshot': _handleSnapshot,
      'waitIdle': _handleWaitIdle,
      'tapOn': _gestures.handleTapOn,
      'tapPoint': _gestures.handleTapPoint,
      'assertVisible': _handleAssertVisible,
      'assertNotVisible': _handleAssertNotVisible,
      'inputText': _inputNav.handleInputText,
      'eraseText': _inputNav.handleEraseText,
      'hideKeyboard': _inputNav.handleHideKeyboard,
      'back': _inputNav.handleBack,
      'armBack': _inputNav.handleArmBack,
      'awaitBack': _inputNav.handleAwaitBack,
      'pressKey': _inputNav.handlePressKey,
      'swipe': _gestures.handleSwipe,
      'scrollUntilVisible': _gestures.handleScrollUntilVisible,
      'copyTextFrom': _handleCopyTextFrom,
      'darkMode': _handleDarkMode,
      'appIssues': _handleAppIssues,
      'httpLog': _handleHttpLog,
    };
  }

  /// Warning emitted when iOS `hideKeyboard` succeeds although an editable
  /// field keeps focus (stock Maestro iOS fails in that state).
  static const String iosHideKeyboardFocusWarning =
      InputNavigationHandlers.iosHideKeyboardFocusWarning;

  final DriverContext _context;
  final GestureHandlers _gestures;
  final InputNavigationHandlers _inputNav;
  late final Map<String, RpcCommandHandler> _handlers;

  /// Exposes the shared [DriverContext] for session lifecycle management.
  DriverContext get context => _context;

  /// Dispatches [method] with raw JSON [params], or throws
  /// [DriverError.unknownMethod] if no handler is registered.
  Future<Object?> dispatch(String method, Map<String, Object?> params) {
    final RpcCommandHandler? handler = _handlers[method];
    if (handler == null) {
      throw DriverError.unknownMethod(method);
    }
    return handler(params);
  }

  Future<Object?> _handlePing(Map<String, Object?> _) async => 'pong';

  Future<Object?> _handleConfigure(Map<String, Object?> raw) async {
    final ConfigureParams params = ConfigureParams.fromJson(raw);
    _context.invalidateSemanticsCache();
    if (params.settlePolicy != null) {
      _context.settlePolicy = params.settlePolicy!;
    }
    if (params.softKeyboard != null) {
      _context.idle.configure(softKeyboard: params.softKeyboard);
    }
    if (params.strictInput != null) {
      _context.strictInput = params.strictInput!;
    }
    if (params.platform != null) {
      _context.platform = params.platform!;
    }
    if (params.appName != null) {
      _context.appName = params.appName!.isEmpty ? null : params.appName;
    }
    if (params.strictHideKeyboard != null) {
      _context.strictHideKeyboard = params.strictHideKeyboard!;
    }
    _context.idle.configureSettleLoad(
      timeoutMs: params.settleLoadTimeoutMs,
      httpIgnore: params.settleHttpIgnore,
    );
    final AppIssueLog issues = _context.errorHooks.log;
    if (params.appIssues == true) {
      issues.deliveryEnabled = true;
      _context.errorHooks.install();
    } else if (params.appIssues == false) {
      issues.deliveryEnabled = false;
      _context.errorHooks.uninstall();
      issues.reset();
    }
    final HttpTracker http = _context.idle.httpTracker;
    if (params.httpLog == true) {
      http.log.enabled = true;
      http.ensureInstalled();
    } else if (params.httpLog == false) {
      http.log
        ..enabled = false
        ..reset();
    }
    return ConfigureResult(
      appIssues: issues.deliveryEnabled,
      httpLog: http.log.enabled,
      settleLoadTimeoutMs: http.settleBound?.inMilliseconds ?? 0,
      settleHttpIgnore: http.settleIgnore.length,
      settlePolicy: _context.settlePolicy,
      strictInput: _context.strictInput,
      platform: _context.platform,
      appName: _context.appName,
      strictHideKeyboard: _context.strictHideKeyboard,
      pixelsPerUnit: _context.pixelsPerUnit,
    ).toJson();
  }

  /// Re-reads buffered app health issues after [since] (flow end or after a
  /// reconnect). Responses also carry them as they happen.
  Future<Object?> _handleAppIssues(Map<String, Object?> raw) async {
    final Object? since = raw['since'];
    final AppIssueLog log = _context.errorHooks.log;
    return <String, Object?>{
      'issues': <Map<String, Object?>>[
        for (final AppIssue issue in log.since(since is int ? since : 0))
          issue.toJson(),
      ],
      'latestSeq': log.latestSeq,
      'dropped': log.dropped,
    };
  }

  /// Re-reads finished HTTP log entries after [since] plus requests still in
  /// flight (after native-lane steps and at flow end).
  Future<Object?> _handleHttpLog(Map<String, Object?> raw) async {
    final Object? since = raw['since'];
    final HttpRequestLog log = _context.idle.httpTracker.log;
    return <String, Object?>{
      'entries': <Map<String, Object?>>[
        for (final HttpLogEntry e in log.since(since is int ? since : 0))
          e.toJson(),
      ],
      'inFlight': <Map<String, Object?>>[
        for (final HttpLogEntry e in log.inFlight) e.toJson(),
      ],
      'latestSeq': log.latestSeq,
      'dropped': log.dropped,
      'enabled': log.enabled,
    };
  }

  Future<Object?> _handleBusy(Map<String, Object?> _) async =>
      _context.idle.busyReport().toJson();

  Future<Object?> _handleSnapshot(Map<String, Object?> _) async =>
      SnapshotResult(snapshot: _context.captureSnapshot()).toJson();

  Future<Object?> _handleWaitIdle(Map<String, Object?> raw) async {
    final WaitIdleParams params = WaitIdleParams.fromJson(raw);
    final String state = await _context.settle(params.timeout);
    return WaitIdleResult(state: state).toJson();
  }

  Future<Object?> _handleAssertVisible(Map<String, Object?> raw) async {
    final SelectorTimeoutParams params = SelectorTimeoutParams.fromJson(
      raw,
      defaultTimeoutMs: DriverTimeouts.lookupMs,
    );
    final KNode? match = await _context.waitFor(
      params.selector,
      params.timeout,
    );
    if (match == null) {
      throw _context.elementNotFound(params.selector);
    }
    return ElementResult(element: match).toJson();
  }

  Future<Object?> _handleAssertNotVisible(Map<String, Object?> raw) async {
    // Orchestra.kt:560-561, 1128-1142: assertNotVisible defaults to 17 s
    // (`lookupTimeoutMs`), not 7 s.
    final SelectorTimeoutParams params = SelectorTimeoutParams.fromJson(
      raw,
      defaultTimeoutMs: DriverTimeouts.lookupMs,
    );
    final bool gone = await _context.waitGone(params.selector, params.timeout);
    if (!gone) {
      throw DriverError.stillVisible(params.selector);
    }
    return true;
  }

  Future<Object?> _handleCopyTextFrom(Map<String, Object?> raw) async {
    final SelectorTimeoutParams params = SelectorTimeoutParams.fromJson(
      raw,
      defaultTimeoutMs: DriverTimeouts.lookupMs,
    );
    final KNode? match = await _context.waitFor(
      params.selector,
      params.timeout,
    );
    if (match == null) {
      throw _context.elementNotFound(params.selector);
    }
    final String? resolved = _resolveCopyText(match);
    if (resolved == null) {
      throw DriverError.unableToCopyText(
        'Element does not contain text to copy: ${params.selector.describe()}',
      );
    }
    return CopyTextResult(text: resolved).toJson();
  }

  Future<Object?> _handleDarkMode(Map<String, Object?> _) async {
    final ui.Brightness brightness = _context.readPlatformBrightness();
    return DarkModeResult(darkMode: brightness == ui.Brightness.dark).toJson();
  }

  /// Port of `Orchestra.resolveText` (`Orchestra.kt:1854-1867`).
  static String? _resolveCopyText(KNode node) {
    if (node.text != null && node.text!.isNotEmpty) return node.text;
    if (node.hintText != null && node.hintText!.isNotEmpty) {
      return node.hintText;
    }
    if (node.accessibilityText != null && node.accessibilityText!.isNotEmpty) {
      return node.accessibilityText;
    }
    if (node.error != null && node.error!.isNotEmpty) return node.error;
    // Android's XML dump always sets text="" and content-desc="", so on
    // real Android nodes an element with no visible text resolves to "".
    return node.accessibilityText ?? node.text;
  }
}
