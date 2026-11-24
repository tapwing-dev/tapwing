import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart' show AppLifecycleState;

import '../platform.dart';
import '../protocol/rpc_error.dart';
import '../protocol/rpc_messages.dart';
import 'driver_context.dart';

/// Text input, keyboard, hardware key, and navigation RPC handlers
/// (`inputText`, `eraseText`, `hideKeyboard`, `back`, `armBack`, `awaitBack`,
/// `pressKey`).
class InputNavigationHandlers {
  /// Creates input and navigation handlers operating against [_context].
  InputNavigationHandlers(this._context);

  static const String _hideKeyboardBackWarning =
      'hideKeyboard called without visible soft keyboard; navigated back '
      '(Maestro Android quirk)';

  /// Warning emitted when iOS `hideKeyboard` succeeds although an editable
  /// field keeps focus (stock Maestro iOS would fail here; see
  /// `spikes/ios/hide-keyboard/REPORT.md`).
  static const String iosHideKeyboardFocusWarning =
      'hideKeyboard: soft keyboard hidden, the text field keeps focus (like '
      'Maestro Android). Maestro iOS passes this step only if the app '
      'unfocuses on taps outside the field (EditableTextTapOutsideIntent / '
      'TextField.onTapOutside), since Flutter on iOS does not by default; '
      'otherwise it fails with "Couldn\'t hide the keyboard". Run with '
      '--maestro-compat=strict to check.';

  static const Duration _iosMicroSwipe = Duration(milliseconds: 50);
  static const Duration _backPollInterval = Duration(milliseconds: 2);

  final DriverContext _context;

  /// State recorded by `armBack` right before the host injects a real
  /// Android `KEYCODE_BACK`.
  _BackArm? _backArm;

  /// Handles `inputText`: types text into the currently focused editable field.
  Future<Object?> handleInputText(Map<String, Object?> raw) async {
    final InputTextParams params = InputTextParams.fromJson(raw);
    final int logMark = _context.textInput.log.mark;
    if (!_context.textInput.hasClient) {
      // Maestro (`AndroidDriver.kt:547-559`, `MaestroDriverService.kt:316-332`,
      // `Orchestra.kt:1869-1872`) does not fail when no text field is focused.
      // Fail only when opt-in `--strict-input` is enabled; otherwise succeed
      // with a warning and `ime.noClient: true`.
      if (_context.strictInput) {
        throw DriverError.noFocus();
      }
      return InputTextResult(
        settle: 'idle',
        ime: _context.textInput.diagnosticsSince(logMark, noClient: true),
        warning: 'No focused text field; inputText ignored (Maestro parity)',
      ).toJson();
    }
    final int? usedClient = _context.textInput.clientId;
    await _context.textInput.type(params.text);
    final String settle = await _context.settle(params.settleTimeout);
    return InputTextResult(
      settle: settle,
      ime: _context.textInput.diagnosticsSince(logMark, usedClient: usedClient),
    ).toJson();
  }

  /// Handles `eraseText`: deletes characters from the focused editable field.
  Future<Object?> handleEraseText(Map<String, Object?> raw) async {
    final EraseTextParams params = EraseTextParams.fromJson(raw);
    if (!_context.textInput.hasClient) {
      // Maestro (`AndroidDriver.kt:685-695`, `MaestroDriverService.kt:297-314`)
      // sends KEYCODE_DEL without failing if no field is focused.
      if (_context.strictInput) {
        throw DriverError.noFocus();
      }
      return const SettleResult(
        settle: 'idle',
        warning: 'No focused text field; eraseText ignored (Maestro parity)',
      ).toJson();
    }
    await _context.textInput.erase(params.count);
    final String settle = await _context.settle(params.settleTimeout);
    return SettleResult(settle: settle).toJson();
  }

