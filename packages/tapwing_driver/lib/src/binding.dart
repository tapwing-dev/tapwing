import 'package:flutter/semantics.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'diagnostics/error_hooks.dart';
import 'idle/idle_detector.dart';
import 'input/text_input.dart';
import 'server/tapwing_server.dart';

/// Binding mixin that wires the Tapwing driver into a Flutter application.
///
/// Mix this into your custom binding if the app already defines one (for
/// example with Sentry or crash-reporting bindings), and apply it **last** so
/// its [createBinaryMessenger] wrapper observes `flutter/textinput` traffic.
mixin TapwingBindingMixin on WidgetsBinding {
  SemanticsHandle? _semantics;

  @override
  void initInstances() {
    super.initInstances();
    if (!isDriverEnabled()) {
      // A release build with the Tapwing entrypoint: behave like the plain
      // binding (no forced semantics, no hooks, no connection to a host).
      debugPrint(
        'tapwing_driver: disabled in release builds. Build in profile or '
        'debug mode, or pass --dart-define=TAPWING_ALLOW_RELEASE=true.',
      );
      return;
    }
    // Enable the same semantics tree Maestro reads via Android's
    // AccessibilityBridge.
    _semantics = SemanticsBinding.instance.ensureSemantics();
    IdleDetector.instance.install();
    // Capture app health issues from the very first frame (F7). The host
    // enables delivery (or uninstalls) via `configure` `appIssues`.
    AppErrorHooks.instance.install();
    // Apps typically install crash reporters in main() after the binding;
    // re-wrap once the first frame is out, then at every RPC.
    addPostFrameCallback((_) => AppErrorHooks.instance.ensureInstalled());
    TapwingServer.instance.start();
  }

  @override
  void drawFrame() {
    // A crash reporter that replaces `FlutterError.onError` between two RPCs
    // (a consent-gated Sentry/Crashlytics init, say) must not hide this
    // frame's build/layout/paint errors from Tapwing: re-wrap first. Two
    // identity checks; a no-op while our handler is still outermost.
    AppErrorHooks.instance.ensureInstalled();
    super.drawFrame();
  }

  @override
  BinaryMessenger createBinaryMessenger() => isDriverEnabled()
      ? ObservingBinaryMessenger(super.createBinaryMessenger())
      : super.createBinaryMessenger();

  /// Retained semantics handle keeping the semantics tree active for the
  /// lifetime of the process.
  SemanticsHandle? get debugTapwingSemanticsHandle => _semantics;
}

/// Drop-in [WidgetsFlutterBinding] for `lib/main_tapwing.dart`:
///
/// ```dart
/// import 'package:tapwing_driver/tapwing_driver.dart';
/// import 'package:my_app/main.dart' as app;
///
/// void main() {
///   TapwingBinding.ensureInitialized(); // must run before app.main()
///   app.main();
/// }
/// ```
class TapwingBinding extends WidgetsFlutterBinding with TapwingBindingMixin {
  TapwingBinding._();

  static TapwingBinding? _instance;

  /// Initializes and returns the Tapwing-instrumented [WidgetsBinding].
  ///
  /// Must be called before `app.main()` so the binding singleton is registered
  /// prior to `runApp()` or `WidgetsFlutterBinding.ensureInitialized()`.
  static WidgetsBinding ensureInitialized() {
    _instance ??= TapwingBinding._();
    return WidgetsBinding.instance;
  }
}
