import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';

import 'app_issues.dart';
import 'overflow_detector.dart';

/// Observes Flutter framework errors, uncaught Dart errors, and (in profile
/// and release builds) layout overflows, recording them into an
/// [AppIssueLog]. Read-only: every hook chains to the handler installed
/// before it, so crash reporters and the app's own error UI keep working.
///
/// Apps often replace `FlutterError.onError` after the binding is created
/// (Crashlytics, Sentry). [ensureInstalled] re-wraps the current handler at
/// every RPC boundary; an error that reaches two Tapwing wrappers in one
/// chain (a reporter that calls the handler it replaced) is recorded once.
class AppErrorHooks {
  /// Creates hooks recording into [log].
  AppErrorHooks({
    AppIssueLog? log,
    ui.PlatformDispatcher? dispatcher,
    bool? detectOverflows,
  }) : log = log ?? AppIssueLog.instance,
       _dispatcher = dispatcher ?? ui.PlatformDispatcher.instance,
       // Debug builds report overflows through FlutterError.onError; the
       // detector would double-report them.
       detectOverflows = detectOverflows ?? !kDebugMode;

  /// Shared singleton installed by [TapwingBindingMixin].
  static final AppErrorHooks instance = AppErrorHooks();

  /// Destination log.
  final AppIssueLog log;

  final ui.PlatformDispatcher _dispatcher;

  /// Whether [scanOverflows] runs the [OverflowDetector].
  final bool detectOverflows;

  FlutterExceptionHandler? _ourFlutterHandler;
  FlutterExceptionHandler? _flutterPrevious;
  ui.ErrorCallback? _ourDispatcherHandler;
  ui.ErrorCallback? _dispatcherPrevious;
  bool _installed = false;
  int _flutterDispatchDepth = 0;
  int _dispatcherDispatchDepth = 0;

  /// Errors already recorded, so an error passing several Tapwing wrappers
  /// in one handler chain is recorded once.
  final Expando<bool> _recorded = Expando<bool>('tapwingRecorded');

  /// Render objects whose overflow was already recorded (debug mode reports
  /// each overflowing render object once, so does the detector).
  final Expando<bool> _reportedOverflow = Expando<bool>('tapwingOverflow');

  /// Whether the hooks are installed.
  bool get isInstalled => _installed;

  /// Installs the hooks (idempotent).
  void install() {
    _installed = true;
    ensureInstalled();
  }

  /// Re-wraps `FlutterError.onError` / `PlatformDispatcher.onError` when the
  /// app replaced them after [install]. Cheap; call at every RPC boundary.
  void ensureInstalled() {
    if (!_installed) {
      return;
    }
    final FlutterExceptionHandler? current = FlutterError.onError;
    if (current == null || !identical(current, _ourFlutterHandler)) {
      final FlutterExceptionHandler? previous = current;
      void handler(FlutterErrorDetails details) {
        if (_flutterDispatchDepth == 0) {
          try {
            _recordFlutterError(details);
          } on Object {
            // Diagnostic observation must never break the app's error handler.
          }
        }
        _flutterDispatchDepth++;
        try {
          previous?.call(details);
        } finally {
          _flutterDispatchDepth--;
        }
      }

      _ourFlutterHandler = handler;
      _flutterPrevious = previous;
      FlutterError.onError = handler;
    }
    final ui.ErrorCallback? currentDispatcher = _dispatcher.onError;
    if (currentDispatcher == null ||
        !identical(currentDispatcher, _ourDispatcherHandler)) {
      final ui.ErrorCallback? previous = currentDispatcher;
      bool handler(Object error, StackTrace stack) {
        if (_dispatcherDispatchDepth == 0) {
          try {
            _recordUncaught(error, stack);
          } on Object {
            // Diagnostic observation must never break the app's error handler.
          }
        }
        _dispatcherDispatchDepth++;
        try {
          // Keep the return contract: without a previous handler the engine
          // reports the error as unhandled, exactly as before.
          return previous?.call(error, stack) ?? false;
        } finally {
          _dispatcherDispatchDepth--;
        }
      }

      _ourDispatcherHandler = handler;
      _dispatcherPrevious = previous;
      _dispatcher.onError = handler;
    }
  }

