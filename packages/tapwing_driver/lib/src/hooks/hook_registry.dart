import 'dart:async';

import 'package:flutter/widgets.dart';

/// Signature for an app-registered test hook callable via `callHook` in a
/// Tapwing YAML flow.
///
/// Handlers receive a mounted [BuildContext] from the active visible route (so
/// `Navigator.of(context)`, `GoRouter.of(context)`, `Theme.of(context)`,
/// `Provider`, and `Bloc` lookups work directly without a `GlobalKey`) and the
/// evaluated `args` JSON map from the flow. They may return any JSON-encodable
/// value (or `Future<Object?>`).
typedef TapwingHookHandler = FutureOr<Object?> Function(
  BuildContext context,
  Map<String, Object?> args,
);

/// Resolves a live, mounted [BuildContext] from the top-most visible route
/// under [root] (defaulting to [WidgetsBinding.instance] `rootElement`).
///
/// Skips unmounted subtrees, [Offstage] subtrees with `offstage: true`, and
/// [TickerMode] subtrees with `enabled: false`, preferring descendants inside a
/// [Navigator] so navigator, router, and app-level inherited lookups succeed.
BuildContext? resolveActiveHookContext({Element? root}) {
  final Element? rootElement = root ?? WidgetsBinding.instance.rootElement;
  if (rootElement == null || !rootElement.mounted) return null;

  Element? navigatorLeaf;
  Element? deepestLeaf;

  void walk(Element element, {required bool inNavigator}) {
    if (!element.mounted) return;
    final Widget widget = element.widget;
    if (widget is Offstage && widget.offstage) return;
    if (widget is TickerMode && !widget.enabled) return;

    deepestLeaf = element;
    final bool nowInNavigator = inNavigator || widget is Navigator;
    if (inNavigator) {
      navigatorLeaf = element;
    }
    element.visitChildren((Element child) {
      walk(child, inNavigator: nowInNavigator);
    });
  }

  walk(rootElement, inNavigator: false);
  return navigatorLeaf ?? deepestLeaf ?? rootElement;
}

/// Finds a live, mounted [BuildContext] for the top-most visible widget of
/// type [W] under [root] (defaulting to [WidgetsBinding.instance] `rootElement`).
BuildContext? resolveHookContextOf<W extends Widget>({Element? root}) {
  final Element? rootElement = root ?? WidgetsBinding.instance.rootElement;
  if (rootElement == null || !rootElement.mounted) return null;

  Element? match;

  void walk(Element element) {
    if (!element.mounted) return;
    final Widget widget = element.widget;
    if (widget is Offstage && widget.offstage) return;
    if (widget is TickerMode && !widget.enabled) return;

    if (widget is W) {
      match = element;
    }
    element.visitChildren(walk);
  }

  walk(rootElement);
  return match;
}

/// In-memory registry of gray-box test hooks registered by the app under test.
///
/// Production code never depends on this registry directly; hooks are registered
/// from the test entrypoint (for example `lib/main_tapwing.dart`) via
/// `TapwingBinding.ensureInitialized(hooks: ...)` or
/// `TapwingBinding.registerHook`.
class HookRegistry {
  /// Shared singleton used by the default `TapwingBinding` and `DriverContext`.
  static final HookRegistry instance = HookRegistry();

  final Map<String, TapwingHookHandler> _hooks = <String, TapwingHookHandler>{};

  /// Registers [handler] under [name], replacing any previous handler with the
  /// same name.
  ///
  /// Throws an [ArgumentError] if [name] is empty or whitespace-only.
  void register(String name, TapwingHookHandler handler) {
    if (name.trim().isEmpty) {
      throw ArgumentError.value(name, 'name', 'Hook name must not be empty');
    }
    _hooks[name] = handler;
  }

  /// Registers every `(name, handler)` pair in [hooks].
  void registerAll(Map<String, TapwingHookHandler> hooks) {
    for (final MapEntry<String, TapwingHookHandler> entry in hooks.entries) {
      register(entry.key, entry.value);
    }
  }

  /// Removes the hook registered under [name], returning `true` if one existed.
  bool unregister(String name) => _hooks.remove(name) != null;

  /// Removes all registered hooks.
  void clear() => _hooks.clear();

  /// Whether a hook named [name] is currently registered.
  bool has(String name) => _hooks.containsKey(name);

  /// Sorted list of currently registered hook names.
  List<String> get names => _hooks.keys.toList()..sort();

  /// Returns the handler registered for [name], or `null` when absent.
  TapwingHookHandler? lookup(String name) => _hooks[name];
}
