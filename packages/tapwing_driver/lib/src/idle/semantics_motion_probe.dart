import 'dart:ui' as ui;

import 'package:flutter/rendering.dart';

import '../snapshot/semantics_hash_cache.dart';

/// What the semantics tree changed by since the previous
/// [SemanticsMotionProbe.sample].
typedef SemanticsMotion = ({double shift, int contentHash});

/// Read-only probe that measures how far the semantics tree moved between two
/// samples taken on consecutive rendered frames.
///
/// The structural hash rounds every rect to whole units, so content creeping
/// by less than a unit per frame (the long tail of an iOS bouncing fling)
/// flips it on nearly every frame and the tree never looks stable. The probe
/// separates the two things the hash mixes: [SemanticsMotion.contentHash]
/// covers everything except geometry (ids, labels, values, flags), and
/// [SemanticsMotion.shift] is the largest distance any node edge travelled
/// since the previous sample, in physical pixels, over nodes present in both
/// samples.
class SemanticsMotionProbe {
  /// Creates a probe reading the live semantics tree of the primary view
  /// (or [readRoot] when injected for tests).
  SemanticsMotionProbe({SemanticsNode? Function()? readRoot})
    : _readRoot = readRoot ?? _defaultRoot;

  final SemanticsNode? Function() _readRoot;
  Map<int, ui.Rect>? _previous;

  static SemanticsNode? _defaultRoot() =>
      SemanticsHashCache.defaultSemanticsOwner()?.rootSemanticsNode;

  /// Forgets the previous sample so the next [sample] only seeds the probe.
  void reset() {
    _previous = null;
  }

  /// Samples the live tree and returns how far it moved since the previous
  /// sample, or `null` when there is no previous sample to compare with.
  SemanticsMotion? sample() {
    final SemanticsNode? root = _readRoot();
    if (root == null) {
      _previous = null;
      return null;
    }
    final Map<int, ui.Rect> rects = <int, ui.Rect>{};
    int contentHash = 17;
    void visit(
      SemanticsNode node,
      Matrix4 parentTransform,
      SemanticsData data,
    ) {
      final Matrix4 transform = composeSemanticsTransform(
        parentTransform,
        node.transform,
      );
      final ui.Rect raw = MatrixUtils.transformRect(transform, node.rect);
      rects[node.id] = raw.isFinite ? raw : ui.Rect.zero;
      final ExtractedSemanticsFlags flags = extractSemanticsFlags(
        data.flagsCollection,
        clickable: data.hasAction(SemanticsAction.tap),
        scrollable: isSemanticsScrollable(data),
      );
      contentHash = Object.hash(
        contentHash,
        node.id,
        data.label,
        data.value,
        data.hint,
        data.tooltip,
        data.identifier,
        flags.bits,
      );
      if (node.mergeAllDescendantsIntoThisNode) return;
      for (final SemanticsNode child in node.debugListChildrenInOrder(
        DebugSemanticsDumpOrder.traversalOrder,
      )) {
        final SemanticsData childData = child.getSemanticsData();
        if (childData.flagsCollection.isHidden) continue;
        final Matrix4 childParent = identical(child.parent, node)
            ? transform
            : globalSemanticsTransform(child.parent);
        visit(child, childParent, childData);
      }
    }

    visit(root, Matrix4.identity(), root.getSemanticsData());

    final Map<int, ui.Rect>? previous = _previous;
    _previous = rects;
    if (previous == null) return null;
    double shift = 0;
    rects.forEach((int id, ui.Rect now) {
      final ui.Rect? before = previous[id];
      if (before == null) return;
      final double d = <double>[
        (now.left - before.left).abs(),
        (now.top - before.top).abs(),
        (now.right - before.right).abs(),
        (now.bottom - before.bottom).abs(),
      ].reduce((double a, double b) => a > b ? a : b);
      if (d > shift) shift = d;
    });
    return (shift: shift, contentHash: contentHash);
  }
}
