import '../snapshot/snapshot.dart';
import 'maestro_regex.dart';

/// Maestro element traits (`ElementTrait.kt:3-7`, `TraitFilters.kt:9-26`).
enum ElementTrait {
  /// Node carries a non-null `text` attribute (`Filters.hasText`, `Filters.kt:220-224`).
  text('TEXT', 'Has text'),

  /// Node bounding box is within 3 % of a 1:1 aspect ratio (`Filters.isSquare`, `Filters.kt:226-233`).
  square('SQUARE', 'Is square'),

  /// Node `text` attribute has length > 200 (`Filters.hasLongText`, `Filters.kt:235-239`).
  longText('LONG_TEXT', 'Has long text');

  const ElementTrait(this.wireName, this.description);

  /// SCREAMING_SNAKE_CASE wire name matching Maestro's JSON IR.
  final String wireName;

  /// Human-readable description matching `ElementTrait.description`.
  final String description;

  /// Parses an [ElementTrait] from its wire name (`TEXT`, `SQUARE`, `LONG_TEXT`)
  /// or YAML kebab-case spelling (`text`, `square`, `long-text`).
  static ElementTrait parse(String raw) {
    final String normalized = raw.trim().replaceAll('-', '_').toUpperCase();
    for (final ElementTrait trait in ElementTrait.values) {
      if (trait.wireName == normalized) return trait;
    }
    throw ArgumentError.value(raw, 'trait', 'Unknown ElementTrait');
  }
}

/// Maestro element selector, already normalized by the host CLI.
///
/// Matching semantics are ported from Maestro 2.12.1
/// (`Orchestra.buildFilter`, `Filters.kt`, `TraitFilters.kt`,
/// `ViewHierarchy.filterOutOfBounds`).
class Selector {
  /// Creates a selector with optional text, id, state, size, traits, css,
  /// index, and relative constraints.
  Selector({
    this.text,
    this.id,
    this.index,
    this.enabled,
    this.checked,
    this.focused,
    this.selected,
    this.width,
    this.height,
    this.tolerance,
    this.traits,
    this.css,
    this.below,
    this.above,
    this.leftOf,
    this.rightOf,
    this.childOf,
    this.containsChild,
    this.containsDescendants,
  });

  /// Parses a selector from the normalized JSON map sent by the host CLI.
  factory Selector.fromJson(Map<String, Object?> json) => Selector(
    text: json['text'] as String?,
    id: json['id'] as String?,
    index: (json['index'] as num?)?.toInt(),
    enabled: json['enabled'] as bool?,
    checked: json['checked'] as bool?,
    focused: json['focused'] as bool?,
    selected: json['selected'] as bool?,
    width: (json['width'] as num?)?.toInt(),
    height: (json['height'] as num?)?.toInt(),
    tolerance: (json['tolerance'] as num?)?.toInt(),
    traits: (json['traits'] as List<Object?>?)
        ?.map((Object? e) => ElementTrait.parse(e! as String))
        .toList(),
    css: json['css'] as String?,
    below: _sub(json['below']),
    above: _sub(json['above']),
    leftOf: _sub(json['leftOf']),
    rightOf: _sub(json['rightOf']),
    childOf: _sub(json['childOf']),
    containsChild: _sub(json['containsChild']),
    containsDescendants: (json['containsDescendants'] as List<Object?>?)
        ?.map(
          (Object? e) => Selector.fromJson(
            (e! as Map<Object?, Object?>).cast<String, Object?>(),
          ),
        )
        .toList(),
  );

  static Selector? _sub(Object? value) => value == null
      ? null
      : Selector.fromJson(
          (value as Map<Object?, Object?>).cast<String, Object?>(),
        );

  /// Regex or literal matched against `text`, `hintText`, or `accessibilityText`.
  final String? text;

  /// Regex matched against `resource-id` or its suffix after the last `'/'`.
  final String? id;

  /// Zero-based index into matches sorted by `(top, left)`; negative indices
  /// count backward from the end.
  final int? index;

  /// Required `enabled` state, or `null` if unconstrained.
  final bool? enabled;

  /// Required `checked` state, or `null` if unconstrained.
  final bool? checked;

  /// Required `focused` state, or `null` if unconstrained.
  final bool? focused;

  /// Required `selected` state, or `null` if unconstrained.
  final bool? selected;

