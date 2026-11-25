import 'dart:ui' as ui;

import 'package:flutter/rendering.dart';

import '../platform.dart';
import 'semantics_hash_cache.dart';

export 'semantics_hash_cache.dart'
    show SemanticsHashCache, computeSemanticsHashFromRoot;

/// One UI element as Maestro sees it through the platform accessibility layer.
///
/// - Android: Flutter's `AccessibilityBridge` + `BaseRoleConfigurator`
///   (Flutter 3.47.5 engine), dumped by UiAutomator.
/// - iOS: Flutter's `SemanticsObject` (`SemanticsObject.mm`,
///   `TextInputSemanticsObject.mm`), dumped by XCUITest and mapped by
///   Maestro's `IOSDriver.mapViewHierarchy`.
///
/// Attribute names mirror Maestro's view hierarchy (`text`, `hintText`,
/// `accessibilityText`, `resource-id`, `bounds`) so Tapwing's selector engine
/// evaluates the exact same strings and geometry as Maestro.
class KNode {
  /// Creates a node representing a single entry from the semantics tree.
  KNode({
    required this.id,
    required this.parent,
    required this.bounds,
    required this.text,
    required this.hintText,
    required this.accessibilityText,
    required this.resourceId,
    required this.clickable,
    required this.enabled,
    required this.focused,
    required this.checked,
    required this.selected,
    required this.isTextField,
    required this.scrollable,
    this.error,
    this.platform = DriverPlatform.android,
  });

  /// Semantics node identifier assigned by Flutter's `SemanticsOwner`.
  final int id;

  /// Parent node in the captured semantics tree, or `null` for the root.
  final KNode? parent;

  /// Direct visible children in traversal order.
  final List<KNode> children = <KNode>[];

  /// Bounding box in Maestro units: physical screen pixels on Android,
  /// logical points on iOS (truncated to whole points like Maestro's
  /// `AXFrame.boundsString`).
  ///
  /// The `FlutterView` originates at `(0, 0)` when the app runs edge-to-edge
  /// (the default since Android 15), matching UiAutomator screen bounds.
  final Rect bounds;

  /// Editable value of a text field, or `''` for non-text-field nodes captured
  /// on Android (matching `maestro-android/.../ViewHierarchy.kt:127` and
  /// `AndroidDriver.kt:1152-1155`, which always emit `text=""`). May be `null`
  /// on synthetic nodes in tests.
  final String? text;

  /// Composed `"label, hint"` string for a text field, or `null` when empty.
  final String? hintText;

  /// Composed `"value, label, hint"` (or fallback `tooltip`) string as Android
  /// `contentDescription`, matching `AccessibilityBridge`.
  final String? accessibilityText;

  /// `Semantics.identifier`, mapped to Android `resource-id`.
  ///
  /// Empty string when unset because Maestro's Android dumper always emits
  /// `resource-id=""`, allowing regexes such as `id: ".*"` to match.
  final String resourceId;

  /// Whether the node advertises [SemanticsAction.tap].
  final bool clickable;

  /// Whether the node is enabled (`SemanticsFlags.isEnabled != isFalse`).
  final bool enabled;

  /// Whether the node currently holds accessibility/input focus.
  final bool focused;

  /// Whether the node is checked or toggled on.
  final bool checked;

  /// Whether the node is marked selected.
  final bool selected;

  /// Whether the node represents an editable text field.
  final bool isTextField;

  /// Whether the node advertises any scroll action.
  final bool scrollable;

  /// Optional Android accessibility error text (`AccessibilityNodeInfo.error`).
  ///
  /// Always `null` for pure Flutter UI: Flutter engine 3.47.5 only calls
  /// `setError` in `AccessibilityViewEmbedder` for platform views. Retained for
  /// selector/copyTextFrom parity with Maestro (`Filters.kt:58-132`,
  /// `Orchestra.kt:1861-1863`) and future native-lane platform views.
  final String? error;

  /// Platform whose Maestro attribute composition this node follows.
  final DriverPlatform platform;

  /// Returns `true` if [other] is an ancestor of this node in the snapshot.
  bool isDescendantOf(KNode other) {
    for (KNode? p = parent; p != null; p = p.parent) {
      if (identical(p, other)) return true;
    }
    return false;
  }

