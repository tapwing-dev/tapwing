import 'dart:ui' as ui;

import 'package:flutter/rendering.dart';

/// Cached structural semantics hash backed by a [SemanticsOwner] listener.
///
/// [SemanticsOwner] extends [ChangeNotifier] and only notifies listeners when
/// `PipelineOwner.flushSemantics()` produces a real semantics update. Caching
/// the hash across frames avoids re-walking the entire semantics tree on
/// paint-only animation frames (spinners, blinking cursors, opacity fades) and
/// when [Snapshot.capture] is immediately followed by `computeHash()`.
class SemanticsHashCache {
  /// Creates a semantics hash cache with injectable [readOwner] and
  /// [computeFromRoot] callbacks for testing.
  SemanticsHashCache({
    SemanticsOwner? Function()? readOwner,
    int Function(SemanticsNode root)? computeFromRoot,
  }) : _readOwner = readOwner ?? defaultSemanticsOwner,
       _computeFromRoot = computeFromRoot ?? computeSemanticsHashFromRoot;

  /// Shared singleton used by `Snapshot.computeHash()` and seeded by
  /// `Snapshot.capture()`.
  static final SemanticsHashCache instance = SemanticsHashCache();

  /// Returns the live [SemanticsOwner] attached to the primary [RenderView],
  /// or `null` when semantics are disabled or no view is attached.
  static SemanticsOwner? defaultSemanticsOwner() =>
      RendererBinding.instance.renderViews.firstOrNull?.owner?.semanticsOwner;

  final SemanticsOwner? Function() _readOwner;
  final int Function(SemanticsNode root) _computeFromRoot;

  SemanticsOwner? _owner;
  int? _cachedHash;

  /// Whether a valid hash is currently cached without needing a tree walk.
  bool get hasCachedHash => _cachedHash != null;

  /// Whether this cache is currently listening to a [SemanticsOwner].
  bool get isAttached => _owner != null;

  /// Returns the structural hash of the live semantics tree, computing it on
  /// demand and reusing the cached value until the active [SemanticsOwner]
  /// notifies a change, [invalidate] is called, or the owner changes.
  int hash() {
    final SemanticsOwner? owner = _readOwner();
    if (owner == null) {
      // PipelineOwner disposes its SemanticsOwner before clearing the field,
      // so never call removeListener on a stale owner here.
      _owner = null;
      _cachedHash = null;
      return 0;
    }
    _attachIfNeeded(owner);
    final int? cached = _cachedHash;
    if (cached != null) {
      return cached;
    }
    final SemanticsNode? rootNode = owner.rootSemanticsNode;
    if (rootNode == null) {
      return 0;
    }
    final int computed = _computeFromRoot(rootNode);
    _cachedHash = computed;
    return computed;
  }

  /// Seeds the cache with a [hash] just computed from [owner] (for example
  /// during `Snapshot.capture()`), making an immediate [hash] call O(1).
  ///
  /// Ignored if [owner] is no longer the live [SemanticsOwner].
  void seed(SemanticsOwner owner, int hash) {
    if (!identical(owner, _readOwner())) return;
    _attachIfNeeded(owner);
    _cachedHash = hash;
  }

  /// Marks the cached hash dirty so the next [hash] call re-walks the tree.
  void invalidate() {
    _cachedHash = null;
  }

  /// Detaches from the tracked [SemanticsOwner] (only if it is still the live
  /// owner) and clears any cached hash.
  ///
  /// Guarding `removeListener` with `identical(tracked, _readOwner())` avoids
  /// `ChangeNotifier.debugAssertNotDisposed` throws when `PipelineOwner` has
  /// already disposed the previous `SemanticsOwner` (such as after
  /// `SemanticsHandle.dispose()` between widget tests).
  void dispose() {
    final SemanticsOwner? tracked = _owner;
    _owner = null;
    _cachedHash = null;
    if (tracked != null && identical(tracked, _readOwner())) {
      tracked.removeListener(_onSemanticsChanged);
    }
  }

  void _attachIfNeeded(SemanticsOwner owner) {
    if (identical(_owner, owner)) return;
    // Never call removeListener on the previous `_owner`: when PipelineOwner
    // replaces its SemanticsOwner, the old instance has already been disposed
    // and ChangeNotifier.removeListener would throw in debug mode.
    _owner = owner;
    _cachedHash = null;
    owner.addListener(_onSemanticsChanged);
  }

  void _onSemanticsChanged() {
    _cachedHash = null;
  }
}

/// Extracted selector-relevant boolean flags and their packed hash bitmask.
typedef ExtractedSemanticsFlags = ({
  bool enabled,
  bool focused,
  bool checked,
  bool selected,
  int bits,
});

