import 'package:flutter/semantics.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'diagnostics/error_hooks.dart';
import 'hooks/hook_registry.dart';
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
    // Apps typically install crash reporters or HttpOverrides in main() after
    // the binding; re-wrap once the first frame is out, then at every RPC.
    addPostFrameCallback((_) {
      AppErrorHooks.instance.ensureInstalled();
      IdleDetector.instance.httpTracker.ensureInstalled();
    });
    TapwingServer.instance.start();
  }

  @override
  void drawFrame() {
    // A crash reporter or HttpOverrides installed between two RPCs must not
    // hide this frame's errors or HTTP requests from Tapwing: re-wrap first.
    if (isDriverEnabled()) {
      AppErrorHooks.instance.ensureInstalled();
      IdleDetector.instance.httpTracker.ensureInstalled();
    }
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
  /// Optional [hooks] are registered via [registerHooks] when the driver is
  /// enabled.
  static WidgetsBinding ensureInitialized({
    Map<String, TapwingHookHandler>? hooks,
  }) {
    _instance ??= TapwingBinding._();
    if (hooks != null) {
      registerHooks(hooks);
    }
    return WidgetsBinding.instance;
  }

  /// Registers a gray-box in-app test hook callable via `callHook` in a
  /// Tapwing flow.
  ///
  /// Does nothing in release builds unless `TAPWING_ALLOW_RELEASE=true` was
  /// passed, so shipped binaries never expose test hooks.
  static void registerHook(String name, TapwingHookHandler handler) {
    if (!isDriverEnabled()) return;
    HookRegistry.instance.register(name, handler);
  }

  /// Registers all gray-box in-app test hooks in [hooks] at once.
  ///
  /// Does nothing in release builds unless `TAPWING_ALLOW_RELEASE=true` was
  /// passed.
  static void registerHooks(Map<String, TapwingHookHandler> hooks) {
    if (!isDriverEnabled()) return;
    HookRegistry.instance.registerAll(hooks);
  }

  /// Removes a previously registered in-app hook, returning `true` if one was
  /// registered.
  static bool unregisterHook(String name) {
    if (!isDriverEnabled()) return false;
    return HookRegistry.instance.unregister(name);
  }

  /// Clears all registered in-app hooks.
  static void clearHooks() {
    HookRegistry.instance.clear();
  }

  /// Returns a live [BuildContext] from the active visible route, or `null` if
  /// no widget tree is mounted yet.
  static BuildContext? get activeContext => resolveActiveHookContext();

  /// Returns a live [BuildContext] for the top-most visible widget of type [W],
  /// or `null` if none is currently mounted and visible.
  static BuildContext? contextOf<W extends Widget>() =>
      resolveHookContextOf<W>();
}