  /// Serializes this node to the Maestro-compatible JSON wire shape.
  ///
  /// When [withChildren] is `true`, recursively embeds non-empty [children]
  /// (used for `snapshot` hierarchy dumps).
  ///
  /// iOS nodes carry `""` for every absent attribute (as XCUITest does), so
  /// empty strings are omitted from their JSON to keep dumps readable.
  Map<String, Object?> toJson({bool withChildren = false}) {
    final bool ios = platform == DriverPlatform.ios;
    return _toJson(ios, withChildren);
  }

  Map<String, Object?> _toJson(
    bool ios,
    bool withChildren,
  ) => <String, Object?>{
    'id': id,
    if (text != null && ((isTextField && !ios) || text!.isNotEmpty))
      'text': text,
    if (hintText != null && (!ios || hintText!.isNotEmpty))
      'hintText': hintText,
    if (accessibilityText != null && (!ios || accessibilityText!.isNotEmpty))
      'accessibilityText': accessibilityText,
    if (error != null && error!.isNotEmpty) 'error': error,
    if (resourceId.isNotEmpty) 'resource-id': resourceId,
    'bounds': <int>[
      bounds.left.round(),
      bounds.top.round(),
      bounds.right.round(),
      bounds.bottom.round(),
    ],
    if (clickable) 'clickable': true,
    if (!enabled) 'enabled': false,
    if (focused) 'focused': true,
    if (checked) 'checked': true,
    if (selected) 'selected': true,
    if (isTextField) 'textField': true,
    if (scrollable) 'scrollable': true,
    if (withChildren && children.isNotEmpty)
      'children': children.map((KNode c) => c._toJson(ios, true)).toList(),
  };
}

/// A flat, pre-order (traversal-order) capture of the live semantics tree.
///
/// Only reads public framework semantics data (black-box contract); never
/// inspects widget state or render internals beyond geometry.
class Snapshot {
  /// Creates a snapshot from pre-built nodes, screen size, structural hash,
  /// and optional visible platform-view coverage (`0.0`–`1.0`).
  Snapshot(
    this.root,
    this.nodes,
    this.screen,
    this.hash, {
    double platformViewCoverage = 0.0,
  }) : platformViewCoverage = platformViewCoverage.isFinite
           ? platformViewCoverage.clamp(0.0, 1.0)
           : 0.0;

  /// Root semantics node, or `null` if no semantics tree is attached yet.
  final KNode? root;

  /// Pre-order traversal of all visible nodes (Maestro's `aggregate()`).
  final List<KNode> nodes;

  /// Screen size in Maestro units (physical pixels on Android, logical points
  /// on iOS).
  final Size screen;

  /// Cheap structural hash covering text, identifiers, flags, and rounded
  /// bounds.
  ///
  /// Used by the settle detector to distinguish paint-only infinite animations
  /// (spinners, cursors) from layout or content updates.
  final int hash;

  /// Fraction of the screen area (`0.0`–`1.0`) covered by visible on-screen
  /// platform views (`AndroidView`, `UiKitView`, `WebView`).
  final double platformViewCoverage;

  /// Whether at least one visible platform view intersects the screen.
  bool get hasPlatformView => platformViewCoverage > 0;

  /// Returns the deepest, last-painted node in this snapshot containing ([x], [y]),
  /// matching Maestro `ViewHierarchy.getElementAt` (`ViewHierarchy.kt:65-95`).
  KNode? elementAt(double x, double y) {
    final KNode? r = root;
    if (r == null) return null;
    final ui.Offset point = ui.Offset(x, y);
    KNode? findIn(KNode node) {
      for (int i = node.children.length - 1; i >= 0; i--) {
        final KNode child = node.children[i];
        if (child.children.isNotEmpty) {
          final KNode? deeper = findIn(child);
          if (deeper != null) return deeper;
        }
        if (child.bounds.contains(point)) {
          return child;
        }
      }
      return null;
    }

    return findIn(r);
  }

  /// Returns `true` if [node] is the topmost element at its own center,
  /// matching Maestro `ViewHierarchy.isVisible` (`ViewHierarchy.kt:40-50`).
  bool isVisible(KNode node) {
    final ui.Offset center = node.bounds.center;
    final KNode? atCenter = elementAt(center.dx, center.dy);
    return atCenter != null && atCenter.id == node.id;
  }

