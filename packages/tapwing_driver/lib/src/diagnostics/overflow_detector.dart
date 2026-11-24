import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/rendering.dart';

/// A layout overflow found by [OverflowDetector.scan].
///
/// [message] uses the exact wording of Flutter's debug-mode report
/// (`A RenderFlex overflowed by 12 pixels on the right.`), so issues
/// fingerprint identically whether they came from `FlutterError.onError`
/// (debug builds) or from this detector (profile and release builds).
class LayoutOverflow {
  /// Creates an overflow record.
  const LayoutOverflow({
    required this.renderObject,
    required this.overflow,
    required this.message,
    required this.globalBounds,
    required this.text,
  });

  /// The overflowing render object (identity is stable across scans, so
  /// callers can deduplicate repeated findings).
  final RenderBox renderObject;

  /// Overflow amount per edge in logical pixels (positive = overflowing).
  final RelativeRect overflow;

  /// Flutter's debug-mode error message for this overflow.
  final String message;

  /// Bounds of the overflowing render object in global logical coordinates.
  final Rect globalBounds;

  /// First text painted inside the overflowing object, to locate it on
  /// screen in reports (profile builds have no `debugCreator`), or `null`.
  final String? text;
}

/// Read-only detector for layout overflows that works in **profile and
/// release builds**, where Flutter compiles its overflow reporting out (the
/// report lives inside `assert` in `DebugOverflowIndicatorMixin`).
///
/// Flutter itself still computes the overflow in every build mode
/// (`RenderFlex._overflow` drives the clip), so the detector recomputes it
/// from public, already laid out geometry: children sizes, `spacing`,
/// `parentData.offset`, and constraints. It never lays out, paints, or
/// mutates anything.
///
/// Reporting mirrors debug mode, which reports an overflow from `paint`:
/// - only objects that are painted (offstage routes, `Offstage`, zero
///   opacity, hidden `IndexedStack` children, and kept-alive or cache-extent
///   list items are skipped), and only when their bounds are visible through
///   every ancestor clip (so cache-extent items are skipped);
/// - `RenderFlex` with a non-empty size;
/// - `RenderConstraintsTransformBox` (`UnconstrainedBox`,
///   `ConstraintsTransformBox`) with a non-empty size and `Clip.none`.
abstract final class OverflowDetector {
  /// Same tolerance as `RenderFlex._hasOverflow`.
  static const double _tolerance = precisionErrorTolerance;

  /// Scans [views] (default: every attached render view) and returns the
  /// visible overflows in paint order.
  ///
  /// When [stats] is given, it receives the number of visited render
  /// objects.
  static List<LayoutOverflow> scan({
    Iterable<RenderView>? views,
    ScanStats? stats,
  }) {
    final List<LayoutOverflow> found = <LayoutOverflow>[];
    int visited = 0;
    void visit(RenderObject node) {
      visited++;
      final LayoutOverflow? hit = _check(node);
      if (hit != null) {
        found.add(hit);
      }
      _visitPaintedChildren(node, visit);
    }

    for (final RenderView view
        in views ?? RendererBinding.instance.renderViews) {
      visit(view);
    }
    stats?.visited = visited;
    return found;
  }

  /// Visits the children that are painted, which is what
  /// [RenderObject.visitChildrenForSemantics] implements across the framework
  /// (offstage overlay entries, `Offstage`, zero opacity, `IndexedStack`,
  /// viewports, list keep-alive buckets), except for nodes that only hide
  /// their subtree from **semantics** while still painting it.
  static void _visitPaintedChildren(
    RenderObject node,
    RenderObjectVisitor visitor,
  ) {
    if (node is RenderExcludeSemantics ||
        node is RenderIgnorePointer ||
        node is RenderAbsorbPointer ||
        node is SemanticsAnnotationsMixin) {
      node.visitChildren(visitor);
    } else if (node is RenderOpacity && node.opacity == 0 ||
        node is RenderAnimatedOpacity && node.opacity.value == 0) {
      // Not painted, even when `alwaysIncludeSemantics` keeps semantics.
      return;
    } else {
      node.visitChildrenForSemantics(visitor);
    }
  }

  /// Overflow of [node] from its laid out geometry alone, ignoring whether
  /// it is painted or visible, or `null` when it does not overflow or is not
  /// a reporting render object.
  static RelativeRect? overflowOf(RenderObject node) => switch (node) {
    final RenderFlex flex => _flexOverflow(flex),
    final RenderConstraintsTransformBox box => _transformBoxOverflow(box),
    _ => null,
  };

  static LayoutOverflow? _check(RenderObject node) {
    final RelativeRect? overflow = overflowOf(node);
    if (overflow == null) {
      return null;
    }
    final RenderBox box = node as RenderBox;
    final Rect? visible = _visibleBounds(box);
    if (visible == null) {
      return null;
    }
    return LayoutOverflow(
      renderObject: box,
      overflow: overflow,
      message: overflowMessage(box.runtimeType.toString(), overflow),
      globalBounds: visible,
      text: _firstText(box),
    );
  }