  /// Stops recording and restores the previous handlers when Tapwing's are
  /// still the outermost ones. Wrappers buried inside an app handler's chain
  /// stay there as pass-throughs (they record nothing once uninstalled).
  void uninstall() {
    _installed = false;
    if (_ourFlutterHandler != null &&
        identical(FlutterError.onError, _ourFlutterHandler)) {
      FlutterError.onError = _flutterPrevious;
    }
    if (_ourDispatcherHandler != null &&
        identical(_dispatcher.onError, _ourDispatcherHandler)) {
      _dispatcher.onError = _dispatcherPrevious;
    }
    _ourFlutterHandler = null;
    _flutterPrevious = null;
    _ourDispatcherHandler = null;
    _dispatcherPrevious = null;
  }

  /// Records overflows that are visible now (profile and release builds
  /// only). Call after the UI settled.
  void scanOverflows() {
    // Only when the host asked for issues: older hosts and `--app-errors
    // off` pay nothing for the scan.
    if (!_installed || !detectOverflows || !log.deliveryEnabled) {
      return;
    }
    final List<LayoutOverflow> overflows;
    try {
      overflows = OverflowDetector.scan();
    } on Object {
      return;
    }
    for (final LayoutOverflow overflow in overflows) {
      if (_reportedOverflow[overflow.renderObject] ?? false) {
        continue;
      }
      _reportedOverflow[overflow.renderObject] = true;
      final String where = OverflowDetector.formatRect(overflow.globalBounds);
      log.record(
        kind: AppIssueKind.layoutOverflow,
        message: overflow.message,
        library: 'rendering library',
        summary:
            '${overflow.message}\nAt $where (logical px)'
            '${overflow.text == null ? '' : ', containing "${overflow.text}"'}.',
      );
    }
  }

  void _recordFlutterError(FlutterErrorDetails details) {
    if (!_installed || _alreadyRecorded(details.exception)) {
      return;
    }
    final AppIssueKind kind = classifyFlutterError(details);
    final String rawSummary = _describe(details);
    final String summary =
        kind == AppIssueKind.setupWarning &&
            isZoneMismatchError(details.exceptionAsString())
        ? '$rawSummary\n\nTapwing setup hint: initialize TapwingBinding inside '
              'your zone / appRunner callback (see docs/setup.md).'
        : rawSummary;
    log.record(
      kind: kind,
      message: details.exceptionAsString(),
      library: details.library,
      summary: summary,
      stack: details.stack,
    );
  }

  void _recordUncaught(Object error, StackTrace stack) {
    if (!_installed || _alreadyRecorded(error)) {
      return;
    }
    final bool isZoneMismatch =
        error is FlutterError && isZoneMismatchError(error.toString());
    final AppIssueKind kind = isZoneMismatch
        ? AppIssueKind.setupWarning
        : error is FlutterError
        ? AppIssueKind.frameworkAssertion
        : AppIssueKind.uncaughtError;
    final String rawSummary = '$error';
    final String summary = isZoneMismatch
        ? '$rawSummary\n\nTapwing setup hint: initialize TapwingBinding inside '
              'your zone / appRunner callback (see docs/setup.md).'
        : rawSummary;
    log.record(
      // A framework assertion thrown from async code (`setState() called
      // after dispose()` in a timer) is still a framework assertion.
      kind: kind,
      message: error is FlutterError
          ? error.toString()
          : '${error.runtimeType}: $error',
      summary: summary,
      stack: stack,
    );
  }

  bool _alreadyRecorded(Object error) {
    // Expando keys reject primitives, Records, Symbols, and FFI pointers;
    // catch ArgumentError safely (nested wrappers are already deduplicated by
    // the dispatch-depth guard).
    if (error is String || error is num || error is bool || error is Symbol) {
      return false;
    }
    try {
      if (_recorded[error] ?? false) {
        return true;
      }
      _recorded[error] = true;
      return false;
    } on ArgumentError {
      return false;
    }
  }

  static String _describe(FlutterErrorDetails details) {
    try {
      // Without the stack, which is sent separately and trimmed.
      return details
          .toString(minLevel: DiagnosticLevel.info)
          .split('\nWhen the exception was thrown')[0];
    } on Object {
      return details.exceptionAsString();
    }
  }
}