  /// Captures the current semantics tree from the primary [RenderView].
  ///
  /// [platform] selects Maestro's attribute composition and unit geometry
  /// (see [DriverPlatform]). On iOS, a non-null [appName] adds the
  /// application element Maestro's XCUITest dump starts with
  /// (`accessibilityText = <CFBundleDisplayName>`, full-screen bounds).
  ///
  /// Each [SemanticsNode] calls `getSemanticsData()` at most once because
  /// Flutter allocates a fresh `SemanticsData` and merges descendant subtrees
  /// on every call.
  static Snapshot capture({
    DriverPlatform platform = DriverPlatform.android,
    String? appName,
    SemanticsHashCache? hashCache,
  }) {
    final bool ios = platform == DriverPlatform.ios;
    final double unit = ios ? _primaryDevicePixelRatio() : 1.0;
    final Size physical = _primaryScreenSize();
    final Size screen = ios
        ? Size(
            _truncUnits(physical.width / unit),
            _truncUnits(physical.height / unit),
          )
        : physical;
    final SemanticsOwner? owner = SemanticsHashCache.defaultSemanticsOwner();
    final SemanticsNode? rootNode = owner?.rootSemanticsNode;
    if (owner == null || rootNode == null) {
      return Snapshot(null, const <KNode>[], screen, 0);
    }

    final List<KNode> out = <KNode>[];
    int hash = 17;
    final Rect screenRect = Offset.zero & screen;
    final double screenArea = screen.width * screen.height;
    double platformViewArea = 0.0;

    // IOSDriver (`ViewHierarchyHandler.swift`): the hierarchy starts at the
    // XCUIApplication element, labelled with the app's display name.
    KNode? app;
    if (ios && appName != null) {
      app = KNode(
        id: -1,
        parent: null,
        bounds: Offset.zero & screen,
        text: '',
        hintText: '',
        accessibilityText: appName,
        resourceId: '',
        clickable: false,
        enabled: true,
        focused: false,
        checked: false,
        selected: false,
        isTextField: false,
        scrollable: false,
        platform: DriverPlatform.ios,
      );
      out.add(app);
    }

    KNode visit(
      SemanticsNode node,
      Matrix4 parentTransform,
      KNode? parent,
      SemanticsData data,
    ) {
      final Matrix4 transform = composeSemanticsTransform(
        parentTransform,
        node.transform,
      );
      final Rect rawRect = MatrixUtils.transformRect(transform, node.rect);
      final Rect rect = rawRect.isFinite ? rawRect : Rect.zero;
      final bool clickable = data.hasAction(SemanticsAction.tap);
      final bool scrollable = isSemanticsScrollable(data);
      final ExtractedSemanticsFlags flags = extractSemanticsFlags(
        data.flagsCollection,
        clickable: clickable,
        scrollable: scrollable,
      );

      final KNode k;
      if (ios) {
        final _IosAttributes a = _composeIos(data);
        k = KNode(
          id: node.id,
          parent: parent,
          bounds: _toIosPoints(rect, unit),
          text: a.text,
          hintText: '',
          accessibilityText: a.label,
          resourceId: data.identifier,
          // XCUITest exposes no `clickable` attribute, so Maestro iOS never
          // reorders matches by it (`Filters.clickableFirst`).
          clickable: false,
          enabled: a.enabled,
          // XCUIElement.hasFocus is practically always false for Flutter.
          focused: false,
          checked: a.checked,
          selected: flags.selected,
          isTextField: data.flagsCollection.isTextField,
          scrollable: scrollable,
          platform: DriverPlatform.ios,
        );
      } else {
        final _ComposedText composed = _composeText(data);
        k = KNode(
          id: node.id,
          parent: parent,
          bounds: rect,
          text: composed.text,
          hintText: composed.hintText,
          accessibilityText: composed.accessibilityText,
          resourceId: data.identifier,
          clickable: clickable,
          enabled: flags.enabled,
          focused: flags.focused,
          checked: flags.checked,
          selected: flags.selected,
          isTextField: data.flagsCollection.isTextField,
          scrollable: scrollable,
        );
      }
      out.add(k);
      hash = accumulateSemanticsHash(hash, data, flags.bits, rect);
      parent?.children.add(k);

      final int? platformViewId = data.platformViewId;
      if (platformViewId != null &&
          platformViewId >= 0 &&
          !data.flagsCollection.isHidden &&
          k.bounds.isFinite &&
          !k.bounds.isEmpty) {
        final Rect clipped = k.bounds.intersect(screenRect);
        if (!clipped.isEmpty) {
          platformViewArea += clipped.width * clipped.height;
        }
      }

      // Merged descendants are already folded into getSemanticsData() and are
      // not emitted to the platform as separate accessibility nodes.
      if (!node.mergeAllDescendantsIntoThisNode) {
        for (final SemanticsNode child in node.debugListChildrenInOrder(
          DebugSemanticsDumpOrder.traversalOrder,
        )) {
          final SemanticsData childData = child.getSemanticsData();
          // The Android accessibility bridge omits hidden children (IS_HIDDEN).
          if (childData.flagsCollection.isHidden) continue;
          // Flutter 3.41+ grafts OverlayPortal / traversalChild nodes under
          // their traversalParent in traversalOrder, while `child.transform`
          // stays relative to its structural `child.parent` (e.g. the Overlay).
          // Re-anchor to the structural parent's global transform so portal
          // bounds are never double-translated.
          final Matrix4 childParentTransform = identical(child.parent, node)
              ? transform
              : globalSemanticsTransform(child.parent);
          visit(child, childParentTransform, k, childData);
        }
      }
      return k;
    }

    final KNode flutterRoot = visit(
      rootNode,
      Matrix4.identity(),
      app,
      rootNode.getSemanticsData(),
    );
    (hashCache ?? SemanticsHashCache.instance).seed(owner, hash);
    final double platformViewCoverage =
        screenArea > 0 && screenArea.isFinite && platformViewArea > 0
        ? (platformViewArea / screenArea).clamp(0.0, 1.0)
        : 0.0;
    return Snapshot(
      app ?? flutterRoot,
      out,
      screen,
      hash,
      platformViewCoverage: platformViewCoverage,
    );
  }