  /// Handles `hideKeyboard` with Android and iOS Maestro parity semantics.
  Future<Object?> handleHideKeyboard(Map<String, Object?> raw) async {
    final SettleParams params = SettleParams.fromJson(raw);
    if (_context.platform == DriverPlatform.ios) {
      return _hideKeyboardIos(params);
    }
    // AndroidDriver.kt:490-496: Maestro's `hideKeyboard` sends `input keyevent 4`
    // (Back). When the soft keyboard is visible, Android's IME consumes Back
    // and hides the keyboard while keeping focus; when no keyboard is visible,
    // Android delivers Back to navigation (`popRoute`).
    if (_context.readBottomInset() > 0) {
      _context.textInput.hideKeyboard();
      final String settle = await _context.settle(params.settleTimeout);
      return SettleResult(settle: settle).toJson();
    }
    // The host CLI sends the real KEYCODE_BACK itself (`armBack` /
    // `awaitBack`) so the OS decides where it goes.
    if (raw['hostBack'] == true) {
      return <String, Object?>{
        'settle': 'idle',
        'hostBack': true,
        'warning': _hideKeyboardBackWarning,
      };
    }
    _context.pointer.back();
    final String settle = await _context.settle(params.settleTimeout);
    return SettleResult(
      settle: settle,
      warning: _hideKeyboardBackWarning,
    ).toJson();
  }

  /// Handles `back`: dispatches Android Back or returns a no-op on iOS.
  Future<Object?> handleBack(Map<String, Object?> raw) async {
    final SettleParams params = SettleParams.fromJson(raw);
    if (_context.platform == DriverPlatform.ios) {
      // IOSDriver.backPress() is a no-op (`IOSDriver.kt:372`).
      return const SettleResult(settle: 'idle', noop: true).toJson();
    }
    _dispatchAndroidBack();
    final String settle = await _context.settle(params.settleTimeout);
    return SettleResult(settle: settle).toJson();
  }

  /// `armBack`: snapshot the Back-related state before the host CLI injects a
  /// real `KEYCODE_BACK` (`cmd input keyevent 4`), like Maestro's
  /// `AndroidDriver.backPress`. Android then decides where Back goes: the IME
  /// (closes the keyboard), the Flutter framework (`popRoute` /
  /// `commitBackGesture`), or, with `enableOnBackInvokedCallback="true"` and a
  /// framework that reported it cannot handle Back, the OS itself, which
  /// finishes or backgrounds the activity.
  Future<Object?> handleArmBack(Map<String, Object?> raw) async {
    _backArm = _captureBackArm();
    return <String, Object?>{'armed': true};
  }

  /// `awaitBack`: wait (up to `timeoutMs`, default 1000) until the injected
  /// Back had an effect, then settle. Outcomes: `navigation` (delivered to the
  /// framework), `keyboard` (the IME consumed it), `background` (the app left
  /// the foreground; no settle, frames stop) or `none` (nothing observable).
  Future<Object?> handleAwaitBack(Map<String, Object?> raw) async {
    final SettleParams params = SettleParams.fromJson(raw);
    final Object? rawTimeout = raw['timeoutMs'];
    final Duration timeout = Duration(
      milliseconds: rawTimeout is num ? rawTimeout.toInt() : 1000,
    );
    final _BackArm arm = _backArm ?? _captureBackArm();
    _backArm = null;
    final Stopwatch deadline = Stopwatch()..start();
    Duration waited = Duration.zero;
    String outcome = 'none';
    while (!_context.isCancelled()) {
      if (_context.backSignals.deliveries != arm.deliveries) {
        outcome = 'navigation';
        break;
      }
      final AppLifecycleState? life = _context.readLifecycleState();
      if (life != null && life != AppLifecycleState.resumed) {
        outcome = 'background';
        break;
      }
      if (arm.bottomInset > 0 && _context.readBottomInset() < arm.bottomInset) {
        outcome = 'keyboard';
        break;
      }
      if (deadline.elapsed >= timeout || waited >= timeout) break;
      await Future<void>.delayed(_backPollInterval);
      waited += _backPollInterval;
    }
    if (outcome == 'background') {
      return <String, Object?>{'settle': 'idle', 'outcome': outcome};
    }
    final String settle = await _context.settle(params.settleTimeout);
    return <String, Object?>{'settle': settle, 'outcome': outcome};
  }