  /// Required width in physical pixels (within [tolerance]).
  final int? width;

  /// Required height in physical pixels (within [tolerance]).
  final int? height;

  /// Allowed pixel deviation for [width] and [height]; defaults to `0`.
  final int? tolerance;

  /// Required Maestro element traits (`text`, `square`, `long-text`).
  final List<ElementTrait>? traits;

  /// Web-only CSS selector. On Android, `AndroidDriver` inherits
  /// `Driver.queryOnDeviceElements` (`Driver.kt:122-124`) returning an empty
  /// list, so `Filters.css` (`Filters.kt:299-316`) never matches any node.
  final String? css;

  /// Matches elements positioned below an anchor matching this selector.
  final Selector? below;

  /// Matches elements positioned above an anchor matching this selector.
  final Selector? above;

  /// Matches elements positioned left of an anchor matching this selector.
  final Selector? leftOf;

  /// Matches elements positioned right of an anchor matching this selector.
  final Selector? rightOf;

  /// Scopes matching to the subtree of the first element matching this selector
  /// (inclusive of the parent itself).
  final Selector? childOf;

  /// Requires at least one direct child to match this selector.
  final Selector? containsChild;

  /// Requires every selector in this list to match at least one in-bounds
  /// descendant.
  final List<Selector>? containsDescendants;

  // Compiled once per Selector instance rather than on every frame while
  // waiting for an element to appear or disappear.
  late final MaestroRegex? _textRegex = text == null
      ? null
      : MaestroRegex(text!);
  late final MaestroRegex? _idRegex = id == null ? null : MaestroRegex(id!);

  /// Formats a human-readable description in Maestro's error-message style.
  String describe() {
    final List<String> parts = <String>[
      if (text != null) 'Text matching regex: $text',
      if (id != null) 'Id matching regex: $id',
      if (width != null || height != null) 'Size: ${width}x$height',
      if (below != null) 'Below: ${below!.describe()}',
      if (above != null) 'Above: ${above!.describe()}',
      if (leftOf != null) 'Left of: ${leftOf!.describe()}',
      if (rightOf != null) 'Right of: ${rightOf!.describe()}',
      if (containsChild != null) 'Contains child: ${containsChild!.describe()}',
      if (containsDescendants != null)
        'Contains descendants: ${containsDescendants!.map((Selector d) => d.describe()).toList()}',
      if (childOf != null) 'Child of: ${childOf!.describe()}',
      if (traits != null)
        for (final ElementTrait trait in traits!) trait.description,
      if (index != null) 'Index: $index',
      if (enabled != null) enabled! ? 'Enabled' : 'Disabled',
      if (selected != null) selected! ? 'Selected' : 'Not selected',
      if (checked != null) checked! ? 'Checked' : 'Not checked',
      if (focused != null) focused! ? 'Focused' : 'Not focused',
      if (css != null) 'CSS: $css',
    ];
    return parts.join(', ');
  }
}

typedef _NodeFilter = List<KNode> Function(List<KNode> nodes);
typedef _NodePredicate = bool Function(KNode node);

/// Resolves [selector] against [snap] using Maestro 2.12.1's `findElement`
/// pipeline:
/// 1. Filter out-of-bounds nodes (< 10% visible unless a descendant is kept).
/// 2. Apply `childOf` scoping (first parent match's in-bounds subtree).
/// 3. Apply basic predicates + `deepestMatchingElement`.
/// 4. Intersect relative filters (preserving the basic-filter order).
/// 5. Pick [Selector.index] (sorted by top, left) or stable `clickableFirst`.
/// 6. Return the first remaining match, or `null`.
KNode? resolve(Selector selector, Snapshot snap) {
  final List<KNode> visible = _inBounds(snap);
  final List<KNode> matches = _resolveAllIn(selector, visible);
  return matches.isEmpty ? null : matches.first;
}

/// Returns all matches of [selector] in [snap], ordered by the same pipeline
/// as [resolve].
List<KNode> resolveAll(Selector selector, Snapshot snap) =>
    _resolveAllIn(selector, _inBounds(snap));