  /// Returns the structural hash of the semantics tree without allocating
  /// [KNode]s, child lists, or joined display strings.
  ///
  /// Backed by [SemanticsHashCache.instance], so paint-only frames and calls
  /// immediately following [capture] return in O(1) until the live
  /// [SemanticsOwner] notifies a semantics update.
  static int computeHash() => SemanticsHashCache.instance.hash();
}

/// The binding's dispatcher: `PlatformDispatcher.instance` in an app, the
/// test dispatcher (honoring `tester.view`) in widget tests.
ui.PlatformDispatcher get _dispatcher =>
    RendererBinding.instance.platformDispatcher;

Size _primaryScreenSize() {
  final ui.FlutterView? flutterView =
      _dispatcher.implicitView ?? _dispatcher.views.firstOrNull;
  return flutterView?.physicalSize ?? Size.zero;
}

double _primaryDevicePixelRatio() {
  final ui.FlutterView? flutterView =
      _dispatcher.implicitView ?? _dispatcher.views.firstOrNull;
  final double dpr = flutterView?.devicePixelRatio ?? 1.0;
  return dpr > 0 ? dpr : 1.0;
}

/// Truncates a point coordinate like Kotlin's `Double.toInt()` in Maestro's
/// `AXFrame.boundsString`, tolerating float noise from the px -> pt division
/// (`1206 / 3 = 401.99999…` must stay `402`).
double _truncUnits(double v) {
  final double t = (v + 1e-6).truncateToDouble();
  return t == 0 ? 0 : t; // Normalize -0.0.
}

/// Converts a physical-pixel rect to whole logical points (Maestro iOS
/// `bounds`: `[x.toInt(), y.toInt()][(x + w).toInt(), (y + h).toInt()]`).
Rect _toIosPoints(Rect physical, double unit) => Rect.fromLTRB(
  _truncUnits(physical.left / unit),
  _truncUnits(physical.top / unit),
  _truncUnits(physical.right / unit),
  _truncUnits(physical.bottom / unit),
);

typedef _IosAttributes = ({
  String label,
  String text,
  bool checked,
  bool enabled,
});