  /// Handles `pressKey` for in-app keys routed at the engine boundary.
  Future<Object?> handlePressKey(Map<String, Object?> raw) async {
    final PressKeyParams params = PressKeyParams.fromJson(raw);
    // IOSDriver.pressKey (`IOSDriver.kt:167-189`) only maps BACKSPACE ->
    // "delete" and ENTER -> "return" (plus HOME/LOCK buttons on the host);
    // every other key is silently ignored.
    if (_context.platform == DriverPlatform.ios &&
        params.key != 'ENTER' &&
        params.key != 'BACKSPACE') {
      return const SettleResult(settle: 'idle', noop: true).toJson();
    }
    switch (params.key) {
      case 'ENTER':
        if (_context.textInput.hasClient) {
          await _context.textInput.performAction();
        } else {
          await _context.pointer.pressKey(
            PhysicalKeyboardKey.enter,
            LogicalKeyboardKey.enter,
          );
        }
      case 'BACKSPACE':
        if (_context.textInput.hasClient) {
          await _context.textInput.erase(1);
        } else {
          await _context.pointer.pressKey(
            PhysicalKeyboardKey.backspace,
            LogicalKeyboardKey.backspace,
          );
        }
      case 'TAB':
        await _context.pointer.pressKey(
          PhysicalKeyboardKey.tab,
          LogicalKeyboardKey.tab,
        );
      case 'ESCAPE':
        await _context.pointer.pressKey(
          PhysicalKeyboardKey.escape,
          LogicalKeyboardKey.escape,
        );
      case 'BACK':
        _dispatchAndroidBack();
      default:
        throw DriverError.invalidCommand(
          'Unsupported in-app pressKey code: ${params.key}',
        );
    }
    await Future<void>.delayed(Duration.zero);
    final String settle = await _context.settle(params.settleTimeout);
    return SettleResult(settle: settle).toJson();
  }

  /// iOS `hideKeyboard`.
  ///
  /// Default: hide the soft keyboard via `TextInput.hide` (same post-state as
  /// Maestro Android: keyboard gone, focus kept) and warn when an editable
  /// field stays focused. Without a focused field it succeeds silently like
  /// `IOSDriver.hideKeyboard` (no Android `back` fallback).
  ///
  /// Strict (`strictHideKeyboard`): emulate `IOSDriver.hideKeyboard`
  /// (`IOSDriver.kt:374-409`): two 50 ms micro-swipes from the screen center,
  /// then fail with Maestro's message if an editable field is still focused.
  /// The hardware-keyboard false positive of Maestro's runner is never
  /// emulated.
  Future<Object?> _hideKeyboardIos(SettleParams params) async {
    bool focused() => _context.textInput.hasClient;
    if (_context.strictHideKeyboard) {
      if (focused()) {
        final ui.Size screen = _context.readScreenSize();
        final int w = screen.width.round();
        final int h = screen.height.round();
        final ui.Offset center = ui.Offset(
          (w * 0.5).toInt().toDouble(),
          (h * 0.5).toInt().toDouble(),
        );
        final List<ui.Offset> ends = <ui.Offset>[
          ui.Offset(center.dx, (h * 0.47).toInt().toDouble()),
          ui.Offset((w * 0.47).toInt().toDouble(), center.dy),
        ];
        for (final ui.Offset end in ends) {
          await _context.drag(center, end, _iosMicroSwipe);
          await Future<void>.delayed(Duration.zero);
          await _context.settle(params.settleTimeout);
          if (!focused()) break;
        }
        if (focused()) {
          throw DriverError.hideKeyboardFailure();
        }
      }
      final String settle = await _context.settle(params.settleTimeout);
      return SettleResult(settle: settle).toJson();
    }
    final bool wasFocused = focused();
    if (wasFocused ||
        _context.textInput.keyboardRequested ||
        _context.readBottomInset() > 0) {
      _context.textInput.hideKeyboard();
    }
    final String settle = await _context.settle(params.settleTimeout);
    return SettleResult(
      settle: settle,
      warning: focused() ? iosHideKeyboardFocusWarning : null,
    ).toJson();
  }

  _BackArm _captureBackArm() => _BackArm(
    deliveries: _context.backSignals.deliveries,
    bottomInset: _context.readBottomInset(),
  );

  /// Mirrors Android's `KEYCODE_BACK` delivery (`AndroidDriver.kt:292, 483-488`):
  /// if the soft keyboard is currently open (`viewInsets.bottom > 0`), the IME
  /// consumes Back and dismisses the keyboard without popping the route; only
  /// when no soft keyboard is open does Back trigger `popRoute`.
  void _dispatchAndroidBack() {
    if (_context.readBottomInset() > 0) {
      _context.textInput.hideKeyboard();
    } else {
      _context.pointer.back();
    }
  }
}

/// Back-related state captured by `armBack`.
class _BackArm {
  const _BackArm({required this.deliveries, required this.bottomInset});

  final int deliveries;
  final double bottomInset;
}