/// Computes the structural semantics hash directly by walking [rootNode]
/// without allocating `KNode`s, child lists, or joined display strings.
int computeSemanticsHashFromRoot(SemanticsNode rootNode) {
  int hash = 17;
  void visit(SemanticsNode node, Matrix4 parentTransform, SemanticsData data) {
    final Matrix4 transform = composeSemanticsTransform(
      parentTransform,
      node.transform,
    );
    final Rect rawRect = MatrixUtils.transformRect(transform, node.rect);
    final Rect rect = rawRect.isFinite ? rawRect : Rect.zero;
    final ExtractedSemanticsFlags flags = extractSemanticsFlags(
      data.flagsCollection,
      clickable: data.hasAction(SemanticsAction.tap),
      scrollable: isSemanticsScrollable(data),
    );
    hash = accumulateSemanticsHash(hash, data, flags.bits, rect);

    if (!node.mergeAllDescendantsIntoThisNode) {
      for (final SemanticsNode child in node.debugListChildrenInOrder(
        DebugSemanticsDumpOrder.traversalOrder,
      )) {
        final SemanticsData childData = child.getSemanticsData();
        if (!childData.flagsCollection.isHidden) {
          final Matrix4 childParentTransform = identical(child.parent, node)
              ? transform
              : globalSemanticsTransform(child.parent);
          visit(child, childParentTransform, childData);
        }
      }
    }
  }

  visit(rootNode, Matrix4.identity(), rootNode.getSemanticsData());
  return hash;
}

/// Composes [parentTransform] with an optional local [nodeTransform].
Matrix4 composeSemanticsTransform(
  Matrix4 parentTransform,
  Matrix4? nodeTransform,
) => nodeTransform == null
    ? parentTransform
    : parentTransform.multiplied(nodeTransform);

/// Computes the global transform of [node] by walking its structural `parent`
/// chain (the coordinate space `SemanticsNode.transform` is relative to).
Matrix4 globalSemanticsTransform(SemanticsNode? node) {
  if (node == null) return Matrix4.identity();
  final List<Matrix4> chain = <Matrix4>[];
  for (SemanticsNode? cur = node; cur != null; cur = cur.parent) {
    final Matrix4? t = cur.transform;
    if (t != null) chain.add(t);
  }
  final Matrix4 out = Matrix4.identity();
  for (int i = chain.length - 1; i >= 0; i--) {
    out.multiply(chain[i]);
  }
  return out;
}

/// Returns `true` if [data] advertises any scroll semantics action.
bool isSemanticsScrollable(SemanticsData data) =>
    data.hasAction(SemanticsAction.scrollUp) ||
    data.hasAction(SemanticsAction.scrollDown) ||
    data.hasAction(SemanticsAction.scrollLeft) ||
    data.hasAction(SemanticsAction.scrollRight);

/// Extracts selector-visible flags and packs them into a bitmask for hashing.
ExtractedSemanticsFlags extractSemanticsFlags(
  ui.SemanticsFlags f, {
  required bool clickable,
  required bool scrollable,
}) {
  final bool hasChecked = f.isChecked != ui.CheckedState.none;
  final bool hasToggled = f.isToggled != ui.Tristate.none;
  final bool enabled = f.isEnabled != ui.Tristate.isFalse;
  final bool focused = f.isFocused == ui.Tristate.isTrue;
  final bool checked = hasChecked
      ? f.isChecked == ui.CheckedState.isTrue
      : hasToggled && f.isToggled == ui.Tristate.isTrue;
  final bool selected = f.isSelected == ui.Tristate.isTrue;
  // Include clickable, isTextField, scopesRoute, and scrollable so hash-gated
  // waits and retryTapIfNoChange detect all selector-visible state changes.
  final int bits =
      (enabled ? 1 : 0) |
      (focused ? 2 : 0) |
      (checked ? 4 : 0) |
      (selected ? 8 : 0) |
      (clickable ? 16 : 0) |
      (f.isTextField ? 32 : 0) |
      (f.scopesRoute ? 64 : 0) |
      (scrollable ? 128 : 0);
  return (
    enabled: enabled,
    focused: focused,
    checked: checked,
    selected: selected,
    bits: bits,
  );
}

/// Folds one visible semantics node's text, identifier, flags, and rounded
/// bounds into [hash].
int accumulateSemanticsHash(
  int hash,
  SemanticsData data,
  int flagBits,
  Rect rect,
) => Object.hash(
  hash,
  data.label,
  data.value,
  data.hint,
  data.tooltip,
  data.identifier,
  flagBits,
  rect.left.round(),
  rect.top.round(),
  rect.width.round(),
  rect.height.round(),
);