List<KNode> _resolveAllIn(Selector selector, List<KNode> visible) {
  List<KNode> scope = visible;
  if (selector.childOf != null) {
    final List<KNode> parents = _resolveAllIn(selector.childOf!, visible);
    if (parents.isEmpty) return const <KNode>[];
    scope = _collectVisibleSubtree(parents.first, visible.toSet());
  }
  return _buildFilter(selector, visible)(scope);
}

List<KNode> _collectVisibleSubtree(KNode root, Set<KNode> visibleSet) {
  final List<KNode> subtree = <KNode>[];
  void collect(KNode node) {
    if (visibleSet.contains(node)) subtree.add(node);
    for (final KNode child in node.children) {
      collect(child);
    }
  }

  collect(root);
  return subtree;
}

/// Drops nodes with less than 10% of their area on screen unless a descendant
/// is kept (Maestro `ViewHierarchy.filterOutOfBounds`).
List<KNode> _inBounds(Snapshot snap) {
  final double w = snap.screen.width;
  final double h = snap.screen.height;
  if (w == 0 || h == 0) return snap.nodes;

  final Set<KNode> keep = <KNode>{};
  for (final KNode node in snap.nodes.reversed) {
    final bool childKept = node.children.any(keep.contains);
    if (childKept || visibleFraction(node, w, h) >= 0.1) {
      keep.add(node);
    }
  }
  return snap.nodes.where(keep.contains).toList();
}

/// Visible share of [node]'s area on a screen of the given size, as
/// Maestro's `UiElement.getVisiblePercentage` computes it (used both by the
/// < 10 % out-of-bounds filter and by `scrollUntilVisible`).
double visibleFraction(KNode node, double screenWidth, double screenHeight) {
  final double area = node.bounds.width * node.bounds.height;
  if (area <= 0) return 0;
  // Maestro UiElement.getVisiblePercentage: an element overflowing the screen
  // on all four sides counts as 100% visible even if its area ratio is small.
  if (node.bounds.left <= 0 &&
      node.bounds.top <= 0 &&
      node.bounds.right >= screenWidth &&
      node.bounds.bottom >= screenHeight) {
    return 1.0;
  }
  final double vw =
      (node.bounds.right.clamp(0, screenWidth) -
              node.bounds.left.clamp(0, screenWidth))
          .toDouble();
  final double vh =
      (node.bounds.bottom.clamp(0, screenHeight) -
              node.bounds.top.clamp(0, screenHeight))
          .toDouble();
  if (vw <= 0 || vh <= 0) return 0;
  return (vw * vh) / area;
}

/// Returns whether [node]'s center lies in the target half of the screen
/// (with a 20 % margin) for `scrollUntilVisible` with `centerElement: true`.
///
/// Exact port of `UiElement.isElementNearScreenCenter` (`UiElement.kt:49-68`).
/// [swipeDirection] is the converted swipe direction (`ScrollDirection.toSwipeDirection()`).
bool isElementNearScreenCenter(
  KNode node,
  String swipeDirection,
  double screenWidth,
  double screenHeight,
) {
  final int sw = screenWidth.round();
  final int sh = screenHeight.round();
  final int centerX = sw ~/ 2;
  final int centerY = sh ~/ 2;

  final int bx = node.bounds.left.round();
  final int by = node.bounds.top.round();
  final int bw = node.bounds.right.round() - bx;
  final int bh = node.bounds.bottom.round() - by;
  final int elementCenterX = bx + (bw ~/ 2);
  final int elementCenterY = by + (bh ~/ 2);

  final int margin = switch (swipeDirection) {
    'UP' || 'DOWN' => sh ~/ 5,
    _ => sw ~/ 5,
  };

  return switch (swipeDirection) {
    'RIGHT' => elementCenterX > centerX - margin,
    'LEFT' => elementCenterX < centerX + margin,
    'UP' => elementCenterY < centerY + margin,
    'DOWN' => elementCenterY > centerY - margin,
    _ => false,
  };
}

_NodeFilter _buildFilter(Selector selector, List<KNode> universe) {
  final List<_NodePredicate> basic = _buildBasicPredicates(selector);
  final List<_NodeFilter> relative = _buildRelativeFilters(selector, universe);

  return (List<KNode> nodes) {
    List<KNode> result = basic.isEmpty ? nodes : _deepestMatching(nodes, basic);

    for (final _NodeFilter filter in relative) {
      final Set<KNode> keep = filter(nodes).toSet();
      result = result.where(keep.contains).toList();
    }

    if (selector.index != null) {
      return _selectByIndex(result, selector.index!);
    }
    return _clickableFirst(result);
  };
}