  /// Recomputes `RenderFlex._overflow` in the same order as
  /// `RenderFlex._computeSizes` (spacing, inflexible children, flexible
  /// children), so the result is bit-identical to the framework's value.
  ///
  /// Flutter's report always marks the trailing edge (`right` / `bottom`),
  /// regardless of text or vertical direction, and so does this.
  static RelativeRect? _flexOverflow(RenderFlex flex) {
    if (!flex.hasSize || flex.size.isEmpty) {
      return null;
    }
    final bool horizontal = flex.direction == Axis.horizontal;
    double mainOf(Size size) => horizontal ? size.width : size.height;
    final double maxMainSize = horizontal
        ? flex.constraints.maxWidth
        : flex.constraints.maxHeight;
    final bool canFlex = maxMainSize.isFinite;

    double accumulated = flex.spacing * (flex.childCount - 1);
    final List<RenderBox> flexible = <RenderBox>[];
    for (
      RenderBox? child = flex.firstChild;
      child != null;
      child = flex.childAfter(child)
    ) {
      if (!child.hasSize) {
        return null;
      }
      final int factor = (child.parentData! as FlexParentData).flex ?? 0;
      if (canFlex && factor > 0) {
        flexible.add(child);
      } else {
        accumulated += mainOf(child.size);
      }
    }
    for (final RenderBox child in flexible) {
      accumulated += mainOf(child.size);
    }
    final double overflow = math.max(0, accumulated - mainOf(flex.size));
    if (overflow <= _tolerance) {
      return null;
    }
    return horizontal
        ? RelativeRect.fromLTRB(0, 0, overflow, 0)
        : RelativeRect.fromLTRB(0, 0, 0, overflow);
  }

  /// Mirrors `RenderConstraintsTransformBox._isOverflowing`: the child's
  /// rect sticks out of the box. Only `Clip.none` reports in debug mode.
  static RelativeRect? _transformBoxOverflow(
    RenderConstraintsTransformBox box,
  ) {
    final RenderBox? child = box.child;
    if (child == null ||
        !box.hasSize ||
        !child.hasSize ||
        box.size.isEmpty ||
        box.clipBehavior != Clip.none) {
      return null;
    }
    final Offset offset = (child.parentData! as BoxParentData).offset;
    final RelativeRect overflow = RelativeRect.fromRect(
      Offset.zero & box.size,
      offset & child.size,
    );
    // Same edge test as `DebugOverflowIndicatorMixin.paintOverflowIndicator`.
    if (overflow.left <= 0 &&
        overflow.top <= 0 &&
        overflow.right <= 0 &&
        overflow.bottom <= 0) {
      return null;
    }
    return RelativeRect.fromLTRB(
      math.max(0, overflow.left),
      math.max(0, overflow.top),
      math.max(0, overflow.right),
      math.max(0, overflow.bottom),
    );
  }

  /// Bounds of [box] in global coordinates, clipped by every ancestor paint
  /// clip and the view, or `null` when nothing of it is visible.
  static Rect? _visibleBounds(RenderBox box) {
    Rect visible = MatrixUtils.transformRect(
      box.getTransformTo(null),
      Offset.zero & box.size,
    );
    RenderObject child = box;
    for (
      RenderObject? parent = box.parent;
      parent != null;
      parent = parent.parent
    ) {
      final Rect? clip = parent.describeApproximatePaintClip(child);
      if (clip != null) {
        visible = visible.intersect(
          MatrixUtils.transformRect(parent.getTransformTo(null), clip),
        );
      }
      if (parent is RenderView) {
        visible = visible.intersect(Offset.zero & parent.size);
      }
      if (visible.isEmpty) {
        return null;
      }
      child = parent;
    }
    return visible;
  }

  static String? _firstText(RenderObject root) {
    String? text;
    void visit(RenderObject node) {
      if (text != null) {
        return;
      }
      if (node is RenderParagraph) {
        final String plain = node.text
            .toPlainText(includeSemanticsLabels: false)
            .trim();
        // `Icon` renders a private-use glyph through RenderParagraph; skip it.
        if (plain.isNotEmpty && !_isIconGlyph(plain)) {
          text = plain.length > 60 ? '${plain.substring(0, 60)}…' : plain;
          return;
        }
      }
      node.visitChildren(visit);
    }

    visit(root);
    return text;
  }

  static bool _isIconGlyph(String text) =>
      text.runes.every((int r) => r >= 0xE000 && r <= 0xF8FF || r >= 0xF0000);

  /// Formats [rect] for reports. `Rect.toString` is stripped in profile and
  /// release builds (it prints `Instance of 'Rect'`).
  static String formatRect(Rect rect) =>
      '${rect.left.toStringAsFixed(1)},${rect.top.toStringAsFixed(1)} '
      '${rect.width.toStringAsFixed(1)}x${rect.height.toStringAsFixed(1)}';

  /// Builds Flutter's debug-mode overflow message
  /// (`DebugOverflowIndicatorMixin._reportOverflow`) for [type].
  static String overflowMessage(String type, RelativeRect overflow) {
    final List<String> parts = <String>[
      if (overflow.left > 0)
        '${_formatPixels(overflow.left)} pixels on the left',
      if (overflow.top > 0) '${_formatPixels(overflow.top)} pixels on the top',
      if (overflow.bottom > 0)
        '${_formatPixels(overflow.bottom)} pixels on the bottom',
      if (overflow.right > 0)
        '${_formatPixels(overflow.right)} pixels on the right',
    ];
    final String text = switch (parts.length) {
      1 => parts.first,
      2 => '${parts.first} and ${parts.last}',
      _ => <String>[
        ...parts.take(parts.length - 1),
        'and ${parts.last}',
      ].join(', '),
    };
    return 'A $type overflowed by $text.';
  }

  static String _formatPixels(double value) => switch (value) {
    > 10.0 => value.toStringAsFixed(0),
    > 1.0 => value.toStringAsFixed(1),
    _ => value.toStringAsPrecision(3),
  };
}

/// Counters filled in by [OverflowDetector.scan].
class ScanStats {
  /// Number of render objects visited by the last scan.
  int visited = 0;
}