/// Composes Maestro iOS attributes from what Flutter's iOS embedding exposes
/// (Flutter 3.47.5 `SemanticsObject.mm`, `accessibility_bridge.mm`) as mapped
/// by `IOSDriver.mapViewHierarchy`:
/// - `accessibilityText` = `accessibilityLabel` = `label` (+ `"\n" + tooltip`).
/// - `text` = `title` (always empty for Flutter) falling back to `value`.
/// - Toggles and checkboxes outside a radio group become
///   `FlutterSwitchSemanticsObject` (XCUI `switch`, value `"1"`/`"0"`), the
///   only kind Maestro marks `checked` (`CHECKABLE_ELEMENTS`).
/// - `hint` becomes `accessibilityHint`, which Maestro does not read; Flutter
///   never sets a placeholder, so `hintText` is always empty.
_IosAttributes _composeIos(SemanticsData data) {
  final ui.SemanticsFlags f = data.flagsCollection;
  final String label = data.tooltip.isEmpty
      ? data.label
      : (data.label.isEmpty ? data.tooltip : '${data.label}\n${data.tooltip}');
  final bool hasToggle =
      f.isToggled != ui.Tristate.none || f.isChecked != ui.CheckedState.none;
  final bool on =
      f.isToggled == ui.Tristate.isTrue ||
      f.isChecked == ui.CheckedState.isTrue;
  final bool editable = f.isTextField && !f.isReadOnly;
  final bool isSwitch = !editable && !f.isInMutuallyExclusiveGroup && hasToggle;

  final String value;
  if (isSwitch) {
    value = on ? '1' : '0';
  } else if (data.value.isNotEmpty) {
    value = data.value;
  } else if (!f.isInMutuallyExclusiveGroup && hasToggle) {
    value = on ? '1' : '0';
  } else {
    value = '';
  }
  return (
    label: label,
    text: value,
    checked: isSwitch && on,
    // UISwitch is enabled only for an explicit `isEnabled: true`; other
    // objects are disabled only for an explicit `false` (trait NotEnabled).
    enabled: isSwitch
        ? f.isEnabled == ui.Tristate.isTrue
        : f.isEnabled != ui.Tristate.isFalse,
  );
}

typedef _ComposedText = ({
  String? text,
  String? hintText,
  String? accessibilityText,
});

/// Synthesizes Maestro-facing text attributes to match Android
/// `AccessibilityBridge` / `BaseRoleConfigurator` in Flutter 3.47.5:
/// - Text fields expose `value` as `text` and `"label, hint"` as `hintText`.
/// - Other nodes expose `"value, label, hint"` as `accessibilityText` (unless
///   the node is a `scopesRoute` container).
/// - `tooltip` is used as `accessibilityText` only when no primary text exists.
_ComposedText _composeText(SemanticsData data) {
  final ui.SemanticsFlags f = data.flagsCollection;
  final String value = data.value;
  final String label = data.label;
  final String hint = data.hint;
  final String tooltip = data.tooltip;

  if (f.isTextField) {
    final String? hintText = _join2(label, hint);
    final String? accessibilityText =
        tooltip.isNotEmpty && value.isEmpty && hintText == null
        ? tooltip
        : null;
    return (
      text: value,
      hintText: hintText,
      accessibilityText: accessibilityText,
    );
  }

  final String? combined = _join3(value, label, hint);
  String? accessibilityText = f.scopesRoute ? null : combined;
  if (tooltip.isNotEmpty && combined == null) {
    accessibilityText = tooltip;
  }
  // Android's ViewHierarchy dumper (`maestro-android/.../ViewHierarchy.kt:127`,
  // `AndroidDriver.kt:1152-1155`) always emits `text=""` on non-text-field
  // nodes, which affects `Filters.hasText()` and empty-string regexes.
  return (text: '', hintText: null, accessibilityText: accessibilityText);
}

String? _join2(String a, String b) {
  if (a.isEmpty) return b.isEmpty ? null : b;
  if (b.isEmpty) return a;
  return '$a, $b';
}

String? _join3(String a, String b, String c) {
  if (a.isEmpty) return _join2(b, c);
  final String? rest = _join2(b, c);
  return rest == null ? a : '$a, $rest';
}