List<_NodePredicate> _buildBasicPredicates(Selector selector) {
  final List<_NodePredicate> predicates = <_NodePredicate>[];

  if (selector._textRegex != null) {
    final MaestroRegex regex = selector._textRegex;
    predicates.add(
      (KNode n) =>
          regex.matchesText(n.text) ||
          regex.matchesText(n.hintText) ||
          regex.matchesText(n.accessibilityText) ||
          (n.error != null &&
              n.error!.isNotEmpty &&
              regex.matchesText(n.error)),
    );
  }
  if (selector._idRegex != null) {
    final MaestroRegex regex = selector._idRegex;
    predicates.add((KNode n) {
      // Do not short-circuit on empty resourceId: Maestro's Android dumper
      // always writes `resource-id=""`, so `id: ".*"` matches nodes without
      // an explicit identifier too.
      if (regex.fullMatch(n.resourceId)) return true;
      final int slash = n.resourceId.lastIndexOf('/');
      return slash != -1 && regex.fullMatch(n.resourceId.substring(slash + 1));
    });
  }
  if (selector.width != null || selector.height != null) {
    final int tolerance = selector.tolerance ?? 0;
    predicates.add((KNode n) {
      final int w = n.bounds.right.round() - n.bounds.left.round();
      final int h = n.bounds.bottom.round() - n.bounds.top.round();
      if (selector.width != null && (w - selector.width!).abs() > tolerance) {
        return false;
      }
      if (selector.height != null && (h - selector.height!).abs() > tolerance) {
        return false;
      }
      return true;
    });
  }
  if (selector.traits != null) {
    // TraitFilters.kt:9-26 -> Filters.kt:220-239
    for (final ElementTrait trait in selector.traits!) {
      switch (trait) {
        case ElementTrait.text:
          predicates.add((KNode n) => n.text != null);
        case ElementTrait.square:
          predicates.add((KNode n) {
            final int w = n.bounds.right.round() - n.bounds.left.round();
            final int h = n.bounds.bottom.round() - n.bounds.top.round();
            return h > 0 && (1.0 - (w / h)).abs() < 0.03;
          });
        case ElementTrait.longText:
          predicates.add((KNode n) => (n.text?.length ?? 0) > 200);
      }
    }
  }
  if (selector.enabled != null) {
    predicates.add((KNode n) => n.enabled == selector.enabled);
  }
  if (selector.selected != null) {
    predicates.add((KNode n) => n.selected == selector.selected);
  }
  if (selector.checked != null) {
    predicates.add((KNode n) => n.checked == selector.checked);
  }
  if (selector.focused != null) {
    predicates.add((KNode n) => n.focused == selector.focused);
  }
  if (selector.css != null) {
    // Orchestra.kt:1748-1752 + Filters.kt:299-316 + Driver.kt:122-124:
    // AndroidDriver inherits empty queryOnDeviceElements, so CSS selectors
    // never match on Android.
    predicates.add((KNode _) => false);
  }

  return predicates;
}

/// Keeps only matching nodes that have no matching descendant, preserving
/// pre-order traversal in O(N) time via ancestor marking.
List<KNode> _deepestMatching(
  List<KNode> nodes,
  List<_NodePredicate> predicates,
) {
  final List<KNode> matched = nodes
      .where((KNode n) => predicates.every((_NodePredicate p) => p(n)))
      .toList();
  final Set<KNode> hasMatchingDescendant = <KNode>{};
  for (final KNode node in matched) {
    for (
      KNode? p = node.parent;
      p != null && hasMatchingDescendant.add(p);
      p = p.parent
    ) {}
  }
  return matched
      .where((KNode n) => !hasMatchingDescendant.contains(n))
      .toList();
}

List<_NodeFilter> _buildRelativeFilters(
  Selector selector,
  List<KNode> universe,
) {
  final List<_NodeFilter> filters = <_NodeFilter>[];

  if (selector.below != null) {
    filters.add(
      _relativeFilter(selector.below!, universe, _RelativePosition.below),
    );
  }
  if (selector.above != null) {
    filters.add(
      _relativeFilter(selector.above!, universe, _RelativePosition.above),
    );
  }
  if (selector.leftOf != null) {
    filters.add(
      _relativeFilter(selector.leftOf!, universe, _RelativePosition.leftOf),
    );
  }
  if (selector.rightOf != null) {
    filters.add(
      _relativeFilter(selector.rightOf!, universe, _RelativePosition.rightOf),
    );
  }
  if (selector.containsChild != null) {
    filters.add(_containsChildFilter(selector.containsChild!, universe));
  }
  if (selector.containsDescendants != null) {
    filters.add(
      _containsDescendantsFilter(selector.containsDescendants!, universe),
    );
  }

  return filters;
}

/// Maestro `Filters.containsChild`: keeps nodes with a direct child among the
/// child filter's matches within the current in-bounds node list.
_NodeFilter _containsChildFilter(Selector childSelector, List<KNode> universe) {
  final _NodeFilter childFilter = _buildFilter(childSelector, universe);
  return (List<KNode> nodes) {
    final Set<KNode> matching = childFilter(nodes).toSet();
    return nodes.where((KNode n) => n.children.any(matching.contains)).toList();
  };
}

/// Maestro `Filters.containsDescendants`: keeps nodes where every descendant
/// selector matches at least one in-bounds descendant when evaluated as a
/// single-node list.
_NodeFilter _containsDescendantsFilter(
  List<Selector> descendantSelectors,
  List<KNode> universe,
) {
  final List<_NodeFilter> descendantFilters = descendantSelectors
      .map((Selector d) => _buildFilter(d, universe))
      .toList();
  return (List<KNode> nodes) {
    final Set<KNode> allowed = nodes.toSet();
    bool matches(_NodeFilter filter, KNode node) {
      if (!allowed.contains(node)) return false;
      return filter(<KNode>[node]).isNotEmpty ||
          node.children.any((KNode child) => matches(filter, child));
    }

    return nodes
        .where(
          (KNode n) => descendantFilters.every(
            (_NodeFilter f) => n.children.any((KNode c) => matches(f, c)),
          ),
        )
        .toList();
  };
}

enum _RelativePosition { below, above, leftOf, rightOf }

/// Port of Maestro's `Filters.relativeTo`: keeps nodes whose top-left corner
/// lies on the requested side of any anchor match.
///
/// Maestro sorts candidates by distance inside `relativeTo`, but the
/// subsequent intersection preserves the basic-filter order, so ordering here
/// has no observable effect.
_NodeFilter _relativeFilter(
  Selector anchorSelector,
  List<KNode> universe,
  _RelativePosition rel,
) {
  final _NodeFilter anchorFilter = _buildFilter(anchorSelector, universe);
  return (List<KNode> nodes) {
    final List<KNode> anchors = anchorFilter(nodes);
    if (anchors.isEmpty) return const <KNode>[];
    bool matchesPosition(KNode candidate, KNode anchor) => switch (rel) {
      _RelativePosition.below =>
        candidate.bounds.top.round() > anchor.bounds.top.round(),
      _RelativePosition.above =>
        candidate.bounds.top.round() < anchor.bounds.top.round(),
      _RelativePosition.leftOf =>
        candidate.bounds.left.round() < anchor.bounds.left.round(),
      _RelativePosition.rightOf =>
        candidate.bounds.left.round() > anchor.bounds.left.round(),
    };
    return nodes
        .where((KNode n) => anchors.any((KNode a) => matchesPosition(n, a)))
        .toList();
  };
}

List<KNode> _selectByIndex(List<KNode> nodes, int index) {
  final List<KNode> sorted = List<KNode>.of(nodes)
    ..sort((KNode a, KNode b) {
      final int topCompare = a.bounds.top.round().compareTo(
        b.bounds.top.round(),
      );
      return topCompare != 0
          ? topCompare
          : a.bounds.left.round().compareTo(b.bounds.left.round());
    });
  final int resolved = index >= 0 ? index : sorted.length + index;
  return (resolved >= 0 && resolved < sorted.length)
      ? <KNode>[sorted[resolved]]
      : const <KNode>[];
}

List<KNode> _clickableFirst(List<KNode> nodes) => <KNode>[
  ...nodes.where((KNode n) => n.clickable),
  ...nodes.where((KNode n) => !n.clickable),
];
